//! HTTP/1.1 clients that stop reading a response must not hold a connection
//! slot for ever: `send_timeout` drops a connection whose socket has not
//! accepted a write for that long. (The HTTP/2 counterpart is in
//! `h2_lifecycle.rs`; the exact timer logic has deterministic unit tests in
//! `runtime.rs`.)
//!
//! The two tests that need a socket that really fills up are skipped on
//! Windows: its loopback absorbs a 96 MiB response whole, so nothing blocks.

use std::time::Duration;

use oas_rs::{App, AppRuntime};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn hello() -> &'static str {
    "hello"
}

/// Far more than the kernel's socket buffers on both ends can hold.
async fn huge() -> String {
    "x".repeat(96 * 1024 * 1024)
}

async fn start(
    configure: impl FnOnce(AppRuntime) -> AppRuntime,
) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let mut app = App::new();
    app.get("/", hello);
    app.get("/huge", huge);
    let runtime = configure(app.build().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
    });
    (addr, stop)
}

async fn get(addr: std::net::SocketAddr, path: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    stream
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "Windows loopback absorbs the whole response, so no write ever blocks"
)]
async fn a_client_that_never_reads_a_big_response_loses_its_slot() {
    let (addr, stop) = start(|runtime| {
        runtime
            .max_connections(Some(1))
            .send_timeout(Some(Duration::from_millis(500)))
    })
    .await;
    // Asks for 96 MiB and never reads a byte.
    let _stalled = get(addr, "/huge").await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let mut second = get(addr, "/").await;
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(3), second.read_to_string(&mut out))
        .await
        .expect("a client that never reads kept the only connection slot")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = stop.send(());
}

#[tokio::test]
async fn a_client_that_reads_steadily_is_never_cut_off() {
    let (addr, stop) =
        start(|runtime| runtime.send_timeout(Some(Duration::from_millis(500)))).await;
    let mut stream = get(addr, "/huge").await;
    // A slow reader: 1 MiB at a time with a pause shorter than the limit. It
    // reads for ~3 s, far longer than `send_timeout`, and must never be cut.
    let mut buffer = vec![0u8; 1024 * 1024];
    let started = std::time::Instant::now();
    let mut total = 0usize;
    while started.elapsed() < Duration::from_secs(3) {
        let read = stream
            .read(&mut buffer)
            .await
            .expect("cut off while reading");
        assert!(read > 0, "the server closed after {total} bytes");
        total += read;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(total > 0);
    let _ = stop.send(());
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "Windows loopback absorbs the whole response, so no write ever blocks"
)]
async fn send_timeout_none_disables_the_limit() {
    let (addr, stop) = start(|runtime| runtime.max_connections(Some(1)).send_timeout(None)).await;
    let _stalled = get(addr, "/huge").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // The only slot is still held by the stalled client, so this waits.
    let mut second = get(addr, "/").await;
    let mut out = String::new();
    let result =
        tokio::time::timeout(Duration::from_millis(1500), second.read_to_string(&mut out)).await;
    assert!(
        result.is_err(),
        "send_timeout(None) should not drop the stalled client: {out}"
    );
    let _ = stop.send(());
}
