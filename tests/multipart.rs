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
