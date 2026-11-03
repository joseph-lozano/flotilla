//! A small blocking REST client for the boat calls Flotilla uses.
//!
//! Blocking keeps the core runtime-free (GPUI doesn't use tokio); callers run these on a
//! background thread.

use crate::error::{ApiError, BoatError};
use crate::event::RawEvent;
use crate::util::redact;
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The longest a command may run in one synchronous `exec`: boat's edge answers a request
/// still open at about 60 s with a 502, though the command runs on.
const SYNC_EXEC_MAX: u32 = 50;

pub const API_BASE: &str = "https://boat.dev/api/v1";
/// The model catalog. Outside v1, public, and missing from the OpenAPI file.
pub const CATALOG_URL: &str = "https://boat.dev/api/provider-models";

type BoatResult<T> = Result<T, BoatError>;

/// How long a prompt with files may take to upload.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// How a request treats `ttlSeconds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ttl {
    /// Leave the field out.
    Omit,
    Seconds(u32),
    /// `null`: no auto-stop (paid plans; the free trial refused it).
    Never,
}

impl Ttl {
    fn insert_into(self, body: &mut Map<String, Value>) {
        match self {
            Ttl::Omit => {}
            Ttl::Seconds(s) => {
                body.insert("ttlSeconds".into(), json!(s));
            }
            Ttl::Never => {
                body.insert("ttlSeconds".into(), Value::Null);
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    pub id: String,
    #[serde(default)]
    pub state: String,
    pub name: Option<String>,
    #[serde(default)]
    pub labels: Option<HashMap<String, Value>>,
    /// The environment's name (not its id).
    pub environment: Option<String>,
    pub ttl_seconds: Option<i64>,
    pub archive_after: Option<String>,
    pub stopped_at: Option<String>,
    pub data_expired_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    #[serde(rename = "type")]
    pub machine_type: Option<String>,
    /// The last stop: its snapshot is done when `status` is `completed`.
    #[serde(default)]
    pub stop: Option<SandboxStop>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxStop {
    pub status: Option<String>,
}

impl Sandbox {
    pub fn label(&self, key: &str) -> Option<String> {
        match self.labels.as_ref()?.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            v => Some(v.to_string()),
        }
    }

    /// The machine is up and can take a prompt.
    pub fn is_up(&self) -> bool {
        matches!(self.state.as_str(), "ready" | "idle" | "running")
    }

    /// Stopped, with the snapshot a fork copies written.
    pub fn is_stopped(&self) -> bool {
        self.state == "archived"
            && self.stop.as_ref().and_then(|s| s.status.as_deref()) == Some("completed")
    }
}

/// Only the fields Flotilla keeps. `GET /environments` also returns secret values, which are
/// dropped here and never stored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

/// A GitHub repo boat can clone (`GET /repos`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoatRepo {
    /// What environments take to add the repo.
    pub database_id: String,
    pub full_name: String,
    #[serde(default)]
    pub private: bool,
}

/// A finished command (`POST /commands` without `detached`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub timed_out: bool,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

/// A background command, with the output since the offsets asked for.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandStatus {
    #[serde(default)]
    pub running: bool,
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    pub stdout_next_offset: Option<u64>,
    pub stderr_next_offset: Option<u64>,
    #[serde(default)]
    pub oom_killed: bool,
    pub signal: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub access_tier: Option<String>,
    pub max_active_sandboxes: Option<i64>,
    pub active_sandboxes: Option<i64>,
    pub can_start: Option<bool>,
    pub start_blocked_reason: Option<String>,
    pub blocked_reason: Option<String>,
    #[serde(skip)]
    pub raw: Value,
}

pub use flotilla_proto::{ExtraStarts, Usage};

/// Starts boat must have left this hour before Flotilla spends one on its own work (a setup
/// run, waking a VM to archive it): those are kept for threads.
pub const START_RESERVE: i64 = 5;

impl Limits {
    /// The starts left this hour, as boat counts them: as many as there could be on a plan
    /// whose starts aren't counted (boat gives no `remaining` then), or when boat gives no count,
    /// unless boat says something blocks them (then none to count on).
    pub fn starts_left(&self) -> Option<i64> {
        let unlimited = self.raw.get("unlimited").and_then(Value::as_bool) == Some(true)
            || self
                .raw
                .pointer("/starts/unlimited")
                .and_then(Value::as_bool)
                == Some(true);
        let blocked = self.can_start == Some(false)
            || self
                .blocked_reason
                .as_deref()
                .is_some_and(|r| !r.is_empty());
        match self
            .raw
            .pointer("/starts/hour/remaining")
            .and_then(Value::as_i64)
        {
            Some(left) if !unlimited => Some(left),
            // Boat blocking starts outranks a plan's having no count.
            _ if blocked => None,
            _ => Some(i64::MAX),
        }
    }

    /// Whether boat says it has starts to spare for Flotilla's own work: it would start one
    /// now, and has said at least [`START_RESERVE`] are left this hour.
    pub fn spares_starts(&self) -> bool {
        self.can_start != Some(false) && self.starts_left().is_some_and(|r| r >= START_RESERVE)
    }

