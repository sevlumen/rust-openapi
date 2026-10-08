use http::header::HeaderName;
use oas_rs::{App, Headers, Json, Method};

async fn describe(Headers(headers): Headers) -> String {
    let tenant = headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let tags: Vec<&str> = headers
        .get_all("x-tag")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    format!("{tenant}|{}", tags.join(","))
}

async fn with_body(Headers(headers): Headers, Json(value): Json<serde_json::Value>) -> String {
    format!(
        "{}:{}",
        headers
            .get("x-tenant-id")
            .map_or("-", |v| v.to_str().unwrap()),
        value
    )
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/", describe);
    app.post("/body", with_body);
    app.build().unwrap()
}

#[tokio::test]
async fn headers_exposes_every_header_case_insensitively() {
    let response = runtime()
        .oneshot(
            Method::GET,
            "/",
            &[("X-Tenant-Id", "t-1"), ("x-tag", "a"), ("x-tag", "b")],
            None,
        )
        .await;
    assert_eq!(response.body_string().await, "t-1|a,b");
}

#[tokio::test]
async fn headers_works_next_to_a_body_extractor() {
    let response = runtime()
        .oneshot(
            Method::POST,
            "/body",
            &[("x-tenant-id", "t-2"), ("content-type", "application/json")],
            Some(bytes::Bytes::from_static(b"{\"a\":1}")),
        )
        .await;
    assert_eq!(response.body_string().await, "t-2:{\"a\":1}");
}

#[test]
fn headers_adds_no_openapi_parameters() {
    let mut app = App::new();
    app.get("/", describe);
    let doc = app.openapi_document();
    assert!(doc["paths"]["/"]["get"].get("parameters").is_none());
    let _ = HeaderName::from_static("x-unused");
}
