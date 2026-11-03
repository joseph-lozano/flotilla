//! The app's door through Tailscale, with a stand-in `tailscale` that keeps what `serve` and
//! `off` do: Flotilla's 443 forward follows what listens, and nothing else on Tailscale is
//! touched. Its own test binary: it names the stand-in in the environment before anything runs.

use flotilla_engine::host::Mode;
use flotilla_server::Local;
use std::path::{Path, PathBuf};

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Forwards someone keeps on other ports (say 4000, 4026 and 8443).
const OTHERS: &str = r#""box.t.ts.net:4000":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4000"}}},"box.t.ts.net:4026":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4026"}}},"box.t.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3773"}}}"#;

/// A `tailscale` in `dir` that logs what it's asked and keeps `serve status` in step with
/// `serve --bg --https=443 …` and `serve --https=443 off` (which fails once while `fail-off`
/// is there).
fn stand_in(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(
        dir.join("others.json"),
        format!(r#"{{"Web":{{{OTHERS}}}}}"#),
    )
    .unwrap();
    std::fs::write(dir.join("ours.json"), format!(r#"{{"Web":{{{OTHERS},"box.t.ts.net:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:PORT"}}}}}}}}}}"#)).unwrap();
    std::fs::copy(dir.join("others.json"), dir.join("serve.json")).unwrap();
    let script = format!(
        r#"#!/bin/sh
d='{d}'
echo "$*" >> "$d/asked"
case "$*" in
  'status --json') echo '{{"BackendState":"Running","CertDomains":["box.t.ts.net"],"Self":{{"DNSName":"box.t.ts.net."}}}}' ;;
  'serve status --json') cat "$d/serve.json" ;;
  'serve --https=443 off') if [ -e "$d/fail-off" ]; then rm "$d/fail-off"; echo 'off failed once' >&2; exit 1; fi; cp "$d/others.json" "$d/serve.json" ;;
  'serve --bg --https=443 http://127.0.0.1:'*) to="$4"; sed "s/PORT/${{to##*:}}/" "$d/ours.json" > "$d/serve.json" ;;
  *) echo "not for a test: $*" >> "$d/asked"; exit 1 ;;
esac
"#,
        d = dir.display()
    );
    let p = dir.join("tailscale");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Where Tailscale's 443 forwards now, if it does, and that the other ports are as they were.
fn forwarded(dir: &Path) -> Option<u16> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("serve.json")).unwrap()).unwrap();
    for port in ["4000", "4026", "8443"] {
        assert!(
            v["Web"][format!("box.t.ts.net:{port}")].is_object(),
            "{port} went: {v}"
        );
    }
    v["Web"]["box.t.ts.net:443"]["Handlers"]["/"]["Proxy"]
        .as_str()
        .map(|p| p.rsplit(':').next().unwrap().parse().unwrap())
}

/// A port nothing listens on now (the door's own is in use while it's open, so never that).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Opening forwards 443 to the port that listens; opening again on another port moves it; an
/// old forward that can't be removed is kept and said, and closing removes it; an open that
/// can't listen leaves no forward (nor a pairing link) to a door that's shut; closing removes
/// it. Nothing else on Tailscale changes, and nothing resets it.
#[test]
fn tailscale_forwards_only_to_a_door_that_is_open() {
    let dir = Dir(std::env::temp_dir().join(format!("flotilla-door-ts-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let fake = stand_in(&dir.0);
    // SAFETY: the only test in this binary, and it sets this before anything that reads the
    // environment runs; never the real `tailscale`.
    unsafe { std::env::set_var("FLOTILLA_TAILSCALE", &fake) };
    assert_eq!(
        std::env::var_os("FLOTILLA_TAILSCALE"),
        Some(fake.clone().into_os_string())
    );
    let db = dir.0.join("flotilla.db");
    let url = || {
        flotilla_core::store::Store::open(&db)
            .unwrap()
            .kv_get(flotilla_engine::host::SERVER_URL_KEY)
            .unwrap()
    };
    let local = Local::start(db.clone(), None, Mode::Offline).unwrap();
    let access = local.access();

    let first = access.open(0, true).unwrap();
    assert_eq!(
        (first.url.as_str(), first.tailscale.as_deref()),
        ("https://box.t.ts.net", None)
    );
    assert!(forwarded(&dir.0).is_some());
    // Again, on another port: the forward follows (it isn't someone else's to refuse).
    let two = free_port();
    assert_eq!(access.open(two, true).unwrap().tailscale, None);
    assert_eq!(forwarded(&dir.0), Some(two));
    // The old forward can't be removed this time: it stays Flotilla's, why is said (not "someone
    // else's"), and closing removes it.
    std::fs::write(dir.0.join("fail-off"), "").unwrap();
    let three = free_port();
    let opened = access.open(three, true).unwrap();
    assert!(
        opened
            .tailscale
            .as_deref()
            .is_some_and(|why| why.contains("off failed once")),
        "{opened:?}"
    );
    assert_eq!(
        (forwarded(&dir.0), url()),
        (Some(two), Some(format!("http://127.0.0.1:{three}")))
    );
    access.close();
    assert_eq!((forwarded(&dir.0), url()), (None, None));
    // Its port taken: nothing listens, so nothing forwards to it and no link leads there.
    access.open(0, true).unwrap();
    assert!(forwarded(&dir.0).is_some());
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(
        access
            .open(taken.local_addr().unwrap().port(), true)
            .is_err()
    );
    assert_eq!((forwarded(&dir.0), url()), (None, None));
    // The next open forwards again; closing removes it.
    access.open(0, true).unwrap();
    assert!(forwarded(&dir.0).is_some());
    assert_eq!(url().as_deref(), Some("https://box.t.ts.net"));
    access.close();
    assert_eq!((forwarded(&dir.0), url()), (None, None));
    drop(local);

    let asked = std::fs::read_to_string(dir.0.join("asked")).unwrap();
    assert!(
        !asked.contains("reset") && !asked.contains("not for a test"),
        "{asked}"
    );
    drop(taken);
}
