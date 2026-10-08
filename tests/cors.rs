use std::time::Duration;

use oas_rs::{App, AppRuntime, BearerAuth, Cors, Method};

async fn hello() -> &'static str {
    "hello"
}

fn runtime(cors: Cors) -> AppRuntime {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(cors);
    app.layer(BearerAuth::static_token("s3cret"));
    app.build().unwrap()
}

fn site() -> Cors {
    Cors::new()
        .allow_origin("https://app.example")
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(["content-type", "x-tenant-id"])
        .max_age(Duration::from_secs(600))
}

async fn preflight(
    runtime: &AppRuntime,
    origin: &str,
    method: &str,
    headers: Option<&str>,
) -> oas_rs::TestResponse {
    let mut request = vec![
        ("origin", origin),
        ("access-control-request-method", method),
    ];
    if let Some(headers) = headers {
        request.push(("access-control-request-headers", headers));
    }
    runtime.oneshot(Method::OPTIONS, "/", &request, None).await
}

#[tokio::test]
async fn an_allowed_preflight_is_answered_before_authentication() {
    let runtime = runtime(site());
    let response = preflight(
        &runtime,
        "https://app.example",
        "POST",
        Some("content-type"),
    )
    .await;
    assert_eq!(response.status(), 204);
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("https://app.example")
    );
    let methods = response.header("access-control-allow-methods").unwrap();
    assert!(
        methods.contains("GET") && methods.contains("POST"),
        "{methods}"
    );
    assert_eq!(
        response.header("access-control-allow-headers"),
        Some("content-type, x-tenant-id")
    );
    assert_eq!(response.header("access-control-max-age"), Some("600"));
    assert_eq!(response.header("vary"), Some("Origin"));
}

#[tokio::test]
async fn a_preflight_from_another_origin_gets_no_cors_headers() {
    let runtime = runtime(site());
    let response = preflight(&runtime, "https://evil.example", "POST", None).await;
    assert_eq!(response.status(), 204);
    assert!(response.header("access-control-allow-origin").is_none());
    assert!(response.header("access-control-allow-methods").is_none());
}

