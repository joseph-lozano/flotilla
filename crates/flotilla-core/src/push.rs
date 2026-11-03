//! Notifications to paired phones, through Apple's push service (APNs).
//!
//! - **Off until it's set up.** It needs the team's APNs key (the `.p8` file Apple gives once),
//!   its key id and the team id. `flotilla setup --apns-key AuthKey_<KEY ID>.p8 --apns-team-id
//!   <TEAM ID>` keeps the key with the other secrets (as hex) and the ids in the database; or the
//!   key comes as a secret like the others (`FLOTILLA_APNS_KEY`, `--apns-key-file`, systemd's
//!   `apns-key` credential) with the ids in `FLOTILLA_APNS_KEY_ID` and `FLOTILLA_APNS_TEAM_ID`.
//!   A server with its own key sends to Apple itself.
//! - **Or through the relay.** Without a key, notifications go through Flotilla's push relay
//!   (`relay/`, at [`DEFAULT_RELAY`]; `flotilla setup --push-relay <URL>` for another, or
//!   `FLOTILLA_PUSH_RELAY`), whose team signs them. The phone registers with the relay and
//!   gives its server the handle, secret and key it got ([`PushRelay`]); the server encrypts
//!   each notification with the phone's key ([`seal`]), so the relay sees only ciphertext and
//!   asks APNs for a neutral alert the phone's extension fills in. Only the relay the server
//!   is set up for is ever sent to, and only registrations made with it.
//! - **Or off.** `flotilla setup --no-push-relay` (or `FLOTILLA_PUSH_RELAY=off`) without a key:
//!   nothing is sent anywhere, and the doctor says "Push: Off", as information.
//! - **Who gets them.** A paired device registers its APNs token (`push.register`): one per
//!   device, in place of the last, and a token moves to the device that registered it last. A
//!   revoked or lapsed device's token is never used; one APNs says is gone is forgotten.
//! - **What's sent** ([`alert`]): a turn that finished (not a delegated thread's: its answer
//!   reaches the thread that handed it the task), agents' answers reaching a thread (or word
//!   that a thread it handed work to couldn't start), a pull request merged, a project's setup
//!   failing, and a thread that didn't start. Each says which server and thread it's about, so
//!   a tap opens the thread. The host sends the same alerts to its clients (`Note::Alert`),
//!   whose desktops show them: one rule for every device.
//! - **How.** HTTP/2 to APNs, authenticated with an ES256 JWT made from the key ([`jwt`]) and
//!   made again every 40 minutes (Apple takes one for an hour), on a thread of its own so the
//!   host never waits on Apple. Nothing secret is logged: not the key, the JWT or a device's
//!   token (it's in the request's URL, which errors leave out).

use crate::secrets::{self, Name};
use crate::store::Store;
use base64::Engine;
use flotilla_proto::{Note, PushRelay, PushToken, Request};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// kv: the key's id, and the team's.
pub const KEY_ID_KEY: &str = "apns_key_id";
pub const TEAM_ID_KEY: &str = "apns_team_id";
/// What overrides them.
pub const KEY_ID_ENV: &str = "FLOTILLA_APNS_KEY_ID";
pub const TEAM_ID_ENV: &str = "FLOTILLA_APNS_TEAM_ID";
/// kv: a device's registration, by device id.
pub const TOKEN_PREFIX: &str = "push_token:";
/// The topics a phone may register: Flotilla's apps.
const TOPIC_PREFIX: &str = "com.useflotilla.";
/// A JWT is made again after this (APNs takes one up to an hour old, and refuses new ones
/// more often than every 20 minutes).
const JWT_EVERY: Duration = Duration::from_secs(40 * 60);
const TITLE_CHARS: usize = 80;
const BODY_CHARS: usize = 180;

/// The fix the doctor gives.
pub const SETUP_COMMAND: &str =
    "flotilla setup --apns-key AuthKey_<KEY ID>.p8 --apns-team-id <TEAM ID>";

/// kv: the push relay to send through without a key (its URL), or `off`.
pub const RELAY_KEY: &str = "push_relay";
/// What overrides it.
pub const RELAY_ENV: &str = "FLOTILLA_PUSH_RELAY";
/// Flotilla's own relay, for the Flotilla app.
pub const DEFAULT_RELAY: &str = "https://push.useflotilla.com";
/// The largest ciphertext the relay takes (base64): with its neutral alert, under APNs' 4 KB.
pub const RELAY_MAX_CIPHERTEXT: usize = 3000;

/// The key and who it's for.
pub struct Config {
    pub key_id: String,
    pub team_id: String,
    key: EcdsaKeyPair,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// From the key's text (a `.p8` file's PEM, or its DER in hex, as setup keeps it).
    pub fn new(key: &str, key_id: &str, team_id: &str) -> Result<Config, String> {
        let key_id = key_id.trim();
        let team_id = team_id.trim();
        if !apple_id(key_id) {
            return Err(format!(
                "{key_id:?} isn't an APNs key id (10 letters and digits)"
            ));
        }
        if !apple_id(team_id) {
            return Err(format!(
                "{team_id:?} isn't a team id (10 letters and digits)"
            ));
        }
        Ok(Config {
            key_id: key_id.to_string(),
            team_id: team_id.to_string(),
            key: key_pair(&der(key)?)?,
        })
    }
}

/// Apple's ids are 10 upper-case letters and digits.
fn apple_id(s: &str) -> bool {
    s.len() == 10
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The key's DER, from a PEM `PRIVATE KEY` or hex.
fn der(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.contains("-----BEGIN") {
        if !text.contains("-----BEGIN PRIVATE KEY-----") {
            return Err("The APNs key isn't a PKCS#8 private key (BEGIN PRIVATE KEY)".into());
        }
        let body: String = text
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        return base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| "The APNs key's PEM isn't readable".to_string());
    }
    if text.len().is_multiple_of(2)
        && !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return (0..text.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&text[i..i + 2], 16)
                    .map_err(|_| "The APNs key isn't readable".to_string())
            })
            .collect();
    }
    Err("The APNs key isn't a .p8 file's contents".into())
}

fn key_pair(der: &[u8]) -> Result<EcdsaKeyPair, String> {
    EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        der,
        &ring::rand::SystemRandom::new(),
    )
    .map_err(|e| format!("The APNs key isn't a P-256 private key: {e}"))
}

/// The key's text as `flotilla setup` keeps it (hex of its DER: a secret's characters), after
/// checking it's a key APNs takes.
pub fn keepable(p8: &str) -> Result<String, String> {
    let der = der(p8)?;
    key_pair(&der)?;
    Ok(der.iter().map(|b| format!("{b:02x}")).collect())
}

/// `AuthKey_ABC123DEFG.p8` → `ABC123DEFG`: Apple names the file by the key's id.
pub fn key_id_of_file(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_prefix("AuthKey_")?;
    apple_id(id).then(|| id.to_string())
}

/// Whether push is set up here, for the doctor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Off,
    /// No key: through this relay.
    Relay {
        url: String,
    },
    /// Its own key.
    Ready {
        key_id: String,
        team_id: String,
    },
    /// Set up, but it won't work: why.
    Broken(String),
    /// The relay it's given isn't one: why.
    BadRelay(String),
}

/// How this server sends notifications.
#[derive(Debug)]
pub enum Mode {
    /// To Apple, with its own key.
    Direct(Box<Config>),
    /// Through the relay at this URL.
    Relay(String),
}

/// The longest relay address taken.
pub const RELAY_URL_MAX: usize = 256;

