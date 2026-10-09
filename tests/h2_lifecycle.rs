//! HTTP/2 connection lifecycle: a silent or idle peer must neither hold up
//! shutdown nor pin a `max_connections` slot.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, AppRuntime};
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

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(800)).await;
    "done"
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
