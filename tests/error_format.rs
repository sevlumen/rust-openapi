use http::Request;
use oas_rs::{
    ApiError, ApiSchema, App, AppRuntime, BearerAuth, ErrorFormat, ErrorInfo, IntoResponse, Json,
    Method, Next, RequestBody,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
}

async fn hello() -> &'static str {
    "hello"
}

async fn failing() -> Result<&'static str, ApiError> {
    Err(ApiError::bad_request("it broke"))
}

async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

fn legacy() -> ErrorFormat {
    ErrorFormat::new(|info: &ErrorInfo| info.respond(json!({ "error": info.detail })))
}

fn runtime() -> AppRuntime {
    let mut app = App::new();
    app.layer(legacy());
    app.layer(BearerAuth::static_token("s3cret").exempt_paths(["/", "/fail", "/items", "/teapot"]));
    app.layer(|request: Request<RequestBody>, next: Next| async move {
        if request.uri().path() == "/teapot" {
            let mut response = "short and stout".into_response();
            *response.status_mut() = http::StatusCode::IM_A_TEAPOT;
            return response;
        }
        next.run(request).await
    });
    app.get("/", hello);
    app.get("/fail", failing);
    app.post("/items", create);
    app.build().unwrap()
}

async fn body_json(response: oas_rs::TestResponse) -> Value {
    serde_json::from_str(&response.body_string().await).unwrap()
}

#[tokio::test]
async fn a_handler_error_uses_the_custom_format() {
    let response = runtime().oneshot(Method::GET, "/fail", &[], None).await;
    assert_eq!(response.status(), 400);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(body_json(response).await, json!({ "error": "it broke" }));
}

#[tokio::test]
async fn extractor_errors_not_found_and_method_not_allowed_use_it_too() {
    let runtime = runtime();
    let bad_body = runtime
        .oneshot(
            Method::POST,
            "/items",
            &[("content-type", "application/json")],
            Some(bytes::Bytes::from_static(b"{bad")),
        )
        .await;
    assert_eq!(bad_body.status(), 400);
    assert!(body_json(bad_body).await["error"].is_string());

    let missing = runtime
        .oneshot(
            Method::GET,
            "/missing",
            &[("authorization", "Bearer s3cret")],
            None,
        )
        .await;
    assert_eq!(missing.status(), 404);
    assert!(body_json(missing).await["error"].is_string());

    let wrong_method = runtime
        .oneshot(
            Method::DELETE,
            "/",
            &[("authorization", "Bearer s3cret")],
            None,
        )
        .await;
    assert_eq!(wrong_method.status(), 405);
    assert!(wrong_method.header("allow").is_some(), "Allow must survive");
    assert!(body_json(wrong_method).await["error"].is_string());
}

#[tokio::test]
async fn an_auth_error_keeps_its_challenge_header() {
    let response = runtime().oneshot(Method::GET, "/other", &[], None).await;
    assert_eq!(response.status(), 401);
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));
    assert!(body_json(response).await["error"].is_string());
}

#[tokio::test]
async fn successful_and_application_defined_responses_are_untouched() {
    let runtime = runtime();
    let ok = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.body_string().await, "hello");
    let teapot = runtime.oneshot(Method::GET, "/teapot", &[], None).await;
    assert_eq!(teapot.status(), 418);
    assert_eq!(teapot.body_string().await, "short and stout");
}

#[tokio::test]
async fn a_head_error_response_has_no_body() {
    let response = runtime()
        .oneshot(
            Method::HEAD,
            "/missing",
            &[("authorization", "Bearer s3cret")],
            None,
        )
        .await;
    assert_eq!(response.status(), 404);
    assert_eq!(response.body_string().await, "");
}

#[tokio::test]
async fn without_the_layer_errors_keep_the_default_problem_format() {
    let mut app = App::new();
    app.get("/fail", failing);
    let response = app
        .build()
        .unwrap()
        .oneshot(Method::GET, "/fail", &[], None)
        .await;
    let body = body_json(response).await;
    assert_eq!(body["title"], "Bad Request");
    assert_eq!(body["detail"], "it broke");
}
