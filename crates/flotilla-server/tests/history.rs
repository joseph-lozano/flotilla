//! A long thread's older pages and cut strings over HTTP (`flotilla_proto::history`): a
//! device's token over TCP, none over the Unix socket, and what's refused: no token, a wrong
//! or revoked one, a web page, a page that isn't, a string that changed.

use flotilla_client::Target;
use flotilla_core::event::RawEvent;
use flotilla_core::store::{Store, Thread, ThreadState};
use flotilla_engine::host::{Mode, Role};
use flotilla_engine::views::{self, Live};
use flotilla_proto::history::{CAP, PAGE};
use flotilla_proto::{EngineState, GhStatus, Secret, SyncStatus};
use flotilla_server::{Config, Listen, Server};
use serde_json::{Value, json};
use std::sync::Arc;

/// A raw HTTP/1.1 request: its status line.
fn get(addr: &str, head: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    s.write_all(head.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn thread(id: &str) -> Thread {
    Thread {
        id: id.into(),
        environment_id: Some("e1".into()),
        environment_name: Some("base".into()),
        title: "A long thread".into(),
        sandbox_id: Some(format!("bx_{id}")),
        conversation_id: Some(format!("c_{id}")),
        provider: "claude-code".into(),
        model: "claude-haiku-5-5".into(),
        effort: None,
        fast: false,
        state: ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: "2026-10-31T10:00:00.000Z".into(),
        updated_at: "2026-10-31T10:00:00.000Z".into(),
        repo_project_id: None,
        branch: None,
        base_sha: None,
    }
}

/// `turns` turns of a prompt and a reply, the fifth with a tool result over the cap.
fn long_thread(store: &Store, id: &str, turns: usize) {
    let mut events = Vec::new();
    for k in 0..turns {
        let task = format!("task-{k}");
        let ts = 1_793_000_000_000i64 + k as i64 * 10_000;
        events.push(RawEvent::from_value(json!({"id": format!("p{k}"), "taskId": task, "type": "prompt", "timestamp": ts, "conversationId": "c", "data": {"prompt": format!("prompt {k}"), "status": "finished"}})));
        let mut data = json!({"content": format!("reply {k}"), "model": "m"});
        if k == 5 {
            let result = json!({"type": "bash_output", "command": "cat big.log", "content": "日本語 log line ✓\n".repeat(10_000)}).to_string();
            data["tools"] = json!([{"use": {"id": "toolu_big", "name": "Bash", "input": {"command": "cat big.log"}, "type": "tool_use"}, "result": {"content": result, "is_error": false, "tool_use_id": "toolu_big", "type": "tool_result"}}]);
        }
        events.push(RawEvent::from_value(json!({"id": format!("r{k}"), "taskId": task, "type": "response", "timestamp": ts + 1000, "conversationId": "c", "data": data})));
    }
    store.apply_events(id, &events, false).unwrap();
    store.rebuild_messages(id).unwrap();
}

#[test]
fn older_pages_and_whole_strings_need_what_everything_else_needs() {
    let dir = std::env::temp_dir().join(format!("flotilla-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let store = Store::open(&db).unwrap();
    store.insert_thread(&thread("t1")).unwrap();
    long_thread(&store, "t1", 130);
    let now = flotilla_core::util::now_ms();
    let pair = |name: &str| {
        let code = store
            .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
            .unwrap();
        store
            .pair_device(&code.code, name, "ios", None, now)
            .unwrap()
    };
    let phone = pair("phone");
    let other = pair("other");
    let token = Secret(phone.token.clone());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let socket = dir.join("run").join("server.sock");
    let listen = vec![
        Listen::tcp("127.0.0.1", 0, false).unwrap(),
        Listen::Unix(socket.clone()),
    ];
    let server = rt
        .block_on(Server::bind(Config {
            db: db.clone(),
            listen,
            mode: Mode::Fake(Arc::new(|_| {})),
            role: Role::Server,
        }))
        .unwrap();
    let addr = server.tcp().unwrap().to_string();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));
    let tcp = Target::parse(&format!("ws://{addr}")).unwrap();
    let unix = Target::Unix(socket.clone());

    // The view: its newest pages, and the big result cut.
    let live = Live {
        status: SyncStatus::default(),
        github: GhStatus::Unknown,
        usage: None,
        engine: EngineState::Running,
    };
    let side = views::sidebar(&store, &live).unwrap();
    let view = views::thread(&store, "t1", &side.threads, &side.parents).unwrap();
    assert_eq!(view.older, 150);
    assert_eq!(view.pages.len(), 6);

    // Each older page, with a token over TCP and with none over the socket: what the view's
    // marks say, and together every message before the view.
    let mut older = Vec::new();
    for page in 0..view.older / PAGE {
        let before = (page + 1) * PAGE;
        let p = flotilla_client::history(&tcp, Some(&token), "t1", before).unwrap();
        assert_eq!(p.page, page);
        assert_eq!(p.mark, view.pages[page as usize].mark);
        assert_eq!(
            p,
            flotilla_client::history(&unix, None, "t1", before).unwrap()
        );
        older.extend(p.messages);
    }
    assert_eq!(
        older.iter().map(|m| m.seq).collect::<Vec<_>>(),
        (0..150).collect::<Vec<_>>()
    );
    let big = older
        .iter()
        .find(|m| !m.cut.is_empty())
        .expect("the big result, cut")
        .clone();
    let result = big.tool_result_json.clone().unwrap();
    assert!(result.len() <= CAP);
    let parsed: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["command"], "cat big.log");
    assert!(parsed["content"].as_str().unwrap().ends_with('…'));

    // The whole of it, by its mark, either way in.
    let whole = flotilla_client::full(&tcp, Some(&token), "t1", big.seq, &big.cut[0]).unwrap();
    assert_eq!(whole.len() as u64, big.cut[0].bytes);
    assert_eq!(
        flotilla_proto::history::string_mark(&whole),
        big.cut[0].mark
    );
    assert_eq!(
        flotilla_client::full(&unix, None, "t1", big.seq, &big.cut[0]).unwrap(),
        whole
    );
    let mut stale = big.cut[0].clone();
    stale.mark = "0123456789abcdef".into();
    let e = flotilla_client::full(&tcp, Some(&token), "t1", big.seq, &stale).unwrap_err();
    assert!(e.contains("changed"), "{e}");

    // Refused over TCP without a token, or a wrong one.
    for t in [
        None,
        Some(Secret(
            "fdt_0123456789abcdef_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        )),
    ] {
        let e = flotilla_client::history(&tcp, t.as_ref(), "t1", 50).unwrap_err();
        assert!(e.contains("isn't paired"), "{e}");
        let e = flotilla_client::full(&tcp, t.as_ref(), "t1", big.seq, &big.cut[0]).unwrap_err();
        assert!(e.contains("isn't paired"), "{e}");
    }
    let status = get(
        &addr,
        &format!(
            "GET /v1/threads/t1/history?before=50 HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert!(status.contains("401"), "{status}");
    // A web page, even with a token.
    let status = get(
        &addr,
        &format!(
            "GET /v1/threads/t1/history?before=50 HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://evil.example\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
            token.0
        ),
    );
    assert!(status.contains("403"), "{status}");
    // Pages that aren't: not on a page boundary, none, past the end, another thread.
    for before in ["0", "49", "-50", "x", "100000"] {
        let status = get(
            &addr,
            &format!(
                "GET /v1/threads/t1/history?before={before} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                token.0
            ),
        );
        assert!(
            status.contains(if before == "100000" { "404" } else { "400" }),
            "{before}: {status}"
        );
    }
    let status = get(
        &addr,
        &format!(
            "GET /v1/threads/t1/history HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
            token.0
        ),
    );
    assert!(status.contains("400"), "{status}");
    let e = flotilla_client::history(&tcp, Some(&token), "nope", 50).unwrap_err();
    assert!(e.contains("no page"), "{e}");

    // Revoked: refused at once, and the other device still reads.
    assert!(store.revoke_device(&phone.device).unwrap());
    let e = flotilla_client::history(&tcp, Some(&token), "t1", 50).unwrap_err();
    assert!(e.contains("isn't paired"), "{e}");
    assert!(flotilla_client::history(&tcp, Some(&Secret(other.token.clone())), "t1", 50).is_ok());

    let _ = stop.send(());
    rt.block_on(done).unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
