//! The server's plain HTTP: blobs, its descriptor, a client of another protocol refused, no
//! requests from web pages, and who needs a token: every TCP request that reaches the engine
//! (loopback too), and nothing over the Unix socket.

use flotilla_engine::host::{Mode, Role};
use flotilla_proto::Secret;
use flotilla_server::{Config, Listen, Server};
use std::io::{Read, Write};
use std::sync::Arc;

/// A raw HTTP/1.1 request; the status line and body.
pub fn http(addr: &str, head: &str, body: &[u8]) -> (String, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    // The head, then as much body as it says (a 426 or 101 leaves the connection open).
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&out).to_string();
        if let Some((top, rest)) = text.split_once("\r\n\r\n") {
            let len = top
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if rest.len() >= len {
                return (
                    top.lines().next().unwrap_or("").to_string(),
                    rest.to_string(),
                );
            }
        }
        match s.read(&mut buf) {
            Ok(0) | Err(_) => panic!("the server stopped answering: {text}"),
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
}

#[test]
fn blobs_the_descriptor_and_who_is_refused() {
    let dir = std::env::temp_dir().join(format!("flotilla-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let token = {
        let store = flotilla_core::store::Store::open(&db).unwrap();
        let now = flotilla_core::util::now_ms();
        let code = store
            .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
            .unwrap();
        Secret(
            store
                .pair_device(&code.code, "test", "macos", None, now)
                .unwrap()
                .token,
        )
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let socket = dir.join("run").join("server.sock");
    let listen = vec![
        Listen::tcp("127.0.0.1", 0, false).unwrap(),
        Listen::Unix(socket.clone()),
    ];
    let config = Config {
        db: db.clone(),
        listen,
        mode: Mode::Fake(Arc::new(|_| {})),
        role: Role::Server,
    };
    let server = rt.block_on(Server::bind(config)).unwrap();
    let addr = server.tcp().unwrap().to_string();
    // The lock file says where the CLI finds it: the socket, which needs no token.
    assert_eq!(
        flotilla_core::lock::holder(&db).and_then(|h| h.address),
        Some(format!("unix:{}", socket.display()))
    );
    // Only its owner may use it: the socket 0600, in a directory made 0700.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));

    // A file in, named by its contents; the same again is the same blob. Over TCP only with a
    // device's token; over the socket with none.
    let tcp = flotilla_client::Target::parse(&format!("ws://{addr}")).unwrap();
    let unix = flotilla_client::Target::Unix(socket.clone());
    let blob = flotilla_client::upload(&tcp, Some(&token), b"hello").unwrap();
    assert_eq!(
        blob.id,
        "b_2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(blob.size, 5);
    assert_eq!(
        std::fs::read(dir.join("blobs").join(&blob.id[2..])).unwrap(),
        b"hello"
    );
    assert_eq!(
        flotilla_client::upload(&unix, None, b"hello").unwrap(),
        blob
    );
    assert!(flotilla_client::upload(&tcp, Some(&token), b"").is_err());
    let e = flotilla_client::upload(&tcp, None, b"hello").unwrap_err();
    assert!(e.contains("isn't paired"), "{e}");
    let e = flotilla_client::upload(
        &tcp,
        Some(&Secret(
            "fdt_0123456789abcdef_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        )),
        b"hello",
    )
    .unwrap_err();
    assert!(e.contains("isn't paired"), "{e}");

    // The descriptor and health are public.
    let (status, body) = http(
        &addr,
        &format!("GET /.well-known/flotilla HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"),
        b"",
    );
    assert!(status.contains("200"), "{status}");
    let d: flotilla_proto::wire::Descriptor = serde_json::from_str(&body).unwrap();
    assert_eq!(d.protocol, flotilla_proto::PROTOCOL);
    assert!(d.server.id.starts_with("srv_") && d.capabilities.iter().any(|c| c == "deltas"));
    // Behind `tailscale serve` the Host is the tailnet's name.
    let (status, _) = http(
        &addr,
        "GET /v1/health HTTP/1.1\r\nHost: box.tail1.ts.net\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert!(status.contains("200"), "{status}");

    // Another protocol: 426, saying which this one speaks.
    let upgrade = |proto: &str, auth: &str| {
        format!(
            "GET /v1/ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nflotilla-protocol: {proto}\r\n{auth}\r\n"
        )
    };
    let (status, body) = http(&addr, &upgrade("99", ""), b"");
    assert!(status.contains("426"), "{status}");
    assert!(body.contains("protocol 1"), "{body}");
    // The WebSocket, from loopback too: no token, a wrong one, or another scheme is refused.
    for auth in [
        "",
        "Authorization: Bearer fdt_nope\r\n",
        &format!("Authorization: Basic {}\r\n", token.0),
    ] {
        let (status, body) = http(&addr, &upgrade("1", auth), b"");
        assert!(status.contains("401"), "{auth}: {status}");
        assert!(body.contains("isn't paired"), "{body}");
    }
    let (status, _) = http(
        &addr,
        &upgrade(
            "1",
            &format!("Authorization: Bearer {}\r\nConnection: close\r\n", token.0),
        ),
        b"",
    );
    assert!(status.contains("101"), "{status}");

    // A web page: refused.
    let (status, _) = http(
        &addr,
        &format!(
            "GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://evil.example\r\nConnection: close\r\n\r\n"
        ),
        b"",
    );
    assert!(status.contains("403"), "{status}");
    let (status, _) = http(
        &addr,
        &format!(
            "POST /v1/pair HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
        ),
        b"{}",
    );
    assert!(status.contains("403"), "{status}");

    // A second server on the same database is refused, and once this one stops it's free (and
    // its socket is gone).
    let second = Config {
        db: db.clone(),
        listen: vec![Listen::tcp("127.0.0.1", 0, false).unwrap()],
        mode: Mode::Offline,
        role: Role::Server,
    };
    assert_eq!(
        rt.block_on(Server::bind(second)).err().as_deref(),
        Some(flotilla_core::lock::BUSY)
    );
    let _ = stop.send(());
    rt.block_on(done).unwrap().unwrap();
    assert_eq!(flotilla_core::lock::holder(&db), None);
    assert!(!socket.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
