//! Files sent with a prompt. boat saves them in the VM under `~/attachments` and shows images
//! to models that can see them. The client that picks them checks boat's limits, uploads each
//! ([`upload`]) and sends their blob ids; the engine sends the prompt from those copies
//! ([`load`]), never reading a path a client names.

use crate::boat::Attachment;
use crate::error::{Error, Result};
use base64::Engine;
use std::path::Path;

pub use flotilla_model::attach::{
    MAX_FILE_MB, MAX_FILES, MAX_TOTAL_MB, MB, add, from_path, over_limit, upload,
};
pub use flotilla_proto::{Attached, Kind};

/// The files for boat, from their uploaded copies, checking the limits on what was uploaded.
pub fn load(files: &[Attached], blobs: &crate::blobs::Blobs) -> Result<Vec<Attachment>> {
    let mut now = Vec::with_capacity(files.len());
    for f in files {
        let size = blobs.size(&f.blob)?.ok_or_else(|| gone(&f.name))?;
        now.push(Attached { size, ..f.clone() });
    }
    if let Some(why) = over_limit(&now) {
        return Err(Error::Invalid(why));
    }
    now.iter()
        .map(|f| {
            let bytes = blobs.read(&f.blob).map_err(|e| match e {
                Error::NotFound(_) => gone(&f.name),
                e => Error::Invalid(format!("Couldn't read {}: {e}", f.name)),
            })?;
            Ok(Attachment {
                filename: f.name.clone(),
                mime_type: mime_type(&f.name).to_string(),
                base64_data: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
        })
        .collect()
}

fn gone(name: &str) -> Error {
    Error::Invalid(format!(
        "{name} is no longer on the server: attach it again"
    ))
}

/// Why `files` can't be sent as they are: each must be uploaded, under a name that's only a
/// name. The server checks this as a request comes in.
pub fn check_uploaded(
    files: &[Attached],
    blobs: &crate::blobs::Blobs,
) -> std::result::Result<(), String> {
    for f in files {
        if f.name.is_empty()
            || f.name.contains(['/', '\\', '\0'])
            || f.name == "."
            || f.name == ".."
        {
            return Err(format!("{:?} isn't a file name", f.name));
        }
        if !crate::blobs::valid_id(&f.blob) {
            return Err(format!("{} wasn't uploaded", f.name));
        }
        match blobs.size(&f.blob) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(gone(&f.name).to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// The type boat's own CLI gives a file with this name.
pub fn mime_type(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A file picked on a device, uploaded as it is on disk then, and sent from that copy.
    #[test]
    fn upload_then_load_from_the_blob() {
        let dir = std::env::temp_dir().join(format!("flotilla-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let blobs = crate::blobs::Blobs::new(dir.join("blobs"));
        let put = |b: &[u8]| blobs.put(b).map_err(|e| e.to_string());
        let p = dir.join("Shot 1.PNG");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let f = from_path(&p).unwrap();
        std::fs::write(&p, b"\x89PNG\r\n").unwrap();
        let up = upload(std::slice::from_ref(&f), put).unwrap();
        assert_eq!(
            (up[0].size, up[0].name.as_str(), up[0].path.as_path()),
            (6, "Shot 1.PNG", p.as_path())
        );
        assert!(crate::blobs::valid_id(&up[0].blob));
        assert_eq!(check_uploaded(&up, &blobs), Ok(()));
        // The file changing or going afterwards doesn't matter: the copy is sent.
        std::fs::remove_file(&p).unwrap();
        let got = load(&up, &blobs).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].filename, "Shot 1.PNG");
        assert_eq!(got[0].mime_type, "image/png");
        assert_eq!(got[0].base64_data, "iVBORw0K");
        // Uploaded, and no longer on this device: sent again as it was.
        assert_eq!(upload(&up, |_| Err("no".into())).unwrap(), up);
        let mut elsewhere = up[0].clone();
        elsewhere.path = PathBuf::new();
        assert_eq!(
            upload(std::slice::from_ref(&elsewhere), |_| Err("no".into())).unwrap(),
            vec![elsewhere]
        );
        // Gone from the device before it was uploaded.
        assert!(
            upload(&[f], put)
                .unwrap_err()
                .starts_with("Couldn't read Shot 1.PNG")
        );
        // Still here: read again (what's there now is what goes, and its upload is fresh).
        std::fs::write(&p, b"\x89PNG!").unwrap();
        let again = upload(&up, put).unwrap();
        assert_eq!(again[0].size, 5);
        assert_ne!(again[0].blob, up[0].blob);
        std::fs::remove_file(&p).unwrap();
        // Not uploaded, a name that's a path, or a copy that expired: refused.
        let mut bad = up[0].clone();
        bad.blob = "/etc/passwd".into();
        assert_eq!(
            check_uploaded(&[bad.clone()], &blobs).unwrap_err(),
            "Shot 1.PNG wasn't uploaded"
        );
        assert!(load(&[bad], &blobs).is_err());
        let mut bad = up[0].clone();
        bad.name = "../x".into();
        assert!(check_uploaded(&[bad], &blobs).is_err());
        std::fs::remove_dir_all(blobs.dir()).unwrap();
        assert_eq!(
            check_uploaded(&up, &blobs).unwrap_err(),
            "Shot 1.PNG is no longer on the server: attach it again"
        );
        assert_eq!(
            load(&up, &blobs).unwrap_err().to_string(),
            "Shot 1.PNG is no longer on the server: attach it again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn types_match_boats_cli() {
        assert_eq!(mime_type("notes.log"), "text/plain");
        assert_eq!(mime_type("data.JSON"), "application/json");
        assert_eq!(mime_type("main.rs"), "application/octet-stream");
        assert_eq!(mime_type("Makefile"), "application/octet-stream");
        assert_eq!(mime_type("photo.jpeg"), "image/jpeg");
    }
}
