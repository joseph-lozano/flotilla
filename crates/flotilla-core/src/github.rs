//! GitHub, through the GitHub CLI signed in where the engine runs (`gh auth login` there), or
//! `GH_TOKEN`. gh only hands over its token; Flotilla calls GitHub's REST and GraphQL APIs
//! itself, with ETags so an unchanged answer (`304`) costs nothing against the rate limit.
//! The token is kept in memory only, and never leaves this machine.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const API_BASE: &str = "https://api.github.com";
const GH_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_FOR: Duration = Duration::from_secs(300);

pub use flotilla_proto::github::{HOST, MIN_GH};
pub use flotilla_proto::{
    BranchChoices, Check, GhStatus, OpenPr, PullRequest, RepoInfo, SetupScript, Suggestions,
};

/// Why a GitHub call failed, worded for the user.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubError {
    /// No usable gh sign-in.
    #[error("{}", .0.message())]
    Gh(GhStatus),
    #[error("GitHub refused gh's sign-in. Run “gh auth refresh” in Terminal, then Check again.")]
    Unauthorized,
    #[error("{0}")]
    NotFound(String),
    #[error(
        "GitHub refused: {0}. gh's sign-in may need the “repo” scope: run “gh auth refresh -s repo”."
    )]
    Forbidden(String),
    #[error("GitHub's rate limit is used up{}. Checks resume then.", .reset.as_deref().map(|r| format!(" until {r}")).unwrap_or_default())]
    RateLimited {
        reset: Option<String>,
        reset_at: Option<i64>,
    },
    #[error("can't reach GitHub: {0}")]
    Http(String),
    #[error("GitHub answered {status}: {message}")]
    Api { status: u16, message: String },
}

impl GitHubError {
    /// Offline or a timeout: not worth a toast.
    pub fn is_network(&self) -> bool {
        matches!(self, GitHubError::Http(_))
    }
}

pub type GitHubResult<T> = Result<T, GitHubError>;

/// The gh binary and its token.
#[derive(Debug, Default)]
pub struct Gh {
    path: Mutex<Option<PathBuf>>,
    fixed: Option<PathBuf>,
    token: Mutex<Option<(String, Instant)>>,
    status: Mutex<Option<GhStatus>>,
}

impl Gh {
    pub fn new() -> Gh {
        Gh::default()
    }

    /// Use exactly this gh (dev builds: a fake one for screenshots).
    pub fn with_path(path: PathBuf) -> Gh {
        Gh {
            fixed: Some(path),
            ..Gh::default()
        }
    }

    /// Where gh is. An app opened from the Dock gets a bare PATH, so this asks the login
    /// shell first (gh may come from Homebrew, mise, nix…), then tries the usual places.
    pub fn path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.fixed {
            return p.is_file().then(|| p.clone());
        }
        let mut cached = self.path.lock().unwrap();
        if cached.as_ref().is_none_or(|p| !p.is_file()) {
            *cached = find_gh();
        }
        cached.clone()
    }

    /// The last status from [`Gh::check`], without running gh.
    pub fn last_status(&self) -> GhStatus {
        self.status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(GhStatus::Unknown)
    }

    /// Run gh to see whose it is. Clears the cached token.
    pub fn check(&self) -> GhStatus {
        *self.token.lock().unwrap() = None;
        if self.fixed.is_none() {
            *self.path.lock().unwrap() = None;
        }
        let status = self.check_uncached();
        *self.status.lock().unwrap() = Some(status.clone());
        status
    }

    fn check_uncached(&self) -> GhStatus {
        // A token in the environment is what's used: gh, if there is one, doesn't matter.
        if let Some((token, var)) = env_token() {
            return match token_user(&token) {
                Ok(u) => GhStatus::SignedIn {
                    login: u.login,
                    version: var.into(),
                    scopes: u.scopes.unwrap_or_default(),
                },
                Err(TokenError::Refused) => GhStatus::Failed(format!("GitHub refused {var}")),
                Err(TokenError::Other(e)) => GhStatus::Failed(e),
            };
        }
        let Some(gh) = self.path() else {
            return GhStatus::NotFound;
        };
        let version = match run(&gh, &["--version"]) {
            Ok(out) => match parse_version(&out) {
                Some(v) => v,
                None => {
                    return GhStatus::Failed(format!(
                        "unexpected `gh --version`: {}",
                        first_line(&out)
                    ));
                }
            },
            Err(e) => return GhStatus::Failed(e),
        };
        if !version_at_least(&version, MIN_GH) {
            return GhStatus::TooOld { version };
        }
        // Exits non-zero when not signed in; the JSON is still printed.
        let out = match run_any(
            &gh,
            &["auth", "status", "--json", "hosts", "--hostname", HOST],
        ) {
            Ok(out) => out,
            Err(e) => return GhStatus::Failed(e),
        };
        let Ok(v) = serde_json::from_str::<Value>(&out) else {
            return GhStatus::NotSignedIn { version };
        };
        let accounts = v
            .pointer(&format!("/hosts/{}", HOST.replace('/', "~1")))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let active = accounts.iter().find(|a| {
            a["active"].as_bool() == Some(true) && a["state"].as_str() == Some("success")
        });
        match active {
            Some(a) => GhStatus::SignedIn {
                login: a["login"].as_str().unwrap_or("?").to_string(),
                version,
                scopes: a["scopes"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            },
            None => GhStatus::NotSignedIn { version },
        }
    }

    /// gh's token for github.com, cached for a few minutes. Never logged.
    pub fn token(&self) -> GitHubResult<String> {
        if let Some((t, at)) = self.token.lock().unwrap().as_ref()
            && at.elapsed() < TOKEN_FOR
        {
            return Ok(t.clone());
        }
        if let Some((token, _)) = env_token() {
            return Ok(token);
        }
        let Some(gh) = self.path() else {
            return Err(GitHubError::Gh(GhStatus::NotFound));
        };
        let token = run(&gh, &["auth", "token", "--hostname", HOST])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if token.is_empty() {
            let status = self.check();
            return Err(GitHubError::Gh(if status.signed_in() {
                GhStatus::Failed("`gh auth token` gave nothing".into())
            } else {
                status
            }));
        }
        *self.token.lock().unwrap() = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn forget_token(&self) {
        *self.token.lock().unwrap() = None;
    }
}

/// `GH_TOKEN` (or `GITHUB_TOKEN`), which gh itself honours first, and which works with no gh.
pub fn env_token() -> Option<(String, &'static str)> {
    ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .map(|t| (t, var))
    })
}

