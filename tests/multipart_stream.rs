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
    // Keep what arrived even if the server does not close the connection (it
    // may still be waiting for a body the client stopped sending): a read
    // that is cancelled by the timeout would drop the bytes already read.
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut buffer = [0u8; 4096];
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(read)) => {
                out.extend_from_slice(&buffer[..read]);
                // The status line is all the callers look at; a response with
                // a body is complete once the headers are in and the peer is
                // done, which the loop discovers at EOF or the deadline.
                if out.windows(4).any(|window| window == b"\r\n\r\n") && out.len() > 12 {
                    let text = String::from_utf8_lossy(&out);
                    if text.starts_with("HTTP/1.1 4") || text.starts_with("HTTP/1.1 5") {
                        break;
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Sends `junk` after a request head that declares `declared` bytes, never
/// finishing the body, and returns what the server answers meanwhile.
async fn flood(junk: Vec<u8>, declared: usize) -> String {
    let (tx, _first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(10 * 1024 * 1024, tx)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(declared)).as_bytes())
        .await
        .unwrap();
    for piece in junk.chunks(8192) {
        if stream.write_all(piece).await.is_err() {
            break;
        }
    }
    let started = std::time::Instant::now();
    let response = read_response(&mut stream).await;
    let _ = stop.send(());
    // The server may reset the connection while the client is still sending,
    // which can swallow the `400`: an early close counts as refused too. Only
    // the server waiting for the rest of the body (until the read times out)
    // is a failure.
    if response.is_empty() && started.elapsed() < Duration::from_secs(3) {
        return "HTTP/1.1 400 (connection closed early)".to_owned();
    }
    response
}

#[tokio::test]
async fn a_body_that_never_reaches_a_boundary_is_cut_off_early() {
    // No `--boundary` anywhere: multer would keep buffering until the limit.
    let response = flood(vec![b'a'; 600_000], 2_000_000).await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[tokio::test]
async fn part_headers_that_never_end_are_cut_off_early() {
    let mut junk = format!("--{BOUNDARY}\r\nX-Junk: ").into_bytes();
    junk.extend(vec![b'a'; 600_000]);
    let response = flood(junk, 2_000_000).await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[tokio::test]
async fn a_large_well_formed_upload_is_not_cut_off_by_that_guard() {
    let (tx, mut first) = mpsc::channel(1);
    let (addr, stop) = serve(streaming_app(10 * 1024 * 1024, tx)).await;
    let data = vec![3u8; 3_000_000];
    let (head, tail) = (head("ok.bin"), tail());
    let total = head.len() + data.len() + tail.len();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(total)).as_bytes())
        .await
        .unwrap();
    stream.write_all(&head).await.unwrap();
    stream.write_all(&data).await.unwrap();
    stream.write_all(&tail).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("3000000"), "{response}");
    let _ = first.try_recv();
    let _ = stop.send(());
}

#[test]
#[should_panic(expected = "twice")]
fn documenting_a_field_name_twice_is_rejected() {
    let mut app = App::new();
    app.post("/x", buffered);
    app.multipart_fields([MultipartField::text("a"), MultipartField::file("a")]);
}

#[tokio::test]
async fn skipping_a_large_part_without_reading_it_is_not_an_error() {
    let mut app = App::new();
    app.raw(
        Method::POST,
        "/skip",
        |request: Request<Incoming>| async move {
            let mut form = Multipart::from_stream(request, 64 * 1024 * 1024)?;
            let mut seen = Vec::new();
            while let Some(field) = form.next_field().await? {
                let name = field.name().unwrap_or("-").to_owned();
                if name == "big" {
                    // Dropped unread: the parser drains it on the next call.
                    continue;
                }
                let text = field.text().await?;
                seen.push(format!("{name}={text}"));
            }
            Ok::<_, ApiError>(seen.join(","))
        },
    );
    let (addr, stop) = serve(app).await;
    let part = |name: &str, data: &[u8]| {
        let mut out =
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                .into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
        out
    };
    let mut body = part("small", b"first");
    body.extend(part("big", &vec![b'x'; 1_000_000]));
    body.extend(part("small", b"last"));
    body.extend(format!("--{BOUNDARY}--\r\n").into_bytes());
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /skip HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("small=first,small=last"), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn part_headers_that_never_end_after_a_skipped_part_are_still_cut_off() {
    let mut app = App::new();
    app.raw(
        Method::POST,
        "/up",
        |request: Request<Incoming>| async move {
            let mut form = Multipart::from_stream(request, 10 * 1024 * 1024)?;
            // Every field is dropped unread, so each is drained.
            while let Some(_field) = form.next_field().await? {}
            Ok::<_, ApiError>("done")
        },
    );
    let (addr, stop) = serve(app).await;
    let mut junk = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"big\"\r\n\r\n")
        .into_bytes();
    junk.extend(vec![b'x'; 300_000]);
    junk.extend(format!("\r\n--{BOUNDARY}\r\nX-Junk: ").into_bytes());
    junk.extend(vec![b'a'; 2_000_000]);
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(5_000_000)).as_bytes())
        .await
        .unwrap();
    for piece in junk.chunks(8192) {
        if stream.write_all(piece).await.is_err() {
            break;
        }
    }
    let started = std::time::Instant::now();
    let response = read_response(&mut stream).await;
    let _ = stop.send(());
    let refused = response.starts_with("HTTP/1.1 400")
        || (response.is_empty() && started.elapsed() < Duration::from_secs(3));
    assert!(
        refused,
        "junk headers after a skipped part were buffered: {response}"
    );
}

#[tokio::test]
async fn a_delimiter_split_across_writes_still_ends_a_skipped_part() {
    let mut app = App::new();
    app.raw(
        Method::POST,
        "/up",
        |request: Request<Incoming>| async move {
            let mut form = Multipart::from_stream(request, 64 * 1024 * 1024)?;
            let mut seen = Vec::new();
            while let Some(field) = form.next_field().await? {
                if field.name() == Some("big") {
                    continue;
                }
                seen.push(field.text().await?);
            }
            Ok::<_, ApiError>(seen.join(","))
        },
    );
    let (addr, stop) = serve(app).await;
    let mut body = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"big\"\r\n\r\n")
        .into_bytes();
    body.extend(vec![b'x'; 600_000]);
    body.extend_from_slice(
        format!(
            "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"s\"\r\n\r\nlast\r\n--{BOUNDARY}--\r\n"
        )
        .as_bytes(),
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(request_head(Some(body.len())).as_bytes())
        .await
        .unwrap();
    // Cut right inside the delimiter that ends the skipped part.
    let delimiter_at = body.len()
        - format!(
            "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"s\"\r\n\r\nlast\r\n--{BOUNDARY}--\r\n"
        )
        .len();
    let (first, rest) = body.split_at(delimiter_at + 5);
    stream.write_all(first).await.unwrap();
    stream.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.write_all(rest).await.unwrap();
    let response = read_response(&mut stream).await;
    let _ = stop.send(());
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("last"), "{response}");
}
