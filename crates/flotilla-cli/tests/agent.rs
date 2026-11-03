//! An assistant's way in, end to end with the real binary: `flotilla serve`, an agent code
//! from `flotilla pair --agent`, `flotilla connect` on the assistant's side (its token in a
//! private file, never printed), the thread commands through `--server` (and the server
//! refusing what an agent may not do), `flotilla mcp --server` over stdio, `flotilla devices`
//! saying which is an agent, and `flotilla disconnect` signing it out.

use flotilla_core::store::{Store, Thread, ThreadState};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Serving {
    child: Child,
    dir: PathBuf,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn flotilla() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flotilla"))
}

/// Run it; its stdout, and stderr, and whether it worked.
fn run(args: &[&str]) -> (String, String, bool) {
    let out = flotilla().args(args).stdin(Stdio::null()).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn thread() -> Thread {
    Thread {
        id: "cccc3333-0000-4000-8000-000000000003".into(),
        environment_id: Some("e1".into()),
        environment_name: Some("base".into()),
        title: "Tidy the README".into(),
        sandbox_id: Some("bx_c".into()),
        conversation_id: None,
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

fn serve(dir: &Path) -> (Serving, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = flotilla()
        .args([
            "serve",
            "--offline",
            "--port",
            &port.to_string(),
            "--data-dir",
        ])
        .arg(dir.join("server"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let serving = Serving {
        child,
        dir: dir.to_path_buf(),
    };
    // Listening, and its address kept for `flotilla pair` (written just after it binds).
    let db = dir.join("server").join("flotilla.db");
    let saved = || {
        Store::open(&db)
            .ok()
            .and_then(|s| {
                s.kv_get(flotilla_engine::host::SERVER_URL_KEY)
                    .ok()
                    .flatten()
            })
            .is_some()
    };
    let until = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() || !saved() {
        assert!(Instant::now() < until, "the server didn't start");
        std::thread::sleep(Duration::from_millis(100));
    }
    (serving, format!("http://127.0.0.1:{port}"))
}

#[test]
fn an_assistant_connects_as_an_agent_and_is_held_to_it() {
    let dir = std::env::temp_dir().join(format!("flotilla-cli-agent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("server")).unwrap();
    Store::open(&dir.join("server").join("flotilla.db"))
        .unwrap()
        .insert_thread(&thread())
        .unwrap();
    let (_serving, url) = serve(&dir);
    let server_dir = dir.join("server").display().to_string();
    // The server's side: a code that pairs only an agent.
    let (out, err, ok) = run(&["pair", "--agent", "--data-dir", &server_dir]);
    assert!(ok, "{out}{err}");
    let code = out
        .lines()
        .find_map(|l| l.strip_prefix("Agent pairing code: "))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .unwrap();
    assert!(
        out.contains(&format!("flotilla connect {url} {code}")),
        "{out}"
    );
    // The assistant's side.
    let secrets = dir.join("assistant-secrets").display().to_string();
    let client = dir.join("assistant").display().to_string();
    let mine = |rest: &[&str]| -> Vec<String> {
        ["--data-dir", &client, "--secrets-dir", &secrets]
            .iter()
            .chain(rest)
            .map(|s| s.to_string())
            .collect()
    };
    let args = mine(&["connect", &url, &code, "--name", "Test assistant"]);
    let (out, err, ok) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(ok, "{out}{err}");
    assert!(
        out.contains("as an agent device")
            && out.contains(&format!("mcp --server {url}"))
            && out.contains(&format!("--secrets-dir {secrets}")),
        "{out}"
    );
    let kept: Vec<_> = std::fs::read_dir(&secrets)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(kept.len(), 1);
    let token = std::fs::read_to_string(kept[0].path()).unwrap();
    assert!(
        token.starts_with("fdt_") && !out.contains(token.trim()) && !err.contains(token.trim())
    );
    // Thread commands through --server: reading works, deleting is refused by the server.
    let args = mine(&["--server", &url, "threads"]);
    let (out, err, ok) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(ok && out.contains("Tidy the README"), "{out}{err}");
    let args = mine(&["--server", &url, "delete", "cccc3333", "--yes"]);
    let (out, err, ok) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(
        !ok && err.contains("Agents can't delete the thread"),
        "{out}{err}"
    );
    assert!(
        Store::open(&dir.join("server").join("flotilla.db"))
            .unwrap()
            .thread("cccc3333-0000-4000-8000-000000000003")
            .unwrap()
            .is_some()
    );
    // MCP over stdio.
    let args = mine(&["mcp", "--server", &url]);
    let mut mcp = flotilla()
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = mcp.stdin.take().unwrap();
        for msg in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ] {
            writeln!(stdin, "{msg}").unwrap();
        }
        let mut out = std::io::BufReader::new(mcp.stdout.take().unwrap());
        let mut replies = vec![];
        let mut line = String::new();
        while replies.len() < 2 && out.read_line(&mut line).unwrap() > 0 {
            replies.push(serde_json::from_str::<serde_json::Value>(line.trim()).unwrap());
            line.clear();
        }
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"list_threads","arguments":{{}}}}}}"#).unwrap();
        out.read_line(&mut line).unwrap();
        let listed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        drop(stdin);
        assert!(
            replies
                .iter()
                .any(|r| r["id"] == 1 && r["result"]["serverInfo"]["name"] == "flotilla"),
            "{replies:?}"
        );
        assert!(
            replies.iter().any(|r| r["id"] == 2
                && r["result"]["tools"]
                    .as_array()
                    .is_some_and(|t| t.len() == 12)),
            "{replies:?}"
        );
        assert!(
            listed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("cccc3333  idle      Tidy the README"),
            "{listed}"
        );
    }
    let until = Instant::now() + Duration::from_secs(10);
    while mcp.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < until,
            "flotilla mcp didn't end with its stdin"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Requests piped in, then stdin closed at once: each is still answered before it ends.
    let mut mcp = flotilla()
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = mcp.stdin.take().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"get_thread","arguments":{{"thread":"cccc3333"}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"delete_thread","arguments":{{}}}}}}"#).unwrap();
    }
    let out = mcp.wait_with_output().unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        replies.iter().any(|r| r["id"] == 1
            && r["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|t| t.contains("Tidy the README"))),
        "{replies:?}"
    );
    assert!(
        replies
            .iter()
            .any(|r| r["id"] == 2 && r["error"]["code"] == -32602),
        "{replies:?}"
    );
    // The server's list says which device is the agent.
    let (out, _, _) = run(&["devices", "--data-dir", &server_dir]);
    assert!(
        out.lines()
            .any(|l| l.contains("Test assistant") && l.contains("agent")),
        "{out}"
    );
    // Signed out: the token's gone here, and the device there.
    let args = mine(&["disconnect", &url]);
    let (out, err, ok) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(ok && out.contains("Signed out"), "{out}{err}");
    assert_eq!(std::fs::read_dir(&secrets).unwrap().count(), 0);
    let (out, _, _) = run(&["devices", "--data-dir", &server_dir]);
    assert!(!out.contains("Test assistant"), "{out}");
    let args = mine(&["--server", &url, "threads"]);
    let (_, err, ok) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(!ok && err.contains("flotilla connect"), "{err}");
}
