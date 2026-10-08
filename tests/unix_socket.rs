#![cfg(unix)]

use std::time::Duration;

use oas_rs::App;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::oneshot,
};

async fn hello() -> &'static str {
    "hello"
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

fn socket_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("oas-rs-{}-{name}.sock", std::process::id()))
}

async fn get(path: &std::path::Path, target: &str) -> String {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn serves_http_over_a_unix_socket() {
    let path = socket_path("basic");
    let _ = std::fs::remove_file(&path);
    let mut app = App::new();
    app.get("/", hello);
    let runtime = app.build().unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_unix(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let response = get(&path, "/").await;
    assert!(
        response.starts_with("HTTP/1.1 200") && response.ends_with("hello"),
        "{response}"
    );
    let missing = get(&path, "/nope").await;
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    let _ = stop.send(());
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn shutdown_waits_for_an_in_flight_unix_request() {
    let path = socket_path("shutdown");
    let _ = std::fs::remove_file(&path);
    let mut app = App::new();
    app.get("/slow", slow);
    let runtime = app.build().unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        runtime
            .serve_unix(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = {
        let path = path.clone();
        tokio::spawn(async move { get(&path, "/slow").await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = stop.send(());
    let response = client.await.unwrap();
    assert!(response.ends_with("done"), "{response}");
    server.await.unwrap();
    let _ = std::fs::remove_file(&path);
}
