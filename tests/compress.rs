use std::io::Read;

use http::{HeaderValue, Request};
use oas_rs::{App, AppRuntime, Compress, Json, Method, Next, RequestBody};
use serde_json::{Value, json};

fn big_text() -> String {
    "the quick brown fox jumps over the lazy dog. ".repeat(100)
}

async fn big() -> String {
    big_text()
}

async fn small() -> &'static str {
    "tiny"
}

async fn big_json() -> Json<Value> {
    Json(json!({ "items": vec!["alpha"; 400] }))
}

async fn nothing() {}

fn runtime(compress: Compress) -> AppRuntime {
    let mut app = App::new();
    app.get("/big", big);
    app.get("/json", big_json);
    app.get("/small", small);
    app.get("/none", nothing);
    app.layer(compress);
    app.layer(|request: Request<RequestBody>, next: Next| async move {
        let wants_encoded = request.uri().query() == Some("encoded");
        let wants_headers = request.uri().query() == Some("headers");
        let wants_binary = request.uri().query() == Some("bin");
        let mut response = next.run(request).await;
        if wants_binary {
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("application/octet-stream"),
            );
        }
        if wants_encoded {
            response
                .headers_mut()
                .insert("content-encoding", HeaderValue::from_static("br"));
        }
        if wants_headers {
            let length = response.headers().get("content-length").cloned();
            response
                .headers_mut()
                .insert("etag", HeaderValue::from_static("\"abc\""));
            response.headers_mut().insert(
                "content-length",
                length.unwrap_or_else(|| HeaderValue::from(big_text().len())),
            );
        }
        response
    });
    app.build().unwrap()
}

async fn get(runtime: &AppRuntime, uri: &str, accept: Option<&str>) -> oas_rs::TestResponse {
    let headers: Vec<(&str, &str)> = accept
        .map(|value| ("accept-encoding", value))
        .into_iter()
        .collect();
    runtime.oneshot(Method::GET, uri, &headers, None).await
}

fn gunzip(bytes: &[u8]) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn vary_has(response: &oas_rs::TestResponse, name: &str) -> bool {
    response
        .header_all("vary")
        .iter()
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case(name))
}

#[tokio::test]
async fn a_large_text_response_is_gzipped_for_clients_that_accept_it() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "/big", Some("gzip, deflate")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    assert!(vary_has(&response, "accept-encoding"));
    let bytes = response.body_bytes().await;
    assert!(bytes.len() < big_text().len() / 4, "{}", bytes.len());
    assert_eq!(gunzip(&bytes), big_text());
}

#[tokio::test]
async fn json_is_compressed_too() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "/json", Some("gzip")).await;
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    let value: Value = serde_json::from_str(&gunzip(&response.body_bytes().await)).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 400);
}

#[tokio::test]
async fn small_bodies_and_unaccepting_clients_get_the_plain_body_but_still_vary() {
    let runtime = runtime(Compress::new());
    let small = get(&runtime, "/small", Some("gzip")).await;
    assert!(small.header("content-encoding").is_none());
    assert_eq!(small.body_string().await, "tiny");

    for accept in [None, Some("identity"), Some("gzip;q=0"), Some("br")] {
        let response = get(&runtime, "/big", accept).await;
        assert!(response.header("content-encoding").is_none(), "{accept:?}");
        assert!(vary_has(&response, "accept-encoding"), "{accept:?}");
        assert_eq!(response.body_string().await, big_text(), "{accept:?}");
    }
}

#[tokio::test]
async fn quality_values_and_wildcards_are_understood() {
    let runtime = runtime(Compress::new());
    for accept in ["*", "br, gzip;q=0.5", "GZIP", "gzip;q=1.0, *;q=0"] {
        let response = get(&runtime, "/big", Some(accept)).await;
        assert_eq!(
            response.header("content-encoding"),
            Some("gzip"),
            "{accept}"
        );
    }
    for accept in ["*;q=0", "gzip;q=0, *"] {
        let response = get(&runtime, "/big", Some(accept)).await;
        assert!(response.header("content-encoding").is_none(), "{accept}");
    }
}

#[tokio::test]
async fn incompressible_types_empty_bodies_and_head_are_left_alone() {
    let runtime = runtime(Compress::new());
    let bin = get(&runtime, "/big?bin", Some("gzip")).await;
    assert!(bin.header("content-encoding").is_none());
    assert!(!vary_has(&bin, "accept-encoding"));

    let none = get(&runtime, "/none", Some("gzip")).await;
    assert_eq!(none.status(), 204);
    assert!(none.header("content-encoding").is_none());

    let head = runtime
        .oneshot(Method::HEAD, "/big", &[("accept-encoding", "gzip")], None)
        .await;
    assert!(head.header("content-encoding").is_none());
}

#[tokio::test]
async fn an_already_encoded_response_is_not_compressed_again() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "/big?encoded", Some("gzip")).await;
    assert_eq!(response.header("content-encoding"), Some("br"));
    assert_eq!(response.body_string().await, big_text());
}

#[tokio::test]
async fn content_length_and_etag_follow_the_compressed_body() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "/big?headers", Some("gzip")).await;
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    assert_eq!(response.header("etag"), Some("W/\"abc\""));
    let length: usize = response.header("content-length").unwrap().parse().unwrap();
    assert_eq!(length, response.body_bytes().await.len());
}

#[tokio::test]
async fn min_size_and_level_are_configurable() {
    let runtime = runtime(Compress::new().min_size(1).level(1));
    let response = get(&runtime, "/big", Some("gzip")).await;
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    assert_eq!(gunzip(&response.body_bytes().await), big_text());
    // Never larger than the original: a tiny body stays as it is.
    let tiny = get(&runtime, "/small", Some("gzip")).await;
    assert!(tiny.header("content-encoding").is_none());
    assert_eq!(tiny.body_string().await, "tiny");
}

#[test]
#[should_panic(expected = "0 to 9")]
fn a_level_above_nine_is_rejected() {
    let _ = Compress::new().level(10);
}
