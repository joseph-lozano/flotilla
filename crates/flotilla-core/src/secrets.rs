//! Secrets where the engine runs: the boat API key, and the Claude token the summarizer uses
//! (`claude setup-token`). (A client's device token for each server it's paired with is
//! `flotilla_client::tokens`'.) Clients never send the server a secret: `flotilla setup` keeps
//! them on the server.
//!
//! **Where the engine's secrets come from**, the first found winning ([`SecretStore::get`]):
//! 1. the environment: `BOAT_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`;
//! 2. a file named on the command line (`--boat-key-file`, `--claude-token-file`), which only its
//!    owner may read;
//! 3. systemd's credentials (`LoadCredential=boat-key:…`, `claude-token`), in
//!    `$CREDENTIALS_DIRECTORY`;
//! 4. where `flotilla setup` keeps them: the login Keychain on macOS (as the app always has),
//!    or files only their owner can read in `<data dir>/secrets` (Linux, or `--secrets-dir`).
//!
//! Nothing here logs a secret, and a [`SecretStore`] holds none (only where to look).

use crate::error::{Error, Result};
#[cfg(target_os = "macos")]
use flotilla_model::secrets::keychain;
use flotilla_model::secrets::{safe, write_private};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The Keychain account the boat key is kept under (service: [`crate::flavor::Flavor::keychain_service`]).
pub const KEYCHAIN_ACCOUNT: &str = "boat-api-key";

/// A secret the engine needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Name {
    BoatKey,
    ClaudeToken,
    /// The APNs key (`crate::push`): a `.p8` file's contents, or as setup keeps it, in hex.
    ApnsKey,
}

impl Name {
    /// The environment variable that holds it.
    pub fn env(self) -> &'static str {
        match self {
            Name::BoatKey => "BOAT_API_KEY",
            Name::ClaudeToken => "CLAUDE_CODE_OAUTH_TOKEN",
            Name::ApnsKey => "FLOTILLA_APNS_KEY",
        }
    }

    /// Its file's name: in the secrets folder, and as a systemd credential.
    pub fn file(self) -> &'static str {
        match self {
            Name::BoatKey => "boat-key",
            Name::ClaudeToken => "claude-token",
            Name::ApnsKey => "apns-key",
        }
    }

    /// The command-line flag that names a file holding it.
    pub fn flag(self) -> &'static str {
        match self {
            Name::BoatKey => "--boat-key-file",
            Name::ClaudeToken => "--claude-token-file",
            Name::ApnsKey => "--apns-key-file",
        }
    }

    /// Its Keychain account (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn account(self) -> &'static str {
        match self {
            Name::BoatKey => KEYCHAIN_ACCOUNT,
            Name::ClaudeToken => "claude-token",
            Name::ApnsKey => "apns-key",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Name::BoatKey => "a boat API key",
            Name::ClaudeToken => "a Claude token",
            Name::ApnsKey => "an APNs key",
        }
    }
}

/// Where a secret was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Env(&'static str),
    /// A file named on the command line.
    File(PathBuf),
    /// A systemd credential.
    Credential(PathBuf),
    /// The login Keychain, under this service.
    Keychain(&'static str),
    /// The secrets folder `flotilla setup` writes.
    Dir(PathBuf),
    /// Typed or pasted just now (`flotilla setup`).
    Pasted,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Env(var) => write!(f, "{var}"),
            Source::File(p) | Source::Dir(p) => write!(f, "{}", p.display()),
            Source::Credential(p) => write!(
                f,
                "the systemd credential {}",
                p.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ),
            Source::Keychain(_) => f.write_str("the Keychain"),
            Source::Pasted => f.write_str("what you pasted"),
        }
    }
}

/// Where `flotilla setup` keeps secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Backing {
    /// The login Keychain (macOS only).
    Keychain,
    /// Files only their owner can read, in a folder only it can enter.
    Dir(PathBuf),
    /// Nowhere: only the environment, files and credentials.
    #[default]
    None,
}

/// Where to look for the engine's secrets, in order, and where to keep them.
#[derive(Clone, Default)]
pub struct SecretStore {
    /// Read in place of the process's environment (tests).
    env: Option<HashMap<String, String>>,
    /// `--boat-key-file`, `--claude-token-file`.
    pub files: HashMap<Name, PathBuf>,
    /// systemd's `$CREDENTIALS_DIRECTORY`.
    pub credentials: Option<PathBuf>,
    pub backing: Backing,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("files", &self.files)
            .field("credentials", &self.credentials)
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

impl SecretStore {
    /// This machine's: the real environment and systemd's credentials, kept in the Keychain on
    /// macOS, or in `secrets_dir` (default: `secrets` beside the default database) elsewhere.
    pub fn system(secrets_dir: Option<PathBuf>) -> SecretStore {
        let backing = match secrets_dir {
            Some(dir) => Backing::Dir(dir),
            None if cfg!(target_os = "macos") => Backing::Keychain,
            None => Backing::Dir(default_dir()),
        };
        let credentials = std::env::var_os("CREDENTIALS_DIRECTORY")
            .map(PathBuf::from)
            .filter(|d| !d.as_os_str().is_empty());
        SecretStore {
            env: None,
            files: HashMap::new(),
            credentials,
            backing,
        }
    }

