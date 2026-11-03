//! How big a thread's view is on the wire, and how long it takes to arrive: a raw WebSocket
//! over a server's Unix socket says hello, subscribes to the thread, and times the snapshot.
//! It also counts its messages and the largest tool input or result in it.
//!
//!     cargo run --release -p flotilla-client --example thread_snapshot -- /path/to/run/server.sock <thread id> [runs]

use flotilla_proto::wire::{Down, Hello, PROTOCOL_HEADER, Up, WS_PATH};
use flotilla_proto::{PROTOCOL, ViewKey};
use futures_util::{SinkExt, StreamExt};
use std::time::Instant;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(socket), Some(thread)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: thread_snapshot <server.sock> <thread id> [runs]");
        std::process::exit(2);
    };
    let runs: usize = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(5);
    let mut times = Vec::new();
    for run in 0..runs {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("the socket");
        let mut req = format!("ws://localhost{WS_PATH}")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(PROTOCOL_HEADER, PROTOCOL.into());
        let (mut ws, _) = tokio_tungstenite::client_async_with_config(
            req,
            stream,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(None)
                    .max_frame_size(None),
            ),
        )
        .await
        .expect("the upgrade");
        let send = |up: &Up| Message::Text(serde_json::to_string(up).unwrap().into());
        let hello = Hello {
            proto: PROTOCOL,
            app: "thread_snapshot".into(),
            version: "0".into(),
            platform: "macos".into(),
            session: uuid::Uuid::new_v4().to_string(),
            epoch: None,
            subs: vec![],
            got: None,
            scope: None,
        };
        ws.send(send(&Up::Hello(hello))).await.unwrap();
        // Caught up first, so only the thread's own view is timed.
        while let Some(Ok(frame)) = ws.next().await {
            if let Message::Text(text) = frame
                && matches!(serde_json::from_str::<Down>(&text), Ok(Down::Synced))
            {
                break;
            }
        }
        let view = ViewKey::Thread(thread.clone());
        let started = Instant::now();
        ws.send(send(&Up::Sub { view: view.clone() }))
            .await
            .unwrap();
        while let Some(Ok(frame)) = ws.next().await {
            let Message::Text(text) = frame else { continue };
            let Ok(Down::Snap {
                view: got, data, ..
            }) = serde_json::from_str::<Down>(&text)
            else {
                continue;
            };
            if got != view {
                continue;
            }
            let ms = started.elapsed().as_secs_f64() * 1000.;
            times.push(ms);
            if run == 0 {
                let messages = data["messages"].as_array().map_or(0, Vec::len);
                let biggest = data["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|m| [&m["tool_input_json"], &m["tool_result_json"]])
                    .filter_map(|v| v.as_str())
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                let older = data.get("older").and_then(|o| o.as_i64());
                println!(
                    "snapshot: {} bytes, {messages} messages (older: {older:?}), largest tool string {biggest} bytes",
                    text.len()
                );
            }
            break;
        }
        let _ = ws.close(None).await;
    }
    times.sort_by(f64::total_cmp);
    println!(
        "sub → snapshot: median {:.1} ms (min {:.1}, max {:.1}) over {} runs",
        times[times.len() / 2],
        times[0],
        times[times.len() - 1],
        times.len()
    );
}
