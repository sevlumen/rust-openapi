use std::time::Instant;

use oas_rs::{App, Method};

async fn plaintext() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    let mut app = App::new();
    app.get("/plaintext", plaintext);
    let iterations = std::env::var("OAS_BENCH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000u64);
    let start = Instant::now();
    for _ in 0..iterations {
        let response = app.oneshot(Method::GET, "/plaintext", &[], None).await;
        assert_eq!(response.status(), http::StatusCode::OK);
    }
    let elapsed = start.elapsed();
    println!(
        "case=plaintext iterations={iterations} elapsed_ns={} ns_per_op={:.2}",
        elapsed.as_nanos(),
        elapsed.as_nanos() as f64 / iterations as f64
    );
}
