//! Linux, started from an AppImage: nothing registers an AppImage's `flotilla://` handler, and
//! pairing links need one. So the app writes `flotilla.desktop` into the user's applications
//! folder, opening links with this AppImage file (`$APPIMAGE`, set by its runtime), and makes it
//! the scheme's handler. Again only when the file changes: the AppImage moved, or is new.

use std::path::{Path, PathBuf};
use std::process::Command;

/// From an AppImage: write the handler and register it. Before a start hands its link to the
/// open app and goes, so the AppImage moved and started again is registered where it is now.
pub fn integrate() {
    let Some(appimage) = std::env::var_os("APPIMAGE") else {
        return;
    };
    let Some(appimage) = appimage.to_str() else {
        return;
    };
    // The spec's rule: a relative XDG_DATA_HOME is ignored.
    let Some(applications) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
        .map(|d| d.join("applications"))
    else {
        return;
    };
    match install(&applications, appimage) {
        Ok(Some((path, mut register))) => {
            match register.status() {
                Ok(s) if s.success() => return,
                Ok(s) => eprintln!("[appimage] {register:?}: {s}"),
                Err(e) => eprintln!("[appimage] couldn't run {register:?}: {e}"),
            }
            // Not registered: no file either, so the next start tries again.
            let _ = std::fs::remove_file(path);
        }
        Ok(None) => {}
        Err(e) => eprintln!("[appimage] couldn't write the link handler: {e}"),
    }
}

/// Write `<scheme>.desktop` into `applications` for `appimage`, unless it's there already.
/// Then its path, and the command that makes it the scheme's handler; `None` if nothing changed.
fn install(applications: &Path, appimage: &str) -> std::io::Result<Option<(PathBuf, Command)>> {
    let flavor = flotilla_model::flavor::get();
    let (scheme, name) = (flavor.url_scheme(), flavor.app_name());
    if appimage.chars().any(char::is_control) {
        return Err(std::io::Error::other("its path has a control character"));
    }
    let file = format!("{scheme}.desktop");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={} %u\nTerminal=false\n\
         NoDisplay=true\nMimeType=x-scheme-handler/{scheme};\n",
        exec_arg(appimage)
    );
    let path = applications.join(&file);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == entry) {
        return Ok(None);
    }
    std::fs::create_dir_all(applications)?;
    std::fs::write(&path, entry)?;
    let mut register = Command::new("xdg-mime");
    register.args(["default", &file, &format!("x-scheme-handler/{scheme}")]);
    Ok(Some((path, register)))
}

/// A path as an `Exec` argument (the Desktop Entry spec): bare if it can be, which even the
/// simplest `xdg-open` reads, else quoted and escaped.
fn exec_arg(path: &str) -> String {
    let path = path.replace('%', "%%");
    if !path.contains([
        ' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`',
    ]) {
        return path;
    }
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            // Escaped for the quotes, then that backslash for the key file.
            '"' | '`' | '$' => quoted.extend(['\\', '\\', c]),
            '\\' => quoted.push_str(r"\\\\"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_handler_once_and_registers_it() {
        let dir = std::env::temp_dir().join(format!("flotilla-appimage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let applications = dir.join("applications");
        let appimage = "/home/u/Apps/Flotilla-x86_64.AppImage";

        let (path, register) = install(&applications, appimage).unwrap().unwrap();
        assert_eq!(path, applications.join("flotilla.desktop"));
        assert_eq!(register.get_program(), "xdg-mime");
        assert_eq!(
            register.get_args().collect::<Vec<_>>(),
            ["default", "flotilla.desktop", "x-scheme-handler/flotilla"]
        );
        assert_eq!(
            std::fs::read_to_string(applications.join("flotilla.desktop")).unwrap(),
            "[Desktop Entry]\nType=Application\nName=Flotilla\n\
             Exec=/home/u/Apps/Flotilla-x86_64.AppImage %u\nTerminal=false\nNoDisplay=true\n\
             MimeType=x-scheme-handler/flotilla;\n"
        );
        // Started again from the same file: nothing to do.
        assert!(install(&applications, appimage).unwrap().is_none());
        // Moved: written again, and registered again.
        assert!(
            install(&applications, "/opt/Flotilla.AppImage")
                .unwrap()
                .is_some()
        );
        assert!(
            std::fs::read_to_string(applications.join("flotilla.desktop"))
                .unwrap()
                .contains("Exec=/opt/Flotilla.AppImage %u\n")
        );
        assert!(install(&applications, "/tmp/a\nb").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quotes_only_paths_that_need_it() {
        assert_eq!(
            exec_arg("/opt/Flotilla-1.AppImage"),
            "/opt/Flotilla-1.AppImage"
        );
        assert_eq!(exec_arg("/opt/100%/F.AppImage"), "/opt/100%%/F.AppImage");
        assert_eq!(
            exec_arg("/home/u/My Apps/F.AppImage"),
            "\"/home/u/My Apps/F.AppImage\""
        );
        assert_eq!(exec_arg(r#"/a b/$x`"\F"#), r#""/a b/\\$x\\`\\"\\\\F""#);
    }
}
