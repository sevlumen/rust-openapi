use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use oas_rs::App;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

async fn request(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn shutdown_waits_for_in_flight_requests() {
    let mut app = App::new();
    app.get("/slow", slow);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        Instant::now()
    });
    let client = tokio::spawn(async move { request(addr, "/slow").await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let shutdown_at = Instant::now();
    shutdown_tx.send(()).unwrap();

    let response = client.await.unwrap();
    let server_returned_at = server.await.unwrap();

    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(response.ends_with("done"), "got: {response}");
    assert!(
        server_returned_at.duration_since(shutdown_at) >= Duration::from_millis(150),
        "serve_listener returned {:?} after shutdown, before the in-flight request finished",
        server_returned_at.duration_since(shutdown_at)
    );
}

async fn very_slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "never"
}

#[tokio::test]
async fn shutdown_timeout_bounds_the_wait_for_stuck_requests() {
    let mut app = App::new();
    app.get("/stuck", very_slow);
    let runtime = app
        .build()
        .unwrap()
        .shutdown_timeout(Duration::from_millis(100));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        Instant::now()
    });
    let _client = tokio::spawn(async move { request(addr, "/stuck").await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let shutdown_at = Instant::now();
    shutdown_tx.send(()).unwrap();

    let server_returned_at = server.await.unwrap();
    let waited = server_returned_at.duration_since(shutdown_at);
    assert!(
        waited < Duration::from_secs(2),
        "serve_listener waited {waited:?} despite a 100ms shutdown timeout"
    );
}

#[tokio::test]
async fn idle_keep_alive_connections_do_not_block_shutdown() {
    let mut app = App::new();
    app.get("/slow", slow);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        Instant::now()
    });

    // Open a keep-alive connection, finish one request, then leave it idle.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0u8; 512];
    let mut seen = Vec::new();
    while !seen.ends_with(b"done") {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0, "connection closed before the response completed");
        seen.extend_from_slice(&buffer[..read]);
    }

    let shutdown_at = Instant::now();
    shutdown_tx.send(()).unwrap();
    let server_returned_at = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("an idle keep-alive connection blocked shutdown")
        .unwrap();
    assert!(server_returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
}