/// Whose a token is, as GitHub says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUser {
    pub login: String,
    /// A classic token's scopes; none for a fine-grained token, whose permissions GitHub
    /// doesn't list.
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// 401: the token doesn't work.
    Refused,
    /// Anything else (no network, GitHub down): not the token's fault.
    Other(String),
}

/// Ask GitHub whose `token` is (`GET /user`).
pub fn token_user(token: &str) -> Result<TokenUser, TokenError> {
    let http = Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("flotilla/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| TokenError::Other(e.to_string()))?;
    let resp = http
        .get(format!("{API_BASE}/user"))
        .bearer_auth(token)
        .send()
        .map_err(|e| TokenError::Other(format!("can't reach GitHub: {e}")))?;
    if resp.status() == StatusCode::UNAUTHORIZED {
        return Err(TokenError::Refused);
    }
    if !resp.status().is_success() {
        return Err(TokenError::Other(format!(
            "GitHub answered {}",
            resp.status()
        )));
    }
    let scopes = resp
        .headers()
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });
    let body: Value = resp.json().map_err(|e| TokenError::Other(e.to_string()))?;
    let login = body["login"]
        .as_str()
        .ok_or_else(|| TokenError::Other("GitHub didn't say whose the token is".into()))?
        .to_string();
    Ok(TokenUser { login, scopes })
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// `gh version 2.100.0 (2026-09-30)` → `2.100.0`.
fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().find(|l| l.starts_with("gh version "))?;
    line.split_whitespace().nth(2).map(str::to_string)
}

