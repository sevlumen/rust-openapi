//! Long-running mixed load, opt-in: it is `#[ignore]`d so `cargo test` stays
//! fast. Run it for a while before a release:
//!
//! ```text
//! OAS_SOAK_SECS=300 cargo test --release --features "test-util http2 websocket" \
//!     --test soak -- --ignored --nocapture
//! ```
//!
//! HTTP/1.1 connections that churn, persistent h2c connections, SSE streams and
//! WebSocket sessions all run at once against a server with a small
//! `max_connections`. Every answer must be right, and afterwards the slots must
//! all be back and shutdown prompt.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, Event, Json, Method, Sse, WebSocketUpgrade};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message as Wire};

#[derive(Serialize, Deserialize, oas_rs::ApiSchema)]
struct Echo {
    n: u64,
    text: String,
}

async fn echo(Json(body): Json<Echo>) -> Json<Echo> {
    Json(body)
}

async fn hello() -> &'static str {
    "hello"
}

async fn big() -> String {
    "x".repeat(256 * 1024)
}

struct Events(mpsc::Receiver<Event>);

impl futures_core::Stream for Events {
    type Item = Event;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Event>> {
        self.0.poll_recv(context)
    }
}

async fn events() -> Sse<Events> {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        for n in 0..8 {
            tokio::time::sleep(Duration::from_millis(15)).await;
            if tx.send(Event::data(n.to_string())).await.is_err() {
                return;
            }
        }
    });
    Sse::new(Events(rx))
}

fn seconds() -> u64 {
    std::env::var("OAS_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10)
}

#[derive(Default)]
struct Tally {
    ok: AtomicU64,
    failed: AtomicU64,
}

impl Tally {
    fn check(&self, good: bool, what: &str) {
        if good {
            self.ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed);
            eprintln!("soak failure: {what}");
        }
    }
}

async fn h1_churn(addr: std::net::SocketAddr, running: Arc<AtomicBool>, tally: Arc<Tally>) {
    while running.load(Ordering::Relaxed) {
        let Ok(mut stream) = TcpStream::connect(addr).await else {
            tally.check(false, "h1 connect");
            continue;
        };
        let request = b"GET /hello HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n";
        let mut out = String::new();
        let good = stream.write_all(request).await.is_ok()
            && tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut out))
                .await
                .is_ok()
            && out.starts_with("HTTP/1.1 200")
            && out.ends_with("hello");
        tally.check(good, "h1 churn response");
        // Each closed connection leaves a TIME_WAIT socket on the client side
        // (about two minutes on Windows, with only 16k dynamic ports): paced to
        // stay under that, a long soak measures the server, not the OS.
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn h2_client(
    addr: std::net::SocketAddr,
) -> hyper::client::conn::http2::SendRequest<Full<Bytes>> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

async fn h2_worker(addr: std::net::SocketAddr, running: Arc<AtomicBool>, tally: Arc<Tally>) {
    let mut sender = h2_client(addr).await;
    let mut n = 0u64;
    while running.load(Ordering::Relaxed) {
        n += 1;
        let body = serde_json::to_vec(&serde_json::json!({ "n": n, "text": "soak" })).unwrap();
        let request = Request::builder()
            .method("POST")
            .uri("http://t/echo")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let good = match sender.send_request(request).await {
            Ok(response) => {
                response.status() == 200
                    && response
                        .into_body()
                        .collect()
                        .await
                        .map(|collected| {
                            let echoed: Result<Echo, _> =
                                serde_json::from_slice(&collected.to_bytes());
                            echoed.is_ok_and(|echo| echo.n == n)
                        })
                        .unwrap_or(false)
            }
            Err(_) => false,
        };
        tally.check(good, "h2 echo");
        if n & 15 == 0 {
            let request = Request::builder()
                .uri("http://t/big")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let good = match sender.send_request(request).await {
                Ok(response) => response
                    .into_body()
                    .collect()
                    .await
                    .is_ok_and(|collected| collected.to_bytes().len() == 256 * 1024),
                Err(_) => false,
            };
            tally.check(good, "h2 big body");
        }
    }
}

