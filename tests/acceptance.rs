use bytes::Bytes;
use futures_core::Stream;
use http::StatusCode;
use oas_rs::{
    ApiError, App, Header, HeaderSpec, Json, Method, NoContent, NotModified, Params, Path, Query,
    State, StreamResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

#[derive(Clone, Default)]
struct TestState;

#[derive(Deserialize, oas_rs::OpenApi)]
struct Search {
    page: u32,
    active: bool,
}

#[derive(Deserialize, Serialize, PartialEq, Debug, oas_rs::OpenApi)]
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

async fn optional_trace(value: Option<Header<TraceId>>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "missing"
    }
}

async fn cancellation_handler(State(counter): State<Arc<AtomicUsize>>) -> &'static str {
    counter.fetch_add(1, Ordering::Relaxed);
    "called"
}

struct OneChunk(Option<Bytes>);

impl Stream for OneChunk {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}

async fn stream_response() -> StreamResponse<OneChunk> {
    StreamResponse(OneChunk(Some(Bytes::from_static(b"streamed"))))
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

async fn created() -> oas_rs::Created<Payload> {
    oas_rs::Created(Payload {
        name: "created".to_owned(),
    })
}

#[tokio::test]
async fn app_registers_static_dynamic_and_query_routes() {
    let mut app = App::new().with_state(TestState);
    app.get("/plaintext", hello)
        .tag("Benchmark")
        .summary("Static response")
        .operation_id("getPlaintext");
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
    app.get("/optional-trace", optional_trace);
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
    let response = app.oneshot(Method::GET, "/trace", &[], None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let response = app.oneshot(Method::GET, "/optional-trace", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "missing");
    let response = app
        .oneshot(
            Method::POST,
            "/echo",
            &[],
            Some(Bytes::from_static(br#"{"name":"Ada"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let response = app
        .oneshot(
            Method::POST,
            "/echo",
            &[("content-type", "application/json")],
            Some(Bytes::from_static(br#"not-json"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        .summary("Static response")
        .operation_id("getPlaintext");
    let document = app.openapi_document();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(
        document["paths"]["/plaintext"]["get"]["summary"],
        "Static response"
    );
    assert_eq!(
        document["paths"]["/plaintext"]["get"]["operationId"],
        "getPlaintext"
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

    let response = app.oneshot(Method::HEAD, "/users/42", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), Some("10"));
    assert_eq!(response.body_string().await, "");
}

#[test]
#[should_panic(expected = "duplicate route")]
fn duplicate_routes_are_rejected_at_registration() {
    let mut app = App::new();
    app.get("/duplicate", hello);
    app.get("/duplicate", hello);
}

#[test]
#[should_panic(expected = "duplicate operation id")]
fn duplicate_operation_ids_are_rejected_at_registration() {
    let mut app = App::new();
    app.get("/one", hello).operation_id("same");
    app.get("/two", hello).operation_id("same");
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

#[test]
fn openapi_uses_extractor_types_and_response_statuses() {
    let mut app = App::new().with_state(TestState);
    app.get("/users/{id}", user);
    app.get("/uuid/{id}", uuid_user);
    app.get("/search", search);
    app.post("/created", created);
    app.post("/echo", echo);
    app.get("/trace", trace);
    app.get("/optional-trace", optional_trace);
    let document = app.openapi_document();

    assert_eq!(
        document["paths"]["/users/{id}"]["get"]["parameters"][0]["schema"]["type"],
        "integer"
    );
    assert_eq!(
        document["paths"]["/users/{id}"]["get"]["parameters"][0]["schema"]["format"],
        "int64"
    );
    assert_eq!(
        document["paths"]["/uuid/{id}"]["get"]["parameters"][0]["schema"]["format"],
        "uuid"
    );
    assert_eq!(
        document["paths"]["/created"]["post"]["responses"]["201"]["description"],
        "Created"
    );
    assert_eq!(
        document["paths"]["/echo"]["post"]["requestBody"]["content"]["application/json"]["schema"]
            ["type"],
        "object"
    );
    assert_eq!(
        document["paths"]["/echo"]["post"]["requestBody"]["content"]["application/json"]["schema"]
            ["properties"]["name"]["type"],
        "string"
    );
    assert_eq!(
        document["paths"]["/trace"]["get"]["parameters"][0]["in"],
        "header"
    );
    assert_eq!(
        document["paths"]["/optional-trace"]["get"]["parameters"][0]["required"],
        false
    );
    assert_eq!(
        document["paths"]["/search"]["get"]["parameters"][0]["name"],
        "page"
    );
    assert_eq!(
        document["paths"]["/search"]["get"]["parameters"][1]["schema"]["type"],
        "boolean"
    );
}

#[tokio::test]
async fn all_registered_http_methods_and_bodyless_responses_conform() {
    let mut app = App::new();
    app.get("/resource", hello);
    app.head("/head-only", hello);
    app.post("/resource", hello);
    app.put("/resource", hello);
    app.patch("/resource", hello);
    app.delete("/resource", empty);
    app.options("/explicit-options", || async { NoContent });
    app.get("/cached", not_modified);

    for method in [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ] {
        let response = app.oneshot(method, "/resource", &[], None).await;
        assert_ne!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    let response = app.oneshot(Method::OPTIONS, "/resource", &[], None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.body_string().await, "");

    let response = app.oneshot(Method::GET, "/resource/", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.oneshot(Method::GET, "/cached", &[], None).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.header("content-length"), Some("0"));
    assert_eq!(response.body_string().await, "");
}

#[tokio::test]
async fn response_headers_and_allow_metadata_follow_http_semantics() {
    let mut app = App::new();
    app.get("/text", hello);
    app.post("/text", hello);
    app.get("/json", || async {
        Json(Payload {
            name: "Ada".to_owned(),
        })
    });
    app.get("/empty", empty);
    app.get("/not-modified", not_modified);

    let response = app.oneshot(Method::GET, "/text", &[], None).await;
    assert_eq!(
        response.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(response.header("content-length"), Some("2"));

    let response = app.oneshot(Method::GET, "/json", &[], None).await;
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("content-length"), Some("14"));

    let response = app.oneshot(Method::GET, "/empty", &[], None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.header("content-length"), Some("0"));
    assert_eq!(response.body_string().await, "");

    let response = app.oneshot(Method::PUT, "/text", &[], None).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.header("allow"), Some("GET, POST"));
    assert_eq!(response.header("content-length"), Some("0"));
}

#[tokio::test]
async fn tcp_server_supports_keep_alive_and_connection_close() {
    let mut app = App::new();
    app.get("/text", hello);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.serve_listener(listener, async move {
        let _ = shutdown_rx.await;
    }));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /text HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = vec![0_u8; 4096];
    let bytes = stream.read(&mut buffer).await.unwrap();
    let first = String::from_utf8_lossy(&buffer[..bytes]);
    assert!(first.contains("HTTP/1.1 200 OK"));
    assert!(first.contains("content-length: 2"));
    assert!(first.contains("\r\n\r\nOK"));

    stream
        .write_all(b"GET /text HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).await.unwrap();
    let second = String::from_utf8_lossy(&rest);
    assert!(second.contains("HTTP/1.1 200 OK"));
    assert!(second.contains("\r\n\r\nOK"));

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn interrupted_request_body_does_not_dispatch_handler() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut app = App::new().with_state(counter.clone());
    app.post("/cancel", cancellation_handler);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.serve_listener(listener, async move {
        let _ = shutdown_rx.await;
    }));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"POST /cancel HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\npartial",
        )
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    sleep(Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::Relaxed), 0);

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn streaming_response_delivers_chunks_without_content_length() {
    let mut app = App::new();
    app.get("/stream", stream_response);
    let response = app.oneshot(Method::GET, "/stream", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), None);
    assert_eq!(response.body_string().await, "streamed");
}
