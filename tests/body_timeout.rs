use std::time::{Duration, Instant};

use oas_rs::{ApiSchema, App, AppRuntime, Json};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
}

async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

async fn hello() -> &'static str {
    "hello"
}

async fn start(
    configure: impl FnOnce(AppRuntime) -> AppRuntime,
) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let mut app = App::new();
    app.post("/items", create);
    app.get("/", hello);
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

/// Sends a head promising `declared` bytes and `sent` of them, then waits.
async fn stalled_upload(
    addr: std::net::SocketAddr,
    declared: usize,
    sent: &[u8],
    wait: Duration,
) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /items HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream.write_all(sent).await.unwrap();
    let mut out = String::new();
    let _ = tokio::time::timeout(wait, stream.read_to_string(&mut out)).await;
    out
}

#[test]
fn the_default_body_timeout_is_a_minute() {
    assert_eq!(oas_rs::DEFAULT_BODY_READ_TIMEOUT, Duration::from_secs(60));
}

#[tokio::test]
async fn a_stalled_body_gets_408_instead_of_holding_the_connection() {
    let (addr, stop) =
        start(|runtime| runtime.body_read_timeout(Some(Duration::from_millis(300)))).await;
    let started = Instant::now();
    let response = stalled_upload(addr, 100, b"{\"na", Duration::from_secs(5)).await;
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn a_body_that_arrives_in_time_is_unaffected() {
    let (addr, stop) =
        start(|runtime| runtime.body_read_timeout(Some(Duration::from_millis(500)))).await;
    let body = "{\"name\":\"ada\"}";
    let response = stalled_upload(addr, body.len(), body.as_bytes(), Duration::from_secs(2)).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("{\"name\":\"ada\"}"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn requests_without_a_body_do_not_wait_on_the_timer() {
    let (addr, stop) =
        start(|runtime| runtime.body_read_timeout(Some(Duration::from_millis(50)))).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = stop.send(());
}

#[tokio::test]
async fn the_timeout_can_be_disabled() {
    let (addr, stop) = start(|runtime| runtime.body_read_timeout(None)).await;
    let response = stalled_upload(addr, 100, b"{\"na", Duration::from_millis(700)).await;
    assert!(
        response.is_empty(),
        "no response is expected while the body stalls: {response}"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn a_stalled_upload_through_a_layer_is_cut_off_too() {
    let mut app = App::new();
    app.post("/items", create);
    app.layer(
        |request: http::Request<oas_rs::RequestBody>, next: oas_rs::Next| async move {
            next.run(request).await
        },
    );
    let runtime = app
        .build()
        .unwrap()
        .body_read_timeout(Some(Duration::from_millis(300)));
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
    let response = stalled_upload(addr, 100, b"{\"na", Duration::from_secs(5)).await;
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    let _ = stop.send(());
}
