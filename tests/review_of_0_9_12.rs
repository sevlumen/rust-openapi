//! Findings of the 0.9.12 source reviews: the document lists only the errors
//! the framework produces, and a handler documents its own.
#![cfg(feature = "multipart")]
use http::{Request, StatusCode};
use hyper::body::Incoming;
use oas_rs::{App, Json, Method, MultipartField};
use serde_json::Value;

async fn raw_upload(_request: Request<Incoming>) -> &'static str {
    "ok"
}

async fn buffered_upload(_form: oas_rs::Multipart) -> &'static str {
    "ok"
}

async fn echo(Json(value): Json<Value>) -> Json<Value> {
    Json(value)
}

fn responses(document: &Value, path: &str) -> Value {
    document["paths"][path]["post"]["responses"].clone()
}

fn codes(document: &Value, path: &str) -> Vec<String> {
    let mut codes: Vec<String> = responses(document, path)
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    codes.sort();
    codes
}

#[test]
fn a_raw_route_lists_no_error_the_framework_does_not_produce() {
    let mut app = App::new();
    app.raw(Method::POST, "/raw", raw_upload)
        .multipart_fields([MultipartField::file("firmware")]);
    let document = app.openapi_document();
    // A raw handler reads (or ignores) its own body: no 400/408/413/415.
    assert_eq!(codes(&document, "/raw"), ["200"]);
}

#[test]
fn a_buffered_body_keeps_the_framework_errors() {
    let mut app = App::new();
    app.post("/typed", buffered_upload)
        .multipart_fields([MultipartField::file("firmware")]);
    let document = app.openapi_document();
    assert_eq!(
        codes(&document, "/typed"),
        ["200", "400", "408", "413", "415"]
    );
}

#[test]
fn a_handler_documents_the_errors_it_returns() {
    let mut app = App::new();
    app.raw(Method::POST, "/raw", raw_upload)
        .multipart_fields([MultipartField::file("firmware")])
        .problem_responses([
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ]);
    app.post("/typed", echo)
        .problem_responses([StatusCode::CONFLICT]);
    let document = app.openapi_document();
    assert_eq!(codes(&document, "/raw"), ["200", "413", "415"]);
    assert_eq!(
        responses(&document, "/raw")["413"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
    assert_eq!(
        responses(&document, "/raw")["413"]["description"],
        "Payload Too Large"
    );
    assert!(document["components"]["schemas"]["Problem"].is_object());
    // Added to the framework's own.
    assert_eq!(
        codes(&document, "/typed"),
        ["200", "400", "409", "413", "415", "408"]
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .tap_sorted()
    );
}

#[test]
fn declared_errors_survive_document_errors_false() {
    let mut app = App::new();
    app.openapi().document_errors(false);
    app.post("/typed", echo)
        .problem_responses([StatusCode::CONFLICT]);
    let document = app.openapi_document();
    assert_eq!(codes(&document, "/typed"), ["200", "409"]);
}

#[test]
#[should_panic(expected = "an error status")]
fn a_success_status_is_not_a_problem_response() {
    let mut app = App::new();
    app.post("/typed", echo).problem_responses([StatusCode::OK]);
}

trait Sorted {
    fn tap_sorted(self) -> Self;
}

impl Sorted for Vec<String> {
    fn tap_sorted(mut self) -> Self {
        self.sort();
        self
    }
}
