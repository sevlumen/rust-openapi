//! Moderate-load correctness: many concurrent clients, every answer right, no
//! leaked connection slots. Sizes are chosen to finish in a few seconds on a
//! laptop; they exercise concurrency, not peak throughput.

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{
    ApiError, ApiSchema, App, AppRuntime, Json, Message, Method, Multipart, WebSocketUpgrade,
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message as Wire};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Echo {
    n: u32,
    text: String,
}

async fn echo(Json(body): Json<Echo>) -> Json<Echo> {
    Json(body)
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(20)).await;
    "done"
}

fn app() -> App {
    let mut app = App::new();
    app.post("/echo", echo);
    app.get("/slow", slow);
    app.raw(
        Method::GET,
        "/ws",
        |request: Request<Incoming>| async move {
            WebSocketUpgrade::new(request).on_upgrade(|mut socket| async move {
                while let Some(Ok(message)) = socket.recv().await {
                    match message {
                        Message::Text(text) => {
                            if socket.send(Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            })
        },
    );
    app.raw(
        Method::POST,
        "/upload",
        |request: Request<Incoming>| async move {
            let mut form = Multipart::from_stream(request, 64 * 1024 * 1024)?;
            let mut total = 0usize;
            while let Some(mut field) = form.next_field().await? {
                while let Some(chunk) = field.chunk().await? {
                    total += chunk.len();
                }
            }
            Ok::<_, ApiError>(total.to_string())
        },
    );
    app
}

async fn serve(
    configure: impl FnOnce(AppRuntime) -> AppRuntime,
) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let runtime = configure(app().build().unwrap());
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

#[tokio::test]
async fn many_keep_alive_clients_get_correct_answers() {
    let (addr, stop) = serve(|runtime| runtime).await;
    let mut tasks = Vec::new();
    for client in 0..100u32 {
        tasks.push(tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            for request in 0..30u32 {
                let n = client * 1000 + request;
                let body = format!("{{\"n\":{n},\"text\":\"c{client}\"}}");
                stream
                    .write_all(
                        format!(
                            "POST /echo HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let mut seen = Vec::new();
                let mut buffer = [0u8; 512];
                while !seen.ends_with(body.as_bytes()) {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0, "connection closed early");
                    seen.extend_from_slice(&buffer[..read]);
                }
                assert!(String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 200"));
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn http2_multiplexes_many_streams_on_few_connections() {
    let (addr, stop) = serve(|runtime| runtime.h2c(true)).await;
    let mut connections = Vec::new();
    for _ in 0..4 {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
                .await
                .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        connections.push(sender);
    }
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for index in 0..400usize {
        let mut sender = connections[index % connections.len()].clone();
        tasks.push(tokio::spawn(async move {
            let request = http::Request::builder()
                .uri("http://localhost/slow")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            let status = response.status().as_u16();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, body)
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!((status, &body[..]), (200, &b"done"[..]));
    }
    // 400 requests of 20 ms over 4 connections of up to 200 streams: well under
    // the 8 s a serial server would need.
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn many_websocket_sessions_echo_independently() {
    let (addr, stop) = serve(|runtime| runtime).await;
    let mut tasks = Vec::new();
    for session in 0..150u32 {
        tasks.push(tokio::spawn(async move {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let request = format!("ws://{addr}/ws").into_client_request().unwrap();
            let (mut client, _) = tokio_tungstenite::client_async(request, tcp).await.unwrap();
            for message in 0..10u32 {
                let text = format!("s{session}-m{message}");
                client.send(Wire::text(text.clone())).await.unwrap();
                assert_eq!(client.next().await.unwrap().unwrap(), Wire::text(text));
            }
            client.close(None).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn concurrent_streaming_uploads_all_complete_with_the_right_size() {
    let (addr, stop) = serve(|runtime| runtime.body_read_timeout(None)).await;
    const BOUNDARY: &str = "XB";
    let data = vec![5u8; 3 * 1024 * 1024];
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let data = data.clone();
        tasks.push(tokio::spawn(async move {
            let head = format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\n\r\n"
            );
            let tail = format!("\r\n--{BOUNDARY}--\r\n");
            let length = head.len() + data.len() + tail.len();
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(
                    format!(
                        "POST /upload HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\nContent-Length: {length}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&data).await.unwrap();
            stream.write_all(tail.as_bytes()).await.unwrap();
            let mut out = String::new();
            tokio::time::timeout(Duration::from_secs(20), stream.read_to_string(&mut out))
                .await
                .expect("upload timed out")
                .unwrap();
            assert!(out.starts_with("HTTP/1.1 200"), "{out}");
            assert!(out.ends_with(&data.len().to_string()), "{out}");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn the_connection_limit_is_respected_and_slots_are_returned_under_churn() {
    let (addr, stop) = serve(|runtime| runtime.max_connections(Some(8))).await;
    let mut tasks = Vec::new();
    for _ in 0..200u32 {
        tasks.push(tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET /slow HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut out = String::new();
            tokio::time::timeout(Duration::from_secs(20), stream.read_to_string(&mut out))
                .await
                .expect("a request never finished: a slot leaked")
                .unwrap();
            assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn an_upload_dropped_halfway_does_not_leak_its_slot_or_hang_the_handler() {
    let (addr, stop) =
        serve(|runtime| runtime.max_connections(Some(1)).body_read_timeout(None)).await;
    const BOUNDARY: &str = "XB";
    for _ in 0..5 {
        let head = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\n\r\n"
        );
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!(
                    "POST /upload HTTP/1.1\r\nHost: t\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\nContent-Length: 10000000\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(&vec![1u8; 500_000]).await.unwrap();
        drop(stream); // the client vanishes mid-upload
    }
    // The single slot must come back: an ordinary request is served.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut out))
        .await
        .expect("the slot of an aborted upload was never released")
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = stop.send(());
}

/// A request body that sends a few bytes and then never finishes.
struct Stalled(bool);

impl http_body::Body for Stalled {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if self.0 {
            return std::task::Poll::Pending;
        }
        self.0 = true;
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(
            b"{\"na",
        )))))
    }
}

#[tokio::test]
async fn a_slow_http2_body_is_cut_off_by_the_body_timeout() {
    let (addr, stop) = serve(|runtime| {
        runtime
            .h2c(true)
            .body_read_timeout(Some(Duration::from_millis(300)))
    })
    .await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/echo")
        .header("content-type", "application/json")
        .body(Stalled(false))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("the stalled HTTP/2 body held the stream open")
        .unwrap();
    assert_eq!(response.status(), 408);
    let _ = stop.send(());
}

#[tokio::test]
async fn many_silent_websocket_sessions_all_end_at_their_idle_timeout() {
    let mut app = App::new();
    app.raw(
        Method::GET,
        "/ws",
        |request: Request<Incoming>| async move {
            WebSocketUpgrade::new(request)
                .idle_timeout(Duration::from_millis(300))
                .on_upgrade(
                    |mut socket| async move { while let Some(Ok(_)) = socket.recv().await {} },
                )
        },
    );
    let runtime = app.build().unwrap();
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
    let mut tasks = Vec::new();
    for _ in 0..100 {
        tasks.push(tokio::spawn(async move {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let request = format!("ws://{addr}/ws").into_client_request().unwrap();
            let (mut client, _) = tokio_tungstenite::client_async(request, tcp).await.unwrap();
            // Say nothing: the server must end the session by itself.
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(message) = client.next().await {
                    if message.is_err() {
                        break;
                    }
                }
            })
            .await
            .expect("a silent session outlived its idle timeout");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let _ = stop.send(());
}
