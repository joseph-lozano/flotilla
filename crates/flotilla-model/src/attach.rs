//! Files sent with a prompt, as the device that picks them sees them: what each is, boat's
//! limits, and the upload (`flotilla_core::attach` sends them from the uploaded copies).

use flotilla_proto::wire::Blob;
use flotilla_proto::{Attached, Kind};
use std::path::{Path, PathBuf};

/// boat's limits on one prompt's files, on their bytes (not the base64 sent).
pub const MAX_FILES: usize = 12;
pub const MAX_FILE_MB: u64 = 25;
pub const MAX_TOTAL_MB: u64 = 64;
/// boat's megabytes are binary: it took a 26,000,000-byte file and refused 26,300,000.
pub const MB: u64 = 1024 * 1024;

/// The file at `path`, to send with a prompt.
pub fn from_path(path: &Path) -> Result<Attached, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let meta = std::fs::metadata(path).map_err(|e| format!("Couldn't read {name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} isn't a file"));
    }
    Ok(Attached {
        path: path.to_path_buf(),
        kind: kind_of_file(&name, path),
        name,
        size: meta.len(),
        blob: String::new(),
        from: None,
    })
}

/// What the file at `path` holds: by its name, or when that doesn't say (a Makefile, a
/// `.env`), by whether its start reads as text.
fn kind_of_file(name: &str, path: &Path) -> Kind {
    match Kind::of_name(name) {
        Kind::Other => match reads_as_text(path) {
            Some(true) => Kind::Text,
            Some(false) => Kind::Binary,
            None => Kind::Other,
        },
        kind => kind,
    }
}

/// Whether the file's first 8 KB are UTF-8 without NULs (a character cut off at the end is
/// fine); `None` if it can't be read.
fn reads_as_text(path: &Path) -> Option<bool> {
    use std::io::Read;
    let mut head = Vec::with_capacity(8192);
    std::fs::File::open(path)
        .ok()?
        .take(8192)
        .read_to_end(&mut head)
        .ok()?;
    if head.contains(&0) {
        return Some(false);
    }
    Some(match std::str::from_utf8(&head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    })
}

/// Why `files` can't go with one prompt, if they can't.
pub fn over_limit(files: &[Attached]) -> Option<String> {
    if files.len() > MAX_FILES {
        return Some(format!("{MAX_FILES} files at most"));
    }
    if let Some(f) = files.iter().find(|f| f.size > MAX_FILE_MB * MB) {
        return Some(format!("{} is over {MAX_FILE_MB} MB", f.name));
    }
    if files.iter().map(|f| f.size).sum::<u64>() > MAX_TOTAL_MB * MB {
        return Some(format!("Over {MAX_TOTAL_MB} MB in total"));
    }
    None
}

/// Add the files at `paths` to `files`, skipping any already there and any that would break a
/// limit. Returns why the first one skipped for a limit (or unreadable) was left out.
pub fn add(files: &mut Vec<Attached>, paths: &[PathBuf]) -> Option<String> {
    let mut refused = None;
    for p in paths {
        if files.iter().any(|f| f.path == *p) {
            continue;
        }
        let f = match from_path(p) {
            Ok(f) => f,
            Err(e) => {
                refused.get_or_insert(e);
                continue;
            }
        };
        files.push(f);
        if let Some(why) = over_limit(files) {
            files.pop();
            refused.get_or_insert(why);
        }
    }
    refused
}

