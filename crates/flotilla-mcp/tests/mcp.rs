//! `flotilla mcp` against a real server with a stand-in engine that plays each turn out at
//! once (the prompt in, a reply out): the MCP protocol itself (initialize, tools/list,
//! tools/call, its errors), every tool's happy path as an agent device over the network, and on
//! this machine's socket, where the server holds it to an agent's scope too. Then remote MCP,
//! the server's own listener: streamable HTTP and its sessions, who it lets in, OAuth with a
//! stand-in client, and that it serves nothing of the main API.

use flotilla_core::event::RawEvent;
use flotilla_core::store::{Store, Thread, ThreadState};
use flotilla_engine::host::{Mode, Role};
use flotilla_engine::{Action, Changes, Cmd, Event, Events, Outcome};
use flotilla_mcp::{Door, Link, Mcp};
use flotilla_proto::{Scope, Secret};
use flotilla_server::{Config, Listen, Server};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

struct World {
    rt: tokio::runtime::Runtime,
    dir: PathBuf,
    db: PathBuf,
    url: String,
    socket: PathBuf,
    /// The agent device's token.
    token: Secret,
    /// Where remote MCP listens.
    mcp: String,
    /// What the stand-in engine was asked to do.
    acts: Arc<Mutex<Vec<Action>>>,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    done: Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>,
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop_server();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn thread(id: &str, title: &str) -> Thread {
    Thread {
        id: id.into(),
        environment_id: Some("e1".into()),
        environment_name: Some("base".into()),
        title: title.into(),
        sandbox_id: Some(format!("bx_{id}")),
        conversation_id: Some("c".into()),
        provider: "claude-code".into(),
        model: "claude-haiku-4-5".into(),
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

/// A finished turn: the prompt in, a reply out (with a tool call when `tool` says).
fn turn(
    store: &Store,
    id: &str,
    task: &str,
    ts: i64,
    prompt: &str,
    reply: &str,
    tool: Option<Value>,
) {
    let mut data = json!({"content": reply, "model": "m"});
    if let Some(t) = tool {
        data["tools"] = json!([t]);
    }
    let events = [
        RawEvent::from_value(
            json!({"id": format!("p-{task}"), "taskId": task, "type": "prompt", "timestamp": ts, "conversationId": "c", "data": {"prompt": prompt, "status": "finished"}}),
        ),
        RawEvent::from_value(
            json!({"id": format!("r-{task}"), "taskId": task, "type": "response", "timestamp": ts + 1, "conversationId": "c", "data": data}),
        ),
    ];
    store.apply_events(id, &events, false).unwrap();
    for mut t in store.unsettled_turns(id).unwrap() {
        (t.done, t.settled) = (true, true);
        store.update_turn(&t).unwrap();
    }
    store.rebuild_messages(id).unwrap();
}

fn bash(id: &str, command: &str, out: &str) -> Value {
    let result = json!({"type": "bash_output", "command": command, "content": out}).to_string();
    json!({"use": {"id": id, "name": "Bash", "input": {"command": command}, "type": "tool_use"}, "result": {"content": result, "is_error": false, "tool_use_id": id, "type": "tool_result"}})
}

/// What the stand-in engine does with a prompt: the turn, over at once.
fn play(db: &Path, thread: &str, local: Option<&str>, text: &str) {
    let store = Store::open(db).unwrap();
    if let Some(l) = local {
        let _ = store.delete_sent_prompt(thread, l);
    }
    store.set_thread_state(thread, ThreadState::Idle).unwrap();
    let task = uuid::Uuid::new_v4().to_string();
    turn(
        &store,
        thread,
        &task,
        flotilla_core::util::now_ms(),
        text,
        &format!("Done: {text}"),
        None,
    );
}

fn start(name: &str) -> World {
    start_with(name, |_| {})
}

/// `start`, with `before` given the database first.
fn start_with(name: &str, before: impl FnOnce(&Store)) -> World {
    let dir = std::env::temp_dir().join(format!("flotilla-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let store = Store::open(&db).unwrap();
    let catalog = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../m0-samples/provider-models.json"
    ))
    .unwrap();
    store.kv_set("catalog", &catalog).unwrap();
    let env = |id: &str, name: &str| flotilla_core::boat::Environment {
        id: id.into(),
        name: name.into(),
        is_default: id == "e1",
    };
    store
        .replace_environments(&[env("e1", "base"), env("e2", "flotilla-web")])
        .unwrap();
    let project = flotilla_core::store::Project {
        id: "p1".into(),
        owner: "acme".into(),
        repo: "web".into(),
        default_branch: "main".into(),
        environment_name: "flotilla-web".into(),
        created_at: "2026-10-31T10:00:00.000Z".into(),
        template_sandbox_id: None,
        template_sha: None,
        template_built_at: None,
        prev_template_sandbox_id: None,
        head_sha: None,
        head_etag: None,
        checked_at: None,
        machine_type: None,
    };
    store.insert_project(&project, &[]).unwrap();
    // A long thread: 130 turns, so its oldest pages come over HTTP; turn 5 ran a command with
    // output over the cap, cut on the wire.
    store
        .insert_thread(&thread(
            "aaaa1111-0000-4000-8000-000000000001",
            "Fix the login redirect",
        ))
        .unwrap();
    let t1 = "aaaa1111-0000-4000-8000-000000000001";
    for k in 0..130 {
        let tool = match k {
            5 => Some(bash(
                "toolu_big",
                "cat big.log",
                &"log line ✓\n".repeat(10_000),
            )),
            7 => Some(bash("toolu_test", "npm test", "12 passed")),
            _ => None,
        };
        turn(
            &store,
            t1,
            &format!("task-{k}"),
            1_793_000_000_000 + k * 10_000,
            &format!("prompt {k}"),
            &format!("reply {k}"),
            tool,
        );
    }
    store.mark_viewed(t1).unwrap();
    // A thread whose agent waits on an answer.
    let t2 = "bbbb2222-0000-4000-8000-000000000002";
    store.insert_thread(&thread(t2, "Make a file")).unwrap();
    let at = flotilla_core::util::now_ms();
    let shown = "Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt";
    turn(
        &store,
        t2,
        "ask-1",
        at - 5000,
        "Ask me, then make it.",
        &format!("{shown}\nWhich one?"),
        None,
    );
    let q = flotilla_proto::ask::Question {
        header: None,
        question: "Which file name should I create?".into(),
        options: vec![
            flotilla_proto::ask::Choice {
                label: "alpha.txt (recommended)".into(),
                description: None,
            },
            flotilla_proto::ask::Choice {
                label: "beta.txt".into(),
                description: None,
            },
        ],
        multi: false,
    };
    let asked = flotilla_core::asks::Asked {
        id: "call_1".into(),
        at_ms: at,
        text: shown.into(),
        tool: flotilla_core::asks::CODEX_TOOL.into(),
        questions: vec![q],
        prompt: None,
        followed: false,
        call: None,
        turn: None,
    };
    flotilla_core::asks::store(&store, t2, &[asked]).unwrap();
    store.rebuild_messages(t2).unwrap();
    // The assistant's device, paired as an agent.
    let now = flotilla_core::util::now_ms();
    let code = store
        .mint_code_as(flotilla_core::devices::CODE_TTL, "cli", Scope::Agent, now)
        .unwrap();
    let token = Secret(
        store
            .pair_device(&code.code, "Claude Code", "macos", None, now)
            .unwrap()
            .token,
    );
    before(&store);
    drop(store);

    let events: Arc<OnceLock<Events>> = Arc::default();
    let acts: Arc<Mutex<Vec<Action>>> = Arc::default();
    let (to_host, seen, at) = (events.clone(), acts.clone(), db.clone());
    let mode = Mode::Fake(Arc::new(move |cmd| {
        let Cmd::Act { action, req } = cmd else {
            return;
        };
        seen.lock().unwrap().push(action.clone());
        let Some(events) = to_host.get() else { return };
        let (thread, outcome) = match &action {
            Action::Launch { thread, req, local } => {
                play(&at, thread, Some(local), &req.prompt);
                (Some(thread.clone()), Outcome::Done)
            }
            Action::Send {
                thread,
                text,
                local,
                ..
            } => {
                play(&at, thread, local.as_deref(), text);
                (
                    Some(thread.clone()),
                    Outcome::Sent(flotilla_proto::Sent::Prompt("p".into())),
                )
            }
            Action::Preview(thread, port) => (
                Some(thread.clone()),
                Outcome::Url(format!("https://{port}-bx.boat.example/?_token=preview")),
            ),
            _ => (None, Outcome::Done),
        };
        if let Some(t) = thread {
            let _ = events.send_blocking(Event::Changed(Changes::thread(&t).with_sidebar()));
        }
        let _ = events.send_blocking(Event::Done {
            action,
            result: Ok(outcome),
            req,
        });
    }));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let socket = dir.join("run").join("s.sock");
    let listen = vec![
        Listen::tcp("127.0.0.1", 0, false).unwrap(),
        Listen::Unix(socket.clone()),
        Listen::Mcp("127.0.0.1:0".parse().unwrap(), None),
    ];
    let server = rt
        .block_on(Server::bind(Config {
            db: db.clone(),
            listen,
            mode,
            role: Role::Server,
        }))
        .unwrap();
    let _ = events.set(server.host().events());
    let url = format!("http://{}", server.tcp().unwrap());
    let mcp = format!("http://{}", server.mcp().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));
    World {
        rt,
        dir,
        db,
        url,
        socket,
        token,
        mcp,
        acts,
        stop: Mutex::new(Some(stop)),
        done: Mutex::new(Some(done)),
    }
}

impl World {
    /// Stop the server as it stops when asked, and wait for it.
    fn stop_server(&self) {
        if let Some(stop) = self.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
        if let Some(done) = self.done.lock().unwrap().take() {
            let _ = self.rt.block_on(done);
        }
    }

    /// The MCP server as an agent device, over the network.
    fn remote(&self) -> Mcp {
        Mcp::new(Link::connect(
            Door {
                target: flotilla_client::Target::parse(&self.url).unwrap(),
                token: Some(self.token.clone()),
            },
            "flotilla-mcp-test",
        ))
    }

    /// The MCP server on this machine's socket.
    fn local(&self) -> Mcp {
        Mcp::new(Link::connect(
            Door {
                target: flotilla_client::Target::Unix(self.socket.clone()),
                token: None,
            },
            "flotilla-mcp-test",
        ))
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn s(v: &str) -> String {
    v.to_string()
}

fn rpc(mcp: &Mcp, method: &str, params: Value) -> Value {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    mcp.handle_value(
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        &|_| {},
    )
    .expect("a reply")
}

/// A tool's text, and whether it says it failed.
fn tool(mcp: &Mcp, name: &str, args: Value) -> (String, bool) {
    let r = rpc(mcp, "tools/call", json!({"name": name, "arguments": args}));
    let result = &r["result"];
    assert!(result.is_object(), "{name}: {r}");
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"].as_bool().unwrap(),
    )
}

/// A tool's text, which must not be a failure.
fn ok(mcp: &Mcp, name: &str, args: Value) -> String {
    let (text, failed) = tool(mcp, name, args.clone());
    assert!(!failed, "{name} {args}: {text}");
    text
}

#[test]
fn the_protocol_initializes_lists_its_tools_and_says_what_went_wrong() {
    let w = start("protocol");
    let mcp = w.remote();
    let init = rpc(
        &mcp,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "flotilla");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("wait_for_turn")
    );
    // A version it doesn't speak: it answers with its newest.
    let other = rpc(&mcp, "initialize", json!({"protocolVersion": "2099-01-01"}));
    assert_eq!(
        other["result"]["protocolVersion"],
        flotilla_mcp::PROTOCOL_VERSIONS[0]
    );
    let older = rpc(&mcp, "initialize", json!({"protocolVersion": "2024-11-05"}));
    assert_eq!(older["result"]["protocolVersion"], "2024-11-05");
    // Notifications and responses get no reply.
    assert_eq!(
        mcp.handle(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &|_| {}
        ),
        None
    );
    assert_eq!(
        mcp.handle(r#"{"jsonrpc":"2.0","id":99,"result":{}}"#, &|_| {}),
        None
    );
    assert_eq!(rpc(&mcp, "ping", json!({}))["result"], json!({}));
    let tools = rpc(&mcp, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 12, "{names:?}");
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert!(t["annotations"]["readOnlyHint"].is_boolean(), "{t}");
    }
    // Errors: JSON-RPC's codes.
    let code = |v: Value| v["error"]["code"].as_i64();
    assert_eq!(
        code(mcp.handle("{not json", &|_| {}).unwrap()),
        Some(-32700)
    );
    assert_eq!(code(mcp.handle("[]", &|_| {}).unwrap()), Some(-32600));
    assert_eq!(
        code(mcp.handle(r#"{"jsonrpc":"2.0","id":1}"#, &|_| {}).unwrap()),
        Some(-32600)
    );
    assert_eq!(code(rpc(&mcp, "resources/list", json!({}))), Some(-32601));
    assert_eq!(
        code(rpc(
            &mcp,
            "tools/call",
            json!({"name": "delete_thread", "arguments": {}})
        )),
        Some(-32602)
    );
    assert_eq!(
        code(rpc(&mcp, "tools/call", json!({"arguments": {}}))),
        Some(-32602)
    );
    assert_eq!(
        code(rpc(
            &mcp,
            "tools/call",
            json!({"name": "get_thread", "arguments": "t1"})
        )),
        Some(-32602)
    );
    // A tool that can't do what it's asked says why, as its result.
    let (why, failed) = tool(&mcp, "get_thread", json!({"thread": "zzzz9999"}));
    assert!(failed && why.contains("No thread"), "{why}");
    let (why, failed) = tool(&mcp, "get_thread", json!({}));
    assert!(failed && why.contains("thread"), "{why}");
    let (why, failed) = tool(
        &mcp,
        "start_thread",
        json!({"prompt": "x", "model": "no-such-model"}),
    );
    assert!(failed && why.contains("list_models"), "{why}");
    let (why, failed) = tool(
        &mcp,
        "read_thread",
        json!({"thread": "aaaa1111", "limit": 0}),
    );
    assert!(failed && why.contains("limit"), "{why}");
}

#[test]
fn every_tool_works_for_an_agent_device() {
    let w = start("tools");
    let mcp = w.remote();
    // Listing and searching.
    let list = ok(&mcp, "list_threads", json!({}));
    assert!(
        list.contains("aaaa1111  idle")
            && list.contains("Fix the login redirect")
            && list.contains("reply 129"),
        "{list}"
    );
    assert!(
        list.contains("bbbb2222") && list.contains("Make a file"),
        "{list}"
    );
    let found = ok(&mcp, "list_threads", json!({"query": "login redirect"}));
    assert!(found.contains("aaaa1111"), "{found}");
    let found = ok(&mcp, "list_threads", json!({"query": "reply 77"}));
    assert!(
        found.contains("aaaa1111") && found.contains("[seq"),
        "{found}"
    );
    // Status, with the question waiting.
    let card = ok(&mcp, "get_thread", json!({"thread": "bbbb2222"}));
    assert!(
        card.contains("waiting on your answer")
            && card.contains("1. Which file name should I create?")
            && card.contains("alpha.txt (recommended) | beta.txt"),
        "{card}"
    );
    assert!(card.contains("project: none"), "{card}");
    // Reading: the newest first page, tool calls folded, then back a page over HTTP, then one
    // message whole (its output fetched whole, then cut to what an assistant can take).
    let read = ok(
        &mcp,
        "read_thread",
        json!({"thread": "aaaa1111", "limit": 6}),
    );
    assert!(
        read.contains("messages 25") || read.contains("of 26"),
        "{read}"
    );
    assert!(
        read.contains("agent: reply 129") && read.contains("you: prompt 129"),
        "{read}"
    );
    let older = ok(
        &mcp,
        "read_thread",
        json!({"thread": "aaaa1111", "before": 20, "limit": 8}),
    );
    assert!(
        older.contains("messages 12–19")
            && older.contains("tool Bash: ")
            && older.contains("before=12"),
        "{older}"
    );
    let tool_seq: i64 = older
        .lines()
        .find(|l| l.contains("tool Bash"))
        .and_then(|l| l.trim_start_matches('[').split(']').next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    let whole = ok(
        &mcp,
        "read_thread",
        json!({"thread": "aaaa1111", "seq": tool_seq}),
    );
    assert!(
        whole.contains("tool: Bash")
            && whole.contains("cat big.log")
            && whole.contains("more characters"),
        "{}",
        &whole[..300]
    );
    assert!(
        whole.len() > 30_000 && whole.len() < 60_000,
        "{}",
        whole.len()
    );
    // What a thread can start with.
    let projects = ok(&mcp, "list_projects", json!({}));
    assert!(
        projects.contains("acme/web") && projects.contains("(no project): a blank VM (base)"),
        "{projects}"
    );
    let all = ok(&mcp, "list_models", json!({}));
    assert!(
        all.starts_with("codex (default ")
            && all.contains("\nclaude-code (default sonnet): ")
            && all.contains("more (list_models provider=pi)"),
        "{all}"
    );
    assert!(all.len() < 6000, "{}", all.len());
    let models = ok(&mcp, "list_models", json!({"provider": "claude-code"}));
    assert!(
        models.starts_with("claude-code")
            && !models.contains("\ncodex")
            && models.contains("effort: "),
        "{models}"
    );
    let model = s("claude-haiku-5-5");
    assert!(models.contains("  claude-haiku-5-5"), "{models}");
    // Start a thread, wait for its turn, follow up and wait again.
    let started = ok(
        &mcp,
        "start_thread",
        json!({"prompt": "Write a haiku about VMs", "model": model}),
    );
    // Several harnesses run it: claude-code, listed first of them.
    assert!(
        started.starts_with("Started thread ")
            && started.contains("on claude-code claude-haiku-5-5 (no project)"),
        "{started}"
    );
    let id = started
        .trim_start_matches("Started thread ")
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    let reply = ok(
        &mcp,
        "wait_for_turn",
        json!({"thread": id, "timeout_seconds": 30}),
    );
    assert!(
        reply.contains("turn has ended: idle") && reply.contains("Done: Write a haiku about VMs"),
        "{reply}"
    );
    let sent = ok(
        &mcp,
        "send_message",
        json!({"thread": id, "text": "Now one about boats"}),
    );
    assert!(sent.contains("Sent to"), "{sent}");
    let reply = ok(
        &mcp,
        "wait_for_turn",
        json!({"thread": id, "timeout_seconds": 30}),
    );
    assert!(reply.contains("Done: Now one about boats"), "{reply}");
    // In a project.
    let started = ok(
        &mcp,
        "start_thread",
        json!({"prompt": "Fix the checks", "model": model, "project": "acme/web", "title": "Checks"}),
    );
    assert!(started.contains("(acme/web)"), "{started}");
    // Answer the question.
    let answered = ok(
        &mcp,
        "answer_question",
        json!({"thread": "bbbb2222", "answers": ["beta.txt"]}),
    );
    assert!(answered.contains("Answered"), "{answered}");
    let reply = ok(
        &mcp,
        "wait_for_turn",
        json!({"thread": "bbbb2222", "timeout_seconds": 30}),
    );
    assert!(
        reply.contains("Done: Which file name should I create?\nbeta.txt"),
        "{reply}"
    );
    // Interrupt, preview, archive.
    assert!(ok(&mcp, "interrupt_turn", json!({"thread": "aaaa1111"})).starts_with("Interrupted"));
    let preview = ok(
        &mcp,
        "open_preview",
        json!({"thread": "aaaa1111", "port": 3000}),
    );
    assert!(
        preview.contains("https://3000-bx.boat.example/"),
        "{preview}"
    );
    assert!(ok(&mcp, "archive_thread", json!({"thread": id})).starts_with("Archived"));
    // What the engine was asked: each in turn.
    let acts = w.acts.lock().unwrap();
    let kinds: Vec<&str> = acts
        .iter()
        .map(|a| match a {
            Action::Launch { req, .. } if req.project_id.as_deref() == Some("p1") => "launch in p1",
            Action::Launch { .. } => "launch",
            Action::Send { .. } => "send",
            Action::Interrupt(_) => "interrupt",
            Action::Preview(..) => "preview",
            Action::Archive(_) => "archive",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "launch",
            "send",
            "launch in p1",
            "send",
            "interrupt",
            "preview",
            "archive"
        ]
    );
}

/// On this machine's socket, as the CLI connects: the same tools, and reading a thread leaves
/// it unread for you, as an agent device's reading does.
#[test]
fn on_this_machines_socket_it_reads_without_marking_anything_read() {
    let w = start("local");
    let store = Store::open(&w.db).unwrap();
    store
        .mark_unread("aaaa1111-0000-4000-8000-000000000001")
        .unwrap();
    let mcp = w.local();
    let read = ok(&mcp, "read_thread", json!({"thread": "aaaa1111"}));
    assert!(read.contains("reply 129"), "{read}");
    assert!(ok(&mcp, "get_thread", json!({"thread": "aaaa1111"})).contains("status: idle, unread"));
    std::thread::sleep(std::time::Duration::from_millis(700));
    let row = store
        .thread_summaries()
        .unwrap()
        .into_iter()
        .find(|t| t.thread.id.starts_with("aaaa1111"))
        .unwrap();
    assert_eq!(row.activity, flotilla_proto::Activity::Unread);
    let started = ok(
        &mcp,
        "start_thread",
        json!({"prompt": "hello", "model": "claude-haiku-5-5", "provider": "claude-code"}),
    );
    assert!(started.starts_with("Started thread"), "{started}");
}

// Remote MCP: the server's own listener, over HTTP.

/// An HTTP client that follows no redirects (an authorization's answer is one).
fn web() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap()
}

/// One JSON-RPC message to `/mcp`: the response.
fn post(
    w: &World,
    token: Option<&str>,
    session: Option<&str>,
    msg: Value,
) -> reqwest::blocking::Response {
    let mut r = web()
        .post(format!("{}/mcp", w.mcp))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(msg.to_string());
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    if let Some(s) = session {
        r = r
            .header("mcp-session-id", s)
            .header("mcp-protocol-version", "2025-06-18");
    }
    r.send().unwrap()
}

/// A JSON-RPC answer, from JSON or the last event of a stream.
fn answer(r: reqwest::blocking::Response) -> Value {
    let streamed = r
        .headers()
        .get("content-type")
        .is_some_and(|t| t.to_str().unwrap().starts_with("text/event-stream"));
    let text = r.text().unwrap();
    if !streamed {
        return serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    }
    let last = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("data: "))
        .unwrap_or_else(|| panic!("no event: {text}"));
    serde_json::from_str(last).unwrap()
}

/// A session for `token`: its id.
fn session(w: &World, token: &str) -> String {
    let r = post(
        w,
        Some(token),
        None,
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
    );
    assert_eq!(r.status(), 200);
    let id = r
        .headers()
        .get("mcp-session-id")
        .expect("a session")
        .to_str()
        .unwrap()
        .to_string();
    let init = answer(r);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    let r = post(
        w,
        Some(token),
        Some(&id),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!(r.status(), 202);
    id
}

/// A tool's text over HTTP, which must not be a failure.
fn http_tool(w: &World, token: &str, session: &str, name: &str, args: Value) -> String {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let r = post(
        w,
        Some(token),
        Some(session),
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}}),
    );
    assert_eq!(r.status(), 200);
    let v = answer(r);
    assert_eq!(v["id"], id, "{v}");
    assert_eq!(v["result"]["isError"], false, "{name}: {v}");
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A device paired straight into the database: its id and token.
fn device(w: &World, name: &str, scope: Scope) -> (String, String) {
    let store = Store::open(&w.db).unwrap();
    let now = flotilla_core::util::now_ms();
    let code = store
        .mint_code_as(flotilla_core::devices::CODE_TTL, "cli", scope, now)
        .unwrap();
    let p = store
        .pair_device(&code.code, name, "linux", None, now)
        .unwrap();
    (p.device, p.token)
}

#[test]
fn remote_mcp_speaks_streamable_http_to_agent_devices_only() {
    let w = start("http");
    let token = w.token.0.clone();
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}});

    // No token, a wrong one, a full device's: refused, the first two saying where to sign in.
    let r = post(&w, None, None, init.clone());
    assert_eq!(r.status(), 401);
    let challenge = r
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        challenge,
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            w.mcp
        )
    );
    let r = post(
        &w,
        Some("fdt_0123456789abcdef_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        None,
        init.clone(),
    );
    assert_eq!(r.status(), 401);
    assert!(
        r.headers()
            .get("www-authenticate")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("error=\"invalid_token\"")
    );
    let (_, full) = device(&w, "My Mac", Scope::Full);
    let r = post(&w, Some(&full), None, init.clone());
    assert_eq!(r.status(), 403);
    assert!(r.text().unwrap().contains("flotilla pair --agent --token"));

    // An agent's: a session, its tools, a call answered as JSON or as a stream.
    let s1 = session(&w, &token);
    let r = post(
        &w,
        Some(&token),
        Some(&s1),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let tools = answer(r);
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 12);
    let list = http_tool(&w, &token, &s1, "list_threads", json!({}));
    assert!(list.contains("Fix the login redirect"), "{list}");
    let streamed = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_thread", "arguments": {"thread": "bbbb2222"}}}).to_string())
        .send()
        .unwrap();
    assert_eq!(streamed.headers()["content-type"], "text/event-stream");
    let text = streamed.text().unwrap();
    assert!(
        text.starts_with("event: message\ndata: {") && text.contains("waiting on your answer"),
        "{text}"
    );
    // JSON alone, when that's all the client takes.
    let plain = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "list_projects", "arguments": {}}}).to_string())
        .send()
        .unwrap();
    assert_eq!(plain.headers()["content-type"], "application/json");
    assert!(
        answer(plain)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("acme/web")
    );

    // A thread started and followed over HTTP, as an assistant in someone's cloud would.
    let started = http_tool(
        &w,
        &token,
        &s1,
        "start_thread",
        json!({"prompt": "Write a haiku about VMs", "model": "claude-haiku-5-5"}),
    );
    let id = started
        .trim_start_matches("Started thread ")
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    let reply = http_tool(
        &w,
        &token,
        &s1,
        "wait_for_turn",
        json!({"thread": id, "timeout_seconds": 30}),
    );
    assert!(reply.contains("Done: Write a haiku about VMs"), "{reply}");

    // It's an agent to the server: its reading marks nothing read.
    let store = Store::open(&w.db).unwrap();
    store
        .mark_unread("aaaa1111-0000-4000-8000-000000000001")
        .unwrap();
    assert!(
        http_tool(
            &w,
            &token,
            &s1,
            "read_thread",
            json!({"thread": "aaaa1111"})
        )
        .contains("reply 129")
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    let row = store
        .thread_summaries()
        .unwrap()
        .into_iter()
        .find(|t| t.thread.id.starts_with("aaaa1111"))
        .unwrap();
    assert_eq!(row.activity, flotilla_proto::Activity::Unread);

    // Sessions: none said, one unknown, another device's: each refused as MCP says.
    let call = json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"});
    assert_eq!(post(&w, Some(&token), None, call.clone()).status(), 400);
    assert_eq!(
        post(&w, Some(&token), Some("nope"), call.clone()).status(),
        404
    );
    let (other_id, other) = device(&w, "Muse", Scope::Agent);
    assert_eq!(
        post(&w, Some(&other), Some(&s1), call.clone()).status(),
        404
    );
    let s2 = session(&w, &other);
    assert_ne!(s1, s2);
    // What isn't MCP over HTTP.
    let bad_version = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("mcp-protocol-version", "1999-01-01")
        .header("content-type", "application/json")
        .body(call.to_string())
        .send()
        .unwrap();
    assert_eq!(bad_version.status(), 400);
    // No version said (as some clients send none on a reused session): taken as it is.
    let unversioned = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("content-type", "application/json")
        .body(call.to_string())
        .send()
        .unwrap();
    assert_eq!(unversioned.status(), 200);
    assert_eq!(
        post(&w, Some(&token), Some(&s1), json!([call.clone()])).status(),
        400
    );
    let not_json = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("content-type", "application/json")
        .body("{nope")
        .send()
        .unwrap();
    assert_eq!(not_json.status(), 400);
    assert_eq!(answer(not_json)["error"]["code"], -32700);
    let get = web()
        .get(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .send()
        .unwrap();
    assert_eq!(get.status(), 405);
    let page = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("origin", "https://evil.example")
        .header("content-type", "application/json")
        .body(init.to_string())
        .send()
        .unwrap();
    assert_eq!(page.status(), 403);
    // Too big: 413, or the connection closed on it before it was all sent.
    let huge = web()
        .post(format!("{}/mcp", w.mcp))
        .bearer_auth(&token)
        .header("mcp-session-id", &s1)
        .header("content-type", "application/json")
        .body(format!("{{\"x\":\"{}\"}}", "a".repeat(2 << 20)))
        .send();
    assert!(
        huge.as_ref()
            .map_or_else(|e| e.is_request(), |r| r.status() == 413),
        "{huge:?}"
    );

    // Bodies that never finish arriving hold no room in flight (more of them than it has): the
    // listener still answers at once.
    let held: Vec<_> = (0..140)
        .map(|_| slow(&w, "192.0.2.10", "/mcp", 1000, Some(&token)))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        web()
            .get(format!("{}/.well-known/oauth-protected-resource", w.mcp))
            .send()
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        post(&w, Some(&token), Some(&s1), call.clone()).status(),
        200
    );
    drop(held);

    // Ending a session; revoking a device ends its way in at once.
    let del = web()
        .delete(format!("{}/mcp", w.mcp))
        .bearer_auth(&other)
        .header("mcp-session-id", &s2)
        .send()
        .unwrap();
    assert_eq!(del.status(), 204);
    assert_eq!(
        post(&w, Some(&other), Some(&s2), call.clone()).status(),
        404
    );
    assert!(store.revoke_device(&other_id).unwrap());
    assert_eq!(post(&w, Some(&other), None, init.clone()).status(), 401);
    // The first device is still in.
    assert_eq!(post(&w, Some(&token), Some(&s1), call).status(), 200);
}

