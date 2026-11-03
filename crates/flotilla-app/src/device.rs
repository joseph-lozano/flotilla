//! What this device keeps for itself, not shared with your other devices: how the window
//! looks and the sidebar is laid out, the one server it's paired with (its token is in the
//! Keychain, never here) and whether it's in use, and whether This Mac lets other devices in.
//! What belongs to one server's threads and projects (the sidebar's filter, the last project,
//! messages whose send failed) is kept for This Mac and for the paired server apart. A small
//! JSON file beside the database.

use flotilla_proto::Attached;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    /// `system`, `light` or `dark`.
    pub appearance: String,
    /// The sidebar's width as dragged; None: the default.
    pub sidebar_width: Option<f32>,
    /// Hidden down to its rail (⌃⌘S).
    pub sidebar_hidden: bool,
    /// Sidebar groups folded away; None: as first opened (Archived folded).
    pub collapsed: Option<Vec<String>>,
    /// What's kept for This Mac and for the paired server, by [`Device::key`].
    pub by_server: BTreeMap<String, ForServer>,
    /// The server this Mac is paired with.
    pub paired: Option<SavedServer>,
    /// It's in use; false: This Mac's own engine.
    pub on_server: bool,
    /// Before one server per app: every server paired with, and the one in use (by id). Read
    /// once, by [`Device::load`], and never written.
    #[serde(skip_serializing)]
    servers: Vec<SavedServer>,
    #[serde(skip_serializing)]
    server: Option<String>,
    /// This Mac lets other devices connect to it while the app is open, and through Tailscale.
    pub let_devices_in: bool,
    pub over_tailscale: bool,
    /// Banners and the unread count on the app's icon (`crate::notify`); None: on.
    pub notifications: Option<bool>,
}

/// What's kept for one server: its project ids and thread ids mean nothing to another.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForServer {
    /// The sidebar's filter: projects (by id, and `none`) whose threads are hidden.
    pub hidden_projects: BTreeSet<String>,
    /// The project New thread last started in ("" for No project).
    pub last_project: Option<String>,
    /// Messages whose send failed after you'd moved on, by thread: they go back in its
    /// composer, with their files, the next time it's open.
    pub unsent: BTreeMap<String, Unsent>,
}

impl ForServer {
    /// Keep a message whose send failed, with its files, after any already kept for the thread.
    pub fn keep_unsent(&mut self, thread: &str, text: &str, files: &[Attached]) {
        let kept = self.unsent.entry(thread.to_string()).or_default();
        kept.text = if kept.text.trim().is_empty() {
            text.to_string()
        } else {
            format!("{}\n\n{text}", kept.text)
        };
        for f in files {
            if !kept.files.iter().any(|k| k.path == f.path) {
                kept.files.push(f.clone());
            }
        }
    }
}

/// A server this Mac is paired with. Its device token is in the Keychain, under the server's id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    pub id: String,
    /// `https://box.tail1234.ts.net`, or `http://127.0.0.1:7777`.
    pub url: String,
    /// This Mac's device id there.
    pub device: String,
}

impl SavedServer {
    /// Its host, for showing.
    pub fn name(&self) -> &str {
        if self.url.starts_with("unix:") {
            return SERVER_HERE;
        }
        self.url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest)
    }
}

/// What's left to do for the server a pairing took the place of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replaced {
    /// None was paired, or it's the same pairing again.
    Nothing,
    /// Another server, or this one now through its socket (which needs no token): its token
    /// goes, and it's asked to revoke this device with it.
    Forget(SavedServer),
    /// The same server, paired again: its new token is kept under the same id, and the old
    /// device is revoked with it.
    Revoke(SavedServer),
}

