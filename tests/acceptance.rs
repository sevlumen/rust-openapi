use bytes::Bytes;
use futures_core::Stream;
use http::{Request, StatusCode};
use hyper::body::Incoming;
use oas_rs::{
    ApiError, ApiSchema, App, BuildError, FromRequest, Header, HeaderSpec, Json, Method, NoContent,
    NotModified, Params, Path, Query, State, StreamResponse,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::json;
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

#[derive(Deserialize, oas_rs::ApiSchema)]
struct Search {
    page: u32,
    active: bool,
}

#[derive(Deserialize, Serialize, PartialEq, Debug, oas_rs::ApiSchema)]
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

struct CustomValue;

impl FromRequest<TestState> for CustomValue {
    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<TestState>,
    ) -> Result<Self, ApiError> {
        let Some(value) = request.headers().get("x-custom") else {
            return Err(ApiError::missing("custom value is absent"));
        };
        if value.to_str().ok() == Some("valid") {
            Ok(Self)
        } else {
            Err(ApiError::bad_request("custom value is invalid"))
        }
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

#[allow(clippy::too_many_arguments)]
async fn eight_extractors(
    Path(id): Path<u64>,
    Query(query): Query<Search>,
    Header(trace): Header<TraceId>,
    State(_state): State<TestState>,
    Json(payload): Json<Payload>,
    optional_trace: Option<Header<TraceId>>,
    optional_json: Option<Json<Payload>>,
    _params: Params,
) -> String {
    format!(
        "{id}:{}:{}:{}:{}:{}",
        query.active,
        trace.0,
        payload.name,
        optional_trace.is_some(),
        optional_json.is_some(),
    )
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

async fn optional_json(value: Option<Json<Payload>>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "missing"
    }
}

async fn optional_custom(value: Option<CustomValue>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "missing"
    }
}

#[derive(Clone, Debug)]
struct StrictTraceId;

impl HeaderSpec for StrictTraceId {
    const NAME: &'static str = "x-strict-trace-id";

    fn parse(value: &str) -> Result<Self, ApiError> {
        if value == "valid" {
            Ok(Self)
        } else {
            Err(ApiError::bad_request("invalid strict trace id"))
        }
    }
}

async fn optional_strict_trace(value: Option<Header<StrictTraceId>>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "missing"
    }
}

async fn body_cancellation_handler(
    Json(_payload): Json<Payload>,
    State(counter): State<Arc<AtomicUsize>>,
) -> &'static str {
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

struct NeverStream;

impl Stream for NeverStream {
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

async fn never_stream() -> StreamResponse<NeverStream> {
    StreamResponse(NeverStream)
}

async fn raw_incoming(request: Request<Incoming>) -> &'static str {
    assert_eq!(request.uri().path(), "/raw-incoming");
    "OK"
}

struct FailingSerialize;

impl Serialize for FailingSerialize {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(serde::ser::Error::custom(
            "intentional serialization failure",
        ))
    }
}

impl ApiSchema for FailingSerialize {
    fn schema() -> serde_json::Value {
        json!({"type": "object"})
    }
}

