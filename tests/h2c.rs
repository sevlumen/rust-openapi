use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, AppRuntime};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn hello() -> &'static str {
    "hello"
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

async fn start(
    configure: impl FnOnce(AppRuntime) -> AppRuntime,
) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let mut app = App::new();
    app.get("/", hello);
    app.get("/slow", slow);
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

async fn h2_get(addr: std::net::SocketAddr) -> Result<(u16, http::Version, String), hyper::Error> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp)).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await?;
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await?.to_bytes();
    Ok((
        parts.status.as_u16(),
        parts.version,
        String::from_utf8(bytes.to_vec()).unwrap(),
    ))
}

#[tokio::test]
async fn h2c_serves_http2_with_prior_knowledge() {
    let (addr, stop) = start(|runtime| runtime.h2c(true)).await;
    let (status, version, body) = h2_get(addr).await.unwrap();
    assert_eq!(
        (status, version, body.as_str()),
        (200, http::Version::HTTP_2, "hello")
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn http11_still_works_on_an_h2c_listener() {
    let (addr, stop) = start(|runtime| runtime.h2c(true)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with("hello"),
        "{out}"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn without_h2c_a_prior_knowledge_client_gets_no_http2() {
    let (addr, stop) = start(|runtime| runtime).await;
    let outcome = tokio::time::timeout(Duration::from_secs(3), h2_get(addr)).await;
    assert!(
        !matches!(outcome, Ok(Ok(_))),
        "h2c must be opt-in: {outcome:?}"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn h2c_respects_the_stream_limit() {
    let (addr, stop) =
        start(|runtime| runtime.h2c(true).http2_max_concurrent_streams(Some(1))).await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let mut sender = sender.clone();
        tasks.push(tokio::spawn(async move {
            let request = http::Request::builder()
                .uri("http://localhost/slow")
                .body(Full::new(Bytes::new()))
                .unwrap();
            sender
                .send_request(request)
                .await
                .map(|r| r.status().as_u16())
        }));
    }
    let mut served = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(200) => served += 1,
            Ok(other) => panic!("unexpected {other}"),
            Err(_) => refused += 1,
        }
    }
    assert_eq!(served, 1, "one stream at a time is served");
    assert_eq!(refused, 2, "the others are refused");
    let _ = stop.send(());
}

#[tokio::test]
async fn an_h2c_listener_shuts_down_gracefully() {
    let (addr, stop) = start(|runtime| {
        runtime
            .h2c(true)
            .shutdown_timeout(Duration::from_millis(200))
    })
    .await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .uri("http://localhost/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(sender.send_request(request).await.unwrap().status(), 200);
    let _ = stop.send(());
    // The idle h2 connection is closed by the server's graceful shutdown.
    tokio::time::timeout(Duration::from_secs(3), driver)
        .await
        .expect("the connection stayed open after shutdown")
        .unwrap();
}
