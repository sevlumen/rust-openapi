use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use oas_rs::{
    ApiError, App, AppRuntime, Header, HeaderSpec, Json, JsonBytes, Method, Params, Path, Query,
};
use serde::Deserialize;
use uuid::Uuid;

async fn plaintext() -> &'static str {
    "OK"
}

async fn raw_plaintext() -> Response<Full<Bytes>> {
    raw_plaintext_with_request("/plaintext", &[]).await
}

fn raw_request(uri: &str, headers: &[(&str, &str)]) -> Request<Bytes> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Bytes::new()).unwrap()
}

async fn raw_plaintext_with_request(uri: &str, headers: &[(&str, &str)]) -> Response<Full<Bytes>> {
    let _request = raw_request(uri, headers);
    raw_ok_response()
}

fn raw_ok_response() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", "2")
        .body(Full::new(Bytes::from_static(b"OK")))
        .unwrap()
}

async fn typed_path(Path(_id): Path<u64>) -> &'static str {
    "OK"
}

async fn typed_params(_params: Params) -> &'static str {
    "OK"
}

async fn typed_uuid(Path(_id): Path<Uuid>) -> &'static str {
    "OK"
}

#[derive(Deserialize, oas_rs::OpenApi)]
struct BenchQuery {
    page: u32,
    active: bool,
}

async fn typed_query(Query(query): Query<BenchQuery>) -> &'static str {
    let _ = (query.page, query.active);
    "OK"
}

#[derive(Deserialize, oas_rs::OpenApi)]
struct BenchJsonRequest {
    name: String,
}

async fn typed_json_request(Json(payload): Json<BenchJsonRequest>) -> &'static str {
    let _ = payload.name;
    "OK"
}

struct BenchTrace;

impl HeaderSpec for BenchTrace {
    const NAME: &'static str = "x-trace-id";

    fn parse(_value: &str) -> Result<Self, ApiError> {
        Ok(Self)
    }
}

async fn typed_header(Header(_header): Header<BenchTrace>) -> &'static str {
    "OK"
}

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