    /// Tests: this environment instead of the process's.
    pub fn with_env(mut self, env: &[(&str, &str)]) -> SecretStore {
        self.env = Some(
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        self
    }

    fn var(&self, name: &str) -> Option<String> {
        let v = match &self.env {
            Some(env) => env.get(name).cloned(),
            None => std::env::var(name).ok(),
        };
        v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }

    /// The secret and where it came from, if any source has it. A file named on the command
    /// line that's missing, empty or readable by others is an error, not a miss.
    pub fn get(&self, name: Name) -> Result<Option<(String, Source)>> {
        if let Some(v) = self.var(name.env()) {
            return Ok(Some((v, Source::Env(name.env()))));
        }
        if let Some(path) = self.files.get(&name) {
            return read_private(path, name).map(|v| Some((v, Source::File(path.clone()))));
        }
        if let Some(dir) = &self.credentials {
            let path = dir.join(name.file());
            if path.exists() {
                let v = std::fs::read_to_string(&path).map_err(|e| {
                    Error::Invalid(format!("Couldn't read {}: {e}", path.display()))
                })?;
                let v = v.trim().to_string();
                if !v.is_empty() {
                    return Ok(Some((v, Source::Credential(path))));
                }
            }
        }
        match &self.backing {
            Backing::Keychain => keychain_get(name),
            Backing::Dir(dir) => {
                let path = dir.join(name.file());
                if !path.exists() {
                    return Ok(None);
                }
                read_private(&path, name).map(|v| Some((v, Source::Dir(path))))
            }
            Backing::None => Ok(None),
        }
    }

    /// Keep `value` where `flotilla setup` keeps secrets (replacing any old one).
    pub fn set(&self, name: Name, value: &str) -> Result<Source> {
        let value = value.trim();
        if !safe(value) {
            return Err(Error::Invalid(format!(
                "That doesn't look like {}",
                name.what()
            )));
        }
        match &self.backing {
            Backing::Keychain => keychain_set(name, value),
            Backing::Dir(dir) => {
                write_private(dir, name.file(), value)?;
                Ok(Source::Dir(dir.join(name.file())))
            }
            Backing::None => Err(Error::Invalid(format!("Nowhere to keep {}", name.what()))),
        }
    }

    /// What's kept where `flotilla setup` keeps secrets (not the environment, files or
    /// credentials).
    pub fn kept(&self, name: Name) -> Result<Option<(String, Source)>> {
        SecretStore {
            env: Some(HashMap::new()),
            files: HashMap::new(),
            credentials: None,
            backing: self.backing.clone(),
        }
        .get(name)
    }

    /// Where [`SecretStore::set`] keeps it, for messages.
    pub fn kept_in(&self, name: Name) -> String {
        match &self.backing {
            Backing::Keychain => "the Keychain".into(),
            Backing::Dir(dir) => dir.join(name.file()).display().to_string(),
            Backing::None => "nowhere".into(),
        }
    }
}

/// `secrets` beside the default database.
fn default_dir() -> PathBuf {
    crate::lifecycle::default_db_path()
        .parent()
        .map(|d| d.join("secrets"))
        .unwrap_or_else(|| PathBuf::from("secrets"))
}

static STORE: RwLock<Option<SecretStore>> = RwLock::new(None);

/// Where this process finds and keeps its secrets (the CLI's `--boat-key-file`,
/// `--secrets-dir`…). Until it's set: [`SecretStore::system`] with the default folder.
pub fn configure(store: SecretStore) {
    *STORE.write().unwrap_or_else(|e| e.into_inner()) = Some(store);
}

pub fn store() -> SecretStore {
    STORE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| SecretStore::system(None))
}

/// The boat API key, from wherever it's kept.
pub fn api_key() -> Result<String> {
    api_key_from().map(|(k, _)| k)
}

/// The boat API key and where it came from.
pub fn api_key_from() -> Result<(String, Source)> {
    store()
        .get(Name::BoatKey)?
        .ok_or_else(|| Error::NoApiKey("run `flotilla setup`, or set BOAT_API_KEY".into()))
}

/// Keep the boat API key where `flotilla setup` keeps secrets. On a Mac that's the login
/// Keychain, in this build's own item (see [`crate::flavor::Flavor::keychain_service`]).
pub fn set_api_key(key: &str) -> Result<()> {
    store().set(Name::BoatKey, key).map(|_| ())
}

/// The token `claude setup-token` made, for the summarizer, if one is kept. None: the
/// claude CLI's own sign-in.
pub fn claude_token() -> Result<Option<String>> {
    store().get(Name::ClaudeToken).map(|t| t.map(|(t, _)| t))
}

pub fn set_claude_token(token: &str) -> Result<()> {
    store().set(Name::ClaudeToken, token).map(|_| ())
}

/// `name` from the file at `path`, which only its owner may read (`--boat-key-file`).
pub fn read_file(path: &Path, name: Name) -> Result<String> {
    read_private(path, name)
}

/// A secret in a file only its owner may read.
fn read_private(path: &Path, name: Name) -> Result<String> {
    Ok(flotilla_model::secrets::read_private(
        path,
        Some(name.flag()),
    )?)
}

#[cfg(target_os = "macos")]
fn keychain_get(name: Name) -> Result<Option<(String, Source)>> {
    use flotilla_model::secrets::Error::Missing;
    let service = crate::flavor::keychain_service();
    match keychain::password(service, name.account()) {
        Ok(v) => Ok(Some((v, Source::Keychain(service)))),
        // A dev build falls back to the release app's boat key.
        Err(Missing(_)) if crate::flavor::is_dev() && name == Name::BoatKey => {
            let release = crate::flavor::Flavor::Release.keychain_service();
            match keychain::password(release, name.account()) {
                Ok(v) => Ok(Some((v, Source::Keychain(release)))),
                Err(Missing(_)) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        Err(Missing(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_: Name) -> Result<Option<(String, Source)>> {
    Err(Error::Invalid(
        "There's no Keychain here: use --secrets-dir".into(),
    ))
}

#[cfg(target_os = "macos")]
fn keychain_set(name: Name, value: &str) -> Result<Source> {
    let service = crate::flavor::keychain_service();
    keychain::set(service, name.account(), value, name.what())?;
    Ok(Source::Keychain(service))
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_: Name, _: &str) -> Result<Source> {
    Err(Error::Invalid(
        "There's no Keychain here: use --secrets-dir".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("flotilla-secrets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn private(path: &Path, value: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, value).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// The environment, then a file named on the command line, then systemd's credentials,
    /// then the store `flotilla setup` writes.
    #[test]
    fn lookup_order() {
        let root = scratch();
        let (creds, kept) = (root.join("creds"), root.join("secrets"));
        std::fs::create_dir_all(&creds).unwrap();
        let mut s = SecretStore {
            backing: Backing::Dir(kept.clone()),
            ..SecretStore::default()
        }
        .with_env(&[]);
        assert_eq!(s.get(Name::BoatKey).unwrap(), None);

        s.set(Name::BoatKey, "boat_kept").unwrap();
        assert_eq!(
            s.get(Name::BoatKey).unwrap(),
            Some(("boat_kept".into(), Source::Dir(kept.join("boat-key"))))
        );

        private(&creds.join("boat-key"), "boat_credential\n");
        s.credentials = Some(creds.clone());
        assert_eq!(
            s.get(Name::BoatKey).unwrap().unwrap().1,
            Source::Credential(creds.join("boat-key"))
        );

        let file = root.join("key");
        private(&file, " boat_file \n");
        s.files.insert(Name::BoatKey, file.clone());
        assert_eq!(
            s.get(Name::BoatKey).unwrap(),
            Some(("boat_file".into(), Source::File(file.clone())))
        );

        let s = s.with_env(&[
            ("BOAT_API_KEY", "boat_env"),
            ("CLAUDE_CODE_OAUTH_TOKEN", " "),
        ]);
        assert_eq!(
            s.get(Name::BoatKey).unwrap(),
            Some(("boat_env".into(), Source::Env("BOAT_API_KEY")))
        );
        // An empty variable doesn't count; each secret has its own sources.
        assert_eq!(s.get(Name::ClaudeToken).unwrap(), None);
        s.set(Name::ClaudeToken, "sk-ant-oat01-abc_DEF").unwrap();
        assert_eq!(
            s.get(Name::ClaudeToken).unwrap().unwrap().0,
            "sk-ant-oat01-abc_DEF"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_must_be_private_and_kept_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let file = root.join("key");
        std::fs::write(&file, "boat_x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut s = SecretStore {
            backing: Backing::None,
            ..SecretStore::default()
        }
        .with_env(&[]);
        s.files.insert(Name::BoatKey, file.clone());
        let err = s.get(Name::BoatKey).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
        s.files.insert(Name::BoatKey, root.join("missing"));
        assert!(s.get(Name::BoatKey).is_err());
        // Nowhere to keep it.
        assert!(s.set(Name::BoatKey, "boat_x").is_err());

        let kept = root.join("secrets");
        let s = SecretStore {
            backing: Backing::Dir(kept.clone()),
            ..SecretStore::default()
        }
        .with_env(&[]);
        s.set(Name::BoatKey, "boat_x").unwrap();
        assert_eq!(
            std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(kept.join("boat-key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(s.set(Name::BoatKey, "has \"quotes\"").is_err());
        assert!(s.set(Name::BoatKey, "").is_err());
        // What it says holds no secret.
        assert!(!format!("{s:?}").contains("boat_x"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