/// A POST's head and the first byte of a body that never comes, from `from` (as a proxy in front
/// says, `X-Forwarded-For`), with `token` if one's given: the connection, held open.
fn slow(w: &World, from: &str, path: &str, len: usize, token: Option<&str>) -> std::net::TcpStream {
    use std::io::Write;
    let host = w.mcp.trim_start_matches("http://");
    let mut c = std::net::TcpStream::connect(host).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    c.write_all(format!("POST {path} HTTP/1.1\r\nHost: {host}\r\nX-Forwarded-For: {from}\r\n{auth}Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{{").as_bytes()).unwrap();
    c
}

/// What each held request was answered, if it was within `within`: read side by side.
fn answers(conns: &mut [std::net::TcpStream], within: std::time::Duration) -> Vec<Option<u16>> {
    std::thread::scope(|s| {
        let reads: Vec<_> = conns
            .iter_mut()
            .map(|c| {
                s.spawn(move || {
                    use std::io::Read;
                    c.set_read_timeout(Some(within)).unwrap();
                    let mut head = [0u8; 12];
                    let mut n = 0;
                    while n < head.len() {
                        match c.read(&mut head[n..]) {
                            Ok(0) | Err(_) => break,
                            Ok(k) => n += k,
                        }
                    }
                    std::str::from_utf8(&head[..n])
                        .ok()?
                        .strip_prefix("HTTP/1.1 ")?
                        .get(..3)?
                        .parse()
                        .ok()
                })
            })
            .collect();
        reads.into_iter().map(|r| r.join().unwrap()).collect()
    })
}