/// A relay's URL as kept: `https://` (or `http://` to this machine, for trying one out),
/// with no query, fragment, password or trailing `/`, normalized.
pub fn relay_url(url: &str) -> Result<String, String> {
    if url.len() > RELAY_URL_MAX {
        return Err(format!(
            "A push relay's address is at most {RELAY_URL_MAX} characters"
        ));
    }
    let url = url.trim();
    let bad = || format!("{url:?} isn't a push relay's address (https://…)");
    let parsed = reqwest::Url::parse(url).map_err(|_| bad())?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let ok = match parsed.scheme() {
        "https" => parsed.host_str().is_some_and(|h| !h.is_empty()),
        "http" => local,
        _ => false,
    };
    if !ok
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(bad());
    }
    // As parsed: the host in lower case, no default port, no trailing `/`, so the same relay
    // written two ways is one.
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// The relay this server sends through when it has no key: `FLOTILLA_PUSH_RELAY`, else what
/// setup kept, else Flotilla's. None: off.
pub fn relay(store: Option<&Store>) -> Result<Option<String>, String> {
    let env = std::env::var(RELAY_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let kept = || {
        store
            .and_then(|s| s.kv_get(RELAY_KEY).ok().flatten())
            .filter(|v| !v.trim().is_empty())
    };
    let from_env = env.is_some();
    match env.or_else(kept) {
        Some(v) if v.trim().eq_ignore_ascii_case("off") => Ok(None),
        Some(v) => relay_url(&v).map(Some).map_err(|e| {
            if from_env {
                format!("{RELAY_ENV}: {e}")
            } else {
                e
            }
        }),
        None => Ok(Some(DEFAULT_RELAY.to_string())),
    }
}

/// How this server sends notifications: with its key, else through its relay. None: off.
pub fn mode(store: Option<&Store>) -> Result<Option<Mode>, String> {
    mode_given(config(store)?, store)
}

/// [`mode`], with the key this process found (`config`).
pub fn mode_given(key: Option<Config>, store: Option<&Store>) -> Result<Option<Mode>, String> {
    if let Some(c) = key {
        return Ok(Some(Mode::Direct(Box::new(c))));
    }
    Ok(relay(store)?.map(Mode::Relay))
}

/// The configuration this process finds: the key with the secrets, the ids in the environment
/// or the database. None: not set up.
pub fn config(store: Option<&Store>) -> Result<Option<Config>, String> {
    let Some((key, _)) = secrets::store()
        .get(Name::ApnsKey)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let kv = |k: &str| {
        store
            .and_then(|s| s.kv_get(k).ok().flatten())
            .filter(|v| !v.trim().is_empty())
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let key_id = env(KEY_ID_ENV)
        .or_else(|| kv(KEY_ID_KEY))
        .ok_or_else(|| format!("There's an APNs key but no key id: {SETUP_COMMAND}"))?;
    let team_id = env(TEAM_ID_ENV)
        .or_else(|| kv(TEAM_ID_KEY))
        .ok_or_else(|| format!("There's an APNs key but no team id: {SETUP_COMMAND}"))?;
    Config::new(&key, &key_id, &team_id).map(Some)
}

pub fn status(store: Option<&Store>) -> Status {
    match config(store) {
        Ok(Some(c)) => Status::Ready {
            key_id: c.key_id,
            team_id: c.team_id,
        },
        Err(e) => Status::Broken(e),
        Ok(None) => match relay(store) {
            Ok(Some(url)) => Status::Relay { url },
            Ok(None) => Status::Off,
            Err(e) => Status::BadRelay(e),
        },
    }
}

// MARK: The JWT

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// What the JWT signs: its header and claims, as APNs documents them.
pub fn signing_input(key_id: &str, team_id: &str, issued_secs: i64) -> String {
    let header = format!(r#"{{"alg":"ES256","kid":"{key_id}"}}"#);
    let claims = format!(r#"{{"iss":"{team_id}","iat":{issued_secs}}}"#);
    format!("{}.{}", b64(header.as_bytes()), b64(claims.as_bytes()))
}

/// The provider token for APNs' `authorization: bearer`, issued at `issued_secs`.
pub fn jwt(c: &Config, issued_secs: i64) -> Result<String, String> {
    let input = signing_input(&c.key_id, &c.team_id, issued_secs);
    let sig = c
        .key
        .sign(&ring::rand::SystemRandom::new(), input.as_bytes())
        .map_err(|_| "Couldn't sign the APNs token".to_string())?;
    Ok(format!("{input}.{}", b64(sig.as_ref())))
}

// MARK: What's sent

/// A notification: the same words go to phones (APNs) and to the server's clients, whose
/// desktops show them (`Note::Alert`).
pub use flotilla_proto::{Alert, AlertKind};

/// What an alert says about a thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadFacts {
    pub title: String,
    /// The agent's latest reply, as the sidebar previews it.
    pub reply: Option<String>,
    /// It was handed its task by another thread.
    pub delegated: bool,
    /// The question its agent last asked that still waits on you (`crate::asks`).
    pub asking: Option<String>,
}

/// What an alert says about a project's latest setup run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupFacts {
    pub branch: String,
    pub sha: Option<String>,
}

/// Where alerts find their words.
pub trait Lookup {
    fn thread(&self, id: &str) -> Option<ThreadFacts>;
    fn setup(&self, project: &str) -> Option<SetupFacts>;
}

impl Lookup for Store {
    fn thread(&self, id: &str) -> Option<ThreadFacts> {
        let t = Store::thread(self, id).ok().flatten()?;
        // The model's summary of the latest reply, if it's of that reply; else its first line.
        let reply = self.latest_reply(id).ok().flatten().map(|(key, text)| {
            match self.summary(id, "reply").ok().flatten() {
                Some((source, summary)) if source == key && !summary.trim().is_empty() => summary,
                _ => text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or_default()
                    .to_string(),
            }
        });
        let delegated = crate::orchestrate::parents(self).contains_key(id);
        let asking = self.waiting_question(id).ok().flatten();
        Some(ThreadFacts {
            title: t.title,
            reply,
            delegated,
            asking,
        })
    }

    fn setup(&self, project: &str) -> Option<SetupFacts> {
        let p = self.project(project).ok().flatten()?;
        let run = self
            .setup_runs(project, 1)
            .ok()
            .and_then(|r| r.into_iter().next());
        Some(SetupFacts {
            branch: p.default_branch,
            sha: run.map(|r| r.sha).filter(|s| !s.is_empty()),
        })
    }
}

fn title_of(t: &ThreadFacts) -> String {
    let title = crate::util::one_line(&t.title);
    crate::util::ellipsize(
        if title.is_empty() {
            "New thread"
        } else {
            &title
        },
        TITLE_CHARS,
    )
}

fn body(s: &str) -> String {
    crate::util::ellipsize(&crate::util::one_line(s), BODY_CHARS)
}

/// The notification a note makes, if it's one that notifies. A request's answer counts as a
/// note here too (`Note::Done` with the request): a start that failed tells whoever started it,
/// wherever they are.
pub fn alert(note: &Note, look: &dyn Lookup) -> Option<Alert> {
    match note {
        Note::TurnFinished(id) => {
            let t = look.thread(id)?;
            if t.delegated {
                return None;
            }
            let words = match t.reply.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                Some(r) => format!("Finished. {r}"),
                None => "Finished.".into(),
            };
            Some(Alert {
                kind: AlertKind::TurnFinished,
                title: title_of(&t),
                body: body(&words),
                thread: Some(id.clone()),
                about: id.clone(),
            })
        }
        // Its agent asked you something and waits: what, as a finished turn says how it went.
        // (A delegated thread's agent is answered by the thread that handed it the work.)
        Note::Asked(id) => {
            let t = look.thread(id)?;
            let question = t
                .asking
                .as_deref()
                .map(str::trim)
                .filter(|q| !q.is_empty() && !t.delegated)?;
            Some(Alert {
                kind: AlertKind::Asked,
                title: title_of(&t),
                body: body(&format!("Asks: {question}")),
                thread: Some(id.clone()),
                about: id.clone(),
            })
        }
        // Agents' answers, sent to the thread that asked for them; or word that a thread it
        // handed work to couldn't start.
        Note::Done {
            request: Request::Send { thread, text, .. },
            result: Ok(_),
        } => {
            let words = if let Some(members) = crate::side::parse_report(text) {
                match members.as_slice() {
                    [(title, _)] => format!("{} answered.", crate::util::one_line(title)),
                    m => format!("{} agents answered.", m.len()),
                }
            } else if let Some((title, _, how, _)) = crate::orchestrate::parse_answer(text) {
                format!("{} {how}.", crate::util::one_line(title))
            } else if let Some((title, _, _)) = crate::orchestrate::parse_couldnt_start(text) {
                format!("Couldn't start {}.", crate::util::one_line(title))
            } else {
                return None;
            };
            let t = look.thread(thread)?;
            Some(Alert {
                kind: AlertKind::AgentsAnswered,
                title: title_of(&t),
                body: body(&words),
                thread: Some(thread.clone()),
                about: thread.clone(),
            })
        }
        // A thread that didn't start: yours, or one another thread handed work to (which waits
        // on it). One refused before it was written has no thread, and its page says why.
        Note::Done {
            request: request @ (Request::NewThread(_) | Request::Retry(_)),
            result: Err(e),
        } => {
            let id = request.thread()?;
            let t = look.thread(id)?;
            let words = match e.trim() {
                "" => "Couldn't start.".to_string(),
                e => format!("Couldn't start: {e}"),
            };
            Some(Alert {
                kind: AlertKind::StartFailed,
                title: title_of(&t),
                body: body(&crate::util::redact(&words)),
                thread: Some(id.to_string()),
                about: id.to_string(),
            })
        }
        Note::PrMerged {
            thread,
            number,
            title,
            branch,
            archived,
        } => {
            let t = look.thread(thread);
            let heading = t.as_ref().map(title_of).unwrap_or_else(|| {
                crate::util::ellipsize(&crate::util::one_line(title), TITLE_CHARS)
            });
            let words = if *archived {
                format!("#{number} merged into {branch}. Archived.")
            } else {
                format!("#{number} merged into {branch}.")
            };
            Some(Alert {
                kind: AlertKind::PrMerged,
                title: heading,
                body: body(&words),
                thread: Some(thread.clone()),
                about: thread.clone(),
            })
        }
        // A scheduled prompt that didn't run (said once for a run of failures alike).
        Note::ScheduleFailed {
            schedule,
            prompt,
            thread,
            why,
        } => {
            let t = thread.as_deref().and_then(|t| look.thread(t));
            let heading = t.as_ref().map(title_of).unwrap_or_else(|| {
                crate::util::ellipsize(&crate::util::one_line(prompt), TITLE_CHARS)
            });
            let words = format!("Scheduled prompt didn't run. {}", why.trim());
            Some(Alert {
                kind: AlertKind::ScheduleFailed,
                title: heading,
                body: body(&crate::util::redact(&words)),
                thread: t.and_then(|_| thread.clone()),
                about: schedule.clone(),
            })
        }
        Note::SetupDone {
            project,
            name,
            ok: false,
            error,
            ..
        } => {
            let facts = look.setup(project).unwrap_or_default();
            let words = match facts.sha.as_deref() {
                Some(sha) if !facts.branch.is_empty() => format!(
                    "Setup failed on {}@{}.",
                    facts.branch,
                    crate::setup::short(sha)
                ),
                _ => match error.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
                    Some(e) => format!("Setup failed: {e}"),
                    None => "Setup failed.".into(),
                },
            };
            let heading = if name.trim().is_empty() {
                "Setup".to_string()
            } else {
                crate::util::ellipsize(&crate::util::one_line(name), TITLE_CHARS)
            };
            Some(Alert {
                kind: AlertKind::SetupFailed,
                title: heading,
                body: body(&crate::util::redact(&words)),
                thread: None,
                about: project.clone(),
            })
        }
        _ => None,
    }
}

#[derive(Serialize)]
struct Payload<'a> {
    aps: Aps<'a>,
    flotilla: Ours<'a>,
}

#[derive(Serialize)]
struct Aps<'a> {
    alert: Words<'a>,
    sound: &'a str,
    /// Notification Center groups by it.
    #[serde(rename = "thread-id")]
    thread_id: &'a str,
}

#[derive(Serialize)]
struct Words<'a> {
    title: &'a str,
    body: &'a str,
}

/// What the app reads when the notification is tapped.
#[derive(Serialize)]
struct Ours<'a> {
    server: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<&'a str>,
}

/// The JSON APNs is sent for `a`, from server `server` (its id).
pub fn payload(a: &Alert, server: &str) -> String {
    let p = Payload {
        aps: Aps {
            alert: Words {
                title: &a.title,
                body: &a.body,
            },
            sound: "default",
            thread_id: &a.about,
        },
        flotilla: Ours {
            server,
            kind: a.kind.as_str(),
            thread: a.thread.as_deref(),
        },
    };
    serde_json::to_string(&p).unwrap_or_default()
}

// MARK: Through the relay

/// Flotilla's push encryption, which the phone's extension undoes (`relay/README.md`, with
/// test vectors in `relay/test/vectors.json`): ChaCha20-Poly1305 with the phone's 32-byte key
/// and a random 12-byte nonce, no associated data; base64 of nonce ‖ ciphertext ‖ tag, as
/// CryptoKit's `ChaChaPoly.SealedBox.combined` is.
pub fn seal(key: &[u8; 32], plaintext: &str) -> Result<String, String> {
    let mut nonce = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .map_err(|_| "Couldn't make a nonce".to_string())?;
    seal_with(key, nonce, plaintext)
}

fn seal_with(key: &[u8; 32], nonce: [u8; 12], plaintext: &str) -> Result<String, String> {
    use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
    let key = LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| "That isn't a key".to_string())?,
    );
    let mut sealed = plaintext.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut sealed,
    )
    .map_err(|_| "Couldn't encrypt the notification".to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode([&nonce[..], &sealed].concat()))
}

/// A note as it's sealed for the relay: the payload with when it was sealed
/// (`flotilla.sent_at`, Unix seconds), so the phone shows one replayed much later as the
/// neutral alert.
pub fn stamped(payload: &str, now_secs: i64) -> String {
    let mut v: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    if let Some(ours) = v.get_mut("flotilla").and_then(|f| f.as_object_mut()) {
        ours.insert("sent_at".into(), now_secs.into());
    }
    v.to_string()
}

/// What [`seal`] sealed with `key`, if it did.
pub fn open(key: &[u8; 32], sealed: &str) -> Option<String> {
    use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(sealed)
        .ok()?;
    if bytes.len() < 12 + 16 {
        return None;
    }
    let (nonce, rest) = bytes.split_at(12);
    let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).ok()?);
    let mut rest = rest.to_vec();
    let plain = key
        .open_in_place(
            Nonce::try_assume_unique_for_key(nonce).ok()?,
            Aad::empty(),
            &mut rest,
        )
        .ok()?;
    String::from_utf8(plain.to_vec()).ok()
}

/// The collapse id the relay passes to APNs for a notification's own (`Job::collapse`): the
/// same for the same thread and kind, so a newer one replaces the last, and meaningless to
/// anyone without the phone's key.
pub fn collapse_id(key: &[u8; 32], collapse: &str) -> String {
    let tag = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key),
        format!("collapse:{collapse}").as_bytes(),
    );
    tag.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A phone's key, from its base64.
fn key_of(b64: &str) -> Option<[u8; 32]> {
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?
        .try_into()
        .ok()
}

/// What a phone registered with a relay, as kept: what the server sends the relay, and the
/// key it encrypts with. Never logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relayed {
    pub url: String,
    handle: String,
    secret: String,
    key: String,
}

impl std::fmt::Debug for Relayed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relayed")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn relayed(r: &PushRelay) -> Result<Relayed, String> {
    let shaped = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    let bad = || "That isn't a push relay's registration".to_string();
    if !shaped(&r.handle, 8..=64) || !shaped(&r.secret.0, 16..=128) || key_of(&r.key.0).is_none() {
        return Err(bad());
    }
    Ok(Relayed {
        url: relay_url(&r.url)?,
        handle: r.handle.clone(),
        secret: r.secret.0.clone(),
        key: r.key.0.trim().to_string(),
    })
}

// MARK: Who gets them

/// A device's registration, as kept.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Registered {
    token: String,
    env: String,
    topic: String,
    at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay: Option<Relayed>,
}

impl Registered {
    fn id(&self) -> String {
        id(&self.token, self.relay.as_ref())
    }
}

/// What tells one registration from another: the token, and the relay's handle if any (the
/// same phone registered again with the relay is a new registration).
fn id(token: &str, relay: Option<&Relayed>) -> String {
    match relay {
        Some(r) => format!("{token}@{}", r.handle),
        None => token.to_string(),
    }
}

/// One phone to notify.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub device: String,
    token: String,
    pub sandbox: bool,
    pub topic: String,
    /// What it registered with a relay, if it did.
    pub relay: Option<Relayed>,
}

impl Target {
    /// Its registration's [`id`].
    pub fn id(&self) -> String {
        id(&self.token, self.relay.as_ref())
    }
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("device", &self.device)
            .field("sandbox", &self.sandbox)
            .field("topic", &self.topic)
            .field("relay", &self.relay)
            .finish_non_exhaustive()
    }
}