fn version_at_least(v: &str, min: (u32, u32)) -> bool {
    let mut parts = v
        .split('.')
        .map(|p| p.trim_start_matches('v').parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= min
}

fn find_gh() -> Option<PathBuf> {
    if let Some(p) = crate::tools::named("FLOTILLA_GH") {
        return p;
    }
    if !crate::tools::beyond_path() {
        return crate::tools::on_path("gh");
    }
    if let Some(p) = login_shell_lookup() {
        return Some(p);
    }
    let home = dirs::home_dir().unwrap_or_default();
    let candidates = [
        PathBuf::from("/opt/homebrew/bin/gh"),
        PathBuf::from("/usr/local/bin/gh"),
        home.join(".local/share/mise/shims/gh"),
        home.join(".local/bin/gh"),
        home.join(".nix-profile/bin/gh"),
        PathBuf::from("/run/current-system/sw/bin/gh"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| crate::tools::on_path("gh"))
}

/// `command -v gh` in the user's interactive login shell, which sets up PATH the way
/// Terminal does.
fn login_shell_lookup() -> Option<PathBuf> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    let out = run_with_timeout(
        Command::new(shell).args(["-ilc", "command -v gh"]),
        Duration::from_secs(5),
    )
    .ok()?;
    if !out.0 {
        return None;
    }
    // Shell start-up can print other lines; the answer is the last absolute path.
    out.1
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Run gh; `Err` on a non-zero exit or no answer in time.
fn run(gh: &Path, args: &[&str]) -> Result<String, String> {
    let (ok, out, err) = run_gh(gh, args)?;
    if ok {
        Ok(out)
    } else {
        Err(first_line(if err.trim().is_empty() { &out } else { &err }).to_string())
    }
}

/// Run gh and return its stdout whatever the exit code.
fn run_any(gh: &Path, args: &[&str]) -> Result<String, String> {
    run_gh(gh, args).map(|(_, out, _)| out)
}

fn run_gh(gh: &Path, args: &[&str]) -> Result<(bool, String, String), String> {
    let mut cmd = Command::new(gh);
    cmd.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    let (ok, out, err) = run_with_timeout(&mut cmd, GH_TIMEOUT).map_err(|e| e.to_string())?;
    Ok((ok, out, err))
}

/// Run a command, killing it if it takes longer than `timeout`.
fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> std::io::Result<(bool, String, String)> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let out = child.wait_with_output()?;
            return Ok((
                status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {} s", timeout.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// From the last answer: (remaining, limit, reset as a Unix time).
type Rate = (i64, i64, Option<i64>);

/// GitHub's REST and GraphQL APIs with gh's token. Cheap to clone; clones share the token
/// and the ETag cache.
#[derive(Clone)]
pub struct GitHub {
    http: Client,
    base: String,
    pub gh: Arc<Gh>,
    /// path → (ETag, last body), so an unchanged answer costs nothing.
    cache: Arc<Mutex<HashMap<String, (String, Value)>>>,
    rate: Arc<Mutex<Option<Rate>>>,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    pub fn new(gh: Arc<Gh>) -> GitHub {
        GitHub::with_base(gh, API_BASE)
    }

    pub fn with_base(gh: Arc<Gh>, base: &str) -> GitHub {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("flotilla/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("an HTTP client");
        GitHub {
            http,
            base: base.trim_end_matches('/').to_string(),
            gh,
            cache: Arc::default(),
            rate: Arc::default(),
        }
    }

    /// Below a tenth of the hourly quota: skip checks that can wait.
    pub fn low_on_quota(&self) -> bool {
        self.rate
            .lock()
            .unwrap()
            .is_some_and(|(remaining, limit, _)| limit > 0 && remaining * 10 < limit)
    }

    /// GET with the cached ETag. Returns the body and whether it changed since last time.
    pub fn get(&self, path: &str) -> GitHubResult<(Value, bool)> {
        let cached = self.cache.lock().unwrap().get(path).cloned();
        let resp = self.send(
            reqwest::Method::GET,
            path,
            None,
            cached.as_ref().map(|c| c.0.as_str()),
        )?;
        match resp {
            Resp::NotModified => Ok((cached.map(|c| c.1).unwrap_or(Value::Null), false)),
            Resp::Ok { body, etag } => {
                if let Some(etag) = etag {
                    self.cache
                        .lock()
                        .unwrap()
                        .insert(path.to_string(), (etag, body.clone()));
                }
                Ok((body, true))
            }
        }
    }

    pub fn post(&self, path: &str, body: &Value) -> GitHubResult<Value> {
        match self.send(reqwest::Method::POST, path, Some(body), None)? {
            Resp::Ok { body, .. } => Ok(body),
            Resp::NotModified => Ok(Value::Null),
        }
    }

    pub fn put(&self, path: &str, body: &Value) -> GitHubResult<Value> {
        match self.send(reqwest::Method::PUT, path, Some(body), None)? {
            Resp::Ok { body, .. } => Ok(body),
            Resp::NotModified => Ok(Value::Null),
        }
    }

    /// A GraphQL query. GraphQL errors come back as `Api`.
    pub fn graphql(&self, query: &str, variables: Value) -> GitHubResult<Value> {
        let body = self.post("/graphql", &json!({"query": query, "variables": variables}))?;
        if let Some(errors) = body
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let message = errors
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ");
            // GraphQL's own rate limit comes back as 200 with an error.
            if errors
                .iter()
                .any(|e| e["type"].as_str() == Some("RATE_LIMITED"))
            {
                let reset_at = self.rate.lock().unwrap().and_then(|(_, _, reset)| reset);
                let reset = reset_at
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%-I:%M %p")
                            .to_string()
                    });
                return Err(GitHubError::RateLimited { reset, reset_at });
            }
            if errors
                .iter()
                .any(|e| e["type"].as_str() == Some("NOT_FOUND"))
            {
                return Err(GitHubError::NotFound(message));
            }
            return Err(GitHubError::Api {
                status: 200,
                message,
            });
        }
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    }

    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> GitHubResult<Resp> {
        let mut retried = false;
        loop {
            let token = self.gh.token()?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{}", self.base, path))
                .bearer_auth(&token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(e) = etag {
                req = req.header("If-None-Match", e);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req
                .send()
                .map_err(|e| GitHubError::Http(redact(&e.to_string())))?;
            let status = res.status();
            let headers = res.headers().clone();
            let header = |k: &str| {
                headers
                    .get(k)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse::<i64>().ok());
            if let (Some(r), Some(l)) = (
                remaining,
                header("x-ratelimit-limit").and_then(|v| v.parse::<i64>().ok()),
            ) {
                *self.rate.lock().unwrap() = Some((
                    r,
                    l,
                    header("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok()),
                ));
            }
            if status == StatusCode::NOT_MODIFIED {
                return Ok(Resp::NotModified);
            }
            let text = res
                .text()
                .map_err(|e| GitHubError::Http(redact(&e.to_string())))?;
            let json: Value = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text.clone()))
            };
            if status.is_success() {
                return Ok(Resp::Ok {
                    body: json,
                    etag: header("etag"),
                });
            }
            let message = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(text.as_str())
                .to_string();
            match status.as_u16() {
                401 if !retried => {
                    // gh may have refreshed its token since; ask again once.
                    self.gh.forget_token();
                    retried = true;
                    continue;
                }
                401 => return Err(GitHubError::Unauthorized),
                403 | 429
                    if remaining == Some(0)
                        || header("retry-after").is_some()
                        || message.to_lowercase().contains("rate limit") =>
                {
                    let reset_at = header("x-ratelimit-reset")
                        .and_then(|v| v.parse::<i64>().ok())
                        .or_else(|| {
                            header("retry-after")
                                .and_then(|v| v.parse::<i64>().ok())
                                .map(|s| chrono::Utc::now().timestamp() + s)
                        });
                    let reset = reset_at
                        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%-I:%M %p")
                                .to_string()
                        });
                    return Err(GitHubError::RateLimited { reset, reset_at });
                }
                403 => return Err(GitHubError::Forbidden(message)),
                404 => return Err(GitHubError::NotFound(message)),
                s => return Err(GitHubError::Api { status: s, message }),
            }
        }
    }

    // ---- repos ----

    /// The repos gh's account can see, most recently pushed first (up to 1,000).
    pub fn viewer_repos(&self) -> GitHubResult<Vec<RepoInfo>> {
        self.viewer_repos_paged(|_| {}).map(|(_, repos)| repos)
    }

    /// `viewer_repos`, calling `page` with the list so far after each page (newest pushed
    /// first), so a first load can show something before all pages are in. Also returns the
    /// login the list belongs to: the account gh's token is for now, which a cache must be
    /// kept under (gh may have switched accounts since its status was last checked).
    pub fn viewer_repos_paged(
        &self,
        mut page_done: impl FnMut(&[RepoInfo]),
    ) -> GitHubResult<(String, Vec<RepoInfo>)> {
        let mut out = Vec::new();
        let mut login = String::new();
        let mut after: Option<String> = None;
        for _ in 0..10 {
            let data = self.graphql(
                "query($after: String) { viewer { login repositories(first: 100, after: $after, orderBy: {field: PUSHED_AT, direction: DESC}, \
                   affiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER], ownerAffiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER]) { \
                   pageInfo { hasNextPage endCursor } \
                   nodes { name owner { login } isPrivate isArchived pushedAt description } } } }",
                json!({ "after": after }),
            )?;
            if let Some(l) = data.pointer("/viewer/login").and_then(Value::as_str) {
                login = l.to_string();
            }
            let page = data
                .pointer("/viewer/repositories")
                .cloned()
                .unwrap_or(Value::Null);
            for n in page["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|n| n["isArchived"].as_bool() != Some(true))
            {
                let (Some(owner), Some(name)) = (
                    n.pointer("/owner/login").and_then(Value::as_str),
                    n["name"].as_str(),
                ) else {
                    continue;
                };
                out.push(RepoInfo {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    private: n["isPrivate"].as_bool().unwrap_or(false),
                    // Not asked for: `defaultBranchRef` takes GitHub ~4 s more a page of 100
                    // (5.5 s, not 1.4). The repo picked is asked for its own.
                    default_branch: String::new(),
                    pushed_at: n["pushedAt"].as_str().map(str::to_string),
                    description: n["description"].as_str().map(str::to_string),
                });
            }
            after = page
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if page
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                != Some(true)
                || after.is_none()
            {
                break;
            }
            page_done(&out);
        }
        Ok((login, out))
    }

    pub fn repo(&self, owner: &str, name: &str) -> GitHubResult<RepoInfo> {
        let (v, _) = self
            .get(&format!("/repos/{owner}/{name}"))
            .map_err(|e| match e {
                GitHubError::NotFound(_) => {
                    GitHubError::NotFound(self.cant_see(&format!("{owner}/{name}")))
                }
                e => e,
            })?;
        Ok(RepoInfo {
            owner: v
                .pointer("/owner/login")
                .and_then(Value::as_str)
                .unwrap_or(owner)
                .to_string(),
            name: v["name"].as_str().unwrap_or(name).to_string(),
            private: v["private"].as_bool().unwrap_or(false),
            default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
            pushed_at: v["pushed_at"].as_str().map(str::to_string),
            description: v["description"].as_str().map(str::to_string),
        })
    }

    fn cant_see(&self, what: &str) -> String {
        match self.gh.last_status().login() {
            Some(login) => format!(
                "gh's account ({login}) can't see {what}. Check its access, or remove the project."
            ),
            None => format!("gh's account can't see {what}."),
        }
    }

    /// The file and folder names at the repo's root on `branch`.
    pub fn root_files(&self, owner: &str, name: &str, branch: &str) -> GitHubResult<Vec<String>> {
        self.dir_files(owner, name, "", branch)
    }

    /// The file and folder names in folder `path` on `branch` (none if it doesn't exist).
    pub fn dir_files(
        &self,
        owner: &str,
        name: &str,
        path: &str,
        branch: &str,
    ) -> GitHubResult<Vec<String>> {
        let path = if path.is_empty() {
            String::new()
        } else {
            format!("/{}", enc(path))
        };
        let v = match self.get(&format!(
            "/repos/{owner}/{name}/contents{path}?ref={}",
            enc(branch)
        )) {
            Ok((v, _)) => v,
            Err(GitHubError::NotFound(_)) if !path.is_empty() => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A text file's contents on `branch`, if it exists.
    pub fn file_text(
        &self,
        owner: &str,
        name: &str,
        path: &str,
        branch: &str,
    ) -> GitHubResult<Option<String>> {
        match self.get(&format!(
            "/repos/{owner}/{name}/contents/{path}?ref={}",
            enc(branch)
        )) {
            Ok((v, _)) => {
                use base64::Engine as _;
                let b64: String = v["content"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                Ok(base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).into_owned()))
            }
            Err(GitHubError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The commit at the head of `branch`, and whether it changed since the last call (an
    /// unchanged answer is a free `304`).
    pub fn branch_head(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
    ) -> GitHubResult<(String, bool)> {
        let (v, changed) = self
            .get(&format!("/repos/{owner}/{name}/branches/{}", enc(branch)))
            .map_err(|e| match e {
                GitHubError::NotFound(_) => GitHubError::NotFound(
                    self.cant_see(&format!("{owner}/{name} (branch {branch})")),
                ),
                e => e,
            })?;
        let sha = v
            .pointer("/commit/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| GitHubError::Api {
                status: 200,
                message: "no commit in the branch answer".into(),
            })?;
        Ok((sha.to_string(), changed))
    }
}

/// Read a quiet PR again after this long; one closed without merging, after this.
const PR_QUIET_EVERY: Duration = Duration::from_secs(5 * 60);
const PR_CLOSED_EVERY: Duration = Duration::from_secs(15 * 60);
/// An open PR that hasn't changed for this long counts as quiet.
const PR_QUIET_AFTER_SECS: i64 = 30 * 60;
/// Sweeps come a minute apart, give or take: don't miss one by a few milliseconds.
const PR_SWEEP_SLACK: Duration = Duration::from_secs(10);

/// Whether the minute sweep should read a thread's pull request again (T3 Code's
/// schedule). No PR yet, or not read since Flotilla started: yes. Merged: never. The thread
/// open in Flotilla (`selected`), or an open PR changed in the last half hour: every sweep.
/// A quiet open PR: every 5 min. Closed without merging: every 15 min.
pub fn pr_due(
    pr: Option<&PullRequest>,
    since_read: Option<Duration>,
    selected: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(pr) = pr else { return true };
    if pr.state == "MERGED" {
        return false;
    }
    let Some(since) = since_read else { return true };
    if selected {
        return true;
    }
    let every = match pr.state.as_str() {
        "OPEN" => {
            let quiet = pr
                .updated_at
                .as_deref()
                .and_then(crate::util::parse_iso)
                .is_some_and(|t| (now - t).num_seconds() >= PR_QUIET_AFTER_SECS);
            if quiet {
                PR_QUIET_EVERY
            } else {
                Duration::ZERO
            }
        }
        _ => PR_CLOSED_EVERY,
    };
    since + PR_SWEEP_SLACK >= every
}

const PR_FIELDS: &str = "number url title headRefName baseRefName state isDraft mergeable reviewDecision additions deletions updatedAt mergedAt \
    commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 50) { nodes { __typename \
    ... on CheckRun { name status conclusion detailsUrl title } ... on StatusContext { context state targetUrl description } } } } } } }";

/// A non-empty string field.
fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn check_state(n: &Value) -> Option<Check> {
    match n["__typename"].as_str()? {
        "CheckRun" => {
            let state = match (n["status"].as_str(), n["conclusion"].as_str()) {
                (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => "pass",
                (Some("COMPLETED"), _) => "fail",
                _ => "pending",
            };
            Some(Check {
                name: n["name"].as_str()?.to_string(),
                state: state.into(),
                url: n["detailsUrl"].as_str().map(str::to_string),
                description: text(&n["title"]),
            })
        }
        "StatusContext" => {
            let state = match n["state"].as_str() {
                Some("SUCCESS") => "pass",
                Some("PENDING" | "EXPECTED") => "pending",
                _ => "fail",
            };
            Some(Check {
                name: n["context"].as_str()?.to_string(),
                state: state.into(),
                url: n["targetUrl"].as_str().map(str::to_string),
                description: text(&n["description"]),
            })
        }
        _ => None,
    }
}

fn pull_request(n: &Value) -> Option<PullRequest> {
    let contexts = n
        .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some(PullRequest {
        number: n["number"].as_i64()?,
        url: n["url"].as_str()?.to_string(),
        title: n["title"].as_str().unwrap_or("").to_string(),
        head: n["headRefName"].as_str().unwrap_or("").to_string(),
        base: n["baseRefName"].as_str().unwrap_or("").to_string(),
        state: n["state"].as_str()?.to_string(),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
        mergeable: n["mergeable"].as_str().map(str::to_string),
        review: n["reviewDecision"].as_str().map(str::to_string),
        additions: n["additions"].as_i64().unwrap_or(0),
        deletions: n["deletions"].as_i64().unwrap_or(0),
        checks: contexts.iter().filter_map(check_state).collect(),
        updated_at: n["updatedAt"].as_str().map(str::to_string),
        merged_at: n["mergedAt"].as_str().map(str::to_string),
    })
}

pub use flotilla_model::github::ReviewNote;

/// What to look up for one thread: the newest PR from its branch, or a PR linked by number.
#[derive(Debug, Clone)]
pub enum PrLookup {
    Branch(String),
    Number(i64),
}

impl GitHub {
    /// Each thread's pull request in one repo, in one query. Threads without one map to `None`.
    pub fn pull_requests(
        &self,
        owner: &str,
        name: &str,
        lookups: &[(String, PrLookup)],
    ) -> GitHubResult<HashMap<String, Option<PullRequest>>> {
        if lookups.is_empty() {
            return Ok(HashMap::new());
        }
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let mut fields = String::new();
        for (i, (_, l)) in lookups.iter().enumerate() {
            match l {
                PrLookup::Branch(b) => fields.push_str(&format!(
                    "t{i}: pullRequests(headRefName: {}, first: 1, orderBy: {{field: CREATED_AT, direction: DESC}}, states: [OPEN, CLOSED, MERGED]) {{ nodes {{ {PR_FIELDS} }} }} ",
                    q(b)
                )),
                PrLookup::Number(n) => fields.push_str(&format!("t{i}: pullRequest(number: {n}) {{ {PR_FIELDS} }} ")),
            }
        }
        let data = self.graphql(
            &format!(
                "query {{ repository(owner: {}, name: {}) {{ {fields} }} }}",
                q(owner),
                q(name)
            ),
            json!({}),
        )?;
        let repo = data.get("repository").cloned().unwrap_or(Value::Null);
        Ok(lookups
            .iter()
            .enumerate()
            .map(|(i, (id, l))| {
                let node = &repo[format!("t{i}")];
                let pr = match l {
                    PrLookup::Branch(_) => node.pointer("/nodes/0").and_then(pull_request),
                    PrLookup::Number(_) => pull_request(node),
                };
                (id.clone(), pr)
            })
            .collect())
    }

    /// Merge with the method GitHub offers you first on this repo.
    pub fn merge(&self, owner: &str, name: &str, number: i64) -> GitHubResult<()> {
        let data = self.graphql(
            "query($o: String!, $n: String!) { repository(owner: $o, name: $n) { viewerDefaultMergeMethod } }",
            json!({"o": owner, "n": name}),
        )?;
        let method = data
            .pointer("/repository/viewerDefaultMergeMethod")
            .and_then(Value::as_str)
            .unwrap_or("MERGE")
            .to_lowercase();
        self.put(
            &format!("/repos/{owner}/{name}/pulls/{number}/merge"),
            &json!({"merge_method": method}),
        )?;
        Ok(())
    }

    /// Take a draft out of draft.
    pub fn mark_ready(&self, owner: &str, name: &str, number: i64) -> GitHubResult<()> {
        let data = self.graphql(
            "query($o: String!, $n: String!, $num: Int!) { repository(owner: $o, name: $n) { pullRequest(number: $num) { id } } }",
            json!({"o": owner, "n": name, "num": number}),
        )?;
        let id = data
            .pointer("/repository/pullRequest/id")
            .and_then(Value::as_str)
            .ok_or_else(|| GitHubError::NotFound(format!("pull request #{number}")))?;
        self.graphql(
            "mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { isDraft } } }",
            json!({"id": id}),
        )?;
        Ok(())
    }

    /// One pull request by number, to link it to a thread.
    pub fn pull_request(&self, owner: &str, name: &str, number: i64) -> GitHubResult<PullRequest> {
        let found =
            self.pull_requests(owner, name, &[(String::new(), PrLookup::Number(number))])?;
        found.into_values().next().flatten().ok_or_else(|| {
            GitHubError::NotFound(format!("{owner}/{name} has no pull request #{number}"))
        })
    }

    /// What reviewers asked for on a pull request: the bodies of reviews that requested
    /// changes, then the first comment of each unresolved review thread.
    pub fn review_notes(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> GitHubResult<Vec<ReviewNote>> {
        let data = self.graphql(
            "query($o: String!, $n: String!, $num: Int!) { repository(owner: $o, name: $n) { pullRequest(number: $num) { \
             reviews(last: 20, states: [CHANGES_REQUESTED]) { nodes { author { login } body } } \
             reviewThreads(first: 100) { nodes { isResolved path line originalLine comments(first: 1) { nodes { author { login } body } } } } } } }",
            json!({"o": owner, "n": name, "num": number}),
        )?;
        let pr = data
            .pointer("/repository/pullRequest")
            .ok_or_else(|| GitHubError::NotFound(format!("pull request #{number}")))?;
        let login = |n: &Value| {
            n.pointer("/author/login")
                .and_then(Value::as_str)
                .unwrap_or("someone")
                .to_string()
        };
        let mut notes: Vec<ReviewNote> = pr
            .pointer("/reviews/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(ReviewNote {
                    author: login(r),
                    path: None,
                    line: None,
                    body: text(&r["body"])?,
                })
            })
            .collect();
        for t in pr
            .pointer("/reviewThreads/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if t["isResolved"].as_bool() == Some(true) {
                continue;
            }
            let Some(c) = t.pointer("/comments/nodes/0") else {
                continue;
            };
            let Some(body) = text(&c["body"]) else {
                continue;
            };
            notes.push(ReviewNote {
                author: login(c),
                path: text(&t["path"]),
                line: t["line"].as_i64().or(t["originalLine"].as_i64()),
                body,
            });
        }
        Ok(notes)
    }

    /// The first line of a commit's message.
    pub fn commit_title(&self, owner: &str, name: &str, sha: &str) -> GitHubResult<String> {
        let (v, _) = self.get(&format!("/repos/{owner}/{name}/commits/{}", enc(sha)))?;
        Ok(v.pointer("/commit/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string())
    }

    /// How many commits `head` has that `base` doesn't (how far a thread's start is behind).
    pub fn commits_between(
        &self,
        owner: &str,
        name: &str,
        base: &str,
        head: &str,
    ) -> GitHubResult<i64> {
        let (v, _) = self.get(&format!("/repos/{owner}/{name}/compare/{base}...{head}"))?;
        Ok(v["ahead_by"].as_i64().unwrap_or(0))
    }

    /// What a thread can start on besides a new branch: the open pull requests (latest
    /// activity first) and the other branches (newest commit first), up to 100 of each. The
    /// default branch and the open PRs' own branches aren't listed as branches.
    pub fn branch_choices(
        &self,
        owner: &str,
        name: &str,
        default_branch: &str,
    ) -> GitHubResult<BranchChoices> {
        let data = self.graphql(
            "query($o: String!, $n: String!) { repository(owner: $o, name: $n) { \
             pullRequests(states: [OPEN], first: 100, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { \
             number title url isDraft isCrossRepository headRefName baseRefName } } \
             refs(refPrefix: \"refs/heads/\", first: 100, orderBy: {field: TAG_COMMIT_DATE, direction: DESC}) { nodes { name } } } }",
            json!({"o": owner, "n": name}),
        )?;
        let repo = data
            .get("repository")
            .ok_or_else(|| GitHubError::NotFound(self.cant_see(&format!("{owner}/{name}"))))?;
        let prs: Vec<OpenPr> = repo
            .pointer("/pullRequests/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|n| {
                Some(OpenPr {
                    number: n["number"].as_i64()?,
                    title: n["title"].as_str().unwrap_or("").to_string(),
                    url: n["url"].as_str().unwrap_or("").to_string(),
                    head: n["headRefName"].as_str()?.to_string(),
                    base: n["baseRefName"]
                        .as_str()
                        .unwrap_or(default_branch)
                        .to_string(),
                    draft: n["isDraft"].as_bool().unwrap_or(false),
                    from_fork: n["isCrossRepository"].as_bool().unwrap_or(false),
                })
            })
            .collect();
        let branches = repo
            .pointer("/refs/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|n| n["name"].as_str())
            .filter(|b| *b != default_branch && !prs.iter().any(|p| !p.from_fork && p.head == *b))
            .map(str::to_string)
            .collect();
        Ok(BranchChoices { prs, branches })
    }

    /// Whether `branch` is on GitHub (pushed at least once).
    pub fn has_branch(&self, owner: &str, name: &str, branch: &str) -> GitHubResult<bool> {
        match self.branch_head(owner, name, branch) {
            Ok(_) => Ok(true),
            Err(GitHubError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

pub use flotilla_model::github::parse_pr_ref;

enum Resp {
    NotModified,
    Ok { body: Value, etag: Option<String> },
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Strip anything that looks like a GitHub token from an error message.
fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        let w = word.trim_end_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
        if ["ghp_", "gho_", "ghu_", "ghs_", "github_pat_"]
            .iter()
            .any(|p| w.starts_with(p))
        {
            out.push_str("[token]");
            out.push_str(&word[w.len()..]);
        } else {
            out.push_str(word);
        }
    }
    out
}

/// Suggestions from the names at a repo's root, in its `.agents` folder, and its
/// package.json: none beyond the repo's own `.agents/setup` if it has one.
pub fn suggest(root: &[String], agents: &[String], package_json: Option<&str>) -> Suggestions {
    if agents.iter().any(|f| f == "setup") {
        return Suggestions {
            scripts: vec![],
            agents_setup: true,
        };
    }
    Suggestions {
        scripts: suggest_scripts(root, package_json),
        agents_setup: false,
    }
}

/// Setup scripts that fit the files at a repo's root (and its package.json, if any).
pub fn suggest_scripts(root: &[String], package_json: Option<&str>) -> Vec<SetupScript> {
    let has = |f: &str| root.iter().any(|r| r == f);
    let mut out = Vec::new();
    if has("mise.toml") || has(".mise.toml") || has(".tool-versions") {
        out.push(SetupScript::every("mise trust --yes && mise install"));
    }
    let pkg: Option<Value> = package_json.and_then(|t| serde_json::from_str(t).ok());
    let script = |name: &str| {
        pkg.as_ref()
            .and_then(|p| p.pointer(&format!("/scripts/{name}")))
            .is_some()
    };
    if has("pnpm-lock.yaml") {
        out.push(SetupScript::every("pnpm install --frozen-lockfile"));
        if script("build") {
            out.push(SetupScript::every("pnpm run build"));
        }
    } else if has("yarn.lock") {
        out.push(SetupScript::every("yarn install --frozen-lockfile"));
        if script("build") {
            out.push(SetupScript::every("yarn build"));
        }
    } else if has("bun.lock") || has("bun.lockb") {
        out.push(SetupScript::every("bun install --frozen-lockfile"));
        if script("build") {
            out.push(SetupScript::every("bun run build"));
        }
    } else if has("package.json") {
        // Not `npm ci`: it deletes node_modules every time, and setup runs at every thread start.
        out.push(SetupScript::every("npm install"));
        if script("build") {
            out.push(SetupScript::every("npm run build"));
        }
    }
    if has("Cargo.toml") {
        out.push(SetupScript::every("cargo build"));
    }
    if has("go.mod") {
        out.push(SetupScript::every("go mod download"));
    }
    if has("uv.lock") {
        out.push(SetupScript::every("uv sync"));
    } else if has("poetry.lock") {
        out.push(SetupScript::every("poetry install"));
    } else if has("requirements.txt") {
        out.push(SetupScript::every("pip install -r requirements.txt"));
    }
    if has("Gemfile") {
        out.push(SetupScript::every("bundle install"));
    }
    if has("mix.exs") {
        out.push(SetupScript::every("mix deps.get && mix compile"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_and_closed_pull_requests_are_read_less_often() {
        let now = chrono::Utc::now();
        let ago = |mins: i64| Some((now - chrono::Duration::minutes(mins)).to_rfc3339());
        let pr = |state: &str, updated: Option<String>| PullRequest {
            state: state.into(),
            updated_at: updated,
            ..Default::default()
        };
        let read = |mins: u64| Some(Duration::from_secs(mins * 60));
        let (busy, quiet, closed, merged) = (
            pr("OPEN", ago(5)),
            pr("OPEN", ago(45)),
            pr("CLOSED", ago(90)),
            pr("MERGED", ago(1)),
        );
        // No PR yet, or never read: look.
        assert!(pr_due(None, read(1), false, now));
        assert!(pr_due(Some(&quiet), None, false, now));
        // Open and busy: every sweep.
        assert!(pr_due(Some(&busy), read(1), false, now));
        // Quiet: every 5 min, a sweep that comes a moment early included.
        assert!(!pr_due(Some(&quiet), read(1), false, now));
        assert!(pr_due(
            Some(&quiet),
            Some(Duration::from_secs(299)),
            false,
            now
        ));
        // Closed: every 15 min. Merged: never, even when it's the open thread.
        assert!(!pr_due(Some(&closed), read(5), false, now));
        assert!(pr_due(Some(&closed), read(15), false, now));
        assert!(!pr_due(Some(&merged), None, true, now));
        // The thread open in Flotilla: every sweep.
        assert!(pr_due(Some(&quiet), read(1), true, now));
        assert!(pr_due(Some(&closed), read(1), true, now));
    }

    #[test]
    fn versions() {
        assert_eq!(
            parse_version(
                "gh version 2.100.0 (2026-09-30)\nhttps://github.com/cli/cli/releases/tag/v2.100.0"
            )
            .as_deref(),
            Some("2.100.0")
        );
        assert!(version_at_least("2.100.0", (2, 81)));
        assert!(version_at_least("2.81.0", (2, 81)));
        assert!(!version_at_least("2.40.1", (2, 81)));
        assert!(version_at_least("3.0.0", (2, 81)));
    }

    #[test]
    fn suggestions_follow_the_repo() {
        let files = |f: &[&str]| f.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let pkg = r#"{"scripts":{"build":"tsc -p .","test":"node --test"}}"#;
        let s = suggest_scripts(
            &files(&["package.json", "package-lock.json", "src"]),
            Some(pkg),
        );
        assert_eq!(
            s.iter().map(|s| s.command.as_str()).collect::<Vec<_>>(),
            ["npm install", "npm run build"]
        );
        let s = suggest_scripts(&files(&["Cargo.toml", "Cargo.lock", "mise.toml"]), None);
        assert_eq!(
            s.iter().map(|s| s.command.as_str()).collect::<Vec<_>>(),
            ["mise trust --yes && mise install", "cargo build"]
        );
        assert!(suggest_scripts(&files(&["README.md"]), None).is_empty());
        let s = suggest(
            &files(&[".agents", "package.json", "package-lock.json"]),
            &files(&["setup", "resume"]),
            Some(pkg),
        );
        assert_eq!(
            s,
            Suggestions {
                scripts: vec![],
                agents_setup: true
            }
        );
        assert_eq!(
            suggest(&files(&["Cargo.toml"]), &files(&["resume"]), None)
                .scripts
                .len(),
            1
        );
    }

    #[test]
    fn tokens_are_redacted() {
        assert_eq!(
            redact("bad token gho_abc123 here"),
            "bad token [token] here"
        );
        assert_eq!(redact("\"ghu_xyz\""), "\"[token]\"");
    }
}