#[test]
fn one_address_holding_slow_bodies_leaves_room_for_everyone_else() {
    let w = start("slow");
    let (_, theirs) = device(&w, "Their agent", Scope::Agent);
    let (one, other) = ("203.0.113.7", "198.51.100.9");
    // From one address, bodies that never finish arriving, more than there's room for in all: to
    // nowhere, to MCP without a token and with one, and OAuth's forms.
    let nowhere: Vec<_> = (0..32)
        .map(|_| slow(&w, one, "/does-not-exist", 1 << 20, None))
        .collect();
    let unsigned: Vec<_> = (0..4)
        .map(|_| slow(&w, one, "/mcp", 1 << 20, None))
        .collect();
    let signed: Vec<_> = (0..40)
        .map(|_| slow(&w, one, "/mcp", 1 << 20, Some(&theirs)))
        .collect();
    let forms: Vec<_> = (0..40)
        .map(|_| slow(&w, one, "/token", 16 << 10, None))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Another address still gets in: a session and a call, and a sign-in's registration and token.
    let token = w.token.0.clone();
    let mcp = |session: Option<&str>, msg: Value| {
        let mut r = web()
            .post(format!("{}/mcp", w.mcp))
            .header("x-forwarded-for", other)
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(msg.to_string());
        if let Some(s) = session {
            r = r.header("mcp-session-id", s);
        }
        r.send().unwrap()
    };
    let init = mcp(
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
    );
    assert_eq!(init.status(), 200, "another address's initialize");
    let session = init.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_string();
    let listed = mcp(
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(listed.status(), 200, "another address's call");
    assert_eq!(
        answer(listed)["result"]["tools"].as_array().unwrap().len(),
        12
    );
    let registered = web()
        .post(format!("{}/register", w.mcp))
        .header("x-forwarded-for", other)
        .json(&json!({"client_name": "Grok", "redirect_uris": ["https://grok.com/cb"]}))
        .send()
        .unwrap();
    assert_eq!(registered.status(), 201, "another address's registration");
    let exchanged = web()
        .post(format!("{}/token", w.mcp))
        .header("x-forwarded-for", other)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", "not-one"),
            ("code_verifier", "v"),
        ])
        .send()
        .unwrap();
    assert_eq!(
        exchanged.status(),
        400,
        "another address's token request, read and answered"
    );
    assert_eq!(exchanged.json::<Value>().unwrap()["error"], "invalid_grant");

    // The first address: what goes nowhere, or to MCP without a token, refused without reading
    // its body; past its own share of MCP bodies and of forms, refused at once (the rest still
    // waiting on theirs); a form too big to be one, too.
    let mut held: Vec<_> = nowhere
        .into_iter()
        .chain(unsigned)
        .chain(signed)
        .chain(forms)
        .chain([slow(&w, one, "/register", (16 << 10) + 1, None)])
        .collect();
    let got = answers(&mut held, std::time::Duration::from_secs(4));
    let (nowhere, rest) = got.split_at(32);
    let (unsigned, rest) = rest.split_at(4);
    let (signed, rest) = rest.split_at(40);
    let (forms, big) = rest.split_at(40);
    assert!(nowhere.iter().all(|s| *s == Some(404)), "{nowhere:?}");
    assert!(unsigned.iter().all(|s| *s == Some(401)), "{unsigned:?}");
    for (what, got) in [("MCP", signed), ("forms", forms)] {
        let refused = got.iter().filter(|s| **s == Some(503)).count();
        let waiting = got.iter().filter(|s| s.is_none()).count();
        assert!(
            refused > 0 && waiting > 0 && refused + waiting == got.len(),
            "{what}: {got:?}"
        );
    }
    assert_eq!(big, [Some(413)]);
    let tiny = web()
        .post(format!("{}/mcp", w.mcp))
        .header("x-forwarded-for", one)
        .bearer_auth(&theirs)
        .header("content-type", "application/json")
        .body(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}).to_string());
    assert_eq!(
        tiny.try_clone().unwrap().send().unwrap().status(),
        503,
        "the first address, while it holds its share"
    );
    // Its bodies gone, it's in again.
    drop(held);
    let again = (0..50).find_map(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        Some(tiny.try_clone().unwrap().send().unwrap().status()).filter(|s| *s != 503)
    });
    assert_eq!(again.map(|s| s.as_u16()), Some(200));
}

