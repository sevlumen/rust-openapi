use bytes::Bytes;
use http::StatusCode;
use oas_rs::{ApiError, App, Header, HeaderSpec, Json, Method, Path, Query, State};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default)]
struct TestState;

#[derive(Deserialize)]
struct Search {
    page: u32,
    active: bool,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
struct Payload {
    name: String,
}

#[derive(Clone, Debug, PartialEq)]
struct TraceId(String);

impl HeaderSpec for TraceId {
    const NAME: &'static str = "x-trace-id";

    fn parse(value: &str) -> Result<Self, ApiError> {
        Ok(Self(value.to_owned()))
    }
}

async fn hello() -> &'static str {
    "OK"
}

async fn user(Path(id): Path<u64>, State(_state): State<TestState>) -> String {
    id.to_string()
}

async fn search(Query(query): Query<Search>) -> String {
    format!("{}:{}", query.page, query.active)
}

async fn echo(Json(payload): Json<Payload>) -> Json<Payload> {
    Json(payload)
}

async fn trace(Header(trace): Header<TraceId>) -> String {
    trace.0
}

#[tokio::test]
async fn app_registers_static_dynamic_and_query_routes() {
    let mut app = App::new().with_state(TestState);
    app.get("/plaintext", hello)
        .tag("Benchmark")
        .summary("Static response");
    app.get("/users/{id}", user).tag("Users");
    app.get("/search", search);

    let response = app
        .oneshot(Method::GET, "/users/123456", &[("", "")], None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "123456");

    let response = app
        .oneshot(Method::GET, "/search?page=42&active=true", &[], None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "42:true");
}

#[tokio::test]
async fn http_semantics_and_typed_body_header_are_preserved() {
    let mut app = App::new();
    app.get("/plaintext", hello);
    app.post("/echo", echo);
    app.get("/trace", trace);
    app.openapi("/openapi.json").swagger("/swagger");

    let response = app.oneshot(Method::HEAD, "/plaintext", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.oneshot(Method::POST, "/plaintext", &[], None).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.header("allow"), Some("GET"));
    let response = app.oneshot(Method::OPTIONS, "/plaintext", &[], None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .oneshot(
            Method::POST,
            "/echo",
            &[("content-type", "application/json")],
            Some(Bytes::from_static(br#"{"name":"Ada"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, r#"{"name":"Ada"}"#);
    let response = app
        .oneshot(Method::GET, "/trace", &[("x-trace-id", "abc123")], None)
        .await;
    assert_eq!(response.body_string().await, "abc123");
    let response = app.oneshot(Method::GET, "/openapi.json", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.oneshot(Method::GET, "/swagger", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.oneshot(Method::GET, "/missing", &[], None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn openapi_describes_registered_operations() {
    let mut app = App::new();
    app.get("/plaintext", hello)
        .tag("Benchmark")
        .summary("Static response");
    let document = app.openapi_document();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(
        document["paths"]["/plaintext"]["get"]["summary"],
        "Static response"
    );
}
