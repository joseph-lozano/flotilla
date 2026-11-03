//! Two Flotillas on one boat account (plan R1): the app on a Mac and a server on a Linux box,
//! each with its own database, against one stand-in boat. Each labels the VMs it makes with
//! its id, and syncs, takes in and cleans up only its own; a VM made before the labels belongs
//! to the database that knows it.

use flotilla_core::boat::BoatClient;
use flotilla_core::lifecycle::{Core, NewThread};
use flotilla_core::owner::{self, SERVER_ID_KEY};
use flotilla_core::store::{Project, Store, Thread, ThreadState};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// The account: its VMs, what was made and what was deleted.
#[derive(Default)]
struct Account {
    vms: Vec<Value>,
    made: Vec<Value>,
    deleted: Vec<String>,
    /// Run as a list is answered, once: what happens while a sync reads it.
    on_list: Option<Box<dyn FnOnce() + Send>>,
}

/// A stand-in for boat.dev over HTTP, holding `vms`. Lists filter by label as boat does, and
/// keys may delete only the VMs they made (`sandbox.delete-own`): any other, even one that's
/// gone, is refused with a 403. Each VM's `_key` is the key that made it.
fn boat(vms: Vec<Value>) -> (String, Arc<Mutex<Account>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let account = Arc::new(Mutex::new(Account {
        vms,
        ..Account::default()
    }));
    let acc = account.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let (line, key, body) = read_request(&mut conn);
            let (status, answer) = answer(&acc, &line, &key, &body);
            let text = answer.to_string();
            let _ = write!(
                conn,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (base, account)
}

/// The request line, the bearer key, and the body.
fn read_request(conn: &mut std::net::TcpStream) -> (String, String, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head, want) = loop {
        let n = conn.read(&mut chunk).unwrap_or(0);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let len = head
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            break (head, end + 4 + len);
        }
        if n == 0 {
            return (String::new(), String::new(), String::new());
        }
    };
    while buf.len() < want {
        match conn.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body_at = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(buf.len(), |e| e + 4);
    let key = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("authorization: Bearer ")
                .or_else(|| l.strip_prefix("Authorization: Bearer "))
        })
        .unwrap_or_default()
        .to_string();
    (
        head.lines().next().unwrap_or_default().to_string(),
        key,
        String::from_utf8_lossy(&buf[body_at..]).to_string(),
    )
}

