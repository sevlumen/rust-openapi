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

async fn closed_within(stream: &mut TcpStream, limit: Duration) -> bool {
    let mut buffer = [0u8; 64];
    matches!(
        tokio::time::timeout(limit, stream.read(&mut buffer)).await,
        Ok(Ok(0)) | Ok(Err(_))
    )
}

#[tokio::test]
async fn a_silent_connection_is_closed_by_the_header_timeout_with_h2c() {
    let (addr, stop) = start(|runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let mut silent = TcpStream::connect(addr).await.unwrap();
    assert!(
        closed_within(&mut silent, Duration::from_secs(2)).await,
        "a connection that sends nothing must not live forever"
    );
    let mut partial = TcpStream::connect(addr).await.unwrap();
    partial.write_all(b"PRI * HT").await.unwrap();
    assert!(
        closed_within(&mut partial, Duration::from_secs(2)).await,
        "a partial h2 preface must time out too"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn silent_h2c_connections_do_not_exhaust_max_connections() {
    let (addr, stop) = start(|runtime| {
        runtime
            .h2c(true)
            .max_connections(Some(2))
            .header_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let _a = TcpStream::connect(addr).await.unwrap();
    let _b = TcpStream::connect(addr).await.unwrap();
    // Both slots are held by silent clients until the timeout frees them.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (status, _, _) = tokio::time::timeout(Duration::from_secs(3), h2_get(addr))
        .await
        .expect("slots were never released")
        .unwrap();
    assert_eq!(status, 200);
    let _ = stop.send(());
}

#[tokio::test]
async fn idle_h2c_connections_end_quietly_at_shutdown() {
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorder = std::sync::Arc::clone(&errors);
    let (addr, stop) = start(move |runtime| {
        runtime
            .h2c(true)
            .header_read_timeout(None)
            .shutdown_timeout(Duration::from_secs(5))
            .on_connection_error(move |error| recorder.lock().unwrap().push(error.to_string()))
    })
    .await;
    let mut idle = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    let _ = stop.send(());
    assert!(
        closed_within(&mut idle, Duration::from_secs(2)).await,
        "an idle connection should be closed when shutdown starts"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        errors.lock().unwrap().is_empty(),
        "{:?}",
        errors.lock().unwrap()
    );
}