async fn sse_worker(addr: std::net::SocketAddr, running: Arc<AtomicBool>, tally: Arc<Tally>) {
    let mut sender = h2_client(addr).await;
    while running.load(Ordering::Relaxed) {
        let request = Request::builder()
            .uri("http://t/events")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let good = match sender.send_request(request).await {
            Ok(response) => response
                .into_body()
                .collect()
                .await
                .is_ok_and(|collected| collected.to_bytes().ends_with(b"data: 7\n\n")),
            Err(_) => false,
        };
        tally.check(good, "sse stream");
    }
}

async fn ws_worker(addr: std::net::SocketAddr, running: Arc<AtomicBool>, tally: Arc<Tally>) {
    while running.load(Ordering::Relaxed) {
        let Ok(tcp) = TcpStream::connect(addr).await else {
            tally.check(false, "ws connect");
            continue;
        };
        let request = format!("ws://{addr}/ws").into_client_request().unwrap();
        let Ok((mut client, _)) = tokio_tungstenite::client_async(request, tcp).await else {
            tally.check(false, "ws handshake");
            continue;
        };
        let mut good = true;
        for n in 0..10 {
            let text = format!("m{n}");
            good &= client.send(Wire::text(text.clone())).await.is_ok();
            good &=
                matches!(client.next().await, Some(Ok(Wire::Text(got))) if got == text.as_str());
        }
        let _ = client.close(None).await;
        tally.check(good, "ws echo");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
#[ignore = "long-running; see the module docs"]
async fn mixed_load_for_a_while_is_always_correct_and_returns_every_slot() {
    let mut app = App::new();
    app.get("/hello", hello);
    app.get("/big", big);
    app.post("/echo", echo);
    app.get("/events", events);
    app.raw(
        Method::GET,
        "/ws",
        |request: Request<Incoming>| async move {
            WebSocketUpgrade::new(request).on_upgrade(|mut socket| async move {
                while let Some(Ok(message)) = socket.recv().await {
                    if matches!(message, oas_rs::Message::Close(_)) {
                        break;
                    }
                    if socket.send(message).await.is_err() {
                        break;
                    }
                }
            })
        },
    );
    // Workers hold at most 12 + 8 + 4 + 4 connections; the cap leaves little
    // slack, so a leaked slot shows up as failures quickly.
    let runtime = app
        .build()
        .unwrap()
        .h2c(true)
        .max_connections(Some(32))
        .header_read_timeout(Some(Duration::from_secs(5)))
        .shutdown_timeout(Duration::from_secs(3));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
        Instant::now()
    });

    let running = Arc::new(AtomicBool::new(true));
    let tally = Arc::new(Tally::default());
    let mut workers = Vec::new();
    for _ in 0..12 {
        workers.push(tokio::spawn(h1_churn(
            addr,
            Arc::clone(&running),
            Arc::clone(&tally),
        )));
    }
    for _ in 0..8 {
        workers.push(tokio::spawn(h2_worker(
            addr,
            Arc::clone(&running),
            Arc::clone(&tally),
        )));
    }
    for _ in 0..4 {
        workers.push(tokio::spawn(sse_worker(
            addr,
            Arc::clone(&running),
            Arc::clone(&tally),
        )));
        workers.push(tokio::spawn(ws_worker(
            addr,
            Arc::clone(&running),
            Arc::clone(&tally),
        )));
    }

    tokio::time::sleep(Duration::from_secs(seconds())).await;
    running.store(false, Ordering::Relaxed);
    for worker in workers {
        worker.await.unwrap();
    }
    let ok = tally.ok.load(Ordering::Relaxed);
    let failed = tally.failed.load(Ordering::Relaxed);
    eprintln!(
        "soak: {ok} correct answers, {failed} failures in {} s",
        seconds()
    );
    assert_eq!(failed, 0, "{failed} of {} operations failed", ok + failed);
    assert!(ok > 100, "the soak did almost nothing: {ok}");

    // Every slot is back: a burst up to the cap is all served.
    let mut burst = Vec::new();
    for _ in 0..32 {
        burst.push(tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET /hello HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut out = String::new();
            let _ =
                tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut out)).await;
            out.starts_with("HTTP/1.1 200")
        }));
    }
    for request in burst {
        assert!(request.await.unwrap(), "a slot was leaked");
    }

    let started = Instant::now();
    let _ = stop.send(());
    let finished = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("shutdown hung")
        .unwrap();
    assert!(
        finished.duration_since(started) < Duration::from_secs(4),
        "shutdown took {:?}",
        finished.duration_since(started)
    );
}