    pub fn usage(&self) -> Usage {
        let raw = &self.raw;
        let int = |path: &str| raw.pointer(path).and_then(Value::as_i64);
        let pair = |used: &str, limit: &str| int(used).zip(int(limit));
        let unlimited = raw.get("unlimited").and_then(Value::as_bool) == Some(true)
            || raw.pointer("/starts/unlimited").and_then(Value::as_bool) == Some(true);
        let starts = |w: &str| {
            if unlimited {
                None
            } else {
                pair(&format!("/starts/{w}/used"), &format!("/starts/{w}/limit"))
            }
        };
        let hours = |path: &str| {
            raw.pointer(path)
                .and_then(Value::as_f64)
                .map(|h| h.round() as i64)
        };
        let blocked = match (
            self.can_start,
            &self.start_blocked_reason,
            &self.blocked_reason,
        ) {
            (_, _, Some(r)) | (Some(false), Some(r), _) if !r.is_empty() => Some(r.clone()),
            (Some(false), _, _) => Some("boat isn't starting VMs right now".into()),
            _ => None,
        };
        let text = |path: &str| {
            raw.pointer(path)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let cents = |path: &str| {
            raw.pointer(path)
                .and_then(Value::as_f64)
                .map(|d| (d * 100.).round() as i64)
        };
        let flag = |path: &str| raw.pointer(path).and_then(Value::as_bool);
        // A plan without extra starts (`available: false`) has nothing to show unless they're on.
        let extra_starts = raw
            .get("keepGoing")
            .filter(|_| {
                flag("/keepGoing/available") != Some(false)
                    || flag("/keepGoing/enabled") == Some(true)
            })
            .map(|_| ExtraStarts {
                on: flag("/keepGoing/enabled") == Some(true),
                used: int("/keepGoing/usedUnits").unwrap_or(0),
                spend_cents: cents("/keepGoing/usedDollars").zip(cents("/keepGoing/capDollars")),
                cap_reached: flag("/keepGoing/capReached") == Some(true),
            });
        Usage {
            vms: self.active_sandboxes.zip(self.max_active_sandboxes),
            minute: starts("minute"),
            hour: starts("hour"),
            day: starts("day"),
            vm_hours: hours("/planIncluded/vmHoursRemaining").zip(hours("/planIncluded/vmHours")),
            renews: text("/planIncluded/periodEnd"),
            blocked,
            starts_free: text("/startWindowFull/freesAt"),
            extra_starts,
            read_ms: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRun {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub done: bool,
    pub conversation_id: Option<String>,
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub prompt_id: String,
    pub conversation_id: String,
    pub prompt_run: Option<PromptRun>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerResponse {
    /// The steer's own id (its events have `taskId` = this, `id` = `steer-<this>`).
    pub prompt_id: String,
    pub native: Option<bool>,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub created_at: Option<String>,
    pub last_prompt_at: Option<String>,
    #[serde(default)]
    pub prompts: i64,
    #[serde(default)]
    pub running: bool,
    pub last_harness: Option<String>,
    pub last_model: Option<String>,
    pub last_prompt_preview: Option<String>,
    #[serde(default)]
    pub current: bool,
}

/// What a thread's settings send with every prompt. All four are locked at creation.
#[derive(Debug, Clone)]
pub struct PromptRequest<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub fast: bool,
    /// `None` starts a new conversation (`new: true`).
    pub conversation_id: Option<&'a str>,
    pub prompt: &'a str,
    pub attachments: &'a [Attachment],
}

/// A file sent with a prompt, as boat takes it (not in the published OpenAPI file; boat's
/// CLI sends it, and its prompt events echo it back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub base64_data: String,
}

#[derive(Debug, Clone)]
pub struct CreateSandbox<'a> {
    /// The environment's name.
    pub environment: &'a str,
    pub name: &'a str,
    pub ttl: Ttl,
    pub labels: &'a [(String, String)],
    pub machine_type: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub enum EventsPage {
    /// `304`: the page is unchanged since the `If-None-Match` tag.
    NotModified,
    Page {
        events: Vec<RawEvent>,
        next_cursor: Option<String>,
        has_more: bool,
        etag: Option<String>,
    },
}

struct Resp {
    status: u16,
    headers: HeaderMap,
    body: Value,
}

#[derive(Clone)]
pub struct BoatClient {
    http: Client,
    base: String,
    catalog_url: String,
    key: String,
    log: bool,
}

impl std::fmt::Debug for BoatClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoatClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl BoatClient {
    pub fn new(api_key: String) -> BoatResult<BoatClient> {
        Self::with_base(api_key, API_BASE, CATALOG_URL)
    }

    pub fn with_base(api_key: String, base: &str, catalog_url: &str) -> BoatResult<BoatClient> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("flotilla/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| BoatError::Http(e.to_string()))?;
        Ok(BoatClient {
            http,
            base: base.trim_end_matches('/').to_string(),
            catalog_url: catalog_url.to_string(),
            key: api_key,
            log: std::env::var_os("FLOTILLA_LOG").is_some(),
        })
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> BoatResult<Resp> {
        self.send_with(method, path, body, headers, None)
    }

    fn send_with(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
        timeout: Option<Duration>,
    ) -> BoatResult<Resp> {
        let url = if path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        };
        let mut req = self.http.request(method.clone(), &url);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        if !path.starts_with("https://") {
            req = req.bearer_auth(&self.key);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let started = Instant::now();
        let res = req
            .send()
            .map_err(|e| BoatError::Http(redact(&e.to_string())))?;
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let text = res
            .text()
            .map_err(|e| BoatError::Http(redact(&e.to_string())))?;
        if self.log {
            eprintln!(
                "[boat] {method} {} → {status} ({} ms)",
                redact(path),
                started.elapsed().as_millis()
            );
        }
        if status == 304 {
            return Ok(Resp {
                status,
                headers,
                body: Value::Null,
            });
        }
        let body: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|_| {
                BoatError::Decode(format!(
                    "{status} from {}: {}",
                    redact(path),
                    redact(&crate::util::ellipsize(&text, 300))
                ))
            })?
        };
        let ok_flag = body.get("ok").and_then(Value::as_bool);
        if !(200..300).contains(&status) || ok_flag == Some(false) {
            return Err(api_error(status, &headers, &body));
        }
        Ok(Resp {
            status,
            headers,
            body,
        })
    }

    fn get(&self, path: &str) -> BoatResult<Value> {
        Ok(self.send(Method::GET, path, None, &[])?.body)
    }

    fn post(&self, path: &str, body: &Value) -> BoatResult<Value> {
        Ok(self.send(Method::POST, path, Some(body), &[])?.body)
    }

    // ---- account ----

    /// Whose the key is (`GET /me`): it works.
    pub fn me(&self) -> BoatResult<Value> {
        self.get("/me")
    }

    /// The account's API keys (`GET /api-keys`), with each one's prefix, last four and scope.
    pub fn api_keys(&self) -> BoatResult<Value> {
        self.get("/api-keys")
    }

    pub fn limits(&self) -> BoatResult<Limits> {
        let body = self.get("/limits")?;
        let mut limits: Limits = decode(&body)?;
        limits.raw = body;
        Ok(limits)
    }

    pub fn environments(&self) -> BoatResult<Vec<Environment>> {
        let body = self.get("/environments")?;
        decode(body.get("environments").unwrap_or(&Value::Null))
    }

    /// The model catalog, raw. Parse it with [`crate::catalog::Catalog::from_value`].
    pub fn provider_models(&self) -> BoatResult<Value> {
        Ok(self
            .send(Method::GET, &self.catalog_url.clone(), None, &[])?
            .body)
    }

    // ---- sandboxes ----

    pub fn create_sandbox(
        &self,
        req: &CreateSandbox,
        idempotency_key: &str,
    ) -> BoatResult<Sandbox> {
        let mut body = Map::new();
        body.insert("environment".into(), json!(req.environment));
        body.insert("name".into(), json!(req.name));
        let labels: Map<String, Value> = req
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        body.insert("labels".into(), Value::Object(labels));
        req.ttl.insert_into(&mut body);
        if let Some(t) = req.machine_type {
            body.insert("type".into(), json!(t));
        }
        let resp = self.send(
            Method::POST,
            "/sandboxes",
            Some(&Value::Object(body)),
            &[("Idempotency-Key", idempotency_key)],
        )?;
        // The create response shows `labels: {}` and `environment: null` even when both were
        // set, so callers should read the sandbox back with GET before trusting those.
        sandbox_from(&resp.body)
    }

    pub fn get_sandbox(&self, id: &str) -> BoatResult<Sandbox> {
        sandbox_from(&self.get(&format!("/sandboxes/{}", enc(id)))?)
    }

    /// Every sandbox carrying all of `labels` (ANDed), including stopped ones, across pages.
    pub fn list_sandboxes(&self, labels: &[(&str, &str)]) -> BoatResult<Vec<Sandbox>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = String::from("/sandboxes?limit=200");
            for (k, v) in labels {
                q.push_str(&format!("&label={}", enc(&format!("{k}={v}"))));
            }
            if let Some(c) = &cursor {
                q.push_str(&format!("&cursor={}", enc(c)));
            }
            let body = self.get(&q)?;
            let page: Vec<Sandbox> = decode(body.get("sandboxes").unwrap_or(&Value::Null))?;
            out.extend(page);
            let (next, more) = page_info(&body);
            match next {
                Some(n) if more => cursor = Some(n),
                _ => break,
            }
        }
        Ok(out)
    }

