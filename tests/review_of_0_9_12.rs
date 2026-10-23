//! Findings of the 0.9.12 source review.
#![cfg(feature = "multipart")]
use http::Request;
use hyper::body::Incoming;
use oas_rs::{App, Method, MultipartField};
use serde_json::Value;

async fn raw_upload(_request: Request<Incoming>) -> &'static str {
    "ok"
}

async fn buffered_upload(_form: oas_rs::Multipart) -> &'static str {
    "ok"
}

fn responses(document: &Value, path: &str) -> Value {
    document["paths"][path]["post"]["responses"].clone()
}

#[test]
fn a_raw_route_does_not_document_a_timeout_the_framework_never_applies() {
    let mut app = App::new();
    app.raw(Method::POST, "/raw", raw_upload)
        .multipart_fields([MultipartField::file("firmware")]);
    app.post("/typed", buffered_upload)
        .multipart_fields([MultipartField::file("firmware")]);
    let document = app.openapi_document();
    // A raw handler reads its own body: no `body_read_timeout`, so no 408.
    assert!(
        responses(&document, "/raw")["408"].is_null(),
        "{}",
        responses(&document, "/raw")
    );
    // The size and media-type checks of `Multipart::from_stream` still apply.
    assert!(responses(&document, "/raw")["413"].is_object());
    // A buffered body does get the timeout.
    assert!(responses(&document, "/typed")["408"].is_object());
}