/// A server on this machine, reached through its socket (`unix:/…`), as it's shown.
pub const SERVER_HERE: &str = if cfg!(target_os = "macos") {
    "Server on this Mac"
} else {
    "Server on this computer"
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Unsent {
    pub text: String,
    pub files: Vec<Attached>,
}

/// [`Device::key`] for This Mac's own engine.
pub const THIS_MAC: &str = "this-mac";

/// What the app calls the machine it runs on: "This Mac", or "This computer" off a Mac.
pub const THIS_MACHINE: &str = if cfg!(target_os = "macos") {
    "This Mac"
} else {
    "This computer"
};
/// [`THIS_MACHINE`] mid-sentence.
pub const THIS_MACHINE_LOWER: &str = if cfg!(target_os = "macos") {
    "this Mac"
} else {
    "this computer"
};

impl Device {
    pub fn notifications_on(&self) -> bool {
        self.notifications.unwrap_or(true)
    }

    /// Whose things are kept under: This Mac's, or a server's (its id; with none, as a dev
    /// `FLOTILLA_SERVER` has, its URL).
    pub fn key(server: Option<&SavedServer>) -> String {
        match server {
            None => THIS_MAC.to_string(),
            Some(s) if s.id.is_empty() => format!("url:{}", s.url),
            Some(s) => s.id.clone(),
        }
    }

    /// What's kept for `key`.
    pub fn for_server(&self, key: &str) -> ForServer {
        self.by_server.get(key).cloned().unwrap_or_default()
    }

    pub fn for_server_mut(&mut self, key: &str) -> &mut ForServer {
        self.by_server.entry(key.to_string()).or_default()
    }

    /// Where a device's file is, for a database at `db`.
    pub fn path_for(db: &Path) -> PathBuf {
        db.with_file_name("device.json")
    }

    /// The file at `path`, or a fresh start if there's none (or it doesn't read). One from
    /// before one server per app keeps the server it was using (else the one paired last):
    /// the others are forgotten (their tokens and what was kept for them), and the file is
    /// written again.
    pub fn load(path: &Path) -> Device {
        Self::load_with(path, crate::backend::forget)
    }

    /// [`Device::load`], with `forget` for each server it lets go of.
    pub fn load_with(path: &Path, forget: impl Fn(SavedServer)) -> Device {
        let mut d: Device = std::fs::read_to_string(path)
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        if d.servers.is_empty() {
            return d;
        }
        let mut servers = std::mem::take(&mut d.servers);
        let in_use = d.server.take();
        let kept = servers
            .iter()
            .position(|s| Some(&s.id) == in_use.as_ref())
            .unwrap_or(servers.len() - 1);
        d.on_server = in_use.as_ref() == Some(&servers[kept].id);
        d.paired = Some(servers.remove(kept));
        d.tidy();
        d.save(path);
        for s in servers {
            forget(s);
        }
        d
    }

    /// Write it whole (through a temporary file, so a crash can't leave half of one).
    pub fn save(&self, path: &Path) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// The server in use: the paired one, unless it's This Mac.
    pub fn current_server(&self) -> Option<&SavedServer> {
        self.paired.as_ref().filter(|_| self.on_server)
    }

    /// Paired with `server` (the same one again, or another), and using it: in place of the
    /// one before, which is returned with what's left to do for it.
    pub fn pair(&mut self, server: SavedServer) -> Replaced {
        let (id, device) = (server.id.clone(), server.device.clone());
        let old = self.paired.replace(server);
        self.on_server = true;
        self.tidy();
        match old {
            None => Replaced::Nothing,
            // Over its socket now: no token was kept in place of the old one.
            Some(old) if old.id != id || device.is_empty() => Replaced::Forget(old),
            Some(old) if old.device != device => Replaced::Revoke(old),
            Some(_) => Replaced::Nothing,
        }
    }

    /// No longer paired: back to This Mac. The server is returned to be forgotten.
    pub fn unpair(&mut self) -> Option<SavedServer> {
        self.on_server = false;
        let old = self.paired.take();
        self.tidy();
        old
    }

    /// Keep only what's kept for This Mac and the paired server.
    fn tidy(&mut self) {
        let paired = self.paired.as_ref().map(|s| Self::key(Some(s)));
        self.by_server
            .retain(|k, _| k == THIS_MAC || Some(k) == paired.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_file_round_trips_and_keeps_failed_sends_in_order() {
        let dir = std::env::temp_dir().join(format!("flotilla-device-{}", std::process::id()));
        let path = Device::path_for(&dir.join("flotilla.db"));
        let mut d = Device::load(&path);
        assert_eq!(d, Device::default());
        d.appearance = "dark".into();
        d.sidebar_width = Some(300.);
        d.for_server_mut(THIS_MAC)
            .hidden_projects
            .insert("none".into());
        d.collapsed = Some(vec!["archived".into()]);
        let file = |name: &str| Attached {
            path: format!("/tmp/{name}").into(),
            name: name.into(),
            size: 1,
            kind: Default::default(),
            blob: String::new(),
            from: None,
        };
        d.for_server_mut(THIS_MAC)
            .keep_unsent("t1", "first", &[file("a.txt")]);
        d.for_server_mut(THIS_MAC)
            .keep_unsent("t1", "second", &[file("a.txt"), file("b.txt")]);
        d.save(&path);
        assert_eq!(
            d.pair(SavedServer {
                id: "srv_a".into(),
                url: "https://box.tail1.ts.net".into(),
                device: "d1".into()
            }),
            Replaced::Nothing
        );
        // The same server again: in place, the old device revoked with the new token.
        let old = d.pair(SavedServer {
            id: "srv_a".into(),
            url: "https://box.tail1.ts.net".into(),
            device: "d2".into(),
        });
        assert!(matches!(old, Replaced::Revoke(s) if s.device == "d1"));
        d.save(&path);
        let back = Device::load_with(&path, |s| panic!("nothing to forget, but {}", s.id));
        assert_eq!(back, d);
        assert_eq!(
            back.current_server().map(|s| (s.device.as_str(), s.name())),
            Some(("d2", "box.tail1.ts.net"))
        );
        let mine = back.for_server(THIS_MAC);
        assert_eq!(mine.unsent["t1"].text, "first\n\nsecond");
        assert_eq!(mine.unsent["t1"].files.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_server_keeps_its_own_filter_last_project_and_failed_sends() {
        let box_ = SavedServer {
            id: "srv_box".into(),
            url: "https://box.tail1.ts.net".into(),
            device: "d1".into(),
        };
        let dev = SavedServer {
            id: String::new(),
            url: "unix:/tmp/f.sock".into(),
            device: String::new(),
        };
        assert_eq!(
            (
                Device::key(None),
                Device::key(Some(&box_)),
                Device::key(Some(&dev))
            ),
            (
                THIS_MAC.to_string(),
                "srv_box".to_string(),
                "url:unix:/tmp/f.sock".to_string()
            )
        );
        let mut d = Device::default();
        d.for_server_mut(THIS_MAC)
            .hidden_projects
            .insert("p_mac".into());
        d.for_server_mut(THIS_MAC).last_project = Some("p_mac".into());
        d.for_server_mut("srv_box")
            .keep_unsent("t9", "on the box", &[]);
        // Nothing of one is seen from another.
        assert!(
            d.for_server("srv_box").hidden_projects.is_empty()
                && d.for_server("srv_box").last_project.is_none()
        );
        assert!(d.for_server(THIS_MAC).unsent.is_empty());
        assert_eq!(d.for_server("srv_other"), ForServer::default());
        let back: Device = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn pairing_another_server_replaces_the_first_and_its_things() {
        let box_ = SavedServer {
            id: "srv_box".into(),
            url: "https://box.tail1.ts.net".into(),
            device: "d1".into(),
        };
        let work = SavedServer {
            id: "srv_work".into(),
            url: "https://work.example.com".into(),
            device: "d2".into(),
        };
        let mut d = Device::default();
        d.for_server_mut(THIS_MAC).last_project = Some("p_mac".into());
        assert_eq!(d.pair(box_.clone()), Replaced::Nothing);
        d.for_server_mut("srv_box").last_project = Some("p_box".into());
        // Back to This Mac: the box stays paired, not in use.
        d.on_server = false;
        assert_eq!((d.current_server(), d.paired.as_ref()), (None, Some(&box_)));
        assert_eq!(d.pair(work.clone()), Replaced::Forget(box_.clone()));
        assert_eq!(d.current_server(), Some(&work));
        assert_eq!(
            d.by_server.keys().collect::<Vec<_>>(),
            vec![THIS_MAC],
            "the box's things go with it"
        );
        assert_eq!(d.unpair(), Some(work));
        assert_eq!(
            (d.current_server(), d.paired.as_ref(), d.on_server),
            (None, None, false)
        );
        assert_eq!(
            d.for_server(THIS_MAC).last_project.as_deref(),
            Some("p_mac")
        );
    }

    /// Pairing the same server again: over the network, the old device goes with the new token
    /// (kept under the same id); through its socket, which needs none, the old token goes.
    #[test]
    fn the_same_server_again_revokes_the_old_device_or_forgets_its_token() {
        let tcp = |device: &str| SavedServer {
            id: "srv_box".into(),
            url: "https://box.tail1.ts.net".into(),
            device: device.into(),
        };
        let socket = SavedServer {
            id: "srv_box".into(),
            url: "unix:/tmp/f.sock".into(),
            device: String::new(),
        };
        let mut d = Device::default();
        d.pair(tcp("d1"));
        assert_eq!(d.pair(tcp("d1")), Replaced::Nothing);
        assert_eq!(d.pair(tcp("d2")), Replaced::Revoke(tcp("d1")));
        assert_eq!(
            d.pair(socket.clone()),
            Replaced::Forget(tcp("d2")),
            "the old token isn't needed through the socket"
        );
        assert_eq!(
            d.pair(socket.clone()),
            Replaced::Forget(socket.clone()),
            "nothing kept for it: forgetting is harmless"
        );
        assert_eq!(
            d.pair(tcp("d3")),
            Replaced::Revoke(socket),
            "no device to revoke (`leave` skips it); the new token stays"
        );
    }

    /// A file from before one server per app: the server in use stays (else the one paired
    /// last), with what was kept for it and This Mac; the others are forgotten.
    #[test]
    fn a_file_with_several_servers_keeps_one_and_forgets_the_rest() {
        let dir = std::env::temp_dir().join(format!("flotilla-device-old-{}", std::process::id()));
        let path = Device::path_for(&dir.join("flotilla.db"));
        std::fs::create_dir_all(&dir).unwrap();
        let old = |server: Option<&str>| {
            serde_json::json!({
                "appearance": "dark",
                "servers": [
                    {"id": "srv_a", "url": "https://box.tail1.ts.net", "device": "da"},
                    {"id": "srv_b", "url": "http://127.0.0.1:7792", "device": "db"},
                    {"id": "srv_c", "url": "https://work.example.com", "device": "dc"},
                ],
                "server": server,
                "by_server": {"this-mac": {"last_project": "p_mac"}, "srv_a": {"last_project": "p_a"}, "srv_b": {"last_project": "p_b"}, "srv_c": {"last_project": "p_c"}},
            })
            .to_string()
        };
        let forgot = std::cell::RefCell::new(Vec::new());
        std::fs::write(&path, old(Some("srv_b"))).unwrap();
        let d = Device::load_with(&path, |s| forgot.borrow_mut().push(s.id));
        assert_eq!(
            forgot.take(),
            vec!["srv_a".to_string(), "srv_c".to_string()]
        );
        assert_eq!(d.current_server().map(|s| s.id.as_str()), Some("srv_b"));
        assert_eq!(d.appearance, "dark");
        assert_eq!(
            d.by_server.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["srv_b", THIS_MAC]
        );
        // Written again as one: read again, nothing more is forgotten.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("srv_a") && !written.contains("\"servers\""),
            "{written}"
        );
        assert_eq!(
            Device::load_with(&path, |s| panic!("forgot {} again", s.id)),
            d
        );

        // This Mac in use: the one paired last stays, not in use.
        std::fs::write(&path, old(None)).unwrap();
        let d = Device::load_with(&path, |s| forgot.borrow_mut().push(s.id));
        assert_eq!(
            forgot.take(),
            vec!["srv_a".to_string(), "srv_b".to_string()]
        );
        assert_eq!(
            (d.paired.as_ref().map(|s| s.id.as_str()), d.on_server),
            (Some("srv_c"), false)
        );
        assert_eq!(d.current_server(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
