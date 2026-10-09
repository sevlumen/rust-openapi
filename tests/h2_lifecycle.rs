//! HTTP/2 connection lifecycle: a silent or idle peer must neither hold up
//! shutdown nor pin a `max_connections` slot.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, AppRuntime, Event, Sse};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
/// An empty SETTINGS frame.
const SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];

async fn hello() -> &'static str {
    "hello"
}

async fn tiny() -> String {
    "y".repeat(256)
}

async fn medium() -> String {
    "x".repeat(16 * 1024)
}

async fn big() -> String {
    "x".repeat(8 * 1024 * 1024)
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(800)).await;
    "done"
}

struct Events(tokio::sync::mpsc::Receiver<Event>);

impl futures_core::Stream for Events {
    type Item = Event;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Event>> {
        self.0.poll_recv(context)
    }
}

/// Sixteen events, 250 ms apart: the response outlives the handler by ~4 s.
async fn events() -> Sse<Events> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        for n in 0..16 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if tx.send(Event::data(n.to_string())).await.is_err() {
                return;
            }
        }
    });
    Sse::new(Events(rx))
}

/// One event after 1.5 s of silence: a quiet stream, not a stalled reader.
async fn quiet_events() -> Sse<Events> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = tx.send(Event::data("late")).await;
    });
    Sse::new(Events(rx))
}

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Instant>,
}

async fn start(configure: impl FnOnce(AppRuntime) -> AppRuntime) -> Server {
    let mut app = App::new();
    app.get("/", hello);
    app.get("/slow", slow);
    app.get("/big", big);
    app.get("/medium", medium);
    app.get("/tiny", tiny);
    app.get("/events", events);
    app.get("/quiet", quiet_events);
    let runtime = configure(app.build().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
        Instant::now()
    });
    Server {
        addr,
        stop: Some(stop),
        done,
    }
}

impl Server {
    /// Starts the shutdown and returns how long the server took to finish.
    async fn shut_down(&mut self) -> Duration {
        let started = Instant::now();
        let _ = self.stop.take().unwrap().send(());
        let finished = tokio::time::timeout(Duration::from_secs(15), &mut self.done)
            .await
            .expect("serve_listener never returned")
            .unwrap();
        finished.duration_since(started)
    }
}

async fn h2_peer(addr: std::net::SocketAddr, send_settings: bool) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(PREFACE).await.unwrap();
    if send_settings {
        stream.write_all(SETTINGS).await.unwrap();
    }
    stream
}

#[tokio::test]
async fn a_silent_h2_peer_does_not_hold_up_shutdown() {
    for send_settings in [false, true] {
        let mut server =
            start(|runtime| runtime.h2c(true).shutdown_timeout(Duration::from_secs(10))).await;
        // Never answers a PING, never closes.
        let _peer = h2_peer(server.addr, send_settings).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let took = server.shut_down().await;
        assert!(
            took < Duration::from_secs(5),
            "shutdown waited {took:?} for a silent peer (settings sent: {send_settings})"
        );
    }
}

#[tokio::test]
async fn an_idle_h2_connection_is_closed_after_the_idle_timeout_and_frees_its_slot() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .max_connections(Some(1))
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let _idle = h2_peer(server.addr, true).await;
    // The only slot is held by an idle HTTP/2 peer; it must be reclaimed.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(5), second.read_to_string(&mut out))
        .await
        .expect("the idle HTTP/2 connection kept its slot")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = server.shut_down().await;
}

#[tokio::test]
async fn a_busy_h2_stream_still_finishes_during_shutdown() {
    let mut server =
        start(|runtime| runtime.h2c(true).shutdown_timeout(Duration::from_secs(10))).await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/slow")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let pending = tokio::spawn(async move {
        let response = sender.send_request(request).await.unwrap();
        let status = response.status().as_u16();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, body)
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let took = server.shut_down().await;
    let (status, body) = pending.await.unwrap();
    assert_eq!((status, &body[..]), (200, &b"done"[..]));
    assert!(
        took >= Duration::from_millis(400),
        "shutdown did not wait for the request: {took:?}"
    );
}

#[tokio::test]
async fn a_streaming_response_is_not_idle_while_its_body_is_still_flowing() {
    // Idle timeout 300 ms, the stream lasts ~4 s: the connection must stay up.
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/events")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("stream was cut");
    let text = String::from_utf8(body.to_bytes().to_vec()).unwrap();
    assert!(text.contains("data: 15"), "stream cut short: {text:?}");
    let _ = server.shut_down().await;
}