/// Keep `device`'s token (and what it registered with a relay), in place of its last. The same
/// token on another device (the phone paired again) moves here, so a phone is notified once.
/// Returns the registration's id ([`Target::id`]).
pub fn register(store: &Store, device: &str, t: &PushToken, now_ms: i64) -> Result<String, String> {
    let token = t.token.0.trim().to_ascii_lowercase();
    if !(16..=400).contains(&token.len())
        || !token.len().is_multiple_of(2)
        || !token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("That isn't an APNs device token".into());
    }
    if t.env != "sandbox" && t.env != "production" {
        return Err(format!(
            "{:?} isn't an APNs environment (sandbox or production)",
            t.env
        ));
    }
    let topic = t.topic.trim();
    if !topic.starts_with(TOPIC_PREFIX)
        || topic.len() > 100
        || !topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(format!("{topic:?} isn't Flotilla's app"));
    }
    // Checked before anything changes.
    if let Some(r) = &t.relay {
        relayed(r)?;
    }
    for (key, value) in store.kv_with_prefix(TOKEN_PREFIX).map_err(|e| e.plain())? {
        if key != format!("{TOKEN_PREFIX}{device}")
            && serde_json::from_str::<Registered>(&value).is_ok_and(|r| r.token == token)
        {
            store.kv_delete(&key).map_err(|e| e.plain())?;
        }
    }
    let relay = t.relay.as_ref().map(relayed).transpose()?;
    let r = Registered {
        token,
        env: t.env.clone(),
        topic: topic.to_string(),
        at_ms: now_ms,
        relay,
    };
    store
        .kv_set(
            &format!("{TOKEN_PREFIX}{device}"),
            &serde_json::to_string(&r).unwrap_or_default(),
        )
        .map_err(|e| e.plain())?;
    Ok(r.id())
}

/// The id of what `device` has registered ([`Target::id`]), if anything.
pub fn registration_of(store: &Store, device: &str) -> Option<String> {
    let v = store
        .kv_get(&format!("{TOKEN_PREFIX}{device}"))
        .ok()
        .flatten()?;
    serde_json::from_str::<Registered>(&v).ok().map(|r| r.id())
}

/// Stop notifying `device` (it asked, or it was revoked).
pub fn unregister(store: &Store, device: &str) -> crate::error::Result<()> {
    store.kv_delete(&format!("{TOKEN_PREFIX}{device}"))
}

/// APNs (or the relay) says registration `id` is no good: forget it, if it's still `device`'s.
pub fn forget(store: &Store, device: &str, id: &str) {
    let key = format!("{TOKEN_PREFIX}{device}");
    if store
        .kv_get(&key)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<Registered>(&v).ok())
        .is_some_and(|r| r.id() == id)
    {
        let _ = store.kv_delete(&key);
    }
}

/// Every registered device still paired.
pub fn targets(store: &Store) -> Vec<Target> {
    let paired: std::collections::HashSet<String> = store
        .devices(crate::util::now_ms())
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.id)
        .collect();
    store
        .kv_with_prefix(TOKEN_PREFIX)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| {
            let device = key.strip_prefix(TOKEN_PREFIX)?.to_string();
            let r: Registered = serde_json::from_str(&value).ok()?;
            paired.contains(&device).then_some(Target {
                device,
                token: r.token,
                sandbox: r.env == "sandbox",
                topic: r.topic,
                relay: r.relay,
            })
        })
        .collect()
}

// MARK: Sending

/// One notification for some phones.
#[derive(Debug, Clone)]
pub struct Job {
    pub kind: &'static str,
    pub targets: Vec<Target>,
    pub payload: String,
    /// Replaces an earlier one with the same id.
    pub collapse: String,
}

impl Job {
    pub fn new(alert: &Alert, server: &str, targets: Vec<Target>) -> Job {
        let mut collapse = format!("{}:{}", alert.kind.as_str(), alert.about);
        collapse.truncate(64);
        Job {
            kind: alert.kind.as_str(),
            targets,
            payload: payload(alert, server),
            collapse,
        }
    }
}

/// Each device's registration now ([`Target::id`]), shared with the sending thread: a queued
/// job's target whose device was revoked, unregistered or registered again since is skipped.
#[derive(Clone, Default)]
pub struct Registrations(Arc<Mutex<HashMap<String, String>>>);

impl Registrations {
    pub fn of(targets: &[Target]) -> Registrations {
        let r = Registrations::default();
        for t in targets {
            r.set(&t.device, Some(&t.id()));
        }
        r
    }

    /// `device`'s registration now (none: it gets nothing). A token is one device's.
    pub fn set(&self, device: &str, id: Option<&str>) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match id {
            Some(id) => {
                let token = id.split('@').next().unwrap_or(id);
                m.retain(|d, held| d == device || held.split('@').next() != Some(token));
                m.insert(device.to_string(), id.to_string());
            }
            None => {
                m.remove(device);
            }
        }
    }

    /// Still the device's registration.
    pub fn current(&self, t: &Target) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&t.device)
            == Some(&t.id())
    }
}

/// Where APNs is: Apple's, but for tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub sandbox: String,
    pub production: String,
}

impl Endpoints {
    pub fn apple() -> Endpoints {
        Endpoints {
            sandbox: "https://api.sandbox.push.apple.com".into(),
            production: "https://api.push.apple.com".into(),
        }
    }
}

/// How long a notification the relay didn't take for now (a 429, a 5xx, no answer) waits
/// before each try again: the relay's `retry-after` if that's longer (up to
/// [`RETRY_AFTER_MAX`]). After the last, it's given up. Kept in memory only.
pub const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
/// The longest `retry-after` heeded.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(15 * 60);

/// Where APNs is and how long retries wait: Apple's and [`RETRY_DELAYS`], but for tests.
#[derive(Debug, Clone)]
pub struct Options {
    pub apns: Endpoints,
    pub retries: Vec<Duration>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            apns: Endpoints::apple(),
            retries: RETRY_DELAYS.to_vec(),
        }
    }
}

/// A notification for one phone that didn't go for now, to try again at `due`.
struct Waiting {
    due: Instant,
    job: Job,
    tries: usize,
}

/// Sends jobs off the caller's thread.
pub struct Pusher {
    send: Box<dyn Fn(Job) + Send>,
    pub registrations: Registrations,
    /// The relay it sends through, if it does.
    relay: Option<String>,
}

impl Pusher {
    /// Send as `mode` says (to Apple, or through the relay), on a thread of its own, to the
    /// devices `registrations` says are still registered. `gone(device, id)`: APNs or the
    /// relay says the registration is no longer the device's.
    pub fn start(
        mode: Mode,
        registrations: Registrations,
        gone: impl Fn(String, String) + Send + 'static,
    ) -> Result<Pusher, String> {
        Pusher::start_to(mode, Options::default(), registrations, gone)
    }

    /// [`Pusher::start`], with APNs where `options` says, and its retries.
    pub fn start_to(
        mode: Mode,
        options: Options,
        registrations: Registrations,
        gone: impl Fn(String, String) + Send + 'static,
    ) -> Result<Pusher, String> {
        let (tx, rx) = mpsc::channel::<Job>();
        let live = registrations.clone();
        let relay = match &mode {
            Mode::Relay(url) => Some(url.clone()),
            Mode::Direct(_) => None,
        };
        std::thread::Builder::new()
            .name("flotilla-push".into())
            .spawn(move || {
                let via = match mode {
                    Mode::Direct(config) => {
                        Apns::new(*config, options.apns).map(|a| Via::Apns(Box::new(a)))
                    }
                    Mode::Relay(url) => Relay::new(url).map(Via::Relay),
                };
                let mut via = match via {
                    Ok(v) => v,
                    Err(e) => return eprintln!("[push] can't send notifications: {e}"),
                };
                let delays = options.retries;
                let mut waiting: Vec<Waiting> = Vec::new();
                loop {
                    let job = match waiting.iter().map(|w| w.due).min() {
                        None => match rx.recv() {
                            Ok(job) => Some(job),
                            Err(_) => return,
                        },
                        Some(due) => {
                            match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                                Ok(job) => Some(job),
                                Err(mpsc::RecvTimeoutError::Timeout) => None,
                                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    };
                    if let Some(job) = job {
                        // A newer note about the same thing takes the place of one waiting to
                        // go again, phone by phone.
                        waiting.retain(|w| {
                            w.job.collapse != job.collapse
                                || !job
                                    .targets
                                    .iter()
                                    .any(|t| t.device == w.job.targets[0].device)
                        });
                        via.send(&job, 0, &live, &gone, &delays, &mut waiting);
                    }
                    let now = Instant::now();
                    let (due, later): (Vec<Waiting>, Vec<Waiting>) =
                        waiting.drain(..).partition(|w| w.due <= now);
                    waiting = later;
                    // Each looked at again as it goes (`Via::send`): a phone revoked or
                    // registered again since gets nothing.
                    for w in due {
                        via.send(&w.job, w.tries, &live, &gone, &delays, &mut waiting);
                    }
                }
            })
            .map_err(|e| format!("Couldn't start sending notifications: {e}"))?;
        Ok(Pusher {
            send: Box::new(move |job| drop(tx.send(job))),
            registrations,
            relay,
        })
    }

    /// Tests: hand each job to `f`.
    pub fn with(f: impl Fn(Job) + Send + 'static) -> Pusher {
        Pusher {
            send: Box::new(f),
            registrations: Registrations::default(),
            relay: None,
        }
    }

    /// The relay it sends through, if it does (`push.register`'s answer).
    pub fn relay(&self) -> Option<&str> {
        self.relay.as_deref()
    }

    /// Whether it can notify `t`: any phone with its own key; through the relay, only one
    /// that registered with that relay.
    pub fn reaches(&self, t: &Target) -> bool {
        match &self.relay {
            None => true,
            Some(url) => t.relay.as_ref().is_some_and(|r| &r.url == url),
        }
    }

    pub fn push(&self, job: Job) {
        (self.send)(job)
    }
}

/// What APNs, or the relay, said about one notification.
enum Said {
    Sent,
    /// The registration isn't the phone's any more.
    Gone,
    /// The JWT was refused for its age: make one again.
    Stale,
    Refused(u16, String),
    /// Not now (a 429, a 5xx, no answer): again later, after this if it said.
    Later(u16, String, Option<Duration>),
}

/// How jobs go: to APNs with the server's key, or through the relay.
enum Via {
    Apns(Box<Apns>),
    Relay(Relay),
}

impl Via {
    /// Send `job` to each of its phones still registered. One the relay didn't take for now
    /// waits in `waiting` to go again (`tries` so far), up to `delays.len()` times.
    fn send(
        &mut self,
        job: &Job,
        tries: usize,
        live: &Registrations,
        gone: &dyn Fn(String, String),
        delays: &[Duration],
        waiting: &mut Vec<Waiting>,
    ) {
        let mut sent = 0;
        let to = match self {
            Via::Apns(_) => "APNs",
            Via::Relay(_) => "The relay",
        };
        // Looked at as each goes: an earlier send can take seconds.
        for target in job.targets.iter().filter(|t| live.current(t)) {
            let said = match self {
                Via::Apns(apns) => match apns.post(job, target, false) {
                    Said::Stale => apns.post(job, target, true),
                    said => said,
                },
                Via::Relay(relay) => match &target.relay {
                    Some(r) if r.url == relay.url => relay.post(job, r),
                    // Registered with another relay, or none: it can't be reached from here.
                    _ => continue,
                },
            };
            match said {
                Said::Sent => sent += 1,
                Said::Gone => {
                    eprintln!(
                        "[push] device {} no longer takes notifications: forgotten",
                        target.device
                    );
                    gone(target.device.clone(), target.id());
                }
                Said::Stale => eprintln!("[push] APNs refused the provider token twice"),
                Said::Refused(status, why) => eprintln!(
                    "[push] {to} didn't take a notification for device {}: {status} {why}",
                    target.device
                ),
                Said::Later(status, why, after) => match delays.get(tries) {
                    Some(delay) => {
                        let wait = after.map_or(*delay, |a| a.min(RETRY_AFTER_MAX).max(*delay));
                        eprintln!(
                            "[push] {to} didn't take a notification for device {} ({status} {why}): again in {}s",
                            target.device,
                            wait.as_secs()
                        );
                        waiting.push(Waiting {
                            due: Instant::now() + wait,
                            job: Job {
                                targets: vec![target.clone()],
                                ..job.clone()
                            },
                            tries: tries + 1,
                        });
                    }
                    None => eprintln!(
                        "[push] {to} didn't take a notification for device {} ({status} {why}): given up after {tries} tries",
                        target.device
                    ),
                },
            }
        }
        if sent > 0 {
            eprintln!(
                "[push] sent {} to {sent} device{}",
                job.kind,
                if sent == 1 { "" } else { "s" }
            );
        }
    }
}

/// APNs over HTTP/2, with the JWT it last made.
struct Apns {
    config: Config,
    at: Endpoints,
    http: reqwest::blocking::Client,
    jwt: Option<(String, Instant)>,
}

impl Apns {
    fn new(config: Config, at: Endpoints) -> Result<Apns, String> {
        let http = reqwest::blocking::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.without_url().to_string())?;
        Ok(Apns {
            config,
            at,
            http,
            jwt: None,
        })
    }