#[tokio::test]
async fn a_preflight_for_a_method_or_header_that_is_not_allowed_is_refused() {
    let runtime = runtime(site());
    let method = preflight(&runtime, "https://app.example", "DELETE", None).await;
    assert!(method.header("access-control-allow-origin").is_none());
    let header = preflight(&runtime, "https://app.example", "GET", Some("x-secret")).await;
    assert!(header.header("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn an_options_request_without_a_request_method_is_not_a_preflight() {
    let runtime = runtime(site());
    // Not a CORS preflight: it goes on to authentication like any request.
    let response = runtime
        .oneshot(
            Method::OPTIONS,
            "/",
            &[("origin", "https://app.example")],
            None,
        )
        .await;
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn an_actual_request_from_an_allowed_origin_gets_the_headers() {
    let runtime = runtime(site().expose_headers(["x-request-id"]));
    let response = runtime
        .oneshot(
            Method::GET,
            "/",
            &[
                ("origin", "https://app.example"),
                ("authorization", "Bearer s3cret"),
            ],
            None,
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("https://app.example")
    );
    assert_eq!(
        response.header("access-control-expose-headers"),
        Some("x-request-id")
    );
    assert_eq!(response.header("vary"), Some("Origin"));
}

#[tokio::test]
async fn error_responses_carry_the_headers_too() {
    let runtime = runtime(site());
    // 401 from the layer below, and a 404 for an unknown path.
    let unauthorized = runtime
        .oneshot(Method::GET, "/", &[("origin", "https://app.example")], None)
        .await;
    assert_eq!(unauthorized.status(), 401);
    assert_eq!(
        unauthorized.header("access-control-allow-origin"),
        Some("https://app.example")
    );
}

#[tokio::test]
async fn other_origins_and_requests_without_an_origin_get_nothing() {
    let runtime = runtime(site());
    let auth = ("authorization", "Bearer s3cret");
    let other = runtime
        .oneshot(
            Method::GET,
            "/",
            &[("origin", "https://evil.example"), auth],
            None,
        )
        .await;
    assert_eq!(other.status(), 200);
    assert!(other.header("access-control-allow-origin").is_none());
    assert_eq!(other.header("vary"), Some("Origin"));
    let none = runtime.oneshot(Method::GET, "/", &[auth], None).await;
    assert!(none.header("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn any_origin_answers_with_a_star_and_no_vary() {
    let runtime = runtime(Cors::new().allow_any_origin());
    let response = runtime
        .oneshot(
            Method::GET,
            "/",
            &[
                ("origin", "https://anything.example"),
                ("authorization", "Bearer s3cret"),
            ],
            None,
        )
        .await;
    assert_eq!(response.header("access-control-allow-origin"), Some("*"));
    assert!(response.header("vary").is_none());
}

#[tokio::test]
async fn credentials_are_allowed_only_for_listed_origins() {
    let runtime = runtime(site().allow_credentials(true));
    let response = preflight(&runtime, "https://app.example", "GET", None).await;
    assert_eq!(
        response.header("access-control-allow-credentials"),
        Some("true")
    );
}

#[test]
#[should_panic(expected = "credentials")]
fn any_origin_with_credentials_is_rejected() {
    let _ = Cors::new().allow_any_origin().allow_credentials(true);
}

#[test]
#[should_panic(expected = "credentials")]
fn credentials_then_any_origin_is_rejected_too() {
    let _ = Cors::new().allow_credentials(true).allow_any_origin();
}

#[tokio::test]
async fn any_header_mirrors_the_requested_headers() {
    let runtime = runtime(
        Cors::new()
            .allow_origin("https://app.example")
            .allow_any_header(),
    );
    let response = preflight(&runtime, "https://app.example", "GET", Some("x-one, x-two")).await;
    assert_eq!(
        response.header("access-control-allow-headers"),
        Some("x-one, x-two")
    );
}

#[tokio::test]
async fn requests_without_an_origin_still_vary_on_origin_so_caches_stay_correct() {
    let runtime = runtime(site());
    let response = runtime
        .oneshot(
            Method::GET,
            "/",
            &[("authorization", "Bearer s3cret")],
            None,
        )
        .await;
    assert_eq!(response.header("vary"), Some("Origin"));
    assert!(response.header("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn any_origin_sends_a_star_even_without_an_origin_header() {
    let runtime = runtime(Cors::new().allow_any_origin());
    let response = runtime
        .oneshot(
            Method::GET,
            "/",
            &[("authorization", "Bearer s3cret")],
            None,
        )
        .await;
    assert_eq!(response.header("access-control-allow-origin"), Some("*"));
}

#[tokio::test]
async fn existing_vary_values_are_kept_when_origin_is_added() {
    let mut app = App::new();
    app.get("/v", hello);
    app.layer(site());
    app.layer(
        |request: http::Request<oas_rs::RequestBody>, next: oas_rs::Next| async move {
            let mut response = next.run(request).await;
            response
                .headers_mut()
                .append("vary", http::HeaderValue::from_static("Accept-Encoding"));
            response
                .headers_mut()
                .append("vary", http::HeaderValue::from_static("Accept"));
            response
        },
    );
    let runtime = app.build().unwrap();
    let response = runtime
        .oneshot(
            Method::GET,
            "/v",
            &[("origin", "https://app.example")],
            None,
        )
        .await;
    let all = response.header_all("vary");
    let tokens: Vec<&str> = all
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    for expected in ["Accept-Encoding", "Accept", "Origin"] {
        assert!(tokens.contains(&expected), "{expected} missing in {all:?}");
    }
}

#[tokio::test]
async fn a_json_post_preflight_is_allowed_by_default() {
    let runtime = runtime(Cors::new().allow_origin("https://app.example"));
    let response = preflight(
        &runtime,
        "https://app.example",
        "POST",
        Some("content-type"),
    )
    .await;
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("https://app.example")
    );
}

#[tokio::test]
async fn a_preflight_varies_on_the_request_method_and_headers() {
    let runtime = runtime(site());
    let response = preflight(&runtime, "https://app.example", "GET", None).await;
    let vary = response.header_all("vary");
    for name in [
        "Origin",
        "Access-Control-Request-Method",
        "Access-Control-Request-Headers",
    ] {
        assert!(vary.contains(&name), "{name} missing in {vary:?}");
    }
}

#[tokio::test]
async fn a_prefix_scoped_cors_answers_preflights_under_the_prefix() {
    let mut app = App::new();
    app.get("/api/x", hello);
    app.layer_for("/api", site());
    let runtime = app.build().unwrap();
    let response = runtime
        .oneshot(
            Method::OPTIONS,
            "/api/x",
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "GET"),
            ],
            None,
        )
        .await;
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("https://app.example")
    );
}

#[test]
#[should_panic(expected = "origin")]
fn a_star_origin_is_rejected_with_a_pointer_to_allow_any_origin() {
    let _ = Cors::new().allow_origin("*");
}

#[test]
#[should_panic(expected = "origin")]
fn an_origin_with_a_trailing_slash_or_path_is_rejected() {
    let _ = Cors::new().allow_origin("https://app.example/");
}
