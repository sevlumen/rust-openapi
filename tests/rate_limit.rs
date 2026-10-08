use std::time::Duration;

use oas_rs::{App, AppRuntime, Method, RateLimit};
use serde_json::Value;

async fn hello() -> &'static str {
    "hello"
}

fn runtime(limit: RateLimit) -> AppRuntime {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(limit);
    app.build().unwrap()
}

async fn status(runtime: &AppRuntime, key: Option<&str>) -> u16 {
    let headers: Vec<(&str, &str)> = key.map(|key| ("x-api-key", key)).into_iter().collect();
    runtime
        .oneshot(Method::GET, "/", &headers, None)
        .await
        .status()
        .as_u16()
}

#[tokio::test(start_paused = true)]
async fn a_burst_is_served_then_requests_are_rejected_with_retry_after() {
    let runtime = runtime(RateLimit::new(2, Duration::from_secs(1)));
    assert_eq!(status(&runtime, None).await, 200);
    assert_eq!(status(&runtime, None).await, 200);
    let rejected = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(rejected.status(), 429);
    let retry: u64 = rejected.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=1).contains(&retry), "{retry}");
    let body: Value = serde_json::from_str(&rejected.body_string().await).unwrap();
    assert_eq!(body["title"], "Too Many Requests");
}

#[tokio::test(start_paused = true)]
async fn tokens_come_back_over_time() {
    let runtime = runtime(RateLimit::new(2, Duration::from_secs(1)));
    for _ in 0..2 {
        assert_eq!(status(&runtime, None).await, 200);
    }
    assert_eq!(status(&runtime, None).await, 429);
    tokio::time::sleep(Duration::from_millis(600)).await; // 1.2 tokens
    assert_eq!(status(&runtime, None).await, 200);
    assert_eq!(status(&runtime, None).await, 429);
    tokio::time::sleep(Duration::from_secs(10)).await; // capped at the burst
    assert_eq!(status(&runtime, None).await, 200);
    assert_eq!(status(&runtime, None).await, 200);
    assert_eq!(status(&runtime, None).await, 429);
}

#[tokio::test(start_paused = true)]
async fn burst_can_differ_from_the_rate() {
    let runtime = runtime(RateLimit::new(1, Duration::from_secs(1)).burst(3));
    for _ in 0..3 {
        assert_eq!(status(&runtime, None).await, 200);
    }
    assert_eq!(status(&runtime, None).await, 429);
}

#[tokio::test(start_paused = true)]
async fn each_key_has_its_own_bucket() {
    let runtime = runtime(RateLimit::new(1, Duration::from_secs(1)).key_by_header("x-api-key"));
    assert_eq!(status(&runtime, Some("a")).await, 200);
    assert_eq!(status(&runtime, Some("a")).await, 429);
    assert_eq!(status(&runtime, Some("b")).await, 200);
    // Requests without the header share one anonymous bucket.
    assert_eq!(status(&runtime, None).await, 200);
    assert_eq!(status(&runtime, None).await, 429);
}

#[tokio::test(start_paused = true)]
async fn varying_the_key_cannot_grow_memory_or_bypass_the_limit() {
    let runtime = runtime(
        RateLimit::new(1, Duration::from_secs(60))
            .key_by_header("x-api-key")
            .max_keys(2),
    );
    assert_eq!(status(&runtime, Some("a")).await, 200);
    assert_eq!(status(&runtime, Some("b")).await, 200);
    // The table is full: new keys share one overflow bucket.
    assert_eq!(status(&runtime, Some("c")).await, 200);
    assert_eq!(status(&runtime, Some("d")).await, 429);
    assert_eq!(status(&runtime, Some("e")).await, 429);
    // Known keys keep their own bucket.
    assert_eq!(status(&runtime, Some("a")).await, 429);
}

#[tokio::test(start_paused = true)]
async fn idle_keys_are_forgotten_so_the_table_does_not_fill_for_good() {
    let runtime = runtime(
        RateLimit::new(1, Duration::from_secs(1))
            .key_by_header("x-api-key")
            .max_keys(2),
    );
    assert_eq!(status(&runtime, Some("a")).await, 200);
    assert_eq!(status(&runtime, Some("b")).await, 200);
    tokio::time::sleep(Duration::from_secs(5)).await; // both buckets full again
    assert_eq!(status(&runtime, Some("c")).await, 200);
    // `c` has its own bucket now (a and b were purged), so a second request
    // from `c` is limited by it, and `d` is admitted on its own too.
    assert_eq!(status(&runtime, Some("c")).await, 429);
    assert_eq!(status(&runtime, Some("d")).await, 200);
    assert_eq!(status(&runtime, Some("d")).await, 429);
}

#[test]
#[should_panic(expected = "at least 1")]
fn a_zero_limit_is_rejected() {
    let _ = RateLimit::new(0, Duration::from_secs(1));
}
