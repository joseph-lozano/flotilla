//! Pairing and devices against a real server: codes used once (and why one doesn't pair), the
//! limit on wrong codes from one address, an upload refused before its body is read, tokens
//! wrong or revoked refused, a revoked device's socket closed at
//! once, a session another device can't resume, and the Unix socket needing no token.

use flotilla_client::{Client, Info, Target};
use flotilla_core::devices::CODE_TTL;
use flotilla_core::store::Store;
use flotilla_engine::host::{Mode, REVOKED, Role};
use flotilla_proto::wire::{Down, Hello, Up};
use flotilla_proto::{ClientMsg, Connection, PROTOCOL, Request, Secret, ServerMsg};
use flotilla_server::{Config, Listen, Server};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

struct Running {
    rt: tokio::runtime::Runtime,
    dir: PathBuf,
    db: PathBuf,
    url: String,
    socket: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(done) = self.done.take() {
            let _ = self.rt.block_on(done);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(name: &str) -> Running {
    let dir = std::env::temp_dir().join(format!("flotilla-auth-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let socket = dir.join("run").join("s.sock");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
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
    let url = format!("http://{}", server.tcp().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        rt,
        dir,
        db,
        url,
        socket,
        stop: Some(stop),
        done: Some(done),
    }
}

impl Running {
    /// Another connection to the database, as `flotilla pair` and `flotilla devices` open.
    fn store(&self) -> Store {
        Store::open(&self.db).unwrap()
    }

    fn code(&self) -> String {
        self.store()
            .mint_code(CODE_TTL, "cli", flotilla_core::util::now_ms())
            .unwrap()
            .code
    }

    /// A device paired over HTTP, as the app does it: its id and token.
    fn pair(&self, name: &str) -> (String, Secret) {
        let p = flotilla_client::pair(&self.url, &self.code(), name, "macos").unwrap();
        (p.device, p.token)
    }

    fn client(&self, target: Target, token: Option<Secret>) -> (Client, mpsc::Receiver<ServerMsg>) {
        let (tx, rx) = mpsc::channel();
        let c = Client::connect(
            target,
            Info {
                app: "test".into(),
                version: "0".into(),
                token,
                scope: None,
            },
            move |m| tx.send(m).is_ok(),
        );
        c.send(ClientMsg::Sub {
            view: flotilla_proto::ViewKey::Sidebar,
        });
        (c, rx)
    }

    fn tcp(&self) -> Target {
        Target::parse(&self.url).unwrap()
    }
}

/// The next connection state the client reports, within `secs`.
fn connection(rx: &mpsc::Receiver<ServerMsg>, secs: u64) -> Option<Connection> {
    let until = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(ServerMsg::Connection(c)) => return Some(c),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[test]
fn a_code_pairs_once_and_one_that_doesnt_says_why() {
    let s = start("codes");
    let code = s.code();
    let p = flotilla_client::pair(
        &s.url,
        &flotilla_proto::pair::display_code(&code).to_lowercase(),
        "My MacBook",
        "macos",
    )
    .unwrap();
    assert!(p.token.0.starts_with("fdt_") && p.server.id.starts_with("srv_"));
    assert!(!format!("{p:?}").contains(&p.token.0));
    let again = flotilla_client::pair(&s.url, &code, "Another", "macos").unwrap_err();
    assert!(again.contains("already used"), "{again}");
    // Expired: made ten minutes ago, for five.
    let old = s
        .store()
        .mint_code(CODE_TTL, "cli", flotilla_core::util::now_ms() - 600_000)
        .unwrap();
    let e = flotilla_client::pair(&s.url, &old.code, "x", "macos").unwrap_err();
    assert!(e.contains("expired"), "{e}");
    let revoked = s
        .store()
        .mint_code(CODE_TTL, "cli", flotilla_core::util::now_ms())
        .unwrap();
    assert!(
        s.store()
            .revoke_code(&revoked.id, flotilla_core::util::now_ms())
            .unwrap()
    );
    let e = flotilla_client::pair(&s.url, &revoked.code, "x", "macos").unwrap_err();
    assert!(e.contains("revoked"), "{e}");
    let e = flotilla_client::pair(&s.url, "ABCD-EFGH-JKMN", "x", "macos").unwrap_err();
    assert!(e.contains("isn't right"), "{e}");
    let e = flotilla_client::pair(&s.url, "not a code", "x", "macos").unwrap_err();
    assert!(e.contains("isn't right"), "{e}");
    // Only the first paired.
    let devices = s.store().devices(flotilla_core::util::now_ms()).unwrap();
    assert_eq!(
        devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["My MacBook"]
    );
    // Its token connects.
    let (_c, rx) = s.client(s.tcp(), Some(p.token));
    assert_eq!(connection(&rx, 10), Some(Connection::Online));
}

/// `POST /v1/pair` as forwarded for `from` (`tailscale serve` and Caddy connect from loopback
/// and say who for): its status line and body.
fn pair_from(s: &Running, from: &str, code: &str) -> (String, String) {
    use std::io::{Read, Write};
    let addr = s.url.trim_start_matches("http://");
    let body =
        serde_json::json!({"code": code, "device_name": "x", "platform": "macos"}).to_string();
    let mut c = std::net::TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(c, "POST /v1/pair HTTP/1.1\r\nHost: {addr}\r\nX-Forwarded-For: {from}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[test]
fn wrong_codes_from_one_address_dont_lock_out_another() {
    let s = start("limit");
    let right = s.code();
    let wrong = ["MN", "MP", "MQ", "MR", "MS", "MT", "MU", "MV", "MW", "MX"];
    for i in 0..flotilla_server::PAIR_TRIES {
        let (status, body) =
            pair_from(&s, "203.0.113.9", &format!("ABCD-EFGH-JK{}", wrong[i % 10]));
        assert!(
            status.contains("401") && body.contains("isn't right"),
            "{status} {body}"
        );
    }
    // Now even the right code from there isn't looked at, and so isn't spent.
    let (status, body) = pair_from(&s, "203.0.113.9", &right);
    assert!(
        status.contains("429") && body.contains("Too many"),
        "{status} {body}"
    );
    assert_eq!(s.store().unused_codes().unwrap().len(), 1);
    assert!(
        s.store()
            .devices(flotilla_core::util::now_ms())
            .unwrap()
            .is_empty()
    );
    // From anywhere else it pairs.
    let (status, body) = pair_from(&s, "198.51.100.1", &right);
    assert!(status.contains("200"), "{status} {body}");
    assert_eq!(
        s.store()
            .devices(flotilla_core::util::now_ms())
            .unwrap()
            .len(),
        1
    );
}

/// A stranger's upload is refused as soon as its headers arrive, not once its whole body has:
/// otherwise each connection could make the server hold a file's worth of memory.
#[test]
fn an_upload_without_a_token_is_refused_before_its_body_is_read() {
    use std::io::{Read, Write};
    let s = start("upload");
    let addr = s.url.trim_start_matches("http://");
    let mut c = std::net::TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        c,
        "PUT /v1/blobs HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n\r\n",
        32 << 20
    )
    .unwrap();
    c.write_all(&[0u8; 64 << 10]).unwrap();
    let mut buf = [0u8; 512];
    let n = c
        .read(&mut buf)
        .expect("no answer while the body was still coming");
    let head = String::from_utf8_lossy(&buf[..n]);
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
}

#[test]
fn a_wrong_or_revoked_token_is_refused_for_good() {
    let s = start("tokens");
    let (_, good) = s.pair("A");
    for token in [
        None,
        Some(Secret(
            "fdt_0123456789abcdef_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        )),
        Some(Secret(format!("{}x", good.0))),
    ] {
        let (_c, rx) = s.client(s.tcp(), token);
        assert!(
            matches!(connection(&rx, 10), Some(Connection::Refused { reason }) if reason.contains("isn't paired"))
        );
        // And it stops trying.
        assert_eq!(connection(&rx, 2), None);
    }
}

#[test]
fn revoking_a_device_closes_its_socket_at_once() {
    let s = start("revoke");
    let (a, token_a) = s.pair("A");
    let (b, token_b) = s.pair("B");
    let (_ca, rx_a) = s.client(s.tcp(), Some(token_a.clone()));
    let (_cb, rx_b) = s.client(s.tcp(), Some(token_b.clone()));
    assert_eq!(connection(&rx_a, 10), Some(Connection::Online));
    assert_eq!(connection(&rx_b, 10), Some(Connection::Online));
    // As `flotilla devices revoke` does it: straight into the database. The server notices.
    let at = std::time::Instant::now();
    assert!(s.store().revoke_device(&a).unwrap());
    assert!(
        matches!(connection(&rx_a, 3), Some(Connection::Refused { reason }) if reason == REVOKED)
    );
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    // Its reconnect is refused, and that's the end of it.
    assert!(
        matches!(connection(&rx_a, 10), Some(Connection::Refused { reason }) if reason.contains("isn't paired"))
    );
    assert_eq!(connection(&rx_a, 2), None);
    // From another client (this machine's, over the socket): closed before the answer.
    let (local, rx_l) = s.client(Target::Unix(s.socket.clone()), None);
    assert_eq!(connection(&rx_l, 10), Some(Connection::Online));
    local.send(ClientMsg::Req {
        id: 1,
        req: Request::RevokeDevice(b.clone()),
    });
    assert!(
        matches!(connection(&rx_b, 3), Some(Connection::Refused { reason }) if reason == REVOKED)
    );
    let answered = rx_l.iter().find_map(|m| {
        if let ServerMsg::Res { id: 1, result } = m {
            Some(result)
        } else {
            None
        }
    });
    assert_eq!(answered, Some(Ok(flotilla_proto::Reply::Done)));
    // Neither token works any more.
    for t in [token_a, token_b] {
        assert!(
            flotilla_client::upload(&s.tcp(), Some(&t), b"x")
                .unwrap_err()
                .contains("isn't paired")
        );
    }
}

/// A raw WebSocket with `token`: its hello for `session`, and what the server says first.
async fn hello(url: &str, token: &Secret, session: &str) -> Down {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("{}/v1/ws", url.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("flotilla-protocol", PROTOCOL.into());
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", token.0).parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let hello = Up::Hello(Hello {
        proto: PROTOCOL,
        app: "t".into(),
        version: "0".into(),
        platform: "t".into(),
        session: session.into(),
        epoch: None,
        subs: vec![],
        got: None,
        scope: None,
    });
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&hello).unwrap().into(),
    ))
    .await
    .unwrap();
    let first = loop {
        match ws.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
                break serde_json::from_str::<Down>(&t).unwrap();
            }
            Some(Ok(_)) => continue,
            other => panic!("{other:?}"),
        }
    };
    let _ = ws.close(None).await;
    first
}

#[test]
fn a_session_cant_be_resumed_by_another_device() {
    let s = start("sessions");
    let (a, token_a) = s.pair("A");
    let (_, token_b) = s.pair("B");
    let first = s.rt.block_on(hello(&s.url, &token_a, "s-1"));
    assert!(
        matches!(&first, Down::Welcome(w) if w.device.as_deref() == Some(a.as_str()) && !w.resumed),
        "{first:?}"
    );
    std::thread::sleep(Duration::from_millis(200));
    let stolen = s.rt.block_on(hello(&s.url, &token_b, "s-1"));
    assert!(
        matches!(&stolen, Down::Error { msg } if msg.contains("another device")),
        "{stolen:?}"
    );
    let back = s.rt.block_on(hello(&s.url, &token_a, "s-1"));
    assert!(matches!(&back, Down::Welcome(w) if w.resumed), "{back:?}");
}

#[test]
fn this_machines_user_needs_no_token_on_the_socket() {
    let s = start("socket");
    let (_c, rx) = s.client(Target::Unix(s.socket.clone()), None);
    assert_eq!(connection(&rx, 10), Some(Connection::Online));
    // Pairing over the socket works too (it's a code, either way).
    flotilla_client::pair(
        &format!("unix:{}", s.socket.display()),
        &s.code(),
        "From the socket",
        "macos",
    )
    .unwrap();
    assert_eq!(
        s.store()
            .devices(flotilla_core::util::now_ms())
            .unwrap()
            .len(),
        1
    );
}

/// A small upload without a token is told why every time: its body is read (and dropped) before
/// the answer, so the connection isn't reset on it with the answer still on its way.
#[test]
fn a_small_upload_without_a_token_is_always_told_why() {
    let s = start("small-upload");
    let target = s.tcp();
    for i in 0..200 {
        let e =
            flotilla_client::upload(&target, None, format!("hello {i}").as_bytes()).unwrap_err();
        assert!(e.contains("isn't paired"), "try {i}: {e}");
    }
}

/// A request this server doesn't know is refused, and the connection goes on as before: the
/// refusal is counted in the client's queue like everything else, so what comes after it
/// still goes.
#[test]
fn after_an_unknown_request_the_connection_goes_on() {
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let s = start("unknown");
    let (_, token) = s.pair("Phone");
    s.rt.block_on(async {
        let mut req = format!("{}/v1/ws", s.url.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("flotilla-protocol", PROTOCOL.into());
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", token.0).parse().unwrap(),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let hello = Up::Hello(Hello {
            proto: PROTOCOL,
            app: "t".into(),
            version: "0".into(),
            platform: "t".into(),
            session: "s-unknown".into(),
            epoch: None,
            subs: vec![],
            got: None,
            scope: None,
        });
        let send = |up: String| Message::Text(up.into());
        ws.send(send(serde_json::to_string(&hello).unwrap()))
            .await
            .unwrap();
        // The welcome and what comes with it, up to `synced`.
        loop {
            match tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .expect("nothing came")
            {
                Some(Ok(Message::Text(t)))
                    if matches!(serde_json::from_str::<Down>(&t).unwrap(), Down::Synced) =>
                {
                    break;
                }
                Some(Ok(_)) => continue,
                other => panic!("{other:?}"),
            }
        }
        ws.send(send(
            r#"{"t":"req","id":7,"m":"no.such.request","p":null}"#.into(),
        ))
        .await
        .unwrap();
        ws.send(send(
            serde_json::to_string(&Up::Sub {
                view: flotilla_proto::ViewKey::Sidebar,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        let (mut refused, mut sidebar) = (false, false);
        while !(refused && sidebar) {
            let d = match tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .expect("the sidebar never came")
            {
                Some(Ok(Message::Text(t))) => serde_json::from_str::<Down>(&t).unwrap(),
                Some(Ok(_)) => continue,
                other => panic!("{other:?}"),
            };
            refused |=
                matches!(&d, Down::Res { id: 7, result: Err(e), .. } if e.contains("doesn't know"));
            sidebar |= matches!(
                &d,
                Down::Snap {
                    view: flotilla_proto::ViewKey::Sidebar,
                    ..
                }
            );
        }
    });
}

/// A device paired as an agent (`flotilla mcp`), whatever its client sends: requests outside an
/// agent's scope are refused by the server, the views it may not see never come (subscribed
/// to, or held in a hello it writes itself), and it may not upload; reading a thread's older
/// pages, and what an agent may do, work. A device that asks to pair as an agent with an
/// ordinary code is one too.
#[test]
fn an_agent_device_is_held_to_its_scope_by_the_server() {
    use flotilla_proto::{Scope, ViewKey};
    let s = start("agent");
    let p = flotilla_client::pair_as(
        &s.url,
        &s.code(),
        "Claude Code",
        "macos",
        Some(Scope::Agent),
    )
    .unwrap();
    let device = s
        .store()
        .devices(flotilla_core::util::now_ms())
        .unwrap()
        .into_iter()
        .find(|d| d.id == p.device)
        .unwrap();
    assert_eq!(device.scope, Scope::Agent);
    let (phone, _) = s.pair("iPhone");
    let (c, rx) = s.client(s.tcp(), Some(p.token.clone()));
    assert_eq!(connection(&rx, 10), Some(Connection::Online));
    for view in [
        ViewKey::Settings,
        ViewKey::Devices,
        ViewKey::Schedules,
        ViewKey::Catalog,
    ] {
        c.send(ClientMsg::Sub { view });
    }
    let forbidden = [
        Request::Delete("t1".into()),
        Request::MergePr("t1".into()),
        Request::Set(flotilla_proto::Setting::IdleMinutes(1)),
        Request::SetBoatKey(Secret("boat_x".into())),
        Request::PairCode { ttl_secs: None },
        Request::RevokeDevice(phone.clone()),
        Request::RemoveProject("p1".into()),
        Request::Rebuild,
    ];
    for (i, req) in forbidden.iter().enumerate() {
        c.send(ClientMsg::Req {
            id: i as u64 + 1,
            req: req.clone(),
        });
    }
    c.send(ClientMsg::Req {
        id: 50,
        req: Request::Search {
            query: "x".into(),
            limit: None,
        },
    });
    let until = std::time::Instant::now() + Duration::from_secs(10);
    let mut answers = std::collections::HashMap::new();
    let mut views = std::collections::BTreeSet::new();
    while answers.len() < forbidden.len() + 1 && std::time::Instant::now() < until {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMsg::Res { id, result }) => {
                answers.insert(id, result);
            }
            Ok(ServerMsg::Snap { view }) => {
                views.insert(view.key().name());
            }
            _ => {}
        }
    }
    for (i, req) in forbidden.iter().enumerate() {
        assert!(
            matches!(&answers.get(&(i as u64 + 1)), Some(Err(why)) if why.starts_with("Agents can't")),
            "{req:?}: {:?}",
            answers.get(&(i as u64 + 1))
        );
    }
    assert!(
        matches!(answers.get(&50), Some(Ok(flotilla_proto::Reply::Hits(_)))),
        "{answers:?}"
    );
    // A moment more for any view that was going to come.
    while let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) {
        if let ServerMsg::Snap { view } = m {
            views.insert(view.key().name());
        }
    }
    assert_eq!(
        views,
        std::collections::BTreeSet::from(["catalog", "sidebar"])
    );
    assert!(s.store().device_paired(&phone).unwrap());
    // No uploads; reading history goes on as for any device.
    let e = flotilla_client::upload(&s.tcp(), Some(&p.token), b"a file").unwrap_err();
    assert!(e.contains("Agents can't send files"), "{e}");
    let e = flotilla_client::history(&s.tcp(), Some(&p.token), "no-such-thread", 50).unwrap_err();
    assert!(!e.contains("Agents") && !e.contains("isn't paired"), "{e}");
    // A raw socket's own hello holding the settings: not sent either.
    let first = s.rt.block_on(async {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("{}/v1/ws", s.url.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("flotilla-protocol", PROTOCOL.into());
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", p.token.0).parse().unwrap(),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let subs = vec![
            flotilla_proto::wire::Held {
                view: ViewKey::Settings,
                rev: None,
            },
            flotilla_proto::wire::Held {
                view: ViewKey::Devices,
                rev: None,
            },
        ];
        let hello = Up::Hello(Hello {
            proto: PROTOCOL,
            app: "t".into(),
            version: "0".into(),
            platform: "t".into(),
            session: "raw".into(),
            epoch: None,
            subs,
            got: None,
            scope: Some(Scope::Full),
        });
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&hello).unwrap().into(),
        ))
        .await
        .unwrap();
        let mut kinds = vec![];
        while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
                let d: Down = serde_json::from_str(&t).unwrap();
                match d {
                    Down::Snap { view, .. } => kinds.push(view.name()),
                    Down::Synced => break,
                    _ => {}
                }
            }
        }
        kinds
    });
    assert!(first.is_empty(), "{first:?}");
}
