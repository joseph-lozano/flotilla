//! Where a device keeps secrets: files only their owner can read, and (macOS) the login
//! Keychain. `flotilla_core::secrets` keeps the engine's there (the boat key, the Claude
//! token), and flotilla-client this device's token for each server it's paired with.

use std::path::Path;

/// Why a secret couldn't be read or kept. Each says what `flotilla_core::Error` says for it.
#[derive(Debug)]
pub enum Error {
    /// No such Keychain item, or `security` couldn't run.
    Missing(String),
    Invalid(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing(s) => write!(f, "no boat API key: {s}"),
            Error::Invalid(s) => f.write_str(s),
            Error::Io(e) => write!(f, "File error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// A secret in a file only its owner may read. `flag`: the command-line flag that names the
/// file, if one does, for when it can't be read.
pub fn read_private(path: &Path, flag: Option<&str>) -> Result<String, Error> {
    let meta = std::fs::metadata(path).map_err(|e| match flag {
        Some(flag) => Error::Invalid(format!("Couldn't read {} ({flag}): {e}", path.display())),
        None => Error::Invalid(format!("Couldn't read {}: {e}", path.display())),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid(format!(
                "Other users can read {}: run `chmod 600 {}`",
                path.display(),
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let v = std::fs::read_to_string(path)
        .map_err(|e| Error::Invalid(format!("Couldn't read {}: {e}", path.display())))?;
    let v = v.trim().to_string();
    if v.is_empty() {
        return Err(Error::Invalid(format!("{} is empty", path.display())));
    }
    Ok(v)
}

/// Write `name` in `dir` (made, or kept, so only its owner can enter it), readable by its owner
/// only, written aside and then moved in.
pub fn write_private(dir: &Path, name: &str, value: &str) -> Result<(), Error> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let part = dir.join(format!(".{name}.{}", std::process::id()));
        let _ = std::fs::remove_file(&part);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)?;
        f.write_all(value.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&part, dir.join(name))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)?;
        std::fs::File::create(dir.join(name))?.write_all(value.as_bytes())?;
    }
    Ok(())
}

/// Characters that need no quoting for `security -i`, in an account or a secret. Every key,
/// token and id Flotilla keeps is made of these.
pub fn safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The login Keychain, through `/usr/bin/security` rather than a keychain crate: the items were
/// made by `security`, so `security` reads them without a prompt, where an unsigned dev binary
/// would get a Keychain prompt on every rebuild.
#[cfg(target_os = "macos")]
pub mod keychain {
    use super::{Error, safe};
    use std::process::Command;

    /// The secret goes to `security -i` on stdin, so it never appears in a process's arguments.
    pub fn set(service: &str, account: &str, secret: &str, what: &str) -> Result<(), Error> {
        use std::io::Write;
        let secret = secret.trim();
        if !safe(secret) {
            return Err(Error::Invalid(format!("that doesn't look like {what}")));
        }
        if !safe(account) {
            return Err(Error::Invalid(format!(
                "couldn't save {what}: a bad keychain account"
            )));
        }
        let mut child = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| Error::Invalid("no stdin for security".into()))?
            .write_all(
                format!("add-generic-password -U -s {service} -a {account} -w \"{secret}\"\n")
                    .as_bytes(),
            )?;
        let out = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !stderr.trim().is_empty() {
            return Err(Error::Invalid(format!(
                "couldn't save {what} to the keychain: {}",
                crate::util::redact(stderr.trim())
            )));
        }
        Ok(())
    }

    /// `Missing` when there's no such item.
    pub fn password(service: &str, account: &str) -> Result<String, Error> {
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", service, "-a", account, "-w"])
            .output()
            .map_err(|e| Error::Missing(format!("couldn't run /usr/bin/security: {e}")))?;
        if !out.status.success() {
            return Err(Error::Missing(format!(
                "no keychain item (service {service}, account {account})"
            )));
        }
        let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if key.is_empty() {
            return Err(Error::Missing("the keychain item is empty".into()));
        }
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips a throwaway item (not Flotilla's) through the login keychain.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the login keychain"]
    fn keychain_round_trip() {
        let service = "flotilla-test-roundtrip";
        keychain::set(service, "probe", "boat_test_Abc-123", "a boat API key").unwrap();
        assert_eq!(
            keychain::password(service, "probe").unwrap(),
            "boat_test_Abc-123"
        );
        keychain::set(service, "probe", "boat_test_replaced", "a boat API key").unwrap();
        assert_eq!(
            keychain::password(service, "probe").unwrap(),
            "boat_test_replaced"
        );
        std::process::Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", service, "-a", "probe"])
            .output()
            .unwrap();
        assert!(keychain::set(service, "probe", "has \"quotes\"", "a boat API key").is_err());
        assert!(keychain::set(service, "bad account", "fdt_x", "a device token").is_err());
    }

    #[test]
    fn errors_say_what_flotilla_core_says() {
        assert_eq!(
            Error::Invalid("that doesn't look like a device token".into()).to_string(),
            "that doesn't look like a device token"
        );
        assert_eq!(
            Error::Missing("the keychain item is empty".into()).to_string(),
            "no boat API key: the keychain item is empty"
        );
        let missing = std::env::temp_dir()
            .join(format!("flotilla-secrets-{}", std::process::id()))
            .join("none");
        assert!(
            read_private(&missing, Some("--boat-key-file"))
                .unwrap_err()
                .to_string()
                .contains("(--boat-key-file)")
        );
        assert!(
            !read_private(&missing, None)
                .unwrap_err()
                .to_string()
                .contains("--")
        );
    }
}