/// Upload the files through `put` (the server's `PUT /v1/blobs`, or the local blob store),
/// as they're on disk now, checking the limits again. One no longer on this device (sent
/// again from a prompt another device sent, say) goes as it was uploaded then.
pub fn upload(
    files: &[Attached],
    put: impl Fn(&[u8]) -> Result<Blob, String>,
) -> Result<Vec<Attached>, String> {
    let here = |f: &Attached| !f.path.as_os_str().is_empty() && f.path.is_file();
    let now = files
        .iter()
        .map(|f| match (here(f), f.blob.is_empty()) {
            (false, false) => Ok((f.clone(), false)),
            _ => from_path(&f.path).map(|n| {
                (
                    Attached {
                        name: f.name.clone(),
                        ..n
                    },
                    true,
                )
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(why) = over_limit(&now.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>()) {
        return Err(why);
    }
    now.into_iter()
        .map(|(mut f, read)| {
            if read {
                let bytes =
                    std::fs::read(&f.path).map_err(|e| format!("Couldn't read {}: {e}", f.name))?;
                let blob = put(&bytes).map_err(|e| format!("Couldn't upload {}: {e}", f.name))?;
                (f.blob, f.size) = (blob.id, blob.size);
            }
            Ok(f)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Attached {
        Attached {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.into(),
            size,
            kind: Kind::of_name(name),
            blob: String::new(),
            from: None,
        }
    }

    #[test]
    fn limits() {
        assert_eq!(over_limit(&[]), None);
        assert_eq!(over_limit(&[file("a.png", 25 * MB)]), None);
        assert_eq!(
            over_limit(&[file("big.mov", 25 * MB + 1)]).as_deref(),
            Some("big.mov is over 25 MB")
        );
        let twelve: Vec<_> = (0..12).map(|i| file(&format!("{i}.txt"), 1)).collect();
        assert_eq!(over_limit(&twelve), None);
        let thirteen: Vec<_> = (0..13).map(|i| file(&format!("{i}.txt"), 1)).collect();
        assert_eq!(over_limit(&thirteen).as_deref(), Some("12 files at most"));
        let three = [file("a", 20 * MB), file("b", 20 * MB), file("c", 24 * MB)];
        assert_eq!(over_limit(&three), None);
        let four = [
            file("a", 20 * MB),
            file("b", 20 * MB),
            file("c", 24 * MB),
            file("d", 1),
        ];
        assert_eq!(over_limit(&four).as_deref(), Some("Over 64 MB in total"));
    }

    #[test]
    fn add_skips_duplicates_and_what_breaks_a_limit() {
        let dir = std::env::temp_dir().join(format!("flotilla-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        let mut files = Vec::new();
        assert_eq!(add(&mut files, std::slice::from_ref(&a)), None);
        assert_eq!(
            add(&mut files, &[a.clone(), dir.clone()]).as_deref(),
            Some(
                format!(
                    "{} isn't a file",
                    dir.file_name().unwrap().to_string_lossy()
                )
                .as_str()
            )
        );
        assert_eq!(
            files,
            vec![Attached {
                path: a.clone(),
                name: "a.txt".into(),
                size: 5,
                kind: Kind::Text,
                blob: String::new(),
                from: None
            }]
        );
        files.extend((0..11).map(|i| file(&format!("{i}.txt"), 1)));
        let b = dir.join("b.txt");
        std::fs::write(&b, b"!").unwrap();
        assert_eq!(add(&mut files, &[b]).as_deref(), Some("12 files at most"));
        assert_eq!(files.len(), 12);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::of_name("Shot 1.PNG"), Kind::Image);
        assert_eq!(Kind::of_name("notes.md"), Kind::Text);
        assert_eq!(Kind::of_name("main.rs"), Kind::Code);
        assert_eq!(Kind::of_name("data.JSON"), Kind::Data);
        assert_eq!(Kind::of_name("app.dmg"), Kind::Binary);
        assert_eq!(Kind::of_name("Makefile"), Kind::Other);
        let dir = std::env::temp_dir().join(format!("flotilla-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let kind = |name: &str, bytes: &[u8]| {
            std::fs::write(dir.join(name), bytes).unwrap();
            from_path(&dir.join(name)).unwrap().kind
        };
        assert_eq!(kind("Makefile", b"all:\n\tcargo build\n"), Kind::Text);
        assert_eq!(kind(".env", "NAME=caf\u{e9}".as_bytes()), Kind::Text);
        assert_eq!(kind("blob", b"\x7fELF\x02\x01\x01\x00"), Kind::Binary);
        assert_eq!(kind("latin1", b"caf\xe9 au lait"), Kind::Binary);
        assert_eq!(kind("cut", &"\u{e9}".as_bytes()[..1]), Kind::Text);
        assert_eq!(kind("pic.png", b"not really"), Kind::Image);
        std::fs::remove_dir_all(&dir).unwrap();
        // Files sent before kinds were kept read back as Other.
        let old: Attached =
            serde_json::from_str(r#"{"path":"/tmp/a","name":"a","size":1}"#).unwrap();
        assert_eq!(old.kind, Kind::Other);
    }
}
