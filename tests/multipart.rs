use bytes::Bytes;
use oas_rs::{ApiError, App, Method, Multipart};

struct Part<'a> {
    name: &'a str,
    file_name: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

fn multipart_body(boundary: &str, parts: &[Part<'_>]) -> Bytes {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(file_name) = part.file_name {
            disposition.push_str(&format!("; filename=\"{file_name}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}

/// Echoes `name|file_name|content_type|length` per part, one line each.
async fn summarize(mut form: Multipart) -> Result<String, ApiError> {
    let mut lines = Vec::new();
    while let Some(field) = form.next_field().await? {
        let name = field.name().unwrap_or("-").to_owned();
        let file_name = field.file_name().unwrap_or("-").to_owned();
        let content_type = field.content_type().unwrap_or("-").to_owned();
        let data = field.bytes().await?;
        lines.push(format!("{name}|{file_name}|{content_type}|{}", data.len()));
    }
    Ok(lines.join("\n"))
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.post("/upload", summarize);
    app.build().unwrap()
}

async fn upload(
    runtime: &oas_rs::AppRuntime,
    content_type: Option<&str>,
    body: Bytes,
) -> (u16, String) {
    let headers: Vec<(&str, &str)> = content_type
        .map(|value| ("content-type", value))
        .into_iter()
        .collect();
    let response = runtime
        .oneshot(Method::POST, "/upload", &headers, Some(body))
        .await;
    let status = response.status().as_u16();
    (status, response.body_string().await)
}

#[tokio::test]
async fn reads_text_and_binary_parts() {
    let body = multipart_body(
        "XBOUNDARYX",
        &[
            Part {
                name: "note",
                file_name: None,
                content_type: None,
                data: b"hello",
            },
            Part {
                name: "firmware",
                file_name: Some("fw.bin"),
                content_type: Some("application/octet-stream"),
                data: &[0u8, 255, 10, 13, 0, 1],
            },
        ],
    );
    let (status, text) = upload(
        &runtime(),
        Some("multipart/form-data; boundary=XBOUNDARYX"),
        body,
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(
        text,
        "note|-|-|5\nfirmware|fw.bin|application/octet-stream|6"
    );
}

#[tokio::test]
async fn wrong_media_type_is_415_and_missing_boundary_is_400() {
    let runtime = runtime();
    let body = multipart_body(
        "b",
        &[Part {
            name: "a",
            file_name: None,
            content_type: None,
            data: b"1",
        }],
    );
    assert_eq!(
        upload(&runtime, Some("application/json"), body.clone())
            .await
            .0,
        415
    );
    assert_eq!(
        upload(&runtime, Some("multipart/mixed; boundary=b"), body.clone())
            .await
            .0,
        415
    );
    assert_eq!(upload(&runtime, None, body.clone()).await.0, 415);
    assert_eq!(
        upload(&runtime, Some("multipart/form-data"), body).await.0,
        400
    );
}

#[tokio::test]
async fn quoted_boundary_is_accepted() {
    let body = multipart_body(
        "abc",
        &[Part {
            name: "a",
            file_name: None,
            content_type: None,
            data: b"1",
        }],
    );
    let (status, text) = upload(
        &runtime(),
        Some("multipart/form-data; boundary=\"abc\""),
        body,
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text, "a|-|-|1");
}

#[tokio::test]
async fn part_content_may_contain_the_boundary_text_and_crlf() {
    // Valid content that merely resembles a delimiter: CRLFs, a partial
    // boundary prefix, a lone `--`, and the boundary text mid-line (a real
    // delimiter is CRLF + `--` + boundary, which a client must avoid).
    let tricky = b"line1\r\n--XA looks close\r\n-X\r\n--\r\nmid-line --XB text\r\nlast\r\n";
    let body = multipart_body(
        "XB",
        &[Part {
            name: "f",
            file_name: Some("a.txt"),
            content_type: None,
            data: tricky,
        }],
    );
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text, format!("f|a.txt|-|{}", tricky.len()));
}

#[tokio::test]
async fn truncated_body_is_400_not_a_hang() {
    let full = multipart_body(
        "XB",
        &[Part {
            name: "f",
            file_name: None,
            content_type: None,
            data: b"0123456789",
        }],
    );
    let truncated = full.slice(..full.len() - 12);
    let (status, text) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        upload(
            &runtime(),
            Some("multipart/form-data; boundary=XB"),
            truncated,
        ),
    )
    .await
    .expect("truncated multipart body hung");
    assert_eq!(status, 400, "{text}");
}

#[tokio::test]
async fn file_names_come_back_verbatim() {
    for name in [
        "ünï-コード.bin",
        "../../etc/passwd",
        "a b;c.bin",
        "plain.bin",
    ] {
        let body = multipart_body(
            "XB",
            &[Part {
                name: "f",
                file_name: Some(name),
                content_type: None,
                data: b"x",
            }],
        );
        let (status, text) =
            upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
        assert_eq!(status, 200, "{name}: {text}");
        assert_eq!(text, format!("f|{name}|-|1"), "{name}");
    }
}

#[tokio::test]
async fn empty_field_and_no_parts() {
    let body = multipart_body(
        "XB",
        &[Part {
            name: "empty",
            file_name: None,
            content_type: None,
            data: b"",
        }],
    );
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
    assert_eq!((status, text.as_str()), (200, "empty|-|-|0"));

    let (status, text) = upload(
        &runtime(),
        Some("multipart/form-data; boundary=XB"),
        Bytes::from_static(b"--XB--\r\n"),
    )
    .await;
    assert_eq!((status, text.as_str()), (200, ""));
}

fn body_of_len(len: usize) -> Bytes {
    let overhead = multipart_body(
        "XB",
        &[Part {
            name: "f",
            file_name: Some("fw.bin"),
            content_type: None,
            data: b"",
        }],
    )
    .len();
    let data = vec![7u8; len - overhead];
    multipart_body(
        "XB",
        &[Part {
            name: "f",
            file_name: Some("fw.bin"),
            content_type: None,
            data: &data,
        }],
    )
}

#[tokio::test]
async fn route_limit_accepts_exactly_the_limit_and_rejects_one_more() {
    let mut app = App::new();
    app.post("/upload", summarize).body_limit(4 * 1024 * 1024);
    app.post("/other", summarize);
    let runtime = app.build().unwrap();

    let ct = [("content-type", "multipart/form-data; boundary=XB")];
    let limit = 4 * 1024 * 1024;

    let at = body_of_len(limit);
    assert_eq!(at.len(), limit);
    let ok = runtime
        .oneshot(Method::POST, "/upload", &ct, Some(at.clone()))
        .await;
    assert_eq!(ok.status(), 200);
    let rejected = runtime
        .oneshot(Method::POST, "/upload", &ct, Some(body_of_len(limit + 1)))
        .await;
    assert_eq!(rejected.status(), 413);
    // The default 1 MiB limit still applies to the other route.
    let other = runtime.oneshot(Method::POST, "/other", &ct, Some(at)).await;
    assert_eq!(other.status(), 413);
}

#[tokio::test]
async fn upload_over_a_real_tcp_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let mut app = App::new();
    app.post("/upload", summarize).body_limit(4 * 1024 * 1024);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });

    let data = vec![42u8; 3 * 1024 * 1024];
    let body = multipart_body(
        "XB",
        &[Part {
            name: "firmware",
            file_name: Some("fw.bin"),
            content_type: Some("application/octet-stream"),
            data: &data,
        }],
    );
    let head = format!(
        "POST /upload HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: multipart/form-data; boundary=XB\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert!(
        out.starts_with("HTTP/1.1 200"),
        "{}",
        &out[..out.len().min(300)]
    );
    assert!(
        out.ends_with(&format!(
            "firmware|fw.bin|application/octet-stream|{}",
            data.len()
        )),
        "{}",
        &out[out.len().saturating_sub(200)..]
    );
    let _ = stop.send(());
}
