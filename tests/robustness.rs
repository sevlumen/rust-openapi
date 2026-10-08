use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use oas_rs::{App, AppRuntime, CatchPanic};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

async fn hello() -> &'static str {
    "hello"
}

async fn boom() -> &'static str {
    panic!("secret internal detail");
}

async fn start(runtime: AppRuntime) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    (addr, stop, server)
}

fn app() -> App {
    let mut app = App::new();
    app.get("/", hello);
    app.get("/boom", boom);
    app
}

/// Reads until the peer closes or `limit` passes; returns what arrived.
async fn read_until_closed(stream: &mut TcpStream, limit: Duration) -> Option<String> {
    let mut received = Vec::new();
    let mut buffer = [0u8; 1024];
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            read = stream.read(&mut buffer) => match read {
                Ok(0) | Err(_) => return Some(String::from_utf8_lossy(&received).into_owned()),
                Ok(n) => received.extend_from_slice(&buffer[..n]),
            },
            _ = &mut deadline => return None,
        }
    }
}

#[tokio::test]
async fn header_read_timeout_closes_a_stalled_request() {
    let runtime = app()
        .build()
        .unwrap()
        .header_read_timeout(Some(Duration::from_millis(200)));
    let (addr, stop, server) = start(runtime).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    // Headers never finish: a slowloris client.
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n")
        .await
        .unwrap();
    let outcome = read_until_closed(&mut stream, Duration::from_secs(3)).await;
    assert!(outcome.is_some(), "server kept a stalled request open");

    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn header_read_timeout_can_be_disabled() {
    let runtime = app().build().unwrap().header_read_timeout(None);
    let (addr, stop, server) = start(runtime).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n")
        .await
        .unwrap();
    assert!(
        read_until_closed(&mut stream, Duration::from_millis(600))
            .await
            .is_none(),
        "with the timeout disabled the connection must stay open"
    );
    // Finishing the request still works.
    stream
        .write_all(b"Connection: close\r\n\r\n")
        .await
        .unwrap();
    let response = read_until_closed(&mut stream, Duration::from_secs(3))
        .await
        .expect("response after slow headers");
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");

    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn default_header_read_timeout_is_thirty_seconds() {
    assert_eq!(oas_rs::DEFAULT_HEADER_READ_TIMEOUT, Duration::from_secs(30));
}

async fn keep_alive_request(stream: &mut TcpStream) -> String {
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0u8; 512];
    let n = stream.read(&mut buffer).await.unwrap();
    String::from_utf8_lossy(&buffer[..n]).into_owned()
}

#[tokio::test]
async fn max_connections_holds_extra_clients_until_a_slot_frees() {
    let runtime = app()
        .build()
        .unwrap()
        .header_read_timeout(None)
        .max_connections(Some(1));
    let (addr, stop, server) = start(runtime).await;

    let mut first = TcpStream::connect(addr).await.unwrap();
    assert!(
        keep_alive_request(&mut first)
            .await
            .starts_with("HTTP/1.1 200")
    );

    // The second client connects (the kernel queues it) but is not served.
    let mut second = TcpStream::connect(addr).await.unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    assert!(
        read_until_closed(&mut second, Duration::from_millis(400))
            .await
            .is_none(),
        "the connection over the limit was served"
    );

    drop(first);
    let response = read_until_closed(&mut second, Duration::from_secs(3))
        .await
        .expect("second client served after the slot freed");
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");

    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn max_connections_does_not_block_shutdown() {
    let runtime = app()
        .build()
        .unwrap()
        .header_read_timeout(None)
        .max_connections(Some(1))
        .shutdown_timeout(Duration::from_millis(200));
    let (addr, stop, server) = start(runtime).await;

    let mut first = TcpStream::connect(addr).await.unwrap();
    assert!(
        keep_alive_request(&mut first)
            .await
            .starts_with("HTTP/1.1 200")
    );
    // The accept loop is now waiting for a free slot; shutdown must still win.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("serve_listener did not return while at the connection limit")
        .unwrap();
}

#[tokio::test]
async fn catch_panic_turns_a_handler_panic_into_500() {
    let mut app = app();
    app.layer(CatchPanic::new());
    let runtime = app.build().unwrap();

    let response = runtime
        .oneshot(oas_rs::Method::GET, "/boom", &[], None)
        .await;
    assert_eq!(response.status(), 500);
    let body = response.body_string().await;
    assert!(body.contains("Internal Server Error"), "got: {body}");
    assert!(!body.contains("secret"), "panic message leaked: {body}");

    // A healthy route through the same layer is untouched.
    let ok = runtime.oneshot(oas_rs::Method::GET, "/", &[], None).await;
    assert_eq!(ok.status(), 200);
}

#[tokio::test]
async fn catch_panic_keeps_the_connection_serving() {
    let mut app = app();
    app.layer(CatchPanic::new());
    let (addr, stop, server) = start(app.build().unwrap().header_read_timeout(None)).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /boom HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await.unwrap();
    let first = String::from_utf8_lossy(&buffer[..n]).into_owned();
    assert!(first.starts_with("HTTP/1.1 500"), "got: {first}");
    // Same connection, next request.
    assert!(
        keep_alive_request(&mut stream)
            .await
            .starts_with("HTTP/1.1 200")
    );

    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn catch_panic_reports_the_panic_message_to_the_hook() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let mut app = app();
    app.layer(CatchPanic::with_hook(move |message| {
        sink.lock().unwrap().push(message.to_owned());
    }));
    let runtime = app.build().unwrap();
    let response = runtime
        .oneshot(oas_rs::Method::GET, "/boom", &[], None)
        .await;
    assert_eq!(response.status(), 500);
    assert_eq!(*seen.lock().unwrap(), ["secret internal detail"]);
}

#[tokio::test]
async fn connection_errors_reach_the_observer() {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&errors);
    let runtime = app()
        .build()
        .unwrap()
        .on_connection_error(move |error| sink.lock().unwrap().push(error.to_string()));
    let (addr, stop, server) = start(runtime).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"\x00\x01 not http\r\n\r\n")
        .await
        .unwrap();
    let _ = read_until_closed(&mut stream, Duration::from_secs(2)).await;

    let _ = stop.send(());
    server.await.unwrap();
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn catch_panic_covers_a_panic_while_building_the_downstream_future() {
    let mut app = app();
    app.layer(CatchPanic::new());
    // Panics synchronously, before returning its future.
    app.layer(
        |_request: http::Request<oas_rs::RequestBody>, _next: oas_rs::Next| {
            if true {
                panic!("sync layer failure");
            }
            async { unreachable!() }
        },
    );
    let runtime = app.build().unwrap();
    let response = runtime.oneshot(oas_rs::Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 500);
}