    fn token(&mut self, fresh: bool) -> Result<String, String> {
        match &self.jwt {
            Some((t, at)) if !fresh && at.elapsed() < JWT_EVERY => Ok(t.clone()),
            _ => {
                let t = jwt(&self.config, crate::util::now_ms() / 1000)?;
                self.jwt = Some((t.clone(), Instant::now()));
                Ok(t)
            }
        }
    }

    fn post(&mut self, job: &Job, target: &Target, fresh: bool) -> Said {
        let jwt = match self.token(fresh) {
            Ok(t) => t,
            Err(e) => return Said::Refused(0, e),
        };
        let origin = if target.sandbox {
            &self.at.sandbox
        } else {
            &self.at.production
        };
        let expires = crate::util::now_ms() / 1000 + 24 * 3600;
        let result = self
            .http
            .post(format!("{origin}/3/device/{}", target.token))
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &target.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expires.to_string())
            .header("apns-collapse-id", &job.collapse)
            .header("content-type", "application/json")
            .body(job.payload.clone())
            .send();
        let response = match result {
            Ok(r) => r,
            // Without the URL: it holds the device's token.
            Err(e) => return Said::Refused(0, e.without_url().to_string()),
        };
        let status = response.status().as_u16();
        let reason = response
            .json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
            .unwrap_or_default();
        said(status, &reason, fresh)
    }
}

/// The push relay (`relay/README.md`): each notification sealed with its phone's key.
struct Relay {
    url: String,
    http: reqwest::blocking::Client,
}

impl Relay {
    fn new(url: String) -> Result<Relay, String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("flotilla/", env!("CARGO_PKG_VERSION")))
            // The handle and secret go only to the relay this server is set up for: a redirect
            // would send them on somewhere else.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.without_url().to_string())?;
        Ok(Relay { url, http })
    }

    fn post(&self, job: &Job, r: &Relayed) -> Said {
        let Some(key) = key_of(&r.key) else {
            return Said::Gone;
        };
        let note = stamped(&job.payload, crate::util::now_ms() / 1000);
        let ciphertext = match seal(&key, &note) {
            Ok(c) => c,
            Err(e) => return Said::Refused(0, e),
        };
        if ciphertext.len() > RELAY_MAX_CIPHERTEXT {
            return Said::Refused(413, "too large for the relay".into());
        }
        let body = serde_json::json!({
            "handle": r.handle,
            "secret": r.secret,
            "ciphertext": ciphertext,
            "collapse": collapse_id(&key, &job.collapse),
        });
        let response = match self
            .http
            .post(format!("{}/v1/push", self.url))
            .json(&body)
            .send()
        {
            Ok(r) => r,
            Err(e) => return Said::Later(0, e.without_url().to_string(), None),
        };
        let status = response.status().as_u16();
        let after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        match status {
            200 | 204 => Said::Sent,
            // APNs told the relay the phone's gone.
            410 => Said::Gone,
            300..=399 => Said::Refused(status, "a redirect, which isn't followed".into()),
            _ => {
                let answer = response.json::<serde_json::Value>().ok();
                let field = |k: &str| {
                    answer
                        .as_ref()
                        .and_then(|v| v.get(k).and_then(|e| e.as_str()).map(str::to_string))
                };
                // The relay doesn't know the handle (the phone left it, or it expired): only its
                // own answer says so, not any 404 (a wrong path, a host it isn't on).
                if status == 404 && field("code").as_deref() == Some("no_such_handle") {
                    return Said::Gone;
                }
                let why = crate::util::redact(&field("error").unwrap_or_default());
                match status {
                    429 | 500..=599 => Said::Later(status, why, after),
                    _ => Said::Refused(status, why),
                }
            }
        }
    }
}

/// What an answer from APNs means. `fresh`: the JWT was just made.
fn said(status: u16, reason: &str, fresh: bool) -> Said {
    match (status, reason) {
        (200, _) => Said::Sent,
        (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => Said::Gone,
        (403, "ExpiredProviderToken" | "InvalidProviderToken") if !fresh => Said::Stale,
        (s, r) => Said::Refused(s, crate::util::redact(r)),
    }
}

/// A stand-in relay, for tests here and in other crates.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::sync::{Arc, Mutex};

    /// One answer of the stand-in relay's.
    pub struct Answer {
        pub status: u16,
        /// Extra headers, as `name: value` lines.
        pub headers: String,
        pub body: String,
    }

    impl From<u16> for Answer {
        fn from(status: u16) -> Answer {
            Answer {
                status,
                headers: String::new(),
                body: "{}".into(),
            }
        }
    }

    /// A stand-in relay on a local port: answers each push with the next of `answers` (then
    /// 204), and keeps what it was sent.
    pub fn fake_relay<A: Into<Answer>>(
        answers: Vec<A>,
    ) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let answers: Vec<Answer> = answers.into_iter().map(Into::into).collect();
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let held = seen.clone();
        let answers = Arc::new(Mutex::new(std::collections::VecDeque::from(answers)));
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = conn.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (String::new(), Vec::new());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text[..h]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= h + 4 + len {
                            break (text[..h].to_string(), buf[h + 4..h + 4 + len].to_vec());
                        }
                    }
                };
                let mut sent: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                sent["_line"] = head.lines().next().unwrap_or_default().into();
                held.lock().unwrap().push(sent);
                let a = answers.lock().unwrap().pop_front().unwrap_or(204.into());
                let _ = write!(
                    conn,
                    "HTTP/1.1 {} X\r\ncontent-type: application/json\r\n{}content-length: {}\r\nconnection: close\r\n\r\n{}",
                    a.status,
                    a.headers,
                    a.body.len(),
                    a.body
                );
            }
        });
        (url, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair, UnparsedPublicKey};

    /// A P-256 key made for these tests (`openssl genpkey -algorithm EC -pkeyopt
    /// ec_paramgen_curve:P-256`), and its public key.
    const TEST_P8: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg6z48n4GLbR6j/1dc
