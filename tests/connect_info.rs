use std::time::Duration;

use oas_rs::{App, AppRuntime, ConnectInfo, RateLimit};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn who(ConnectInfo(addr): ConnectInfo) -> String {
    format!("{}", addr.ip())
}

async fn hello() -> &'static str {
    "hello"
}

async fn start(runtime: AppRuntime) -> (std::net::SocketAddr, oneshot::Sender<()>) {
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

async fn get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut out)).await;
    out
}

#[tokio::test]
async fn a_handler_can_read_the_peer_address() {
    let mut app = App::new();
    app.get("/who", who);
    let (addr, stop) = start(app.build().unwrap().connect_info(true)).await;
    let response = get(addr, "/who").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("127.0.0.1"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn without_opting_in_the_extractor_explains_itself() {
    let mut app = App::new();
    app.get("/who", who);
    let (addr, stop) = start(app.build().unwrap()).await;
    let response = get(addr, "/who").await;
    assert!(response.starts_with("HTTP/1.1 500"), "{response}");
    assert!(response.contains("connect_info"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn middleware_sees_the_peer_address_and_rate_limit_can_key_on_it() {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(RateLimit::new(1, Duration::from_secs(60)).key_by_peer_ip());
    let (addr, stop) = start(app.build().unwrap().connect_info(true)).await;
    assert!(get(addr, "/").await.starts_with("HTTP/1.1 200"));
    let limited = get(addr, "/").await;
    assert!(limited.starts_with("HTTP/1.1 429"), "{limited}");
    let _ = stop.send(());
}

#[tokio::test]
async fn in_process_requests_have_no_peer_address() {
    // In-process requests have no peer: the extractor reports it clearly.
    let mut app = App::new();
    app.get("/who", who);
    let runtime = app.build().unwrap().connect_info(true);
    let response = runtime
        .oneshot(oas_rs::Method::GET, "/who", &[], None)
        .await;
    assert_eq!(response.status(), 500);
}
