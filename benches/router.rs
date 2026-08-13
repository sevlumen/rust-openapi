use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use oas_rs::{App, Method};

async fn plaintext() -> &'static str {
    "OK"
}

async fn raw_plaintext() -> Response<Full<Bytes>> {
    let _request = Request::builder()
        .method(Method::GET)
        .uri("/plaintext")
        .body(Bytes::new())
        .unwrap();
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", "2")
        .body(Full::new(Bytes::from_static(b"OK")))
        .unwrap()
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

#[tokio::main]
async fn main() {
    let mut app = App::new();
    app.get("/plaintext", plaintext);
    let iterations = std::env::var("OAS_BENCH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000u64);
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
}