/// What OAuth makes of a PKCE verifier: its S256 challenge.
fn challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// A query parameter of a URL.
fn param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .map(|v| {
            v.replace("%20", " ")
                .replace("%3A", ":")
                .replace("%2F", "/")
        })
}

#[test]
fn oauth_signs_a_client_in_with_a_pairing_code_typed_by_a_person() {
    let w = start("oauth");
    // Discovery, as a client finds it from the 401.
    let prm: Value = web()
        .get(format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            w.mcp
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(prm["resource"], format!("{}/mcp", w.mcp));
    assert_eq!(prm["authorization_servers"][0], w.mcp);
    let meta: Value = web()
        .get(format!("{}/.well-known/oauth-authorization-server", w.mcp))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(meta["issuer"], w.mcp);
    assert_eq!(meta["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        meta["token_endpoint_auth_methods_supported"],
        json!(["none"])
    );
    // Behind a proxy: the address the client used.
    let proxied: Value = web()
        .get(format!("{}/.well-known/oauth-protected-resource", w.mcp))
        .header("x-forwarded-host", "box.tail1234.ts.net:8443")
        .header("x-forwarded-proto", "https")
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(proxied["resource"], "https://box.tail1234.ts.net:8443/mcp");

    // Registration: a redirect that can't take a code is refused; a good one gets an id.
    let reg = |redirects: Value| {
        web().post(format!("{}/register", w.mcp)).json(&json!({"client_name": "Grok", "redirect_uris": redirects, "token_endpoint_auth_method": "client_secret_post"})).send().unwrap()
    };
    assert_eq!(reg(json!(["http://evil.example/cb"])).status(), 400);
    assert_eq!(reg(json!(["javascript:alert(1)"])).status(), 400);
    let callback = "https://grok.com/connectors-oauth-exchange-code/";
    let r = reg(json!([
        callback,
        "cursor://anysphere.cursor-mcp/oauth/callback"
    ]));
    assert_eq!(r.status(), 201);
    let client: Value = r.json().unwrap();
    assert_eq!(client["token_endpoint_auth_method"], "none");
    let client_id = client["client_id"].as_str().unwrap().to_string();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk_and_more";
    let authorize = |redirect: &str, extra: &[(&str, &str)]| -> Vec<(String, String)> {
        let mut q = vec![
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect),
            ("state", "st 1"),
            ("code_challenge_method", "S256"),
        ];
        let ch = challenge(verifier);
        q.push(("code_challenge", ch.as_str()));
        q.extend_from_slice(extra);
        q.into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };

    // The approval page names the client and where access goes; nowhere unregistered.
    let page = web()
        .get(format!("{}/authorize", w.mcp))
        .query(&authorize(callback, &[]))
        .send()
        .unwrap();
    let page_policy = page
        .headers()
        .get("referrer-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(page.status(), 200);
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let html = page.text().unwrap();
    assert!(
        html.contains("Allow Grok to use Flotilla?")
            && html.contains("https://grok.com")
            && html.contains("Pairing code"),
        "{html}"
    );
    let elsewhere = web()
        .get(format!("{}/authorize", w.mcp))
        .query(&authorize("https://evil.example/cb", &[]))
        .send()
        .unwrap();
    assert_eq!(elsewhere.status(), 400);
    assert!(elsewhere.headers().get("location").is_none());
    // Without PKCE: told so, at its own address.
    let no_pkce = web()
        .get(format!("{}/authorize", w.mcp))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", callback),
        ])
        .send()
        .unwrap();
    assert_eq!(no_pkce.status(), 302);
    assert!(
        no_pkce.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with(&format!("{callback}?error=invalid_request"))
    );

    // A wrong code, Deny, then a right one.
    let form = |extra: &[(&str, &str)]| {
        web()
            .post(format!("{}/authorize", w.mcp))
            .form(&authorize(callback, extra))
            .send()
            .unwrap()
    };
    let wrong = form(&[("code", "ABCD-EFGH-JKMN"), ("action", "allow")]);
    assert_eq!(wrong.status(), 400);
    assert!(wrong.text().unwrap().contains("isn&#39;t right"));
    let denied = form(&[("action", "deny")]);
    assert_eq!(denied.status(), 302);
    let to = denied.headers()["location"].to_str().unwrap().to_string();
    assert!(
        to.starts_with(callback)
            && param(&to, "error").as_deref() == Some("access_denied")
            && param(&to, "state").as_deref() == Some("st 1"),
        "{to}"
    );
    // A web page posting the form elsewhere: refused.
    let forged = web()
        .post(format!("{}/authorize", w.mcp))
        .header("origin", "https://evil.example")
        .form(&authorize(callback, &[("action", "deny")]))
        .send()
        .unwrap();
    assert_eq!(forged.status(), 403);
    // The page's own form as an in-app browser posts it (Origin `null`): taken when the browser
    // says it's from the same origin, refused when it says it's from another site or doesn't say.
    let null_from = |site: Option<&str>| {
        let mut r = web()
            .post(format!("{}/authorize", w.mcp))
            .header("origin", "null");
        if let Some(site) = site {
            r = r.header("sec-fetch-site", site);
        }
        r.form(&authorize(callback, &[("action", "deny")]))
            .send()
            .unwrap()
            .status()
    };
    assert_eq!(null_from(Some("same-origin")), 302);
    assert_eq!(null_from(Some("cross-site")), 403);
    assert_eq!(null_from(None), 403);
    // And the page asks the browser to send its origin with that form.
    assert_eq!(page_policy, "same-origin");
    let mint = || {
        let store = Store::open(&w.db).unwrap();
        store
            .mint_code(
                flotilla_core::devices::CODE_TTL,
                "test",
                flotilla_core::util::now_ms(),
            )
            .unwrap()
            .code
    };
    let allow = |code: &str| {
        let r = web()
            .post(format!("{}/authorize", w.mcp))
            .header("origin", w.mcp.as_str())
            .form(&authorize(callback, &[("code", code), ("action", "allow")]))
            .send()
            .unwrap();
        assert_eq!(r.status(), 302, "{}", r.text().unwrap_or_default());
        let to = r.headers()["location"].to_str().unwrap().to_string();
        assert!(
            to.starts_with(callback) && param(&to, "state").as_deref() == Some("st 1"),
            "{to}"
        );
        param(&to, "code").unwrap()
    };
    let token_for = |code: &str, verifier: &str| {
        web()
            .post(format!("{}/token", w.mcp))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", callback),
                ("client_id", client_id.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .unwrap()
    };
    let devices = || {
        Store::open(&w.db)
            .unwrap()
            .devices(flotilla_core::util::now_ms())
            .unwrap()
    };
    let before = devices().len();

    // The wrong verifier: no token, and the device it made goes.
    let code = allow(&mint());
    assert_eq!(devices().len(), before + 1);
    let r = token_for(&code, "a-verifier-that-is-not-the-one-it-started-with-xyz");
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<Value>().unwrap()["error"], "invalid_grant");
    for _ in 0..50 {
        if devices().len() == before {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        devices().len(),
        before,
        "the device a failed exchange made is revoked"
    );

    // The right one: an agent device named for the client, whose token works for MCP.
    let code = allow(&mint());
    let r = token_for(&code, verifier);
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let t: Value = r.json().unwrap();
    assert_eq!(t["token_type"], "Bearer");
    let access = t["access_token"].as_str().unwrap().to_string();
    assert!(access.starts_with("fdt_"));
    let made = devices()
        .into_iter()
        .find(|d| d.name == "Grok")
        .expect("a device named for the client");
    assert_eq!((made.scope, made.platform.as_str()), (Scope::Agent, "mcp"));
    let s = session(&w, &access);
    assert!(http_tool(&w, &access, &s, "list_threads", json!({})).contains("Make a file"));
    // A code works once.
    assert_eq!(token_for(&code, verifier).status(), 400);
    // A refresh gives the token again while its device is paired.
    let refresh = |t: &str| {
        web()
            .post(format!("{}/token", w.mcp))
            .form(&[("grant_type", "refresh_token"), ("refresh_token", t)])
            .send()
            .unwrap()
    };
    let again: Value = refresh(t["refresh_token"].as_str().unwrap())
        .json()
        .unwrap();
    assert_eq!(again["access_token"], access);
    assert!(Store::open(&w.db).unwrap().revoke_device(&made.id).unwrap());
    assert_eq!(
        refresh(&access).json::<Value>().unwrap()["error"],
        "invalid_grant"
    );

    // A code given out and never swapped: its device goes as the server stops (its code goes
    // with it). One whose client has its token stays.
    let pending = || {
        Store::open(&w.db)
            .unwrap()
            .kv_with_prefix("mcp:pending:")
            .unwrap()
            .len()
    };
    let grok = || devices().iter().filter(|d| d.name == "Grok").count();
    assert_eq!(token_for(&allow(&mint()), verifier).status(), 200);
    assert_eq!((grok(), pending()), (1, 0));
    // Its note can't be cleared (the database refuses, injected here): no token goes, and the
    // device stays noted, for the sweep to revoke.
    let db = Store::open(&w.db).unwrap();
    db.conn().execute_batch("CREATE TRIGGER keep_notes BEFORE DELETE ON kv WHEN old.key LIKE 'mcp:pending:%' BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    let r = token_for(&allow(&mint()), verifier);
    assert_eq!(r.status(), 503);
    assert!(r.json::<Value>().unwrap().get("access_token").is_none());
    db.conn().execute_batch("DROP TRIGGER keep_notes;").unwrap();
    assert_eq!((grok(), pending()), (2, 1));
    let _never_swapped = allow(&mint());
    assert_eq!((grok(), pending()), (3, 2));
    w.stop_server();
    assert_eq!(
        (grok(), pending()),
        (2, 1),
        "the device made for a code never swapped is revoked as the server stops"
    );
}

/// Sign-ins a server forgot (it stopped without revoking their devices: a crash) are revoked
/// as the next one starts, once their codes would have expired. One still in time, one whose
/// client has its token, and agent devices made another way are left.
#[test]
fn sign_ins_a_crash_left_unfinished_are_revoked_as_the_server_starts() {
    let now = flotilla_core::util::now_ms();
    let w = start_with("orphans", |store| {
        for (name, pending_at) in [
            ("Stale", Some(now - 10 * 60_000)),
            ("In time", Some(now)),
            ("Finished", None),
        ] {
            let code = store
                .mint_code_as(flotilla_core::devices::CODE_TTL, "cli", Scope::Agent, now)
                .unwrap();
            let paired = store
                .pair_device(&code.code, name, "mcp", None, now)
                .unwrap();
            if let Some(at) = pending_at {
                store
                    .kv_set(&format!("mcp:pending:{}", paired.device), &at.to_string())
                    .unwrap();
            }
        }
    });
    let names = || {
        let mut n: Vec<String> = Store::open(&w.db)
            .unwrap()
            .devices(flotilla_core::util::now_ms())
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        n.sort();
        n
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while names().contains(&"Stale".to_string()) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(names(), ["Claude Code", "Finished", "In time"]);
    assert_eq!(
        Store::open(&w.db)
            .unwrap()
            .kv_with_prefix("mcp:pending:")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_mcp_listener_serves_nothing_of_the_main_api() {
    let w = start("only-mcp");
    let (_, full) = device(&w, "My Mac", Scope::Full);
    for token in [None, Some(w.token.0.as_str()), Some(full.as_str())] {
        let with = |r: reqwest::blocking::RequestBuilder| match token {
            Some(t) => r.bearer_auth(t),
            None => r,
        };
        let c = web();
        for (method, path) in [
            ("GET", "/v1/health"),
            ("GET", "/.well-known/flotilla"),
            ("POST", "/v1/pair"),
            ("PUT", "/v1/blobs"),
            ("GET", "/v1/ws"),
            (
                "GET",
                "/v1/threads/aaaa1111-0000-4000-8000-000000000001/history?before=100",
            ),
            (
                "GET",
                "/v1/threads/aaaa1111-0000-4000-8000-000000000001/full?seq=1&field=text&mark=x",
            ),
            ("GET", "/"),
        ] {
            let r = with(
                c.request(method.parse().unwrap(), format!("{}{path}", w.mcp))
                    .header("flotilla-protocol", "1")
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket")
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                    .body("{}"),
            )
            .send()
            .unwrap();
            assert_eq!(r.status(), 404, "{method} {path} with {token:?}");
        }
    }
    // The main API is where it was, and still answers.
    assert_eq!(
        web()
            .get(format!("{}/v1/health", w.url))
            .send()
            .unwrap()
            .status(),
        200
    );
}