async fn failing_created() -> oas_rs::Created<FailingSerialize> {
    oas_rs::Created(FailingSerialize)
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

    let response = app
        .oneshot(Method::GET, "/search?%70age=42&active=true", &[], None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "42:true");

    let response = app
        .oneshot(
            Method::GET,
            "/search?unknown=%ZZ&page=42&active=true",
            &[],
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn handler_supports_eight_extractors() {
    let mut app = App::new().with_state(TestState);
    app.post("/multi/{id}", eight_extractors);

    let response = app
        .oneshot(
            Method::POST,
            "/multi/42?page=7&active=true",
            &[
                ("content-type", "application/json"),
                ("x-trace-id", "abc123"),
            ],
            Some(Bytes::from_static(br#"{"name":"Ada"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "42:true:abc123:Ada:true:true");
}

#[tokio::test]
async fn build_freezes_routes_into_an_immutable_runtime() {
    let mut app = App::new();
    app.get("/frozen", hello);
    let runtime = app.build().expect("test app builds");

    let response = runtime.oneshot(Method::GET, "/frozen", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "OK");
}

#[tokio::test]
async fn http_semantics_and_typed_body_header_are_preserved() {
    let mut app = App::new();
    app.get("/plaintext", hello);
    app.post("/echo", echo);
    app.post("/echo-suffix", echo);
    app.get("/trace", trace);
    app.get("/optional-trace", optional_trace);
    app.get("/optional-strict-trace", optional_strict_trace);
    app.post("/optional-json", optional_json);
    app.get("/never-stream", never_stream);
    app.get("/failing-created", failing_created);
    app.openapi();
    app.swagger().path("/swagger");

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
            Method::GET,
            "/optional-strict-trace",
            &[("x-strict-trace-id", "invalid")],
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app.oneshot(Method::POST, "/optional-json", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "missing");
    let response = app
        .oneshot(
            Method::POST,
            "/optional-json",
            &[("content-type", "application/json")],
            Some(Bytes::from_static(br#"not-json"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
            "/echo-suffix",
            &[("content-type", "application/problem+json; charset=utf-8")],
            Some(Bytes::from_static(br#"{"name":"Ada"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, r#"{"name":"Ada"}"#);
    let response = app
        .oneshot(
            Method::POST,
            "/echo",
            &[("content-type", "application/jsonfoo")],
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
    let swagger = response.body_string().await;
    assert!(swagger.contains("SwaggerUIBundle"));
    assert!(swagger.contains("/openapi.json"));
    let response = app.oneshot(Method::GET, "/missing", &[], None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .oneshot(Method::GET, "/failing-created", &[], None)
        .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let response = tokio::time::timeout(
        Duration::from_millis(100),
        app.oneshot(Method::HEAD, "/never-stream", &[], None),
    )
    .await
    .expect("HEAD must not consume an unbounded stream");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), None);
    assert_eq!(response.body_string().await, "");
}

#[tokio::test]
async fn custom_optional_extractors_distinguish_missing_from_invalid() {
    let mut app = App::new().with_state(TestState);
    app.get("/optional-custom", optional_custom);

    let response = app
        .oneshot(Method::GET, "/optional-custom", &[], None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "missing");

    let response = app
        .oneshot(
            Method::GET,
            "/optional-custom",
            &[("x-custom", "invalid")],
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .oneshot(
            Method::GET,
            "/optional-custom",
            &[("x-custom", "valid")],
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "present");
}

#[tokio::test]
async fn configured_body_limit_is_enforced_before_collection() {
    let mut app = App::new();
    app.max_body_size(4);
    app.post("/echo", echo);

    let response = app
        .oneshot(
            Method::POST,
            "/echo",
            &[("content-type", "application/json")],
            Some(Bytes::from_static(br#"{"name":"Ada"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
#[should_panic(expected = "with_state must be configured before routes")]
fn with_state_rejects_routes_registered_before_state() {
    let mut app = App::new();
    app.get("/hello", hello);
    let _ = app.with_state(TestState);
}

#[test]
fn raw_handler_can_receive_hyper_incoming_directly() {
    let mut app = App::new();
    app.raw_get("/raw-incoming", raw_incoming);
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
async fn swagger_uses_configured_openapi_path() {
    let mut app = App::new();
    app.get("/plaintext", hello);
    app.openapi().path("/spec.json");
    app.swagger().path("/swagger");
    let response = app.oneshot(Method::GET, "/swagger", &[], None).await;
    let body = response.body_string().await;
    assert!(body.contains("SwaggerUIBundle"));
    assert!(body.contains("/spec.json"));
    assert!(!body.contains("fetch('/openapi.json')"));
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
        app.oneshot(Method::GET, "/orgs/%FF/users/alice", &[], None)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.oneshot(Method::GET, "/users/a%20b", &[], None)
            .await
            .body_string()
            .await,
        "dynamic:a b"
    );
    assert_eq!(
        app.oneshot(Method::GET, "/users/%C3%A9", &[], None)
            .await
            .body_string()
            .await,
        "dynamic:é"
    );

    let response = app.oneshot(Method::HEAD, "/users/42", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), Some("10"));
    assert_eq!(response.body_string().await, "");
}

#[test]
fn percent_decoding_preserves_utf8_and_literal_plus() {
    assert_eq!(
        oas_rs::decode_query_component("%E2%82%AC+value").unwrap(),
        "€+value"
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

#[test]
fn routes_over_capture_limit_fail_during_build() {
    let mut app = App::new();
    app.get(
        "/a/{a}/b/{b}/c/{c}/d/{d}/e/{e}/f/{f}/g/{g}/h/{h}/i/{i}",
        hello,
    );

    let error = match app.build() {
        Ok(_) => panic!("routes over the capture limit must not build"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        BuildError::TooManyCaptures {
            path: "/a/{a}/b/{b}/c/{c}/d/{d}/e/{e}/f/{f}/g/{g}/h/{h}/i/{i}".to_owned(),
            captures: 9,
            max: 8,
        }
    );
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
    app.options("/explicit-options", || async { "explicit" });
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

    let response = app
        .oneshot(Method::OPTIONS, "/explicit-options", &[], None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body_string().await, "explicit");

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
    assert_eq!(response.header("content-length"), None);

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
async fn static_response_routes_return_prebuilt_bytes_and_headers() {
    let mut app = App::new();
    app.static_text("/health", "OK");
    app.static_json("/version", Bytes::from_static(br#"{"version":"0.1.0"}"#));

    let response = app.oneshot(Method::GET, "/health", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(response.header("content-length"), Some("2"));
    assert_eq!(response.body_string().await, "OK");

    let response = app.oneshot(Method::GET, "/version", &[], None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("content-length"), None);
    assert_eq!(response.body_string().await, r#"{"version":"0.1.0"}"#);
}

#[tokio::test]
async fn tcp_server_supports_keep_alive_and_connection_close() {
    let mut app = App::new();
    app.get("/text", hello);
    app.get("/json", || async {
        Json(Payload {
            name: "Ada".to_owned(),
        })
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.build().expect("test app builds").serve_listener(
        listener,
        async move {
            let _ = shutdown_rx.await;
        },
    ));

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

    let mut json_stream = TcpStream::connect(address).await.unwrap();
    json_stream
        .write_all(b"GET /json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut json_bytes = Vec::new();
    json_stream.read_to_end(&mut json_bytes).await.unwrap();
    let json_response = String::from_utf8_lossy(&json_bytes);
    assert!(json_response.contains("content-length: 14"));
    assert!(json_response.ends_with("\r\n\r\n{\"name\":\"Ada\"}"));

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn interrupted_request_body_does_not_dispatch_handler() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut app = App::new().with_state(counter.clone());
    app.post("/cancel", body_cancellation_handler);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.build().expect("test app builds").serve_listener(
        listener,
        async move {
            let _ = shutdown_rx.await;
        },
    ));

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
async fn bodyless_route_does_not_wait_for_request_body() {
    let mut app = App::new();
    app.get("/plaintext", hello);
    app.post("/echo", echo);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.build().expect("test app builds").serve_listener(
        listener,
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /plaintext HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0_u8; 512];
    let bytes = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buffer))
        .await
        .expect("bodyless route waited for request body")
        .unwrap();
    let response = String::from_utf8_lossy(&buffer[..bytes]);
    assert!(response.starts_with("HTTP/1.1 200 OK"));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\n\r\n")
        .await
        .unwrap();
    let bytes = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buffer))
        .await
        .expect("oversized body was not rejected from Content-Length")
        .unwrap();
    let response = String::from_utf8_lossy(&buffer[..bytes]);
    assert!(response.starts_with("HTTP/1.1 413 Payload Too Large"));

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn raw_route_receives_incoming_without_collecting_request_body() {
    let mut app = App::new();
    app.raw(Method::POST, "/raw-incoming", raw_incoming);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(app.build().expect("test app builds").serve_listener(
        listener,
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"POST /raw-incoming HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048576\r\n\r\n",
        )
        .await
        .unwrap();
    let mut buffer = [0_u8; 512];
    let bytes = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buffer))
        .await
        .expect("raw handler waited for request body")
        .unwrap();
    let response = String::from_utf8_lossy(&buffer[..bytes]);
    assert!(response.starts_with("HTTP/1.1 200 OK"));

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