    /// `PATCH` the TTL (deadline becomes now + TTL) and/or the name (deadline unchanged).
    pub fn update_sandbox(&self, id: &str, ttl: Ttl, name: Option<&str>) -> BoatResult<Sandbox> {
        let mut body = Map::new();
        ttl.insert_into(&mut body);
        if let Some(n) = name {
            body.insert("name".into(), json!(n));
        }
        let resp = self.send(
            Method::PATCH,
            &format!("/sandboxes/{}", enc(id)),
            Some(&Value::Object(body)),
            &[],
        )?;
        sandbox_from(&resp.body)
    }

    pub fn stop_sandbox(&self, id: &str) -> BoatResult<()> {
        self.post(&format!("/sandboxes/{}/stop", enc(id)), &json!({}))?;
        Ok(())
    }

    /// Always pass a TTL: a sandbox keeps its last TTL across resumes.
    pub fn resume_sandbox(&self, id: &str, ttl: Ttl) -> BoatResult<()> {
        let mut body = Map::new();
        ttl.insert_into(&mut body);
        self.post(
            &format!("/sandboxes/{}/resume", enc(id)),
            &Value::Object(body),
        )?;
        Ok(())
    }

    /// Delete a VM. One already gone is a 404, as callers expect, even though boat refuses a
    /// key that may delete only its own VMs (`sandbox.delete-own`) with a 403 for any VM it
    /// didn't make, gone ones included. One that's there but another key made (another
    /// Flotilla's, or an imported database's) stays refused.
    pub fn delete_sandbox(&self, id: &str) -> BoatResult<()> {
        match self.send(
            Method::DELETE,
            &format!("/sandboxes/{}", enc(id)),
            None,
            &[("X-Ascii-Confirm-Delete", id)],
        ) {
            Ok(_) => Ok(()),
            Err(e) if e.status() == Some(403) && e.is("api_key_action_forbidden") => {
                match self.get_sandbox(id) {
                    Err(gone) if gone.status() == Some(404) => Err(gone),
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// A preview URL for `port`: private, carrying `?_token=` (never log it unredacted), or
    /// `public`, with none, so anyone with it can open it. Go through `crate::public`, which
    /// knows which each port should be: hosting a public port privately makes it private (with
    /// a new token), and the other way round. An answer that isn't what was asked is an error,
    /// so a link is never shown as private while boat has it public.
    pub fn host_port(&self, id: &str, port: u16, public: bool) -> BoatResult<String> {
        let body = self.post(
            &format!("/sandboxes/{}/host", enc(id)),
            &json!({"port": port, "public": public}),
        )?;
        hosted_url(&body, public)
    }

    // ---- projects: environments, repos, forks, commands ----

    /// Make a named environment (all flags on, not safe for third parties). boat answers
    /// with the full list.
    pub fn create_environment(&self, name: &str) -> BoatResult<Environment> {
        let body = self.post("/environments", &json!({"name": name}))?;
        let all: Vec<Environment> = decode(body.get("environments").unwrap_or(&Value::Null))?;
        all.into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| BoatError::Decode(format!("environment {name} missing after create")))
    }

    /// Change an environment's flags (`{"passGithub": true, …}`). Mints a new version.
    pub fn update_environment(&self, id: &str, changes: &Value) -> BoatResult<()> {
        self.send(
            Method::PUT,
            &format!("/environments/{}", enc(id)),
            Some(changes),
            &[],
        )?;
        Ok(())
    }

    /// Soft-delete an environment (never the default one).
    pub fn delete_environment(&self, id: &str) -> BoatResult<()> {
        self.send(
            Method::DELETE,
            &format!("/environments/{}", enc(id)),
            None,
            &[],
        )?;
        Ok(())
    }

    /// The GitHub repos boat can see (after syncing with GitHub), filtered by `q`.
    pub fn repos(&self, q: &str) -> BoatResult<Vec<BoatRepo>> {
        let body = self.get(&format!("/repos?sync=true&limit=200&q={}", enc(q)))?;
        let mut out = Vec::new();
        for inst in body
            .get("installations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.extend(decode::<Vec<BoatRepo>>(
                inst.get("repositories").unwrap_or(&Value::Null),
            )?);
        }
        Ok(out)
    }

    /// Clone `repo` (boat's `databaseId`) into every new sandbox of the environment, at
    /// `/home/user/<repo name>`. Mints a new version.
    pub fn add_environment_repo(
        &self,
        env_id: &str,
        repository_id: &str,
        base_branch: &str,
    ) -> BoatResult<()> {
        self.post(
            &format!("/environments/{}/repos", enc(env_id)),
            &json!({"repositoryId": repository_id, "baseBranch": base_branch}),
        )?;
        Ok(())
    }

    /// Copy a sandbox from its newest snapshot (fork a stopped one: a running one's newest
    /// snapshot can be ~40 s old). Labels are set here (a fork doesn't inherit them); the
    /// name can't be, so it's renamed after.
    /// `machine_type`: fork onto another machine size (boat refuses a smaller one the disk
    /// doesn't fit); `None`: the source's.
    pub fn fork_sandbox(
        &self,
        source: &str,
        labels: &[(String, String)],
        ttl: Ttl,
        machine_type: Option<&str>,
        idempotency_key: &str,
    ) -> BoatResult<Sandbox> {
        let mut body = Map::new();
        let labels: Map<String, Value> =
            labels.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        body.insert("labels".into(), Value::Object(labels));
        ttl.insert_into(&mut body);
        if let Some(t) = machine_type {
            body.insert("type".into(), json!(t));
        }
        let resp = self.send(
            Method::POST,
            &format!("/sandboxes/{}/fork", enc(source)),
            Some(&Value::Object(body)),
            &[("Idempotency-Key", idempotency_key)],
        )?;
        sandbox_from(&resp.body)
    }

    /// Run a command and wait for it (at most 600 s). `cwd` is relative to `/home/user`.
    /// One that may run longer than boat's edge holds a request runs in the background and
    /// is followed until it ends.
    pub fn exec(
        &self,
        sandbox_id: &str,
        command: &str,
        cwd: Option<&str>,
        timeout_seconds: u32,
    ) -> BoatResult<ExecResult> {
        let timeout_seconds = timeout_seconds.clamp(1, 600);
        if timeout_seconds > SYNC_EXEC_MAX {
            return self.exec_followed(
                sandbox_id,
                command,
                cwd,
                Duration::from_secs(timeout_seconds.into()),
            );
        }
        let mut body = json!({"command": command, "timeoutSeconds": timeout_seconds});
        if let Some(c) = cwd {
            body["cwd"] = json!(c);
        }
        decode(&self.post(&format!("/sandboxes/{}/commands", enc(sandbox_id)), &body)?)
    }

    /// `exec` for a long command: start it in the background, collect its output, and stop it
    /// after `timeout` (reported as `timed_out`, as boat reports a synchronous one).
    fn exec_followed(
        &self,
        sandbox_id: &str,
        command: &str,
        cwd: Option<&str>,
        timeout: Duration,
    ) -> BoatResult<ExecResult> {
        let pid = self.exec_detached(sandbox_id, command, cwd)?;
        let started = Instant::now();
        let mut done = ExecResult {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
        let (mut out_at, mut err_at) = (0u64, 0u64);
        let mut pause = Duration::from_millis(250);
        let mut failures = 0;
        loop {
            let st = match self.command_status(sandbox_id, pid, out_at, err_at) {
                Ok(st) => {
                    failures = 0;
                    st
                }
                // A failed read: try again until the time's up, then stop the command.
                Err(e) => {
                    failures += 1;
                    if failures >= 5 || started.elapsed() > timeout {
                        let _ = self.stop_command(sandbox_id, pid);
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            done.stdout.push_str(&st.stdout);
            done.stderr.push_str(&st.stderr);
            out_at = st
                .stdout_next_offset
                .unwrap_or(out_at + st.stdout.len() as u64);
            err_at = st
                .stderr_next_offset
                .unwrap_or(err_at + st.stderr.len() as u64);
            if !st.running {
                done.exit_code = st.exit_code;
                return Ok(done);
            }
            if started.elapsed() > timeout {
                let _ = self.stop_command(sandbox_id, pid);
                done.timed_out = true;
                return Ok(done);
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_secs(1));
        }
    }

    /// Start a command in the background (no time limit). Returns its process id.
    pub fn exec_detached(
        &self,
        sandbox_id: &str,
        command: &str,
        cwd: Option<&str>,
    ) -> BoatResult<i64> {
        let mut body = json!({"command": command, "detached": true});
        if let Some(c) = cwd {
            body["cwd"] = json!(c);
        }
        let v = self.post(&format!("/sandboxes/{}/commands", enc(sandbox_id)), &body)?;
        v.get("processId")
            .or_else(|| v.get("pid"))
            .and_then(Value::as_i64)
            .ok_or_else(|| BoatError::Decode("no processId in the answer".into()))
    }

    /// A background command's state and the output written since the given byte offsets.
    pub fn command_status(
        &self,
        sandbox_id: &str,
        process_id: i64,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> BoatResult<CommandStatus> {
        decode(&self.get(&format!(
            "/sandboxes/{}/commands/{process_id}?stdoutOffset={stdout_offset}&stderrOffset={stderr_offset}",
            enc(sandbox_id)
        ))?)
    }

    /// Stop a background command and everything it started.
    pub fn stop_command(&self, sandbox_id: &str, process_id: i64) -> BoatResult<()> {
        self.send(
            Method::DELETE,
            &format!("/sandboxes/{}/commands/{process_id}", enc(sandbox_id)),
            None,
            &[],
        )?;
        Ok(())
    }

    // ---- agents ----

    pub fn prompt(&self, sandbox_id: &str, req: &PromptRequest) -> BoatResult<PromptResponse> {
        let path = format!("/sandboxes/{}/prompt", enc(sandbox_id));
        // Up to 64 MB of files takes longer to upload than the usual 30 s allows.
        let timeout = (!req.attachments.is_empty()).then_some(UPLOAD_TIMEOUT);
        decode(
            &self
                .send_with(Method::POST, &path, Some(&prompt_body(req)), &[], timeout)?
                .body,
        )
    }

    pub fn prompt_status(&self, sandbox_id: &str, prompt_id: &str) -> BoatResult<PromptRun> {
        let body = self.get(&format!(
            "/sandboxes/{}/prompts/{}",
            enc(sandbox_id),
            enc(prompt_id)
        ))?;
        decode(body.get("promptRun").unwrap_or(&body))
    }

    pub fn steer(
        &self,
        sandbox_id: &str,
        conversation_id: &str,
        message: &str,
    ) -> BoatResult<SteerResponse> {
        decode(&self.post(
            &format!("/sandboxes/{}/steer", enc(sandbox_id)),
            &json!({"message": message, "conversation": conversation_id}),
        )?)
    }

    /// Interrupt one conversation. (Without `conversation`, boat stops every conversation.)
    pub fn interrupt(&self, sandbox_id: &str, conversation_id: &str) -> BoatResult<()> {
        self.post(
            &format!(
                "/sandboxes/{}/interrupt?conversation={}",
                enc(sandbox_id),
                enc(conversation_id)
            ),
            &json!({}),
        )?;
        Ok(())
    }

    pub fn conversations(&self, sandbox_id: &str) -> BoatResult<Vec<Conversation>> {
        let body = self.get(&format!("/sandboxes/{}/conversations", enc(sandbox_id)))?;
        decode(body.get("conversations").unwrap_or(&Value::Null))
    }

    /// One page of a conversation's events in `(timestamp, id)` order, after `cursor`.
    pub fn events_page(
        &self,
        sandbox_id: &str,
        conversation_id: &str,
        cursor: Option<&str>,
        limit: u32,
        if_none_match: Option<&str>,
    ) -> BoatResult<EventsPage> {
        let mut q = format!(
            "/sandboxes/{}/events?conversation={}&sort=asc&limit={limit}",
            enc(sandbox_id),
            enc(conversation_id)
        );
        if let Some(c) = cursor {
            q.push_str(&format!("&cursor={}", enc(c)));
        }
        let headers: Vec<(&str, &str)> = if_none_match
            .map(|t| vec![("If-None-Match", t)])
            .unwrap_or_default();
        let resp = self.send(Method::GET, &q, None, &headers)?;
        if resp.status == 304 {
            return Ok(EventsPage::NotModified);
        }
        // HeaderMap lookups are case-insensitive; boat spells it `Etag`.
        let etag = resp
            .headers
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let events = match resp.body.get("events") {
            Some(Value::Array(a)) => a.iter().cloned().map(RawEvent::from_value).collect(),
            _ => {
                return Err(BoatError::Decode(
                    "events response has no events array".into(),
                ));
            }
        };
        let (next_cursor, has_more) = page_info(&resp.body);
        Ok(EventsPage::Page {
            events,
            next_cursor,
            has_more,
            etag,
        })
    }
}

fn prompt_body(req: &PromptRequest) -> Value {
    let mut body = Map::new();
    body.insert("provider".into(), json!(req.provider));
    body.insert("model".into(), json!(req.model));
    if let Some(e) = req.reasoning_effort {
        body.insert("reasoningEffort".into(), json!(e));
    }
    // Always explicit: if omitted, boat uses whatever the dashboard says at that moment.
    body.insert("fast".into(), json!(req.fast));
    // Never omit both `new` and `conversationId`: boat would pick the most recent conversation.
    match req.conversation_id {
        Some(c) => body.insert("conversationId".into(), json!(c)),
        None => body.insert("new".into(), json!(true)),
    };
    body.insert("prompt".into(), json!(req.prompt));
    if !req.attachments.is_empty() {
        let files = req.attachments.iter().map(|a| json!({"filename": a.filename, "mimeType": a.mime_type, "base64Data": a.base64_data}));
        body.insert("attachments".into(), Value::Array(files.collect()));
    }
    Value::Object(body)
}

/// `POST /host`'s URL, if boat hosted the port as asked (`access` is `public` or `private`;
/// a public URL has no `_token`).
fn hosted_url(body: &Value, public: bool) -> BoatResult<String> {
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| BoatError::Decode("host response has no url".into()))?;
    let want = if public { "public" } else { "private" };
    let access = body
        .get("access")
        .and_then(Value::as_str)
        .unwrap_or(if url.contains("_token=") {
            "private"
        } else {
            "public"
        });
    if access != want || url.contains("_token=") == public {
        return Err(BoatError::Decode(format!(
            "boat hosted the port as {access}, not {want}"
        )));
    }
    Ok(url.to_string())
}

fn page_info(body: &Value) -> (Option<String>, bool) {
    let pi = body.get("pageInfo");
    let next = pi
        .and_then(|p| p.get("nextCursor"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let more = pi
        .and_then(|p| p.get("hasMore"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (next, more)
}

fn sandbox_from(body: &Value) -> BoatResult<Sandbox> {
    decode(body.get("sandbox").unwrap_or(body))
}

fn decode<T: DeserializeOwned>(v: &Value) -> BoatResult<T> {
    T::deserialize(v).map_err(|e| {
        BoatError::Decode(format!(
            "{e}: {}",
            redact(&crate::util::ellipsize(&v.to_string(), 300))
        ))
    })
}

fn api_error(status: u16, headers: &HeaderMap, body: &Value) -> BoatError {
    let err = body.get("error");
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let code = s(body.get("code"))
        .or_else(|| s(err.and_then(|e| e.get("code"))))
        .unwrap_or_else(|| format!("http_{status}"));
    let message = s(body.get("message"))
        .or_else(|| s(err.and_then(|e| e.get("message"))))
        .or_else(|| err.and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let details = err
        .and_then(|e| e.get("details"))
        .cloned()
        .unwrap_or(Value::Null);
    let retry_after_seconds = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .or_else(|| details.get("retryAfterSeconds").and_then(Value::as_u64))
        .or_else(|| body.get("retryAfterSeconds").and_then(Value::as_u64));
    BoatError::Api(Box::new(ApiError {
        status,
        code,
        message: redact(&message),
        details,
        request_id: s(body.get("requestId")),
        retry_after_seconds,
    }))
}

/// Percent-encode a path segment or query value.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// boat's edge answers a request still open at about 60 s with a 502: a long exec must
    /// still come back with its output. Needs `BOAT_API_KEY` and `FLOTILLA_LIVE_SANDBOX` (an
    /// up VM): `cargo test -p flotilla-core live_long_exec -- --ignored`.
    #[test]
    #[ignore = "needs boat"]
    fn live_long_exec_outlasts_the_edge() {
        let (Ok(key), Ok(sb)) = (
            std::env::var("BOAT_API_KEY"),
            std::env::var("FLOTILLA_LIVE_SANDBOX"),
        ) else {
            return;
        };
        let boat = BoatClient::new(key).unwrap();
        let started = Instant::now();
        let res = boat.exec(&sb, "sleep 70; echo done", None, 120).unwrap();
        assert!(res.ok(), "{res:?}");
        assert_eq!(res.stdout.trim(), "done");
        assert!(started.elapsed() >= Duration::from_secs(70));
    }

    use super::*;

    #[test]
    fn prompt_body_carries_files_only_when_there_are_some() {
        let mut req = PromptRequest {
            provider: "claude-code",
            model: "claude-haiku-5-5",
            reasoning_effort: None,
            fast: false,
            conversation_id: Some("c1"),
            prompt: "look",
            attachments: &[],
        };
        assert_eq!(
            prompt_body(&req),
            json!({"provider": "claude-code", "model": "claude-haiku-5-5", "fast": false, "conversationId": "c1", "prompt": "look"})
        );
        let files = [Attachment {
            filename: "red.png".into(),
            mime_type: "image/png".into(),
            base64_data: "iVBORw0K".into(),
        }];
        req.attachments = &files;
        assert_eq!(
            prompt_body(&req)["attachments"],
            json!([{"filename": "red.png", "mimeType": "image/png", "base64Data": "iVBORw0K"}])
        );
    }

    #[test]
    fn encodes_labels() {
        assert_eq!(enc("flotilla=1"), "flotilla%3D1");
        assert_eq!(enc("a b/é"), "a%20b%2F%C3%A9");
    }

    #[test]
    fn starts_to_spare_only_when_boat_says_so() {
        let limits = |raw: Value| -> Limits {
            let mut l: Limits = serde_json::from_value(raw.clone()).unwrap();
            l.raw = raw;
            l
        };
        assert!(
            limits(json!({"canStart": true, "starts": {"hour": {"remaining": START_RESERVE}}}))
                .spares_starts()
        );
        assert!(
            !limits(
                json!({"canStart": true, "starts": {"hour": {"remaining": START_RESERVE - 1}}})
            )
            .spares_starts()
        );
        assert!(
            !limits(json!({"canStart": false, "starts": {"hour": {"remaining": 50}}}))
                .spares_starts()
        );
        // Starts that aren't counted (boat gives no `remaining`), or no count with nothing
        // blocking them: plenty to spare, unless boat won't start one now.
        let unlimited = json!({"canStart": true, "starts": {"unlimited": true, "hour": {"used": 1, "limit": 60}}});
        assert!(limits(unlimited.clone()).spares_starts());
        assert_eq!(limits(unlimited).starts_left(), Some(i64::MAX));
        assert!(
            limits(
                json!({"canStart": true, "unlimited": true, "starts": {"hour": {"remaining": 0}}})
            )
            .spares_starts()
        );
        assert!(limits(json!({"canStart": true})).spares_starts());
        assert!(limits(json!({})).spares_starts());
        assert!(!limits(json!({"canStart": false, "starts": {"unlimited": true}})).spares_starts());
        assert!(
            !limits(json!({"canStart": true, "blockedReason": "Payment failed"})).spares_starts()
        );
        let blocked = json!({"canStart": true, "blockedReason": "Payment failed", "starts": {"unlimited": true}});
        assert!(!limits(blocked.clone()).spares_starts());
        assert_eq!(limits(blocked).starts_left(), None);
        assert_eq!(limits(json!({"canStart": false})).starts_left(), None);
    }

    #[test]
    fn reads_usage() {
        let raw = json!({
            "activeSandboxes": 3, "maxActiveSandboxes": 100, "canStart": true, "blockedReason": null,
            "starts": {"day": {"limit": 200, "remaining": 80, "used": 120}, "hour": {"limit": 60, "remaining": 59, "used": 1},
                       "minute": {"limit": 12, "remaining": 12, "used": 0}, "unlimited": false},
            "planIncluded": {"periodEnd": "2026-11-30T01:01:38.000Z", "vmHours": 555.56, "vmHoursRemaining": 548.51},
        });
        let mut limits: Limits = serde_json::from_value(raw.clone()).unwrap();
        limits.raw = raw;
        let u = limits.usage();
        assert_eq!(u.vms, Some((3, 100)));
        assert_eq!(
            (u.minute, u.hour, u.day),
            (Some((0, 12)), Some((1, 60)), Some((120, 200)))
        );
        assert_eq!(u.vm_hours, Some((549, 556)));
        assert_eq!(u.renews.as_deref(), Some("2026-11-30T01:01:38.000Z"));
        assert_eq!(u.blocked, None);
        // No `keepGoing` or `startWindowFull`: nothing about them.
        assert_eq!(
            (u.extra_starts, u.starts_free, u.read_ms),
            (None, None, None)
        );

        // Unlimited starts aren't counted; a refusal says why.
        let raw = json!({"canStart": false, "startBlockedReason": "Payment failed", "starts": {"unlimited": true, "hour": {"used": 1, "limit": 60}}});
        let mut limits: Limits = serde_json::from_value(raw.clone()).unwrap();
        limits.raw = raw;
        let u = limits.usage();
        assert_eq!((u.vms, u.hour, u.vm_hours), (None, None, None));
        assert_eq!(u.blocked.as_deref(), Some("Payment failed"));

        // A full window says when starts free up; extra starts, on, with this month's so far.
        let raw = json!({
            "startWindowFull": {"cause": "hour", "freesAt": "2026-10-31T17:04:00.000Z"},
            "keepGoing": {"available": true, "enabled": true, "usedUnits": 7, "usedDollars": 0.29, "capDollars": 80, "capReached": false},
        });
        let mut limits: Limits = serde_json::from_value(raw.clone()).unwrap();
        limits.raw = raw;
        let u = limits.usage();
        assert_eq!(u.starts_free.as_deref(), Some("2026-10-31T17:04:00.000Z"));
        assert_eq!(
            u.extra_starts,
            Some(ExtraStarts {
                on: true,
                used: 7,
                spend_cents: Some((29, 8000)),
                cap_reached: false
            })
        );

        // Off, as boat reports it today; and a plan that doesn't offer them at all.
        let off = |available: bool| {
            let raw = json!({"keepGoing": {"available": available, "enabled": false, "usedUnits": 0, "usedDollars": 0, "capDollars": 80, "capReached": false}});
            let mut limits: Limits = serde_json::from_value(raw.clone()).unwrap();
            limits.raw = raw;
            limits.usage().extra_starts
        };
        assert_eq!(
            off(true),
            Some(ExtraStarts {
                on: false,
                used: 0,
                spend_cents: Some((0, 8000)),
                cap_reached: false
            })
        );
        assert_eq!(off(false), None);
    }

    #[test]
    fn a_hosted_port_is_what_was_asked_or_an_error() {
        let private = json!({"ok": true, "type": "port.hosted", "port": 3000, "url": "https://sub-3000.on.boat.dev?_token=t", "isProtected": true, "access": "private"});
        let public = json!({"ok": true, "type": "port.hosted", "port": 3000, "url": "https://sub-3000.on.boat.dev", "isProtected": false, "access": "public"});
        assert_eq!(
            hosted_url(&private, false).unwrap(),
            "https://sub-3000.on.boat.dev?_token=t"
        );
        assert_eq!(
            hosted_url(&public, true).unwrap(),
            "https://sub-3000.on.boat.dev"
        );
        // Never taken as private while boat says public, or the other way round.
        assert!(hosted_url(&public, false).is_err());
        assert!(hosted_url(&private, true).is_err());
        // Without `access`, the token says which it is.
        assert!(hosted_url(&json!({"url": "https://sub-3000.on.boat.dev"}), false).is_err());
        assert!(
            hosted_url(
                &json!({"url": "https://sub-3000.on.boat.dev?_token=t"}),
                false
            )
            .is_ok()
        );
        assert!(hosted_url(&json!({}), false).is_err());
    }

    #[test]
    fn parses_error_envelope() {
        let body = json!({
            "ok": false, "status": 409, "code": "no_running_turn", "message": "No turn",
            "error": {"code": "no_running_turn", "message": "No turn", "status": 409, "details": {"reason": "turn_not_started"}},
            "requestId": "req_1"
        });
        let e = api_error(409, &HeaderMap::new(), &body);
        assert!(e.is("no_running_turn"));
        assert_eq!(e.reason(), Some("turn_not_started"));
        assert_eq!(e.status(), Some(409));
    }
}