async fn measure_app(
    app: &AppRuntime,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    iterations: u64,
) -> (u128, usize, usize) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        let response = app.oneshot(method.clone(), uri, headers, None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    (
        start.elapsed().as_nanos(),
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

async fn measure_app_with_body(
    app: &AppRuntime,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
    iterations: u64,
) -> (u128, usize, usize) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        let response = app
            .oneshot(method.clone(), uri, headers, Some(body.clone()))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    (
        start.elapsed().as_nanos(),
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

#[tokio::main]
async fn main() {
    let app = {
        let mut app = App::new();
        app.get("/plaintext", plaintext);
        app.build()
    };
    let openapi_app = {
        let mut app = App::new();
        app.get("/plaintext", plaintext);
        app.openapi("/openapi.json").swagger("/swagger");
        app.build()
    };
    let iterations = std::env::var("OAS_BENCH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000u64);
    if std::env::var("OAS_BENCH_CASE").ok().as_deref() == Some("dynamic-header") {
        let mut app = App::new();
        app.get("/trace/{id}", typed_header);
        let app = app.build();
        let (elapsed, allocations, bytes) = measure_app(
            &app,
            Method::GET,
            "/trace/abc123",
            &[("x-trace-id", "abc123")],
            iterations,
        )
        .await;
        println!(
            "case=dynamic-header-focused iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
            elapsed as f64 / iterations as f64,
            allocations as f64 / iterations as f64,
            bytes as f64 / iterations as f64,
        );
        return;
    }
    if std::env::var("OAS_BENCH_CASE").ok().as_deref() == Some("query") {
        let mut app = App::new();
        app.get("/search", typed_query);
        let app = app.build();
        let (elapsed, allocations, bytes) = measure_app(
            &app,
            Method::GET,
            "/search?page=42&active=true",
            &[],
            iterations,
        )
        .await;
        println!(
            "case=query-focused iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
            elapsed as f64 / iterations as f64,
            allocations as f64 / iterations as f64,
            bytes as f64 / iterations as f64,
        );
        return;
    }
    let start = Instant::now();
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for _ in 0..iterations {
        let response = app.oneshot(Method::GET, "/plaintext", &[], None).await;
        assert_eq!(response.status(), http::StatusCode::OK);
    }
    let elapsed = start.elapsed();
    let framework_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let framework_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let openapi_start = Instant::now();
    for _ in 0..iterations {
        let response = openapi_app
            .oneshot(Method::GET, "/plaintext", &[], None)
            .await;
        assert_eq!(response.status(), http::StatusCode::OK);
    }
    let openapi_elapsed = openapi_start.elapsed();
    let openapi_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let openapi_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_start = Instant::now();
    for _ in 0..iterations {
        assert_eq!(raw_plaintext().await.status(), StatusCode::OK);
    }
    let raw_elapsed = raw_start.elapsed();
    let raw_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    println!(
        "case=plaintext iterations={iterations} elapsed_ns={} ns_per_op={:.2} allocations={} allocations_per_op={:.4} bytes={} bytes_per_op={:.2} raw_elapsed_ns={} raw_ns_per_op={:.2} raw_allocations={} raw_allocations_per_op={:.4} raw_bytes={} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        elapsed.as_nanos(),
        elapsed.as_nanos() as f64 / iterations as f64,
        framework_allocations,
        framework_allocations as f64 / iterations as f64,
        framework_bytes,
        framework_bytes as f64 / iterations as f64,
        raw_elapsed.as_nanos(),
        raw_elapsed.as_nanos() as f64 / iterations as f64,
        raw_allocations,
        raw_allocations as f64 / iterations as f64,
        raw_bytes,
        raw_bytes as f64 / iterations as f64,
        (framework_allocations.saturating_sub(raw_allocations)) as f64 / iterations as f64,
        (framework_bytes.saturating_sub(raw_bytes)) as f64 / iterations as f64,
    );
    println!(
        "case=plaintext_openapi_enabled iterations={iterations} elapsed_ns={} ns_per_op={:.2} allocations={} allocations_per_op={:.4} bytes={} bytes_per_op={:.2} disabled_ns_per_op={:.2} delta_ns_per_op={:.2} delta_percent={:.2}",
        openapi_elapsed.as_nanos(),
        openapi_elapsed.as_nanos() as f64 / iterations as f64,
        openapi_allocations,
        openapi_allocations as f64 / iterations as f64,
        openapi_bytes,
        openapi_bytes as f64 / iterations as f64,
        elapsed.as_nanos() as f64 / iterations as f64,
        (openapi_elapsed.as_nanos() as f64 - elapsed.as_nanos() as f64) / iterations as f64,
        ((openapi_elapsed.as_nanos() as f64 / elapsed.as_nanos() as f64) - 1.0) * 100.0,
    );

    let mut static_text_app = App::new();
    static_text_app.static_text("/health", "OK");
    let static_text_app = static_text_app.build();
    let (static_text_elapsed, static_text_allocations, static_text_bytes) =
        measure_app(&static_text_app, Method::GET, "/health", &[], iterations).await;
    println!(
        "case=static-text iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        static_text_elapsed as f64 / iterations as f64,
        static_text_allocations as f64 / iterations as f64,
        static_text_bytes as f64 / iterations as f64,
    );

    let mut static_json_app = App::new();
    static_json_app.static_json("/version", Bytes::from_static(br#"{"version":"0.1.0"}"#));
    let static_json_app = static_json_app.build();
    let (static_json_elapsed, static_json_allocations, static_json_bytes) =
        measure_app(&static_json_app, Method::GET, "/version", &[], iterations).await;
    println!(
        "case=static-json-fast iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        static_json_elapsed as f64 / iterations as f64,
        static_json_allocations as f64 / iterations as f64,
        static_json_bytes as f64 / iterations as f64,
    );

    let mut json_bytes_app = App::new();
    json_bytes_app.get("/json", || async {
        JsonBytes::new(Bytes::from_static(br#"{"status":"ok"}"#))
    });
    let json_bytes_app = json_bytes_app.build();
    let (json_bytes_elapsed, json_bytes_allocations, json_bytes_bytes) =
        measure_app(&json_bytes_app, Method::GET, "/json", &[], iterations).await;
    println!(
        "case=json-bytes-response iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        json_bytes_elapsed as f64 / iterations as f64,
        json_bytes_allocations as f64 / iterations as f64,
        json_bytes_bytes as f64 / iterations as f64,
    );

    let mut path_app = App::new();
    path_app.get("/users/{id}", typed_path);
    let path_app = path_app.build();
    let (path_elapsed, path_allocations, path_bytes) =
        measure_app(&path_app, Method::GET, "/users/123456", &[], iterations).await;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_path_start = Instant::now();
    for _ in 0..iterations {
        let request = raw_request("/users/123456", &[]);
        assert_eq!(
            request
                .uri()
                .path()
                .strip_prefix("/users/")
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            123456
        );
        assert_eq!(raw_ok_response().status(), StatusCode::OK);
    }
    let raw_path_elapsed = raw_path_start.elapsed().as_nanos();
    let raw_path_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_path_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    assert!(
        path_allocations <= raw_path_allocations,
        "typed path added heap allocations"
    );
    assert!(
        path_bytes <= raw_path_bytes,
        "typed path added allocation bytes"
    );
    println!(
        "case=path-integer iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        path_elapsed as f64 / iterations as f64,
        raw_path_elapsed as f64 / iterations as f64,
        path_allocations as f64 / iterations as f64,
        raw_path_allocations as f64 / iterations as f64,
        path_bytes as f64 / iterations as f64,
        raw_path_bytes as f64 / iterations as f64,
        path_allocations.saturating_sub(raw_path_allocations) as f64 / iterations as f64,
        path_bytes.saturating_sub(raw_path_bytes) as f64 / iterations as f64,
    );

    let mut params_app = App::new();
    params_app.get("/params/{org}/{user}", typed_params);
    let params_app = params_app.build();
    let params_path = "/params/acme/alice";
    let (params_elapsed, params_allocations, params_bytes) =
        measure_app(&params_app, Method::GET, params_path, &[], iterations).await;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_params_start = Instant::now();
    for _ in 0..iterations {
        let request = raw_request(params_path, &[]);
        let mut parts = request
            .uri()
            .path()
            .split('/')
            .filter(|part| !part.is_empty());
        assert_eq!(parts.next(), Some("params"));
        assert_eq!(parts.next(), Some("acme"));
        assert_eq!(parts.next(), Some("alice"));
        assert_eq!(raw_ok_response().status(), StatusCode::OK);
    }
    let raw_params_elapsed = raw_params_start.elapsed().as_nanos();
    let raw_params_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_params_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    assert!(
        params_allocations <= raw_params_allocations + iterations as usize * 3,
        "materialized Params exceeded the three-allocation framework budget"
    );
    assert!(
        params_bytes <= raw_params_bytes + iterations as usize * 263,
        "materialized Params exceeded the allocation-byte framework budget"
    );
    println!(
        "case=params iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        params_elapsed as f64 / iterations as f64,
        raw_params_elapsed as f64 / iterations as f64,
        params_allocations as f64 / iterations as f64,
        raw_params_allocations as f64 / iterations as f64,
        params_bytes as f64 / iterations as f64,
        raw_params_bytes as f64 / iterations as f64,
        params_allocations.saturating_sub(raw_params_allocations) as f64 / iterations as f64,
        params_bytes.saturating_sub(raw_params_bytes) as f64 / iterations as f64,
    );

    let mut uuid_app = App::new();
    uuid_app.get("/users/{id}", typed_uuid);
    let uuid_app = uuid_app.build();
    let uuid_path = "/users/550e8400-e29b-41d4-a716-446655440000";
    let (uuid_elapsed, uuid_allocations, uuid_bytes) =
        measure_app(&uuid_app, Method::GET, uuid_path, &[], iterations).await;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_uuid_start = Instant::now();
    for _ in 0..iterations {
        let request = raw_request(uuid_path, &[]);
        assert_eq!(
            request
                .uri()
                .path()
                .strip_prefix("/users/")
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
        assert_eq!(raw_ok_response().status(), StatusCode::OK);
    }
    let raw_uuid_elapsed = raw_uuid_start.elapsed().as_nanos();
    let raw_uuid_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_uuid_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    assert!(
        uuid_allocations <= raw_uuid_allocations,
        "typed UUID path added heap allocations"
    );
    assert!(
        uuid_bytes <= raw_uuid_bytes,
        "typed UUID path added allocation bytes"
    );
    println!(
        "case=path-uuid iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        uuid_elapsed as f64 / iterations as f64,
        raw_uuid_elapsed as f64 / iterations as f64,
        uuid_allocations as f64 / iterations as f64,
        raw_uuid_allocations as f64 / iterations as f64,
        uuid_bytes as f64 / iterations as f64,
        raw_uuid_bytes as f64 / iterations as f64,
        uuid_allocations.saturating_sub(raw_uuid_allocations) as f64 / iterations as f64,
        uuid_bytes.saturating_sub(raw_uuid_bytes) as f64 / iterations as f64,
    );

    let mut query_app = App::new();
    query_app.get("/search", typed_query);
    let query_app = query_app.build();
    let (query_elapsed, query_allocations, query_bytes) = measure_app(
        &query_app,
        Method::GET,
        "/search?page=42&active=true",
        &[],
        iterations,
    )
    .await;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_query_start = Instant::now();
    for _ in 0..iterations {
        let request = raw_request("/search?page=42&active=true", &[]);
        let mut values = request.uri().query().unwrap().split('&');
        let page = values
            .next()
            .unwrap()
            .strip_prefix("page=")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let active = values
            .next()
            .unwrap()
            .strip_prefix("active=")
            .unwrap()
            .parse::<bool>()
            .unwrap();
        assert_eq!((page, active), (42, true));
        assert_eq!(raw_ok_response().status(), StatusCode::OK);
    }
    let raw_query_elapsed = raw_query_start.elapsed().as_nanos();
    let raw_query_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_query_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    assert!(
        query_allocations <= raw_query_allocations,
        "typed query added heap allocations"
    );
    assert!(
        query_bytes <= raw_query_bytes,
        "typed query added allocation bytes"
    );
    println!(
        "case=query iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        query_elapsed as f64 / iterations as f64,
        raw_query_elapsed as f64 / iterations as f64,
        query_allocations as f64 / iterations as f64,
        raw_query_allocations as f64 / iterations as f64,
        query_bytes as f64 / iterations as f64,
        raw_query_bytes as f64 / iterations as f64,
        query_allocations.saturating_sub(raw_query_allocations) as f64 / iterations as f64,
        query_bytes.saturating_sub(raw_query_bytes) as f64 / iterations as f64,
    );

    let mut json_request_app = App::new();
    json_request_app.post("/json-request", typed_json_request);
    let json_request_app = json_request_app.build();
    let (json_request_elapsed, json_request_allocations, json_request_bytes) =
        measure_app_with_body(
            &json_request_app,
            Method::POST,
            "/json-request",
            &[("content-type", "application/json")],
            Bytes::from_static(br#"{"name":"Ada"}"#),
            iterations,
        )
        .await;
    println!(
        "case=json-request iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        json_request_elapsed as f64 / iterations as f64,
        json_request_allocations as f64 / iterations as f64,
        json_request_bytes as f64 / iterations as f64,
    );

    let mut header_app = App::new();
    header_app.get("/trace", typed_header);
    let header_app = header_app.build();
    let (header_elapsed, header_allocations, header_bytes) = measure_app(
        &header_app,
        Method::GET,
        "/trace",
        &[("x-trace-id", "abc123")],
        iterations,
    )
    .await;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let raw_header_start = Instant::now();
    for _ in 0..iterations {
        let request = raw_request("/trace", &[("x-trace-id", "abc123")]);
        assert_eq!(
            request
                .headers()
                .get("x-trace-id")
                .unwrap()
                .to_str()
                .unwrap(),
            "abc123"
        );
        assert_eq!(raw_ok_response().status(), StatusCode::OK);
    }
    let raw_header_elapsed = raw_header_start.elapsed().as_nanos();
    let raw_header_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let raw_header_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    assert!(
        header_allocations <= raw_header_allocations,
        "typed header added heap allocations"
    );
    assert!(
        header_bytes <= raw_header_bytes,
        "typed header added allocation bytes"
    );
    println!(
        "case=header iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2} extra_allocations_per_op={:.4} extra_bytes_per_op={:.2}",
        header_elapsed as f64 / iterations as f64,
        raw_header_elapsed as f64 / iterations as f64,
        header_allocations as f64 / iterations as f64,
        raw_header_allocations as f64 / iterations as f64,
        header_bytes as f64 / iterations as f64,
        raw_header_bytes as f64 / iterations as f64,
        header_allocations.saturating_sub(raw_header_allocations) as f64 / iterations as f64,
        header_bytes.saturating_sub(raw_header_bytes) as f64 / iterations as f64,
    );

    let mut dynamic_header_app = App::new();
    dynamic_header_app.get("/trace/{id}", typed_header);
    let dynamic_header_app = dynamic_header_app.build();
    let (dynamic_header_elapsed, dynamic_header_allocations, dynamic_header_bytes) = measure_app(
        &dynamic_header_app,
        Method::GET,
        "/trace/abc123",
        &[("x-trace-id", "abc123")],
        iterations,
    )
    .await;
    println!(
        "case=dynamic-header-no-capture iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        dynamic_header_elapsed as f64 / iterations as f64,
        dynamic_header_allocations as f64 / iterations as f64,
        dynamic_header_bytes as f64 / iterations as f64,
    );

    for route_count in [1_usize, 10, 100, 1_000, 10_000] {
        let mut route_app = App::new();
        route_app.get("/fixed/path", plaintext);
        let mut raw_routes = HashMap::with_capacity(route_count);
        raw_routes.insert("/fixed/path".to_owned(), vec![Method::GET]);
        for index in 1..route_count {
            let path = format!("/static/{index}");
            route_app.get(&path, plaintext);
            raw_routes.insert(path, vec![Method::GET]);
        }
        let route_app = route_app.build();

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        let route_start = Instant::now();
        for _ in 0..iterations {
            let response = route_app
                .oneshot(Method::GET, "/fixed/path", &[], None)
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let route_elapsed = route_start.elapsed();
        let route_allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let route_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        let raw_route_start = Instant::now();
        for _ in 0..iterations {
            assert!(
                raw_routes
                    .get("/fixed/path")
                    .is_some_and(|methods| methods.contains(&Method::GET))
            );
            assert_eq!(raw_plaintext().await.status(), StatusCode::OK);
        }
        let raw_route_elapsed = raw_route_start.elapsed();
        let raw_route_allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let raw_route_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
        println!(
            "case=static_route_count route_count={route_count} iterations={iterations} ns_per_op={:.2} raw_ns_per_op={:.2} allocations_per_op={:.4} raw_allocations_per_op={:.4} bytes_per_op={:.2} raw_bytes_per_op={:.2}",
            route_elapsed.as_nanos() as f64 / iterations as f64,
            raw_route_elapsed.as_nanos() as f64 / iterations as f64,
            route_allocations as f64 / iterations as f64,
            raw_route_allocations as f64 / iterations as f64,
            route_bytes as f64 / iterations as f64,
            raw_route_bytes as f64 / iterations as f64,
        );
    }

    let dynamic_iterations = std::env::var("OAS_DYNAMIC_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000_u64);
    for route_count in [1_usize, 10, 100, 1_000, 10_000] {
        let mut dynamic_app = App::new();
        for index in 0..route_count {
            let path = format!("/dynamic/{index}/{{id}}");
            dynamic_app.get(&path, typed_path);
        }
        let dynamic_app = dynamic_app.build();
        for (position, target) in [
            ("first", "/dynamic/0/42".to_owned()),
            ("middle", format!("/dynamic/{}/42", route_count / 2)),
            ("last", format!("/dynamic/{}/42", route_count - 1)),
            ("miss", "/dynamic/missing/42".to_owned()),
        ] {
            let expected = if position == "miss" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            };
            let start = Instant::now();
            for _ in 0..dynamic_iterations {
                let response = dynamic_app.oneshot(Method::GET, &target, &[], None).await;
                assert_eq!(response.status(), expected);
            }
            println!(
                "case=dynamic_route_scale route_count={route_count} position={position} iterations={dynamic_iterations} ns_per_op={:.2}",
                start.elapsed().as_nanos() as f64 / dynamic_iterations as f64,
            );
        }

        for (case, method, target, expected) in [
            (
                "dynamic-405",
                Method::POST,
                format!("/dynamic/{}/42", route_count / 2),
                StatusCode::METHOD_NOT_ALLOWED,
            ),
            (
                "dynamic-options",
                Method::OPTIONS,
                format!("/dynamic/{}/42", route_count / 2),
                StatusCode::NO_CONTENT,
            ),
            (
                "dynamic-miss-405",
                Method::POST,
                "/dynamic/missing/42".to_owned(),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let start = Instant::now();
            for _ in 0..dynamic_iterations {
                let response = dynamic_app
                    .oneshot(method.clone(), &target, &[], None)
                    .await;
                assert_eq!(response.status(), expected);
            }
            println!(
                "case=dynamic_route_scale route_count={route_count} position={case} iterations={dynamic_iterations} ns_per_op={:.2}",
                start.elapsed().as_nanos() as f64 / dynamic_iterations as f64,
            );
        }
    }
}
