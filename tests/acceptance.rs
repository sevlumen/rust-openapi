use oas_rs::{App, Method, Path, Query, State};
use serde::Deserialize;

#[derive(Clone, Default)]
struct TestState;

#[derive(Deserialize)]
struct Search {
    page: u32,
    active: bool,
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

#[test]
fn openapi_describes_registered_operations() {
    let mut app = App::new();
    app.get("/plaintext", hello)
        .tag("Benchmark")
        .summary("Static response");
    let document = app.openapi_document();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["paths"]["/plaintext"]["get"]["summary"], "Static response");
}

