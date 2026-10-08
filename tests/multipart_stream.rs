use std::{net::SocketAddr, time::Duration};

use http::Request;
use hyper::body::Incoming;
use oas_rs::{ApiError, App, Method, Multipart, MultipartField};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};

const BOUNDARY: &str = "XBOUNDARY";

fn head(file_name: &str) -> Vec<u8> {
    format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes()
}

fn tail() -> Vec<u8> {
    format!("\r\n--{BOUNDARY}--\r\n").into_bytes()
}

async fn serve(app: App) -> (SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = app.build().unwrap();
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

fn request_head(length: Option<usize>) -> String {
    let framing = match length {
        Some(length) => format!("Content-Length: {length}\r\n"),
        None => "Transfer-Encoding: chunked\r\n".to_owned(),
    };
    format!(
        "POST /up HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\
         Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n{framing}\r\n"
    )
}

async fn read_response(stream: &mut TcpStream) -> String {
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut out)).await;
    out
}

/// Counts the bytes of each field as they arrive, reporting after the first chunk.
fn streaming_app(limit: u64, first_chunk: mpsc::Sender<usize>) -> App {
    let mut app = App::new();
    app.raw(Method::POST, "/up", move |request: Request<Incoming>| {
        let first_chunk = first_chunk.clone();
        async move {
            let mut form = Multipart::from_stream(request, limit)?;
            let mut total = 0usize;
            let mut announced = false;
            while let Some(mut field) = form.next_field().await? {
                while let Some(chunk) = field.chunk().await? {
                    total += chunk.len();
                    if !announced {
                        announced = true;
                        let _ = first_chunk.send(chunk.len()).await;
                    }
                }
            }
            Ok::<_, ApiError>(total.to_string())
        }
    });
    app
}

#[tokio::test]
async fn a_handler_sees_data_before_the_upload_has_finished() {
    let (tx, mut first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(10 * 1024 * 1024, tx)).await;
    let data = vec![7u8; 200_000];
    let (head, tail) = (head("a.bin"), tail());
    let total = head.len() + data.len() + tail.len();

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(total)).as_bytes())
        .await
        .unwrap();
    stream.write_all(&head).await.unwrap();
    stream.write_all(&data[..100_000]).await.unwrap();
    stream.flush().await.unwrap();
    // The upload is only half sent, yet the handler already has bytes.
    let seen = tokio::time::timeout(Duration::from_secs(5), first.recv())
        .await
        .expect("the handler received nothing before the body finished")
        .unwrap();
    assert!(seen > 0 && seen <= 100_000);
    stream.write_all(&data[100_000..]).await.unwrap();
    stream.write_all(&tail).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("200000"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn a_declared_length_over_the_limit_is_refused_before_reading() {
    let (tx, _first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(1024, tx)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(5_000_000)).as_bytes())
        .await
        .unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn an_undeclared_length_is_cut_off_at_the_limit() {
    let (tx, _first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(50_000, tx)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(None).as_bytes())
        .await
        .unwrap();
    // Chunked body, a little over the limit, never finished: the server must
    // answer as soon as the limit is passed instead of waiting for the end.
    let mut payload = head("big.bin");
    payload.extend(vec![1u8; 60_000]);
    for piece in payload.chunks(4096) {
        let mut framed = format!("{:x}\r\n", piece.len()).into_bytes();
        framed.extend_from_slice(piece);
        framed.extend_from_slice(b"\r\n");
        stream.write_all(&framed).await.unwrap();
    }
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn a_request_without_a_boundary_is_a_bad_request() {
    let (tx, _first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(1024, tx)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"POST /up HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: multipart/form-data\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    let _ = stop.send(());
}

async fn buffered(_form: Multipart) -> &'static str {
    "ok"
}

#[test]
fn documented_fields_describe_the_form_in_the_openapi_document() {
    let mut app = App::new();
    app.post("/firmwares", buffered).multipart_fields([
        MultipartField::file("firmware")
            .required()
            .content_type("application/octet-stream")
            .description("The image to flash."),
        MultipartField::text("version").required(),
        MultipartField::text("notes"),
    ]);
    let doc = app.openapi_document();
    let content =
        &doc["paths"]["/firmwares"]["post"]["requestBody"]["content"]["multipart/form-data"];
    assert_eq!(
        content["schema"],
        json!({
            "type": "object",
            "properties": {
                "firmware": {
                    "type": "string",
                    "format": "binary",
                    "description": "The image to flash."
                },
                "version": { "type": "string" },
                "notes": { "type": "string" }
            },
            "required": ["firmware", "version"]
        })
    );
    assert_eq!(
        content["encoding"],
        json!({ "firmware": { "contentType": "application/octet-stream" } })
    );
    assert_eq!(
        doc["paths"]["/firmwares"]["post"]["requestBody"]["required"],
        true
    );
}

#[test]
fn a_raw_streaming_route_can_document_its_form_too() {
    let (tx, _rx) = mpsc::channel(1);
    let mut app = streaming_app(1024, tx);
    let before = app.openapi_document();
    assert!(before["paths"]["/up"]["post"].get("requestBody").is_none());
    app.multipart_fields([MultipartField::file("a")]);
    let after = app.openapi_document();
    assert!(after["paths"]["/up"]["post"]["requestBody"].is_object());
}
