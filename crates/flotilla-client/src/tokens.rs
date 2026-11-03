//! This device's token for each server it's paired with (moved here from
//! `flotilla_core::secrets`: only a client keeps one). In the login Keychain on macOS, one item
//! per server; elsewhere in files only their owner can read (a Linux desktop until Secret
//! Service).

use flotilla_model::flavor::{self, Flavor};
#[cfg(target_os = "macos")]
use flotilla_model::secrets::keychain;
use flotilla_model::secrets::{Error, safe};
#[cfg(not(target_os = "macos"))]
use flotilla_model::secrets::{read_private, write_private};
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;

/// The Keychain service device tokens are kept under (one item per server, by its id).
pub fn device_token_service() -> &'static str {
    match flavor::get() {
        Flavor::Release => "flotilla-device",
        Flavor::Dev => "flotilla-dev-device",
    }
}

/// Keep this device's token for `server` (its id), replacing any old one.
pub fn set_device_token(server: &str, token: &str) -> Result<(), Error> {
    if !safe(server) {
        return Err(Error::Invalid(
            "couldn't save a device token: a bad server id".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    return keychain::set(device_token_service(), server, token, "a device token");
    #[cfg(not(target_os = "macos"))]
    {
        if !safe(token.trim()) {
            return Err(Error::Invalid(
                "that doesn't look like a device token".into(),
            ));
        }
        write_private(&device_dir(), server, token.trim())
    }
}

/// This device's token for `server`, if one is kept.
pub fn device_token(server: &str) -> Result<Option<String>, Error> {
    if !safe(server) {
        return Ok(None);
    }
    #[cfg(target_os = "macos")]
    return match keychain::password(device_token_service(), server) {
        Ok(t) => Ok(Some(t)),
        Err(Error::Missing(_)) => Ok(None),
        Err(e) => Err(e),
    };
    #[cfg(not(target_os = "macos"))]
    {
        let path = device_dir().join(server);
        if !path.exists() {
            return Ok(None);
        }
        read_private(&path, None).map(Some)
    }
}

/// Forget this device's token for `server`.
pub fn forget_device_token(server: &str) -> Result<(), Error> {
    if !safe(server) {
        return Err(Error::Invalid("that isn't a server id".into()));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/security")
            .args([
                "delete-generic-password",
                "-s",
                device_token_service(),
                "-a",
                server,
            ])
            .output()?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = std::fs::remove_file(device_dir().join(server));
    }
    Ok(())
}

/// Where device tokens are kept without a Keychain (a Linux desktop until Secret Service).
#[cfg(not(target_os = "macos"))]
fn device_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(flavor::app_name())
        .join(device_token_service())
}
