use bytes::Bytes;
use oas_rs::{App, Json, Method};
use serde_json::Value;

async fn echo(Json(_body): Json<Value>) -> &'static str {
    "ok"
}

fn json_of_len(len: usize) -> Bytes {
    // {"a":"xxx..."} with `len` bytes in total.
    let filler = "x".repeat(len - 8);
    Bytes::from(format!("{{\"a\":\"{filler}\"}}"))
}

async fn post(runtime: &oas_rs::AppRuntime, uri: &str, body: Bytes) -> u16 {
    runtime
        .oneshot(
            Method::POST,
            uri,
            &[("content-type", "application/json")],
            Some(body),
        )
        .await
        .status()
        .as_u16()
}

#[tokio::test]
async fn body_limit_applies_to_one_route_only() {
    let mut app = App::new();
    app.post("/small", echo);
    app.post("/big", echo).body_limit(2 * 1024 * 1024);
    let runtime = app.build().unwrap();

    let default_limit = 1024 * 1024;
    assert_eq!(
        post(&runtime, "/small", json_of_len(default_limit)).await,
        200
    );
    assert_eq!(
        post(&runtime, "/small", json_of_len(default_limit + 1)).await,
        413
    );
    assert_eq!(
        post(&runtime, "/big", json_of_len(default_limit + 1)).await,
        200
    );
    assert_eq!(
        post(&runtime, "/big", json_of_len(2 * 1024 * 1024)).await,
        200
    );
    assert_eq!(
        post(&runtime, "/big", json_of_len(2 * 1024 * 1024 + 1)).await,
        413
    );
}

#[tokio::test]
async fn body_limit_can_also_lower_a_routes_limit() {
    let mut app = App::new();
    app.post("/tiny", echo).body_limit(64);
    app.post("/normal", echo);
    let runtime = app.build().unwrap();

    assert_eq!(post(&runtime, "/tiny", json_of_len(64)).await, 200);
    assert_eq!(post(&runtime, "/tiny", json_of_len(65)).await, 413);
    assert_eq!(post(&runtime, "/normal", json_of_len(65)).await, 200);
}

#[tokio::test]
async fn body_limit_on_a_route_without_a_buffered_body_is_a_no_op() {
    async fn hello() -> &'static str {
        "hello"
    }
    let mut app = App::new();
    app.get("/", hello).body_limit(10);
    let runtime = app.build().unwrap();
    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
}
