use std::io::Read;

use oas_rs::{App, AppRuntime, Compress, Method};

fn big_text() -> String {
    "the quick brown fox jumps over the lazy dog. ".repeat(100)
}

async fn big() -> String {
    big_text()
}

fn runtime(compress: Compress) -> AppRuntime {
    let mut app = App::new();
    app.get("/big", big);
    app.layer(compress);
    app.build().unwrap()
}

async fn get(runtime: &AppRuntime, accept: &str) -> oas_rs::TestResponse {
    runtime
        .oneshot(Method::GET, "/big", &[("accept-encoding", accept)], None)
        .await
}

fn unbrotli(bytes: &[u8]) -> String {
    let mut text = String::new();
    brotli::Decompressor::new(bytes, 4096)
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn ungzip(bytes: &[u8]) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tokio::test]
async fn brotli_is_used_when_the_client_accepts_it() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "gzip, br").await;
    assert_eq!(response.header("content-encoding"), Some("br"));
    let bytes = response.body_bytes().await;
    assert!(bytes.len() < big_text().len() / 4);
    assert_eq!(unbrotli(&bytes), big_text());
}

#[tokio::test]
async fn gzip_still_works_when_brotli_is_not_accepted() {
    let runtime = runtime(Compress::new());
    let response = get(&runtime, "gzip").await;
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    assert_eq!(ungzip(&response.body_bytes().await), big_text());
}

#[tokio::test]
async fn quality_values_pick_the_preferred_encoding() {
    let runtime = runtime(Compress::new());
    let cases = [
        ("br;q=0.2, gzip;q=0.8", Some("gzip")),
        ("br;q=0.8, gzip;q=0.8", Some("br")), // a tie prefers brotli
        ("br;q=0, gzip", Some("gzip")),
        ("gzip;q=0, br", Some("br")),
        ("*", Some("br")),
        ("*;q=0.1, gzip", Some("gzip")),
        ("br;q=0, gzip;q=0", None),
        ("identity", None),
    ];
    for (accept, expected) in cases {
        let response = get(&runtime, accept).await;
        assert_eq!(response.header("content-encoding"), expected, "{accept}");
    }
}

#[tokio::test]
async fn the_brotli_quality_is_configurable() {
    let fast = runtime(Compress::new().brotli_quality(0));
    let best = runtime(Compress::new().brotli_quality(11));
    let fast = get(&fast, "br").await.body_bytes().await;
    let best = get(&best, "br").await.body_bytes().await;
    assert_eq!(unbrotli(&fast), big_text());
    assert_eq!(unbrotli(&best), big_text());
    assert!(best.len() <= fast.len());
}

#[test]
#[should_panic(expected = "0 to 11")]
fn a_brotli_quality_above_eleven_is_rejected() {
    let _ = Compress::new().brotli_quality(12);
}