fn decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn answer(acc: &Mutex<Account>, line: &str, key: &str, body: &str) -> (&'static str, Value) {
    let mut acc = acc.lock().unwrap();
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let not_found = (
        "404 Not Found",
        json!({"ok": false, "status": 404, "code": "not_found", "message": "Not found"}),
    );
    match (method, path) {
        ("GET", "/environments") => (
            "200 OK",
            json!({"environments": [{"id": "env_1", "name": "default", "isDefault": true}]}),
        ),
        ("GET", "/limits") => ("200 OK", json!({})),
        ("GET", "/sandboxes") => {
            if let Some(f) = acc.on_list.take() {
                f();
            }
            let want: Vec<(String, String)> = query
                .split('&')
                .filter_map(|kv| kv.strip_prefix("label="))
                .filter_map(|l| {
                    decode(l)
                        .split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect();
            let vms: Vec<Value> = acc
                .vms
                .iter()
                .filter(|vm| {
                    want.iter()
                        .all(|(k, v)| vm["labels"][k].as_str() == Some(v))
                })
                .cloned()
                .collect();
            (
                "200 OK",
                json!({"sandboxes": vms, "pageInfo": {"hasMore": false}}),
            )
        }
        ("POST", "/sandboxes") => {
            let req: Value = serde_json::from_str(body).unwrap_or_default();
            // Never comes up: the start stops there (only the VM it made matters here).
            let vm = json!({"id": format!("bx_new{}", acc.made.len()), "state": "error", "name": req["name"], "labels": req["labels"], "environment": req["environment"], "_key": key});
            acc.made.push(req);
            acc.vms.push(vm.clone());
            ("200 OK", vm)
        }
        ("DELETE", p) => {
            let id = p.trim_start_matches("/sandboxes/").to_string();
            if !acc
                .vms
                .iter()
                .any(|vm| vm["id"] == id.as_str() && vm["_key"] == key)
            {
                return (
                    "403 Forbidden",
                    json!({"ok": false, "status": 403, "code": "api_key_action_forbidden", "message": "This API key can delete only the sandboxes it created."}),
                );
            }
            acc.vms.retain(|vm| vm["id"] != id.as_str());
            acc.deleted.push(id);
            ("200 OK", json!({"ok": true}))
        }
        ("GET", p) if p.ends_with("/conversations") => ("200 OK", json!({"conversations": []})),
        ("GET", p) => match acc
            .vms
            .iter()
            .find(|vm| vm["id"] == p.trim_start_matches("/sandboxes/"))
        {
            Some(vm) => ("200 OK", json!({"sandbox": vm})),
            None => not_found,
        },
        _ => not_found,
    }
}

/// The key each Flotilla uses: the Mac's and the box's.
const MAC_KEY: &str = "boat_mac";
const BOX_KEY: &str = "boat_box";

fn thread_vm(id: &str, thread: &str, server: Option<&str>, key: &str) -> Value {
    let mut labels = json!({"flotilla": "1", "flotilla.v": "1", "flotilla.thread": thread, "flotilla.provider": "claude-code", "flotilla.model": "haiku"});
    if let Some(s) = server {
        labels[owner::SERVER_LABEL] = json!(s);
        labels[owner::HOST_LABEL] = json!(if s == "srv_mac" { "Mac" } else { "box" });
    }
    json!({"id": id, "state": "archived", "name": thread, "labels": labels, "ttlSeconds": 1800, "_key": key})
}

fn setup_vm(id: &str, project: &str, server: Option<&str>, key: &str) -> Value {
    let mut labels = json!({"flotilla.setup": project, "flotilla.run": "r1"});
    if let Some(s) = server {
        labels[owner::SERVER_LABEL] = json!(s);
    }
    json!({"id": id, "state": "archived", "labels": labels, "_key": key})
}

fn thread(id: &str, sandbox: Option<&str>) -> Thread {
    Thread {
        id: id.into(),
        environment_id: None,
        environment_name: Some("default".into()),
        title: id.into(),
        sandbox_id: sandbox.map(str::to_string),
        conversation_id: None,
        provider: "claude-code".into(),
        model: "haiku".into(),
        effort: None,
        fast: false,
        state: ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: flotilla_core::util::now_iso(),
        updated_at: flotilla_core::util::now_iso(),
        branch: None,
        repo_project_id: None,
        base_sha: None,
    }
}

fn project(id: &str) -> Project {
    Project {
        id: id.into(),
        owner: "acme".into(),
        repo: "web".into(),
        default_branch: "main".into(),
        environment_name: "flotilla-web".into(),
        created_at: flotilla_core::util::now_iso(),
        template_sandbox_id: None,
        template_sha: None,
        template_built_at: None,
        prev_template_sandbox_id: None,
        head_sha: None,
        head_etag: None,
        checked_at: None,
        machine_type: None,
    }
}

fn core(base: &str, id: &str, key: &str) -> Core {
    let store = Store::open_in_memory().unwrap();
    store.kv_set(SERVER_ID_KEY, id).unwrap();
    Core::new(
        BoatClient::with_base(key.into(), base, &format!("{base}/provider-models")).unwrap(),
        store,
    )
}

#[test]
fn each_flotilla_syncs_takes_in_and_cleans_up_only_its_own() {
    // On the account: the Mac's thread from before the labels, one no database knows, and a
    // setup VM of a project the Mac has (from before the labels).
    let (base, account) = boat(vec![
        thread_vm("bx_old", "t-mac", None, MAC_KEY),
        thread_vm("bx_stray", "t-gone", None, MAC_KEY),
        setup_vm("bx_setup_old", "p-mac", None, MAC_KEY),
    ]);
    let mut mac = core(&base, "srv_mac", MAC_KEY);
    mac.store
        .insert_thread(&thread("t-mac", Some("bx_old")))
        .unwrap();
    mac.store.insert_project(&project("p-mac"), &[]).unwrap();
    let mut server = core(&base, "srv_box", BOX_KEY);

    // A fresh server takes in nothing it doesn't know: the Mac's thread stays the Mac's.
    let r = server.full_sync().unwrap();
    assert_eq!((r.sandboxes, r.created.len()), (0, 0));
    assert!(server.store.threads().unwrap().is_empty());
    assert_eq!(
        r.others.as_deref(),
        Some("2 VMs on this boat account belong to an older Flotilla. This one leaves them alone")
    );
    // The Mac keeps its own (not expired), and doesn't take in the stray.
    let r = mac.full_sync().unwrap();
    assert_eq!((r.sandboxes, r.created.len(), r.expired.len()), (1, 0, 0));
    assert_eq!(mac.store.threads().unwrap().len(), 1);

    // The server starts a thread: its VM carries the server's id and machine.
    server
        .store
        .insert_thread(&Thread {
            state: ThreadState::Creating,
            ..thread("t-box", None)
        })
        .unwrap();
    let req = NewThread {
        environment: "default".into(),
        project_id: None,
        provider: "claude-code".into(),
        model: "haiku".into(),
        effort: None,
        fast: false,
        title: None,
        machine_type: None,
        prompt: "hi".into(),
        attachments: vec![],
        wait_ready: false,
        on_branch: None,
        id: None,
        parent: None,
        branch_tag: None,
    };
    // The stand-in can't take prompts: only the VM matters here.
    let _ = server.launch_thread("t-box", &req, None);
    let made = account.lock().unwrap().made.clone();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["labels"][owner::SERVER_LABEL], "srv_box");
    assert_eq!(made[0]["labels"][owner::HOST_LABEL], owner::host_label());

    // The Mac never takes it in, and says whose it is.
    let r = mac.full_sync().unwrap();
    assert!(mac.store.thread("t-box").unwrap().is_none());
    assert!(
        r.others
            .as_deref()
            .unwrap()
            .contains(&format!("Flotilla on {} (srv_box)", owner::host_label())),
        "{:?}",
        r.others
    );
    // The server, having lost its database (a rebuild), takes its own back in from the label.
    server.store.delete_thread("t-box").unwrap();
    let r = server.full_sync().unwrap();
    assert_eq!(r.created, ["t-box"]);

    // Cleaning up a project both databases have (one was copied from the other): only the
    // remover's own setup VMs go, and the one from before the labels that it knows.
    server.store.insert_project(&project("p-mac"), &[]).unwrap();
    account.lock().unwrap().vms.extend([
        setup_vm("bx_setup_mac", "p-mac", Some("srv_mac"), MAC_KEY),
        setup_vm("bx_setup_box", "p-mac", Some("srv_box"), BOX_KEY),
    ]);
    flotilla_core::projects::remove_project(&mac.boat, &mac.store, "p-mac").unwrap();
    let mut deleted = account.lock().unwrap().deleted.clone();
    deleted.sort();
    assert_eq!(deleted, ["bx_setup_mac", "bx_setup_old"]);
    assert!(
        account
            .lock()
            .unwrap()
            .vms
            .iter()
            .any(|vm| vm["id"] == "bx_setup_box")
    );

    // Keys delete only their own VMs. One already gone deletes cleanly anyway (boat refuses
    // such a key with a 403 even then); one that's there and another key's stays refused.
    let theirs = server.boat.delete_sandbox("bx_stray").unwrap_err();
    assert_eq!(
        (theirs.status(), theirs.is("api_key_action_forbidden")),
        (Some(403), true)
    );
    account
        .lock()
        .unwrap()
        .vms
        .retain(|vm| vm["id"] != "bx_old");
    mac.delete("t-mac", None).unwrap();
    assert!(mac.store.thread("t-mac").unwrap().is_none());
}

/// A copy of a database made without `flotilla import` lists the original's threads, whose VMs
/// are another Flotilla's: its sync calls them gone. Deleting one there only forgets it, even
/// with the same boat key, which could delete the VM; so does removing a project. The owner's
/// delete still deletes.
#[test]
fn deleting_a_thread_whose_vm_is_another_flotillas_leaves_the_vm() {
    let (base, account) = boat(vec![thread_vm("bx_box", "t-box", Some("srv_box"), BOX_KEY)]);
    let mut copy = core(&base, "srv_copy", BOX_KEY);
    copy.store
        .insert_thread(&thread("t-box", Some("bx_box")))
        .unwrap();
    assert_eq!(copy.full_sync().unwrap().expired, ["t-box"]);
    copy.delete("t-box", None).unwrap();
    assert!(copy.store.thread("t-box").unwrap().is_none());
    // Nor does removing a project there delete the VM the project was set up in.
    account
        .lock()
        .unwrap()
        .vms
        .push(setup_vm("bx_setup_box", "p-box", Some("srv_box"), BOX_KEY));
    copy.store
        .insert_project(
            &Project {
                template_sandbox_id: Some("bx_setup_box".into()),
                ..project("p-box")
            },
            &[],
        )
        .unwrap();
    flotilla_core::projects::remove_project(&copy.boat, &copy.store, "p-box").unwrap();
    assert!(account.lock().unwrap().deleted.is_empty());
    assert_eq!(account.lock().unwrap().vms.len(), 2);
    let mut server = core(&base, "srv_box", BOX_KEY);
    server
        .store
        .insert_thread(&thread("t-box", Some("bx_box")))
        .unwrap();
    server.delete("t-box", None).unwrap();
    assert_eq!(account.lock().unwrap().deleted, ["bx_box"]);
}

/// A thread that gets its VM while a sync is reading the list (a hand-off's child, a sibling
/// started from the same prompt) isn't in that list, and isn't called gone for it.
#[test]
fn a_vm_made_while_the_list_is_read_isnt_called_gone() {
    let dir = std::env::temp_dir().join(format!("flotilla-sync-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let (base, account) = boat(vec![]);
    let store = Store::open(&db).unwrap();
    store.kv_set(SERVER_ID_KEY, "srv_box").unwrap();
    store
        .insert_thread(&Thread {
            state: ThreadState::Creating,
            ..thread("t-new", None)
        })
        .unwrap();
    // A VM that went before the sync started is still called gone.
    store
        .insert_thread(&thread("t-old", Some("bx_deleted")))
        .unwrap();
    // As `launch_thread` does once boat has made the VM: on its own connection.
    let at = db.clone();
    account.lock().unwrap().on_list = Some(Box::new(move || {
        let store = Store::open(&at).unwrap();
        let t = store.thread("t-new").unwrap().unwrap();
        store
            .update_thread(&Thread {
                sandbox_id: Some("bx_new0".into()),
                ..t
            })
            .unwrap();
    }));
    let mut lives = std::collections::HashMap::new();
    let boat =
        BoatClient::with_base(BOX_KEY.into(), &base, &format!("{base}/provider-models")).unwrap();
    let r = flotilla_core::sync::full_sync(&boat, &store, &mut lives).unwrap();
    assert_eq!(r.expired, ["t-old"]);
    assert_eq!(
        store.thread("t-new").unwrap().unwrap().state,
        ThreadState::Creating
    );
    let _ = std::fs::remove_dir_all(&dir);
}
