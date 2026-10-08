//! Contention of the rate limiter under many threads: one lock (a table small
//! enough to stay a single shard) against the default 16 shards, same
//! workload. Run with
//! `cargo bench --bench rate_limit --features test-util`; the numbers are for
//! comparing the two lines on the same machine, not absolute.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use oas_rs::{App, AppRuntime, Method, RateLimit};

async fn hello() -> &'static str {
    "hello"
}

fn runtime(limit: RateLimit) -> Arc<AppRuntime> {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(limit);
    Arc::new(app.build().unwrap())
}

fn measure(label: &str, limit: RateLimit, threads: usize, seconds: u64) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();
    let runtime = runtime(limit);
    let done = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    rt.block_on(async {
        let mut tasks = Vec::new();
        for worker in 0..threads {
            let (runtime, done, total) = (runtime.clone(), done.clone(), total.clone());
            tasks.push(tokio::spawn(async move {
                let mut count = 0u64;
                let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (worker as u64 + 1);
                while !done.load(Ordering::Relaxed) {
                    // Spread over 400 keys: far under both table caps, so no
                    // overflow bucket is involved and only the locking differs.
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let key = format!("client-{}", state % 400);
                    let _ = runtime
                        .oneshot(Method::GET, "/", &[("x-api-key", key.as_str())], None)
                        .await;
                    count += 1;
                    // Without this the workers never let the timer run.
                    if count % 64 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                total.fetch_add(count, Ordering::Relaxed);
            }));
        }
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        done.store(true, Ordering::Relaxed);
        for task in tasks {
            let _ = task.await;
        }
    });
    let operations = total.load(Ordering::Relaxed) as f64;
    println!(
        "{label:<28} {threads} threads: {:>10.0} requests/s",
        operations / started.elapsed().as_secs_f64()
    );
}

fn main() {
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    for round in 1..=3 {
        println!("round {round}");
        // A cap below 1,024 keeps one shard (one lock).
        measure(
            "one lock (max_keys 512)",
            RateLimit::new(1_000_000, Duration::from_secs(1))
                .key_by_header("x-api-key")
                .max_keys(512),
            threads,
            2,
        );
        measure(
            "16 shards (default)",
            RateLimit::new(1_000_000, Duration::from_secs(1)).key_by_header("x-api-key"),
            threads,
            2,
        );
    }
}
