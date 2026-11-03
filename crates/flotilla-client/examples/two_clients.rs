//! Two clients on one server: A marks a thread unread, read, archived and unarchived, and B
//! says how long each took to reach it. `--watch` only watches (B), printing what changes.
//!
//!     cargo run -p flotilla-client --example two_clients -- unix:/path/to/server.sock <thread-prefix> [--watch]
//!
//! Over TCP, `FLOTILLA_TOKEN` is a paired device's token.

use flotilla_client::{Client, Info, Target};
use flotilla_proto::{Activity, ClientMsg, Request, ServerMsg, Sidebar, View, ViewKey};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn client(target: &Target, name: &str) -> (Client, mpsc::Receiver<ServerMsg>) {
    let (tx, rx) = mpsc::channel();
    let token = std::env::var("FLOTILLA_TOKEN")
        .ok()
        .map(flotilla_proto::Secret);
    let c = Client::connect(
        target.clone(),
        Info {
            app: name.into(),
            version: "0".into(),
            token,
            scope: None,
        },
        move |m| tx.send(m).is_ok(),
    );
    c.send(ClientMsg::Sub {
        view: ViewKey::Sidebar,
    });
    (c, rx)
}

/// The next sidebar, waiting up to `wait`.
fn sidebar(rx: &mpsc::Receiver<ServerMsg>, wait: Duration) -> Option<Sidebar> {
    let until = Instant::now() + wait;
    loop {
        match rx
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .ok()?
        {
            ServerMsg::Snap { view } => {
                if let View::Sidebar(s) = *view {
                    return Some(s);
                }
            }
            ServerMsg::Connection(c) => println!("[{:?}]", c),
            _ => {}
        }
    }
}

fn activity(s: &Sidebar, id: &str) -> Option<Activity> {
    s.threads
        .iter()
        .find(|t| t.thread.id == id)
        .map(|t| t.activity)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(address), Some(prefix)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: two_clients <address> <thread-prefix> [--watch]");
        std::process::exit(2);
    };
    let target = Target::parse(address).expect("an address");
    let (_b, b_rx) = client(&target, "two-clients-b");
    let first = sidebar(&b_rx, Duration::from_secs(10)).expect("B's first sidebar");
    let id = first
        .threads
        .iter()
        .find(|t| t.thread.id.starts_with(prefix.as_str()))
        .expect("that thread")
        .thread
        .id
        .clone();
    println!("B sees {id}: {:?}", activity(&first, &id));
    if args.iter().any(|a| a == "--watch") {
        let start = Instant::now();
        let mut last = activity(&first, &id);
        while let Some(s) = sidebar(&b_rx, Duration::from_secs(120)) {
            let now = activity(&s, &id);
            if now != last {
                println!("{:>7.3}s  B sees {:?}", start.elapsed().as_secs_f64(), now);
                last = now;
            }
        }
        return;
    }
    let (a, a_rx) = client(&target, "two-clients-a");
    sidebar(&a_rx, Duration::from_secs(10)).expect("A's first sidebar");
    let pause = Duration::from_millis(
        std::env::var("PAUSE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2500),
    );
    for (n, (req, want)) in [
        (Request::MarkUnread(id.clone()), Activity::Unread),
        (Request::MarkViewed(id.clone()), Activity::Idle),
        (Request::Archive(id.clone()), Activity::Archived),
        (Request::Unarchive(id.clone()), Activity::Idle),
    ]
    .into_iter()
    .enumerate()
    {
        let verb = req.verb();
        let sent = Instant::now();
        a.send(ClientMsg::Req {
            id: n as u64 + 1,
            req,
        });
        loop {
            let s = sidebar(&b_rx, Duration::from_secs(5))
                .unwrap_or_else(|| panic!("B never saw {verb}"));
            if activity(&s, &id) == Some(want) {
                println!(
                    "A: {verb} ({want:?}) → B saw it in {:.0} ms",
                    sent.elapsed().as_secs_f64() * 1000.0
                );
                break;
            }
        }
        std::thread::sleep(pause);
    }
}