/// Tiny flow-control windows (1 KiB): a buffered body larger than that cannot
/// be sent while the client sleeps, and the connection must not look idle
/// meanwhile, however small the body is compared with the default window.
async fn slow_client_still_gets(path: &str, length: usize) {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(200)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(1024)
        .initial_connection_window_size(1024)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri(format!("http://localhost{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    // The window fills, then the client stops reading for longer than the idle
    // timeout plus the shutdown grace.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let body = response
        .into_body()
        .collect()
        .await
        .expect("the response was cut while the client was slow")
        .to_bytes();
    assert_eq!(body.len(), length);
    let _ = server.shut_down().await;
}

#[tokio::test]
async fn a_large_buffered_response_is_not_idle_while_the_client_reads_slowly() {
    slow_client_still_gets("/big", 8 * 1024 * 1024).await;
}

/// A client that reads slowly but steadily (a frame every 300 ms, 1 KiB
/// windows, so the 16 KiB body takes ~5 s) is making progress, not idle: its
/// window updates are socket activity, even though the handler returned long ago.
#[tokio::test]
async fn a_slow_but_steady_reader_of_a_small_body_is_never_idle() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(200)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(1024)
        .initial_connection_window_size(1024)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/medium")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let mut body = sender.send_request(request).await.unwrap().into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("the response was cut while the client was still reading");
        received += frame.data_ref().map_or(0, Bytes::len);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(received, 16 * 1024);
    let _ = server.shut_down().await;
}

/// A PING frame (length 8, type 6, stream 0) with a fixed payload.
const PING: &[u8] = &[0, 0, 8, 6, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];

#[tokio::test]
async fn a_peer_that_only_sends_pings_does_not_keep_its_slot() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .max_connections(Some(1))
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let mut pinger = h2_peer(server.addr, true).await;
    // Ping far more often than the idle timeout, for much longer than it and
    // the shutdown grace together: no request is ever sent.
    let keep_pinging = tokio::spawn(async move {
        for _ in 0..60 {
            if pinger.write_all(PING).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = pinger.shutdown().await;
    });
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(3), second.read_to_string(&mut out))
        .await
        .expect("a PING-only peer kept the only connection slot")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    keep_pinging.abort();
    let _ = server.shut_down().await;
}

/// With an idle timeout longer than the reader's pace the connection never
/// starts closing, so it is still good for the next request afterwards (the
/// handler returned ~5 s earlier, far beyond the 1 s idle timeout).
#[tokio::test]
async fn a_connection_is_reusable_after_a_slow_steady_read() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_secs(1)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(1024)
        .initial_connection_window_size(1024)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/medium")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let mut body = sender.send_request(request).await.unwrap().into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("the response was cut while the client was still reading");
        received += frame.data_ref().map_or(0, Bytes::len);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(received, 16 * 1024);
    let again = http::Request::builder()
        .uri("http://localhost/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(again).await.unwrap();
    assert_eq!(response.status(), 200);
    let _ = server.shut_down().await;
}

/// A client that never reads a large response but keeps answering PINGs must
/// not hold its connection slot for ever: `send_timeout` frees it.
#[tokio::test]
async fn a_client_that_stops_reading_a_big_response_loses_its_slot() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .max_connections(Some(1))
            .header_read_timeout(Some(Duration::from_millis(200)))
            .send_timeout(Some(Duration::from_millis(500)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .keep_alive_interval(Duration::from_millis(100))
        .keep_alive_timeout(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .initial_stream_window_size(1024)
        .initial_connection_window_size(1024)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/big")
        .body(Full::new(Bytes::new()))
        .unwrap();
    // Keep the response, never read it; the client still answers PINGs.
    let _stalled = sender.send_request(request).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(3), second.read_to_string(&mut out))
        .await
        .expect("a stalled reader kept the only connection slot")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = server.shut_down().await;
}

/// Even with 16-byte windows (25-byte DATA frames) a steady reader is never
/// idle: progress is judged by HTTP/2 frame type, not by write size.
#[tokio::test]
async fn a_steady_reader_with_16_byte_windows_is_never_idle() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(100)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(16)
        .initial_connection_window_size(16)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/tiny")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let mut body = sender.send_request(request).await.unwrap().into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("the response was cut while the client was still reading");
        received += frame.data_ref().map_or(0, Bytes::len);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(received, 256);
    let _ = server.shut_down().await;
}

/// Several PINGs in one write are answered with several 17-byte acks that
/// may leave in one socket write (over any size threshold): still no progress.
#[tokio::test]
async fn a_burst_of_pings_does_not_keep_a_slot_either() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .max_connections(Some(1))
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let mut pinger = h2_peer(server.addr, true).await;
    let burst: Vec<u8> = PING.repeat(8);
    let keep_pinging = tokio::spawn(async move {
        for _ in 0..60 {
            if pinger.write_all(&burst).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = pinger.shutdown().await;
    });
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(3), second.read_to_string(&mut out))
        .await
        .expect("a PING-burst peer kept the only connection slot")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    keep_pinging.abort();
    let _ = server.shut_down().await;
}

/// `send_timeout` (400 ms here) measures a body waiting for the client, not a
/// stream with nothing to send: an SSE connection quiet for longer than that
/// is left alone.
#[tokio::test]
async fn a_quiet_stream_is_not_a_stalled_reader() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(200)))
            .send_timeout(Some(Duration::from_millis(400)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/quiet")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("quiet stream was cut");
    assert_eq!(
        &body.to_bytes()[..],
        b"data: late

"
    );
    let _ = server.shut_down().await;
}

/// One stream that nobody reads must not be kept alive by another stream on
/// the same connection that keeps moving: `send_timeout` is per stream.
#[tokio::test]
async fn an_active_stream_does_not_hide_a_stalled_one_on_the_same_connection() {
    let mut server = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(200)))
            .send_timeout(Some(Duration::from_millis(500)))
    })
    .await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    // Small per-stream windows only: the 8 MiB response stalls after 1 KiB,
    // the connection-level window stays wide for the other stream.
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(1024)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let stalled = http::Request::builder()
        .uri("http://localhost/big")
        .body(Full::new(Bytes::new()))
        .unwrap();
    // Held, never read.
    let _stalled = sender.send_request(stalled).await.unwrap();
    // A second stream that delivers an event every 250 ms for ~4 s.
    let active = http::Request::builder()
        .uri("http://localhost/events")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(active).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), response.into_body().collect())
        .await
        .expect("the active stream hung");
    assert!(
        outcome.is_err(),
        "the stalled stream was kept alive by the active one: the connection survived"
    );
    let _ = server.shut_down().await;
}
