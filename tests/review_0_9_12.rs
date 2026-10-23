//! Findings of the 0.9.11 source review, each pinned by a test.
use oas_rs::{ApiSchema, App, Json, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn valid(schema: &Value, value: &Value) -> bool {
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap()
        .is_valid(value)
}

#[test]
fn unsigned_integers_are_not_declared_with_signed_ranges() {
    assert!(valid(&u32::schema(), &json!(u32::MAX)));
    assert!(!valid(&u32::schema(), &json!(-1)));
    assert!(!valid(&u32::schema(), &json!(u64::from(u32::MAX) + 1)));
    assert!(valid(&u64::schema(), &json!(u64::MAX)));
    assert!(!valid(&u64::schema(), &json!(-1)));
    assert!(valid(&u8::schema(), &json!(255)));
    assert!(!valid(&u8::schema(), &json!(256)));
    assert!(!valid(&u16::schema(), &json!(65536)));
    assert!(!valid(&i8::schema(), &json!(128)));
    assert!(!valid(&i8::schema(), &json!(-129)));
    assert!(!valid(&i16::schema(), &json!(32768)));
    assert!(!valid(&i32::schema(), &json!(i64::from(i32::MAX) + 1)));
    assert!(!valid(&usize::schema(), &json!(-1)));
    for schema in [u32::schema(), u64::schema()] {
        let format = schema["format"].as_str().unwrap();
        assert!(!format.starts_with("int"), "{schema}");
    }
}

#[test]
fn option_schema_admits_null_like_schema_with() {
    let schema = <Option<u32>>::schema();
    assert!(valid(&schema, &json!(1)));
    assert!(valid(&schema, &json!(null)));
    assert!(!valid(&schema, &json!("x")));
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
}

async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

async fn plain() -> &'static str {
    "ok"
}

#[test]
fn a_buffered_body_route_documents_408() {
    let mut app = App::new();
    app.post("/items", create);
    app.get("/plain", plain);
    let document = app.openapi_document();
    assert!(
        document["paths"]["/items"]["post"]["responses"]["408"].is_object(),
        "{}",
        document["paths"]["/items"]["post"]["responses"]
    );
    assert!(document["paths"]["/plain"]["get"]["responses"]["408"].is_null());
}

#[test]
#[should_panic(expected = "must start with '/'")]
fn a_raw_route_needs_a_leading_slash() {
    let mut app = App::new();
    app.raw_get(
        "health",
        |_request: http::Request<hyper::body::Incoming>| async { "ok" },
    );
}

#[test]
#[should_panic(expected = "must start with '/'")]
fn a_static_route_needs_a_leading_slash() {
    let mut app = App::new();
    app.static_text("health", "ok");
}

#[test]
fn raw_and_static_routes_still_accept_a_leading_slash() {
    let mut app = App::new();
    app.static_text("/health", "ok");
    let _ = Method::GET;
}