2OyUD5ox0tSKqT5xA6JRnsjgB6GhRANCAASrhjQXMLZ4MEuZ5F9lDX0ccqYB+33c
pv0zsscQHkNwBtHmaIeo4wdam38MPEW9MsTfVDD5y0wDXYO99o889Uvj
-----END PRIVATE KEY-----
";
    const PRIVATE: &str = "eb3e3c9f818b6d1ea3ff575cd8ec940f9a31d2d48aa93e7103a2519ec8e007a1";
    const PUBLIC: &str = "04ab86341730b678304b99e45f650d7d1c72a601fb7ddca6fd33b2c7101e437006d1e66887a8e3075a9b7f0c3c45bd32c4df5430f9cb4c035d83bdf68f3cf54be3";
    /// The JWT for key id ABC123DEFG, team DEF123GHIJ, issued at 1700000000, signed with the key
    /// above by OpenSSL 3.6 (`openssl dgst -sha256 -sign`, its DER signature as r‖s).
    const OPENSSL_JWT: &str = "eyJhbGciOiJFUzI1NiIsImtpZCI6IkFCQzEyM0RFRkcifQ.eyJpc3MiOiJERUYxMjNHSElKIiwiaWF0IjoxNzAwMDAwMDAwfQ.DDjcyA3rlxmlxuVrgoQ6C1zlPD0yOc4kMhefYN2m4DkJD0aGAJw4-w33xcfncvSp1GuLY1ZExsSTkGRl3qaTuA";

    fn hex(s: &str) -> Vec<u8> {
        der(s).unwrap()
    }

    fn verifies(jwt: &str) -> bool {
        let Some((input, sig)) = jwt.rsplit_once('.') else {
            return false;
        };
        let Ok(sig) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig) else {
            return false;
        };
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, hex(PUBLIC))
            .verify(input.as_bytes(), &sig)
            .is_ok()
    }

    fn config() -> Config {
        Config::new(TEST_P8, "ABC123DEFG", "DEF123GHIJ").unwrap()
    }

    #[test]
    fn the_jwt_is_apples_es256_token() {
        // The header and claims, byte for byte.
        assert_eq!(
            signing_input("ABC123DEFG", "DEF123GHIJ", 1700000000),
            "eyJhbGciOiJFUzI1NiIsImtpZCI6IkFCQzEyM0RFRkcifQ.eyJpc3MiOiJERUYxMjNHSElKIiwiaWF0IjoxNzAwMDAwMDAwfQ"
        );
        assert_eq!(
            OPENSSL_JWT.rsplit_once('.').unwrap().0,
            signing_input("ABC123DEFG", "DEF123GHIJ", 1700000000)
        );
        // OpenSSL's signature checks out against the key, so the form is right (r‖s, base64url).
        assert!(verifies(OPENSSL_JWT));
        // Ours (ECDSA is randomised: a different signature each time) does too.
        let c = config();
        assert_eq!(c.key.public_key().as_ref(), hex(PUBLIC).as_slice());
        let ours = jwt(&c, 1700000000).unwrap();
        assert!(ours.starts_with(&format!(
            "{}.",
            signing_input("ABC123DEFG", "DEF123GHIJ", 1700000000)
        )));
        assert_eq!(
            ours.rsplit_once('.').unwrap().1.len(),
            86,
            "64 bytes, unpadded"
        );
        assert!(verifies(&ours));
        assert_ne!(jwt(&c, 1700000000).unwrap(), ours);
        // Changed by a byte, it doesn't.
        let mut bad = ours.clone().into_bytes();
        let i = bad.len() - 10;
        bad[i] = if bad[i] == b'A' { b'B' } else { b'A' };
        assert!(!verifies(&String::from_utf8(bad).unwrap()));
        // Nor does it for other claims.
        let sig = ours.rsplit_once('.').unwrap().1;
        assert!(!verifies(&format!(
            "{}.{sig}",
            signing_input("ABC123DEFG", "DEF123GHIJ", 1700000001)
        )));
    }

    #[test]
    fn keys_come_as_apples_p8_or_as_kept() {
        // Apple's .p8 also has the curve inside ([0] parameters): the same key, built by hand.
        let ec = [
            &[0x02, 0x01, 0x01, 0x04, 0x20][..],
            &hex(PRIVATE),
            &[
                0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
            ],
            &[0xa1, 0x44, 0x03, 0x42, 0x00],
            &hex(PUBLIC),
        ]
        .concat();
        let alg = [
            0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
            0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
        ];
        let inner = [&[0x30, ec.len() as u8][..], &ec].concat();
        let body = [
            &[0x02, 0x01, 0x00][..],
            &alg,
            &[0x04, inner.len() as u8],
            &inner,
        ]
        .concat();
        let apple = [&[0x30, 0x81, body.len() as u8][..], &body].concat();
        assert_eq!(apple.len(), 150);
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----",
            base64::engine::general_purpose::STANDARD.encode(&apple)
        );
        let c = Config::new(&pem, "ABC123DEFG", "DEF123GHIJ").unwrap();
        assert!(verifies(&jwt(&c, 1700000000).unwrap()));
        // As setup keeps it: hex, which a secret may hold.
        let kept = keepable(TEST_P8).unwrap();
        assert!(kept.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(verifies(
            &jwt(&Config::new(&kept, "ABC123DEFG", "DEF123GHIJ").unwrap(), 1).unwrap()
        ));
        // What isn't a key, or the right ids, says so.
        assert!(
            Config::new(
                "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----",
                "ABC123DEFG",
                "DEF123GHIJ"
            )
            .unwrap_err()
            .contains("PKCS#8")
        );
        assert!(Config::new("nonsense", "ABC123DEFG", "DEF123GHIJ").is_err());
        assert!(
            Config::new(TEST_P8, "abc", "DEF123GHIJ")
                .unwrap_err()
                .contains("key id")
        );
        assert!(
            Config::new(TEST_P8, "ABC123DEFG", "team")
                .unwrap_err()
                .contains("team id")
        );
        assert!(!format!("{c:?}").contains(&kept[..12]));
        assert_eq!(
            key_id_of_file(std::path::Path::new("/tmp/AuthKey_ABC123DEFG.p8")).as_deref(),
            Some("ABC123DEFG")
        );
        assert_eq!(key_id_of_file(std::path::Path::new("/tmp/key.p8")), None);
    }

    struct Facts;

    impl Lookup for Facts {
        fn thread(&self, id: &str) -> Option<ThreadFacts> {
            match id {
                "t1" => Some(ThreadFacts {
                    title: "Fix flaky retry test".into(),
                    reply: Some("PR #42 is open and its 12 checks pass.".into()),
                    delegated: false,
                    asking: Some("Which file name should I create?\nAnd where?".into()),
                }),
                "t2" => Some(ThreadFacts {
                    title: "Review median.py".into(),
                    reply: None,
                    delegated: false,
                    asking: None,
                }),
                "child" => Some(ThreadFacts {
                    title: "Add tests".into(),
                    reply: Some("Done".into()),
                    delegated: true,
                    asking: Some("Which?".into()),
                }),
                _ => None,
            }
        }

        fn setup(&self, project: &str) -> Option<SetupFacts> {
            (project == "p1").then(|| SetupFacts {
                branch: "main".into(),
                sha: Some("9f3c2ab51e0d".into()),
            })
        }
    }

    fn send(thread: &str, text: &str) -> Note {
        Note::Done {
            request: Request::Send {
                thread: thread.into(),
                text: text.into(),
                files: vec![],
                local: String::new(),
            },
            result: Ok(flotilla_proto::Reply::Done),
        }
    }

    #[test]
    fn the_notes_that_push_and_their_payloads() {
        let server = "srv_3f5c1a7e92d4";
        let pushed = |n: &Note| alert(n, &Facts).map(|a| payload(&a, server));
        // A turn that finished: its title, and what the agent said.
        assert_eq!(
            pushed(&Note::TurnFinished("t1".into())).unwrap(),
            r#"{"aps":{"alert":{"title":"Fix flaky retry test","body":"Finished. PR #42 is open and its 12 checks pass."},"sound":"default","thread-id":"t1"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"turn_finished","thread":"t1"}}"#
        );
        assert_eq!(
            alert(&Note::TurnFinished("t2".into()), &Facts)
                .unwrap()
                .body,
            "Finished."
        );
        // A delegated thread's turn goes back to its parent, which says so; a thread gone, nothing.
        assert_eq!(pushed(&Note::TurnFinished("child".into())), None);
        assert_eq!(pushed(&Note::TurnFinished("gone".into())), None);
        // Its agent asked you something: the question, on one line, as a finished turn's reply.
        assert_eq!(
            pushed(&Note::Asked("t1".into())).unwrap(),
            r#"{"aps":{"alert":{"title":"Fix flaky retry test","body":"Asks: Which file name should I create? And where?"},"sound":"default","thread-id":"t1"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"asked","thread":"t1"}}"#
        );
        // Nothing waits any more, or a delegated thread (its parent answers): nothing.
        assert_eq!(pushed(&Note::Asked("t2".into())), None);
        assert_eq!(pushed(&Note::Asked("child".into())), None);
        // Agents' answers reaching the thread that asked.
        let report = "[flotilla] Agents answered (2):\n\n### \"Codex review\" (a) finished:\n\n7/10\n\n### \"Haiku review\" (b) finished:\n\n3/10";
        assert_eq!(
            pushed(&send("t2", report)).unwrap(),
            r#"{"aps":{"alert":{"title":"Review median.py","body":"2 agents answered."},"sound":"default","thread-id":"t2"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"agents_answered","thread":"t2"}}"#
        );
        assert_eq!(
            alert(
                &send(
                    "t2",
                    "[flotilla] Agents answered (1):\n\n### \"Codex review\" (a) finished:\n\nok"
                ),
                &Facts
            )
            .unwrap()
            .body,
            "Codex review answered."
        );
        assert_eq!(alert(&send("t1", "[flotilla] \"Add tests\" (c1, branch flotilla/add-tests) finished:\n\nAdded 4 tests."), &Facts).unwrap().body, "Add tests finished.");
        assert_eq!(
            alert(
                &send(
                    "t1",
                    "[flotilla] \"Add tests\" (c1) was stopped without an answer."
                ),
                &Facts
            )
            .unwrap()
            .body,
            "Add tests was stopped without an answer."
        );
        // A thread it handed work to couldn't start: the thread that asked (and waits) hears.
        let couldnt = crate::orchestrate::couldnt_start_text("Add tests", "c1", "boat said 402");
        assert_eq!(
            alert(&send("t1", &couldnt), &Facts).unwrap(),
            Alert {
                kind: AlertKind::AgentsAnswered,
                title: "Fix flaky retry test".into(),
                body: "Couldn't start Add tests.".into(),
                thread: Some("t1".into()),
                about: "t1".into()
            }
        );
        // Your own messages, and one that failed to go, aren't news.
        assert_eq!(pushed(&send("t1", "also add tests")), None);
        let failed = Note::Done {
            request: Request::Send {
                thread: "t2".into(),
                text: report.into(),
                files: vec![],
                local: String::new(),
            },
            result: Err("boat refused".into()),
        };
        assert_eq!(pushed(&failed), None);
        // A thread that didn't start, whoever started it: yours, a retry, or a delegate's (its
        // parent waits on it). One refused before it was written isn't a thread yet.
        let new = |id: Option<&str>| flotilla_proto::NewThread {
            environment: "e".into(),
            project_id: None,
            provider: "claude".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "hi".into(),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: id.map(str::to_string),
            parent: None,
            branch_tag: None,
        };
        let start = |id: Option<&str>, e: &str| Note::Done {
            request: Request::NewThread(Box::new(new(id))),
            result: Err(e.into()),
        };
        assert_eq!(
            pushed(&start(Some("t2"), "boat said 402: no VMs left")).unwrap(),
            r#"{"aps":{"alert":{"title":"Review median.py","body":"Couldn't start: boat said 402: no VMs left"},"sound":"default","thread-id":"t2"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"start_failed","thread":"t2"}}"#
        );
        assert_eq!(
            alert(&start(Some("child"), "no VM"), &Facts)
                .unwrap()
                .thread
                .as_deref(),
            Some("child")
        );
        assert_eq!(
            alert(
                &Note::Done {
                    request: Request::Retry("t1".into()),
                    result: Err(String::new())
                },
                &Facts
            )
            .unwrap()
            .body,
            "Couldn't start."
        );
        assert_eq!(pushed(&start(None, "no VM")), None);
        assert_eq!(pushed(&start(Some("gone"), "no VM")), None);
        assert_eq!(
            pushed(&Note::Done {
                request: Request::Retry("t1".into()),
                result: Ok(flotilla_proto::Reply::Done)
            }),
            None
        );
        // A merge.
        assert_eq!(
            pushed(&Note::PrMerged {
                thread: "t1".into(),
                number: 42,
                title: "Fix the retry".into(),
                branch: "main".into(),
                archived: false
            })
            .unwrap(),
            r##"{"aps":{"alert":{"title":"Fix flaky retry test","body":"#42 merged into main."},"sound":"default","thread-id":"t1"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"pr_merged","thread":"t1"}}"##
        );
        // A setup that failed: the project, no thread.
        let failed = Note::SetupDone {
            project: "p1".into(),
            name: "flotilla-playground".into(),
            ok: false,
            error: Some("exit 1".into()),
            recovered: false,
            fixable: None,
        };
        assert_eq!(
            pushed(&failed).unwrap(),
            r#"{"aps":{"alert":{"title":"flotilla-playground","body":"Setup failed on main@9f3c2ab."},"sound":"default","thread-id":"p1"},"flotilla":{"server":"srv_3f5c1a7e92d4","kind":"setup_failed"}}"#
        );
        let unknown = Note::SetupDone {
            project: "p9".into(),
            name: "web".into(),
            ok: false,
            error: Some("boat said 500".into()),
            recovered: false,
            fixable: None,
        };
        assert_eq!(
            alert(&unknown, &Facts).unwrap().body,
            "Setup failed: boat said 500"
        );
        let passed = Note::SetupDone {
            project: "p1".into(),
            name: "web".into(),
            ok: true,
            error: None,
            recovered: true,
            fixable: None,
        };
        assert_eq!(pushed(&passed), None);
        // A scheduled prompt that didn't run: its thread's title (a click opens it), else the
        // prompt's first line.
        let skipped = |thread: Option<&str>| Note::ScheduleFailed {
            schedule: "s1".into(),
            prompt: "Check the nightly CI\nand report".into(),
            thread: thread.map(str::to_string),
            why: "The thread is archived.".into(),
        };
        assert_eq!(
            pushed(&skipped(Some("t1"))).unwrap(),
            format!(
                r#"{{"aps":{{"alert":{{"title":"Fix flaky retry test","body":"Scheduled prompt didn't run. The thread is archived."}},"sound":"default","thread-id":"s1"}},"flotilla":{{"server":"{server}","kind":"schedule_failed","thread":"t1"}}}}"#
            )
        );
        let new_thread = alert(&skipped(None), &Facts).unwrap();
        assert_eq!(
            (new_thread.title.as_str(), new_thread.thread),
            ("Check the nightly CI and report", None)
        );
        assert_eq!(alert(&skipped(Some("gone")), &Facts).unwrap().thread, None);
        assert_eq!(pushed(&Note::Summaries(Ok("now".into()))), None);
        // Long words are cut to fit a notification.
        let long = ThreadFacts {
            title: "t".repeat(300),
            reply: Some("r ".repeat(400)),
            delegated: false,
            asking: None,
        };
        struct Long(ThreadFacts);
        impl Lookup for Long {
            fn thread(&self, _: &str) -> Option<ThreadFacts> {
                Some(self.0.clone())
            }
            fn setup(&self, _: &str) -> Option<SetupFacts> {
                None
            }
        }
        let a = alert(&Note::TurnFinished("x".into()), &Long(long)).unwrap();
        assert!(
            a.title.chars().count() <= TITLE_CHARS + 1 && a.body.chars().count() <= BODY_CHARS + 1
        );
        let job = Job::new(&a, server, vec![]);
        assert!(job.collapse.len() <= 64 && job.payload.len() < 4096);
    }

    /// Dev: the payload the server would send for a note about a real database's thread, for
    /// `xcrun simctl push` (ios/README.md). `FLOTILLA_PUSH_DB`, `FLOTILLA_PUSH_THREAD`,
    /// `FLOTILLA_PUSH_KIND` (`turn_finished`, `agents_answered`, `pr_merged`), `FLOTILLA_PUSH_OUT`.
    #[test]
    #[ignore]
    fn payload_for_simctl() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let store = Store::open(std::path::Path::new(&env("FLOTILLA_PUSH_DB"))).unwrap();
        let thread = env("FLOTILLA_PUSH_THREAD");
        let note = match std::env::var("FLOTILLA_PUSH_KIND").as_deref() {
            Ok("agents_answered") => send(
                &thread,
                "[flotilla] Agents answered (2):\n\n### \"Codex review\" (a) finished:\n\n7/10\n\n### \"Haiku review\" (b) finished:\n\n3/10",
            ),
            Ok("pr_merged") => Note::PrMerged {
                thread: thread.clone(),
                number: 42,
                title: String::new(),
                branch: "main".into(),
                archived: false,
            },
            Ok("asked") => Note::Asked(thread.clone()),
            _ => Note::TurnFinished(thread.clone()),
        };
        let server = store
            .kv_get("server_id")
            .unwrap()
            .expect("the server has run once on it");
        let alert = alert(&note, &store).expect("a note that pushes");
        let mut out = serde_json::from_str::<serde_json::Value>(&payload(&alert, &server)).unwrap();
        // simctl's own key: which app it's for (APNs takes it from the request's topic).
        out["Simulator Target Bundle"] = "com.useflotilla.flotilla".into();
        std::fs::write(
            env("FLOTILLA_PUSH_OUT"),
            serde_json::to_string_pretty(&out).unwrap(),
        )
        .unwrap();
    }

    /// Dev: a note about a real database's thread sent the way its server would through the
    /// relay to the phone that registered with it (the iPhone app, against `relay/script/local`),
    /// and what the fake APNs got written for `xcrun simctl push` (ios/README.md).
    /// `FLOTILLA_PUSH_DB`, `FLOTILLA_PUSH_THREAD`, `FLOTILLA_RELAY_E2E` (the relay),
    /// `FAKE_APNS_OUT` (where the fake writes), `FLOTILLA_PUSH_OUT`.
    #[test]
    #[ignore]
    fn relay_push_for_simctl() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let store = Store::open(std::path::Path::new(&env("FLOTILLA_PUSH_DB"))).unwrap();
        let relay = env("FLOTILLA_RELAY_E2E");
        let out = std::path::PathBuf::from(env("FAKE_APNS_OUT"));
        let server = store
            .kv_get("server_id")
            .unwrap()
            .expect("the server has run once on it");
        let to: Vec<Target> = targets(&store)
            .into_iter()
            .filter(|t| t.relay.as_ref().is_some_and(|r| r.url == relay))
            .collect();
        assert!(!to.is_empty(), "no phone has registered with {relay}");
        let alert = alert(&Note::TurnFinished(env("FLOTILLA_PUSH_THREAD")), &store)
            .expect("a note that pushes");
        let files = || std::fs::read_dir(&out).map(|d| d.count()).unwrap_or(0);
        let before = files();
        let pusher = Pusher::start(Mode::Relay(relay), Registrations::of(&to), |_, _| {}).unwrap();
        pusher.push(Job::new(&alert, &server, to));
        assert!(soon(|| files() > before), "the fake APNs got nothing");
        let mut newest: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        newest.sort();
        let mut sent: serde_json::Value =
            serde_json::from_slice(&std::fs::read(newest.last().unwrap()).unwrap()).unwrap();
        sent["Simulator Target Bundle"] = "com.useflotilla.flotilla".into();
        std::fs::write(
            env("FLOTILLA_PUSH_OUT"),
            serde_json::to_string_pretty(&sent).unwrap(),
        )
        .unwrap();
    }

    /// Dev: what the relay asked APNs for (`relay_push_for_simctl`'s file), sent to the phone's
    /// real token through Apple's sandbox with a real key, as the deployed relay will. The
    /// simulator runs a Notification Service Extension only for a real remote notification,
    /// never for `simctl push`. `FLOTILLA_APNS_KEY_FILE` (never printed), `FLOTILLA_APNS_KEY_ID`,
    /// `FLOTILLA_APNS_TEAM_ID`, `FLOTILLA_PUSH_DB`, `FLOTILLA_PUSH_FILE`.
    #[test]
    #[ignore]
    fn relayed_through_apns_sandbox() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let key = std::fs::read_to_string(env("FLOTILLA_APNS_KEY_FILE")).unwrap();
        let config = Config::new(&key, &env(KEY_ID_ENV), &env(TEAM_ID_ENV)).unwrap();
        let store = Store::open(std::path::Path::new(&env("FLOTILLA_PUSH_DB"))).unwrap();
        let target = targets(&store)
            .into_iter()
            .find(|t| t.relay.is_some() && t.sandbox)
            .expect("a phone registered with the relay, on APNs' sandbox");
        let mut body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(env("FLOTILLA_PUSH_FILE")).unwrap()).unwrap();
        body.as_object_mut()
            .unwrap()
            .remove("Simulator Target Bundle");
        let job = Job {
            kind: "relayed",
            targets: vec![target.clone()],
            payload: body.to_string(),
            collapse: "relay-proof".into(),
        };
        let mut apns = Apns::new(config, Endpoints::apple()).unwrap();
        match apns.post(&job, &target, false) {
            Said::Sent => {}
            Said::Gone => panic!("APNs says the token is gone"),
            Said::Stale => panic!("APNs refused the provider token"),
            Said::Refused(status, why) | Said::Later(status, why, _) => {
                panic!("APNs refused it: {status} {why}")
            }
        }
    }

    #[test]
    fn a_queued_notification_skips_registrations_gone_since() {
        let t = |device: &str, token: &str| Target {
            device: device.into(),
            token: token.into(),
            sandbox: true,
            topic: "com.useflotilla.flotilla".into(),
            relay: None,
        };
        let r = Registrations::of(&[t("a", "aa"), t("b", "bb")]);
        assert!(r.current(&t("a", "aa")) && r.current(&t("b", "bb")));
        // Revoked or unregistered.
        r.set("a", None);
        assert!(!r.current(&t("a", "aa")));
        // A new token: the old one is stale; the token moving to another device leaves this one.
        r.set("b", Some("b2"));
        assert!(!r.current(&t("b", "bb")) && r.current(&t("b", "b2")));
        r.set("c", Some("b2"));
        assert!(!r.current(&t("b", "b2")) && r.current(&t("c", "b2")));
        // Through the relay: the same token registered again (a new handle) is a new one, and a
        // token moving takes its handles with it.
        let relayed = |device: &str, token: &str, handle: &str| Target {
            relay: Some(Relayed {
                url: DEFAULT_RELAY.into(),
                handle: handle.into(),
                secret: "s".repeat(43),
                key: KEY.into(),
            }),
            ..t(device, token)
        };
        let first = relayed("d", "dd", "handle-one");
        r.set("d", Some(&first.id()));
        assert!(r.current(&first) && !r.current(&t("d", "dd")));
        let second = relayed("d", "dd", "handle-two");
        r.set("d", Some(&second.id()));
        assert!(!r.current(&first) && r.current(&second));
        r.set("e", Some("dd"));
        assert!(!r.current(&second) && r.current(&t("e", "dd")));
    }

    #[test]
    fn what_apns_answers_means() {
        assert!(matches!(said(200, "", false), Said::Sent));
        assert!(matches!(said(410, "Unregistered", false), Said::Gone));
        assert!(matches!(said(400, "BadDeviceToken", false), Said::Gone));
        assert!(matches!(
            said(400, "DeviceTokenNotForTopic", false),
            Said::Gone
        ));
        assert!(matches!(
            said(400, "BadTopic", false),
            Said::Refused(400, _)
        ));
        assert!(matches!(
            said(403, "ExpiredProviderToken", false),
            Said::Stale
        ));
        assert!(matches!(
            said(403, "ExpiredProviderToken", true),
            Said::Refused(403, _)
        ));
        assert!(matches!(
            said(429, "TooManyRequests", false),
            Said::Refused(429, _)
        ));
    }

    #[test]
    fn a_token_per_device_and_only_paired_ones() {
        let dir = std::env::temp_dir().join(format!("flotilla-push-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("flotilla.db")).unwrap();
        let now = crate::util::now_ms();
        let a = store
            .pair_device(
                &store
                    .mint_code(Duration::from_secs(60), "local", now)
                    .unwrap()
                    .code,
                "iPhone",
                "ios",
                None,
                now,
            )
            .unwrap()
            .device;
        let b = store
            .pair_device(
                &store
                    .mint_code(Duration::from_secs(60), "local", now)
                    .unwrap()
                    .code,
                "iPad",
                "ios",
                None,
                now,
            )
            .unwrap()
            .device;
        let token = |t: &str| PushToken {
            token: flotilla_proto::Secret(t.into()),
            env: "sandbox".into(),
            topic: "com.useflotilla.flotilla".into(),
            relay: None,
        };
        register(&store, &a, &token(&"ab".repeat(32)), now).unwrap();
        register(
            &store,
            &b,
            &PushToken {
                env: "production".into(),
                ..token(&"cd".repeat(32))
            },
            now,
        )
        .unwrap();
        let mut t = targets(&store);
        t.sort_by(|x, y| x.device.cmp(&y.device));
        assert_eq!(t.len(), 2);
        assert!(
            t.iter()
                .any(|x| x.device == a && x.sandbox && x.token == "ab".repeat(32))
        );
        assert!(t.iter().any(|x| x.device == b && !x.sandbox));
        // Registering again replaces; the same token from another device moves to it.
        register(&store, &a, &token(&"ef".repeat(32)), now).unwrap();
        register(&store, &b, &token(&"EF".repeat(32)), now).unwrap();
        let t = targets(&store);
        assert_eq!(t.len(), 1);
        assert_eq!(
            (t[0].device.as_str(), t[0].token.as_str()),
            (b.as_str(), "ef".repeat(32).as_str())
        );
        // A token APNs says is gone goes, unless the device has a new one meanwhile.
        forget(&store, &b, &"00".repeat(32));
        assert_eq!(targets(&store).len(), 1);
        forget(&store, &b, &"ef".repeat(32));
        assert!(targets(&store).is_empty());
        // Only a revoked device's registration is left out.
        register(&store, &a, &token(&"ab".repeat(32)), now).unwrap();
        assert!(store.revoke_device(&a).unwrap());
        assert!(targets(&store).is_empty());
        // What isn't a token, an environment or Flotilla's app is refused.
        assert!(register(&store, &b, &token("not hex"), now).is_err());
        assert!(register(&store, &b, &token("abc"), now).is_err());
        assert!(
            register(
                &store,
                &b,
                &PushToken {
                    env: "staging".into(),
                    ..token(&"ab".repeat(32))
                },
                now
            )
            .is_err()
        );
        assert!(
            register(
                &store,
                &b,
                &PushToken {
                    topic: "com.example.other".into(),
                    ..token(&"ab".repeat(32))
                },
                now
            )
            .is_err()
        );
        assert!(!format!("{:?}", targets(&store)).contains("abab"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a sealed note says without its time, checking the time is about now.
    fn unstamped(note: &str) -> serde_json::Value {
        let mut v: serde_json::Value = serde_json::from_str(note).unwrap();
        let at = v["flotilla"]
            .as_object_mut()
            .unwrap()
            .remove("sent_at")
            .unwrap()
            .as_i64()
            .unwrap();
        assert!((crate::util::now_ms() / 1000 - at).abs() < 60, "sealed now");
        v
    }

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    /// The test vectors' key (`relay/test/vectors.json`).
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../../../relay/test/vectors.json")).unwrap()
    }

    #[test]
    fn the_encryption_is_the_vectors_node_made() {
        let v = vectors();
        let key = key_of(v["key"].as_str().unwrap()).unwrap();
        let other = key_of(v["other_key"].as_str().unwrap()).unwrap();
        for seal in v["seals"].as_array().unwrap() {
            let plaintext = seal["plaintext"].as_str().unwrap();
            let sealed = seal["ciphertext"].as_str().unwrap();
            let nonce: [u8; 12] = der(seal["nonce"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            // The same bytes as Node's ChaCha20-Poly1305 (and CryptoKit's: FlotillaKit's tests).
            assert_eq!(seal_with(&key, nonce, plaintext).unwrap(), sealed);
            assert_eq!(open(&key, sealed).as_deref(), Some(plaintext));
            // Another key, or a byte changed anywhere, opens nothing.
            assert_eq!(open(&other, sealed), None);
            let mut bytes = base64::engine::general_purpose::STANDARD
                .decode(sealed)
                .unwrap();
            for i in [0, 12, bytes.len() - 1] {
                bytes[i] ^= 1;
                let changed = base64::engine::general_purpose::STANDARD.encode(&bytes);
                assert_eq!(open(&key, &changed), None);
                bytes[i] ^= 1;
            }
        }
        for c in v["collapses"].as_array().unwrap() {
            assert_eq!(
                collapse_id(&key, c["id"].as_str().unwrap()),
                c["collapse"].as_str().unwrap()
            );
        }
        // A fresh nonce each time.
        let a = seal(&key, "hello").unwrap();
        let b = seal(&key, "hello").unwrap();
        assert_ne!(a, b);
        assert_eq!(open(&key, &a).as_deref(), Some("hello"));
        assert_eq!(open(&key, "AAAA"), None);
        assert_eq!(open(&key, "not base64"), None);
    }

    #[test]
    fn the_longest_notification_fits_the_relay() {
        // Every character the most JSON can make of it: a control character, six bytes.
        let worst = ThreadFacts {
            title: "\u{1}".repeat(500),
            reply: Some("\u{1}".repeat(500)),
            delegated: false,
            asking: None,
        };
        struct Worst(ThreadFacts);
        impl Lookup for Worst {
            fn thread(&self, _: &str) -> Option<ThreadFacts> {
                Some(self.0.clone())
            }
            fn setup(&self, _: &str) -> Option<SetupFacts> {
                None
            }
        }
        let a = alert(&Note::TurnFinished("t".repeat(40)), &Worst(worst)).unwrap();
        let job = Job::new(&a, "srv_3f5c1a7e92d4", vec![]);
        let sealed = seal(&key_of(KEY).unwrap(), &stamped(&job.payload, i64::MAX)).unwrap();
        assert!(
            sealed.len() <= RELAY_MAX_CIPHERTEXT,
            "{} > {RELAY_MAX_CIPHERTEXT}",
            sealed.len()
        );
    }

    #[test]
    fn a_relay_written_two_ways_is_one() {
        for same in [
            "https://push.useflotilla.com",
            "HTTPS://PUSH.useflotilla.com/",
            "https://push.useflotilla.com:443",
            "https:push.useflotilla.com",
            "https://push.useflotilla.com\t",
        ] {
            assert_eq!(relay_url(same).unwrap(), DEFAULT_RELAY, "{same:?}");
        }
        assert_eq!(
            relay_url("http://LOCALHOST:8796/").unwrap(),
            "http://localhost:8796"
        );
        assert_eq!(
            relay_url("http://127.1:8796").unwrap(),
            "http://127.0.0.1:8796"
        );
        assert_eq!(
            relay_url("https://Example.com/Relay/").unwrap(),
            "https://example.com/Relay"
        );
    }

    #[test]
    fn a_relay_address_too_long_is_refused_and_changes_nothing() {
        let long = format!("https://example.com/{}", "a".repeat(RELAY_URL_MAX));
        assert!(relay_url(&long).unwrap_err().contains("at most 256"));
        let (dir, store) = scratch_store();
        let phone = paired_phone(&store);
        let with = |url: &str| PushToken {
            token: flotilla_proto::Secret("ab".repeat(32)),
            env: "sandbox".into(),
            topic: "com.useflotilla.flotilla".into(),
            relay: Some(PushRelay {
                url: url.into(),
                handle: "giLjuhbbFalUBROEtNqwBg".into(),
                secret: flotilla_proto::Secret("S".repeat(43)),
                key: flotilla_proto::Secret(KEY.into()),
            }),
        };
        let id = register(&store, &phone, &with(DEFAULT_RELAY), 1).unwrap();
        assert!(register(&store, &phone, &with(&long), 2).is_err());
        assert_eq!(registration_of(&store, &phone), Some(id));
        assert_eq!(
            targets(&store)[0].relay.as_ref().unwrap().url,
            DEFAULT_RELAY
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relay_addresses() {
        assert_eq!(
            relay_url(" https://push.example.com/ ").unwrap(),
            "https://push.example.com"
        );
        assert_eq!(
            relay_url("https://example.com/relay").unwrap(),
            "https://example.com/relay"
        );
        // Plain HTTP only to this machine, for trying a relay out.
        for ok in [
            "http://127.0.0.1:8787",
            "http://localhost:8787",
            "http://[::1]:8787",
        ] {
            assert_eq!(relay_url(ok).unwrap(), ok);
        }
        for bad in [
            "http://push.example.com",
            "http://192.168.1.5:8787",
            "ftp://push.example.com",
            "push.example.com",
            "https://push.example.com/?x=1",
            "https://push.example.com/#x",
            "https://user:pw@push.example.com",
            "",
        ] {
            assert!(relay_url(bad).is_err(), "{bad}");
        }
    }

    fn scratch_store() -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("flotilla-push-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("flotilla.db")).unwrap();
        (dir, store)
    }

    fn paired_phone(store: &Store) -> String {
        let now = crate::util::now_ms();
        store
            .pair_device(
                &store
                    .mint_code(Duration::from_secs(60), "local", now)
                    .unwrap()
                    .code,
                "iPhone",
                "ios",
                None,
                now,
            )
            .unwrap()
            .device
    }

    #[test]
    fn its_own_key_else_the_relay_else_off() {
        let (dir, store) = scratch_store();
        let mode = |key: Option<Config>| match mode_given(key, Some(&store)) {
            Ok(None) => "off".to_string(),
            Ok(Some(Mode::Relay(url))) => url,
            Ok(Some(Mode::Direct(c))) => format!("key {}", c.key_id),
            Err(e) => format!("broken: {e}"),
        };
        // Flotilla's relay unless it's said otherwise; a key goes first.
        assert_eq!(mode(None), DEFAULT_RELAY);
        assert_eq!(mode(Some(config())), "key ABC123DEFG");
        store.kv_set(RELAY_KEY, "https://push.example.com").unwrap();
        assert_eq!(mode(None), "https://push.example.com");
        store.kv_set(RELAY_KEY, "off").unwrap();
        assert_eq!(mode(None), "off");
        assert_eq!(mode(Some(config())), "key ABC123DEFG");
        store.kv_set(RELAY_KEY, "http://push.example.com").unwrap();
        assert!(mode(None).starts_with("broken: "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_phone_registers_what_the_relay_gave_it() {
        let (dir, store) = scratch_store();
        let phone = paired_phone(&store);
        let now = crate::util::now_ms();
        let with = |relay: Option<PushRelay>| PushToken {
            token: flotilla_proto::Secret("ab".repeat(32)),
            env: "production".into(),
            topic: "com.useflotilla.flotilla".into(),
            relay,
        };
        let relay = |handle: &str| PushRelay {
            url: "https://push.useflotilla.com/".into(),
            handle: handle.into(),
            secret: flotilla_proto::Secret("S".repeat(43)),
            key: flotilla_proto::Secret(KEY.into()),
        };
        let id = register(
            &store,
            &phone,
            &with(Some(relay("giLjuhbbFalUBROEtNqwBg"))),
            now,
        )
        .unwrap();
        assert_eq!(id, format!("{}@giLjuhbbFalUBROEtNqwBg", "ab".repeat(32)));
        assert_eq!(
            registration_of(&store, &phone).as_deref(),
            Some(id.as_str())
        );
        let t = targets(&store);
        assert_eq!(t.len(), 1);
        let r = t[0].relay.as_ref().unwrap();
        assert_eq!(r.url, DEFAULT_RELAY);
        assert_eq!(t[0].id(), id);
        // Never in a log.
        let shown = format!("{t:?}");
        assert!(!shown.contains("SSSS") && !shown.contains(KEY) && !shown.contains("abab"));
        // Registered again with the relay: a new registration, and what's gone for the old one
        // leaves it.
        let again = register(&store, &phone, &with(Some(relay("handle-number-two"))), now).unwrap();
        forget(&store, &phone, &id);
        assert_eq!(
            registration_of(&store, &phone).as_deref(),
            Some(again.as_str())
        );
        // What isn't a relay's registration is refused, and changes nothing.
        let bad = [
            PushRelay {
                key: flotilla_proto::Secret("AAAA".into()),
                ..relay("handle-number-two")
            },
            PushRelay {
                url: "http://push.example.com".into(),
                ..relay("handle-number-two")
            },
            relay("no spaces allowed"),
            PushRelay {
                secret: flotilla_proto::Secret("short".into()),
                ..relay("handle-number-two")
            },
        ];
        for r in bad {
            assert!(register(&store, &phone, &with(Some(r)), now).is_err());
        }
        assert_eq!(
            registration_of(&store, &phone).as_deref(),
            Some(again.as_str())
        );
        // An older app's, without: its token only.
        assert_eq!(
            register(&store, &phone, &with(None), now).unwrap(),
            "ab".repeat(32)
        );
        assert!(targets(&store)[0].relay.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the stand-in APNs was sent: (path, body).
    type Sent = Arc<Mutex<Vec<(String, String)>>>;

    /// A stand-in APNs over HTTP/2 (without TLS) on a local port, answering 200: what it was
    /// sent, as (path, body).
    fn fake_apns() -> (String, Sent) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Sent = Arc::default();
        let held = seen.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let held = held.clone();
                    tokio::spawn(async move {
                        let Ok(mut conn) = h2::server::handshake(socket).await else {
                            return;
                        };
                        // Each request on its own task: the connection reads its body only
                        // while it's polled here.
                        while let Some(Ok((request, mut respond))) = conn.accept().await {
                            let held = held.clone();
                            tokio::spawn(async move {
                                let path = request.uri().path().to_string();
                                let mut body = request.into_body();
                                let mut bytes = Vec::new();
                                while let Some(Ok(chunk)) = body.data().await {
                                    let _ = body.flow_control().release_capacity(chunk.len());
                                    bytes.extend_from_slice(&chunk);
                                }
                                held.lock()
                                    .unwrap()
                                    .push((path, String::from_utf8_lossy(&bytes).to_string()));
                                let response = http::Response::builder()
                                    .status(200)
                                    .header("apns-id", "0E9F7B2C-0000-0000-0000-000000000000")
                                    .body(())
                                    .unwrap();
                                let _ = respond.send_response(response, true);
                            });
                        }
                    });
                }
            });
        });
        (url, seen)
    }

    /// Until `f` holds, or five seconds pass.
    fn soon(mut f: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    #[test]
    fn through_the_relay_or_to_apple_and_never_both() {
        let (dir, store) = scratch_store();
        let now = crate::util::now_ms();
        let (relay_at, relayed) = testing::fake_relay(vec![204, 410]);
        let (apns_at, apns) = fake_apns();
        let endpoints = Endpoints {
            sandbox: format!("{apns_at}/sandbox"),
            production: format!("{apns_at}/production"),
        };
        // Two phones: one registered with the relay, one (an older app) without.
        let with_relay = paired_phone(&store);
        let without = paired_phone(&store);
        register(
            &store,
            &with_relay,
            &PushToken {
                token: flotilla_proto::Secret("ab".repeat(32)),
                env: "sandbox".into(),
                topic: "com.useflotilla.flotilla".into(),
                relay: Some(PushRelay {
                    url: relay_at.clone(),
                    handle: "giLjuhbbFalUBROEtNqwBg".into(),
                    secret: flotilla_proto::Secret("S".repeat(43)),
                    key: flotilla_proto::Secret(KEY.into()),
                }),
            },
            now,
        )
        .unwrap();
        register(
            &store,
            &without,
            &PushToken {
                token: flotilla_proto::Secret("cd".repeat(32)),
                env: "production".into(),
                topic: "com.useflotilla.flotilla".into(),
                relay: None,
            },
            now,
        )
        .unwrap();
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        let gone: Arc<Mutex<Vec<(String, String)>>> = Arc::default();

        // Through the relay: only the phone that registered with it, sealed with its key.
        let g = gone.clone();
        let pusher = Pusher::start_to(
            Mode::Relay(relay_at.clone()),
            Options {
                apns: endpoints.clone(),
                ..Options::default()
            },
            Registrations::of(&targets(&store)),
            move |d, id| g.lock().unwrap().push((d, id)),
        )
        .unwrap();
        let reached: Vec<Target> = targets(&store)
            .into_iter()
            .filter(|t| pusher.reaches(t))
            .collect();
        assert_eq!(reached.len(), 1);
        assert_eq!(reached[0].device, with_relay);
        let job = Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store));
        pusher.push(job.clone());
        assert!(soon(|| relayed.lock().unwrap().len() == 1));
        let sent = relayed.lock().unwrap()[0].clone();
        assert_eq!(sent["_line"], "POST /v1/push HTTP/1.1");
        assert_eq!(sent["handle"], "giLjuhbbFalUBROEtNqwBg");
        assert_eq!(sent["secret"], "S".repeat(43));
        let key = key_of(KEY).unwrap();
        assert_eq!(
            unstamped(&open(&key, sent["ciphertext"].as_str().unwrap()).unwrap()),
            json(&job.payload)
        );
        assert_eq!(sent["collapse"], collapse_id(&key, &job.collapse));
        let fields: std::collections::BTreeSet<_> =
            sent.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            fields,
            ["_line", "ciphertext", "collapse", "handle", "secret"]
                .map(String::from)
                .into()
        );
        // The relay says the phone's gone: forgotten, by its registration.
        pusher.push(job.clone());
        assert!(soon(|| gone.lock().unwrap().len() == 1));
        assert_eq!(
            gone.lock().unwrap()[0],
            (
                with_relay.clone(),
                registration_of(&store, &with_relay).unwrap()
            )
        );
        drop(pusher);

        // Its own key: every phone, straight to Apple, the payload as it is.
        let pusher = Pusher::start_to(
            mode_given(Some(config()), None).unwrap().unwrap(),
            Options {
                apns: endpoints,
                ..Options::default()
            },
            Registrations::of(&targets(&store)),
            |_, _| {},
        )
        .unwrap();
        assert!(targets(&store).iter().all(|t| pusher.reaches(t)));
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| apns.lock().unwrap().len() == 2));
        let mut to_apple = apns.lock().unwrap().clone();
        to_apple.sort();
        assert_eq!(
            to_apple,
            [
                (
                    format!("/production/3/device/{}", "cd".repeat(32)),
                    job.payload.clone()
                ),
                (
                    format!("/sandbox/3/device/{}", "ab".repeat(32)),
                    job.payload.clone()
                ),
            ]
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            relayed.lock().unwrap().len(),
            2,
            "nothing more to the relay"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The server's side against the real relay on this machine and its fake APNs
    /// (`relay/script/e2e` runs it): `FLOTILLA_RELAY_E2E` is the relay, `FLOTILLA_FAKE_APNS`
    /// the fake.
    #[test]
    #[ignore]
    fn relay_end_to_end() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let (relay, apns) = (env("FLOTILLA_RELAY_E2E"), env("FLOTILLA_FAKE_APNS"));
        let http = reqwest::blocking::Client::new();
        let fresh = || {
            let mut b = [0u8; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).unwrap();
            b
        };
        let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let log = || -> Vec<serde_json::Value> {
            http.get(format!("{apns}/_log"))
                .send()
                .unwrap()
                .json()
                .unwrap()
        };
        let (dir, store) = scratch_store();
        // A phone registers with the relay, then gives its server what it got and its own key.
        let phone_with = |token: &str| -> (String, [u8; 32], serde_json::Value) {
            let got: serde_json::Value = http
                .post(format!("{relay}/v1/register"))
                .json(&serde_json::json!({"token": token, "topic": "com.useflotilla.flotilla", "env": "sandbox"}))
                .send()
                .unwrap()
                .json()
                .unwrap();
            let key = fresh();
            let device = paired_phone(&store);
            register(
                &store,
                &device,
                &PushToken {
                    token: flotilla_proto::Secret(token.into()),
                    env: "sandbox".into(),
                    topic: "com.useflotilla.flotilla".into(),
                    relay: Some(PushRelay {
                        url: relay.clone(),
                        handle: got["handle"].as_str().unwrap().into(),
                        secret: flotilla_proto::Secret(got["secret"].as_str().unwrap().into()),
                        key: flotilla_proto::Secret(b64(&key)),
                    }),
                },
                crate::util::now_ms(),
            )
            .unwrap();
            (device, key, got)
        };
        let token = hex(&fresh());
        let (phone, key, got) = phone_with(&token);
        log();
        let gone: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let g = gone.clone();
        let pusher = Pusher::start(
            Mode::Relay(relay.clone()),
            Registrations::of(&targets(&store)),
            move |d, id| g.lock().unwrap().push((d, id)),
        )
        .unwrap();
        let note = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        let send = || {
            let to = targets(&store);
            pusher.registrations.set(&to[0].device, Some(&to[0].id()));
            pusher.push(Job::new(&note, "srv_3f5c1a7e92d4", to));
        };
        send();
        let mut seen = Vec::new();
        assert!(soon(|| {
            seen.extend(log());
            !seen.is_empty()
        }));
        // What APNs was asked: the neutral alert for the phone's token, with only the
        // ciphertext, which the phone's key opens to what the server would have sent Apple.
        let to_apple = &seen[0];
        assert_eq!(to_apple["token"], token.as_str());
        assert_eq!(to_apple["env"], "sandbox");
        assert_eq!(to_apple["jwtValid"], true);
        let job = Job::new(&note, "srv_3f5c1a7e92d4", vec![]);
        assert_eq!(to_apple["collapse"], collapse_id(&key, &job.collapse));
        let body: serde_json::Value =
            serde_json::from_str(to_apple["body"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["aps"],
            serde_json::json!({"alert": {"title": "Flotilla", "body": "New activity"}, "mutable-content": 1, "sound": "default"})
        );
        assert_eq!(
            unstamped(&open(&key, body["e"].as_str().unwrap()).unwrap()),
            json(&job.payload)
        );
        assert!(!to_apple["body"].as_str().unwrap().contains("Fix flaky"));
        // The phone leaves the relay ("Forget"): the server hears the handle is gone, and
        // forgets the phone's registration.
        let left = http
            .post(format!("{relay}/v1/unregister"))
            .json(&serde_json::json!({"handle": got["handle"], "secret": got["secret"]}))
            .send()
            .unwrap();
        assert_eq!(left.status().as_u16(), 204);
        send();
        assert!(soon(|| gone.lock().unwrap().len() == 1));
        forget(&store, &phone, &gone.lock().unwrap()[0].1);
        assert!(registration_of(&store, &phone).is_none());
        // APNs says a phone's gone: the relay drops its handle, and the server forgets it.
        let (dead, _, _) = phone_with(&format!("dead{}", &hex(&fresh())[4..]));
        send();
        assert!(soon(|| gone.lock().unwrap().len() == 2));
        assert_eq!(gone.lock().unwrap()[1].0, dead);
        assert!(log().len() == 1, "one try at APNs for the gone phone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_redirect_from_the_relay_is_refused_not_followed() {
        let (dir, store) = scratch_store();
        // Where a redirect would send the handle and secret.
        let (elsewhere, followed) = testing::fake_relay(Vec::<u16>::new());
        let (relay, sent) = testing::fake_relay(vec![testing::Answer {
            status: 307,
            headers: format!("location: {elsewhere}/private\r\n"),
            body: String::new(),
        }]);
        let phone = paired_phone(&store);
        register(
            &store,
            &phone,
            &PushToken {
                token: flotilla_proto::Secret("ab".repeat(32)),
                env: "sandbox".into(),
                topic: "com.useflotilla.flotilla".into(),
                relay: Some(PushRelay {
                    url: relay.clone(),
                    handle: "giLjuhbbFalUBROEtNqwBg".into(),
                    secret: flotilla_proto::Secret("S".repeat(43)),
                    key: flotilla_proto::Secret(KEY.into()),
                }),
            },
            crate::util::now_ms(),
        )
        .unwrap();
        let gone: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let g = gone.clone();
        let pusher = Pusher::start(
            Mode::Relay(relay),
            Registrations::of(&targets(&store)),
            move |d, id| g.lock().unwrap().push((d, id)),
        )
        .unwrap();
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 1));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            followed.lock().unwrap().is_empty(),
            "nothing went elsewhere"
        );
        assert!(
            gone.lock().unwrap().is_empty(),
            "and the phone wasn't forgotten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rebuild_keeps_how_notifications_go_and_who_gets_them() {
        for kept in ["off", "https://push.example.com", "http://127.0.0.1:8796"] {
            let (dir, store) = scratch_store();
            let db = dir.join("flotilla.db");
            store.kv_set(RELAY_KEY, kept).unwrap();
            store.kv_set(KEY_ID_KEY, "ABC123DEFG").unwrap();
            store.kv_set(TEAM_ID_KEY, "DEF123GHIJ").unwrap();
            let phone = paired_phone(&store);
            let id = register(
                &store,
                &phone,
                &PushToken {
                    token: flotilla_proto::Secret("ab".repeat(32)),
                    env: "sandbox".into(),
                    topic: "com.useflotilla.flotilla".into(),
                    relay: None,
                },
                crate::util::now_ms(),
            )
            .unwrap();
            let before = relay(Some(&store)).unwrap();
            drop(store);
            Store::backup(&db).unwrap();
            let store = Store::open(&db).unwrap();
            assert_eq!(relay(Some(&store)).unwrap(), before, "{kept}");
            assert_eq!(
                store.kv_get(KEY_ID_KEY).unwrap().as_deref(),
                Some("ABC123DEFG")
            );
            assert_eq!(
                store.kv_get(TEAM_ID_KEY).unwrap().as_deref(),
                Some("DEF123GHIJ")
            );
            assert_eq!(registration_of(&store, &phone), Some(id));
            assert_eq!(targets(&store).len(), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn only_the_relays_own_unknown_handle_answer_forgets_a_phone() {
        let unknown = || testing::Answer {
            status: 404,
            headers: String::new(),
            body: r#"{"error":"No such handle","code":"no_such_handle"}"#.into(),
        };
        let other = [
            testing::Answer::from(404),
            testing::Answer {
                status: 404,
                headers: String::new(),
                body: "<html>Not found</html>".into(),
            },
            testing::Answer {
                status: 404,
                headers: String::new(),
                body: r#"{"error":"Not found"}"#.into(),
            },
        ];
        let mut answers: Vec<testing::Answer> = other.into_iter().collect();
        answers.push(unknown());
        let (dir, store) = scratch_store();
        let (relay, sent) = testing::fake_relay(answers);
        let phone = paired_phone(&store);
        register(
            &store,
            &phone,
            &PushToken {
                token: flotilla_proto::Secret("ab".repeat(32)),
                env: "sandbox".into(),
                topic: "com.useflotilla.flotilla".into(),
                relay: Some(PushRelay {
                    url: relay.clone(),
                    handle: "giLjuhbbFalUBROEtNqwBg".into(),
                    secret: flotilla_proto::Secret("S".repeat(43)),
                    key: flotilla_proto::Secret(KEY.into()),
                }),
            },
            crate::util::now_ms(),
        )
        .unwrap();
        let gone: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let g = gone.clone();
        let pusher = Pusher::start(
            Mode::Relay(relay),
            Registrations::of(&targets(&store)),
            move |d, id| g.lock().unwrap().push((d, id)),
        )
        .unwrap();
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        // A 404 with no body, with a page, and with another error: the phone stays.
        for n in 1..=3 {
            pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
            assert!(soon(|| sent.lock().unwrap().len() >= n));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(gone.lock().unwrap().is_empty());
        // The relay's own answer: forgotten.
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| gone.lock().unwrap().len() == 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A phone registered with a stand-in relay that answers `answers`, and a sender to it whose
    /// retries wait `delays`.
    fn relaying(
        answers: Vec<testing::Answer>,
        delays: Vec<Duration>,
    ) -> (
        std::path::PathBuf,
        Store,
        String,
        Pusher,
        Arc<Mutex<Vec<serde_json::Value>>>,
    ) {
        let (dir, store) = scratch_store();
        let (relay, sent) = testing::fake_relay(answers);
        let phone = paired_phone(&store);
        register(
            &store,
            &phone,
            &PushToken {
                token: flotilla_proto::Secret("ab".repeat(32)),
                env: "sandbox".into(),
                topic: "com.useflotilla.flotilla".into(),
                relay: Some(PushRelay {
                    url: relay.clone(),
                    handle: "giLjuhbbFalUBROEtNqwBg".into(),
                    secret: flotilla_proto::Secret("S".repeat(43)),
                    key: flotilla_proto::Secret(KEY.into()),
                }),
            },
            crate::util::now_ms(),
        )
        .unwrap();
        let pusher = Pusher::start_to(
            Mode::Relay(relay),
            Options {
                retries: delays,
                ..Options::default()
            },
            Registrations::of(&targets(&store)),
            |_, _| {},
        )
        .unwrap();
        (dir, store, phone, pusher, sent)
    }

    fn after(status: u16, secs: u64) -> testing::Answer {
        testing::Answer {
            status,
            headers: format!("retry-after: {secs}\r\n"),
            body: r#"{"error":"Too many requests: try again in a minute"}"#.into(),
        }
    }

    #[test]
    fn a_notification_the_relay_didnt_take_for_now_goes_again() {
        let key = key_of(KEY).unwrap();
        for first in [after(429, 0), testing::Answer::from(503)] {
            let (dir, store, _, pusher, sent) =
                relaying(vec![first, 204.into()], vec![Duration::from_millis(50); 3]);
            let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
            let job = Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store));
            pusher.push(job.clone());
            assert!(soon(|| sent.lock().unwrap().len() == 2));
            std::thread::sleep(Duration::from_millis(200));
            let sent = sent.lock().unwrap().clone();
            assert_eq!(sent.len(), 2, "once again, then done");
            assert_eq!(
                unstamped(&open(&key, sent[1]["ciphertext"].as_str().unwrap()).unwrap()),
                json(&job.payload)
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_retry_waits_as_long_as_the_relay_says() {
        let (dir, store, _, pusher, sent) =
            relaying(vec![after(429, 1)], vec![Duration::from_millis(10)]);
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 1));
        let first = Instant::now();
        assert!(soon(|| sent.lock().unwrap().len() == 2));
        assert!(first.elapsed() >= Duration::from_millis(900));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retries_end_and_skip_a_phone_gone_since() {
        // Given up after the last delay: one try and two more.
        let (dir, store, _, pusher, sent) = relaying(
            vec![503.into(), 503.into(), 503.into(), 503.into()],
            vec![Duration::from_millis(20); 2],
        );
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 3));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sent.lock().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
        // Revoked while it waited: not sent again.
        let (dir, store, phone, pusher, sent) =
            relaying(vec![503.into()], vec![Duration::from_millis(300)]);
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 1));
        pusher.registrations.set(&phone, None);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(sent.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_note_takes_the_place_of_one_waiting() {
        let (dir, store, _, pusher, sent) = relaying(
            vec![503.into(), 204.into(), 204.into()],
            vec![Duration::from_millis(400)],
        );
        let alert = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 1));
        // The same thread's next turn, before the first went again: only it goes.
        pusher.push(Job::new(&alert, "srv_3f5c1a7e92d4", targets(&store)));
        assert!(soon(|| sent.lock().unwrap().len() == 2));
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(sent.lock().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sealed_note_says_when_it_was_sealed() {
        let a = alert(&Note::TurnFinished("t1".into()), &Facts).unwrap();
        let payload = Job::new(&a, "srv_3f5c1a7e92d4", vec![]).payload;
        let note: serde_json::Value = serde_json::from_str(&stamped(&payload, 1793664000)).unwrap();
        assert_eq!(note["flotilla"]["sent_at"], 1793664000);
        assert_eq!(note["flotilla"]["thread"], "t1");
        assert_eq!(note["aps"], json(&payload)["aps"]);
    }
}
