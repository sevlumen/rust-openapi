use bytes::Bytes;
use http::StatusCode;
use oas_rs::{
    ApiError, App, Header, HeaderSpec, Json, Method, NoContent, NotModified, Params, Path, Query,
    State,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

async fn params(params: Params) -> String {
    format!(
        "{}:{}",
        params.get("org").unwrap(),
        params.get("user").unwrap()
    )
}

async fn uuid_user(Path(id): Path<Uuid>) -> Json<Payload> {
    Json(Payload {
        name: id.to_string(),
    })
}

async fn empty() -> NoContent {
    NoContent
}

async fn not_modified() -> NotModified {
    NotModified
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
    assert_eq!(response.header("content-length"), Some("2"));
    assert_eq!(response.body_string().await, "");
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

#[tokio::test]
async fn router_supports_multiple_params_precedence_and_percent_encoding() {
    let mut app = App::new();
    app.get("/users/{id}", |Path(id): Path<String>| async move {
        format!("dynamic:{id}")
    });
    app.get("/users/me", || async { "static" });
    app.get("/orgs/{org}/users/{user}", params);

    assert_eq!(
        app.oneshot(Method::GET, "/users/me", &[], None)
            .await
            .body_string()
            .await,
        "static"
    );
    assert_eq!(
        app.oneshot(Method::GET, "/users/42", &[], None)
            .await
            .body_string()
            .await,
        "dynamic:42"
    );
    assert_eq!(
        app.oneshot(Method::GET, "/orgs/acme/users/alice", &[], None)
            .await
            .body_string()
            .await,
        "acme:alice"
    );
    assert_eq!(
        app.oneshot(Method::GET, "/users/a%20b", &[], None)
            .await
            .body_string()
            .await,
        "dynamic:a b"
    );
}

#[test]
#[should_panic(expected = "duplicate route")]
fn duplicate_routes_are_rejected_at_registration() {
    let mut app = App::new();
    app.get("/duplicate", hello);
    app.get("/duplicate", hello);
}

#[test]
#[should_panic(expected = "invalid route template")]
fn malformed_route_templates_are_rejected_at_registration() {
    let mut app = App::new();
    app.get("/users/{id", hello);
}

#[tokio::test]
async fn ten_thousand_static_routes_remain_correct() {
    let mut app = App::new();
    for index in 0..10_000 {
        let path = format!("/route/{index}");
        app.get(&path, || async { "OK" });
    }
    let response = app.oneshot(Method::GET, "/route/9999", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "OK");
}

#[test]
fn openapi_marks_uuid_path_and_bodyless_response() {
    let mut app = App::new();
    app.get("/users/{id}", uuid_user);
    app.delete("/users/{id}", empty);
    app.get("/cached", not_modified);
    let document = app.openapi_document();
    assert_eq!(
        document["paths"]["/users/{id}"]["get"]["parameters"][0]["schema"]["format"],
        "uuid"
    );
    assert_eq!(
        document["paths"]["/users/{id}"]["delete"]["responses"]["204"]["description"],
        "No Content"
    );
    assert_eq!(
        document["paths"]["/cached"]["get"]["responses"]["304"]["description"],
        "Not Modified"
    );
}
