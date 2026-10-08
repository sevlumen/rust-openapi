use std::time::Duration;

use http::{HeaderName, HeaderValue, StatusCode};
use oas_rs::{App, Cookies, Html, Method, Redirect, ResponseExt, SameSite, SetCookie};

async fn plain() -> oas_rs::Headered<&'static str> {
    "body"
        .with_header(
            HeaderName::from_static("x-one"),
            HeaderValue::from_static("1"),
        )
        .with_header(
            HeaderName::from_static("x-two"),
            HeaderValue::from_static("2"),
        )
}

async fn created() -> oas_rs::Headered<&'static str> {
    "made".with_status(StatusCode::CREATED)
}

async fn login() -> oas_rs::Headered<&'static str> {
    "ok".with_cookie(SetCookie::new("sid", "abc").path("/"))
        .with_cookie(SetCookie::new("theme", "dark"))
}

async fn go() -> Redirect {
    Redirect::see_other("/target")
}

async fn page() -> Html<&'static str> {
    Html("<p>hi</p>")
}

async fn read(cookies: Cookies) -> String {
    format!(
        "{}|{}|{}",
        cookies.get("a").unwrap_or("-"),
        cookies.get("b").unwrap_or("-"),
        cookies.get("missing").unwrap_or("-")
    )
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/plain", plain);
    app.get("/created", created);
    app.get("/login", login);
    app.get("/go", go);
    app.get("/page", page);
    app.get("/read", read);
    app.build().unwrap()
}

#[tokio::test]
async fn headers_and_status_can_be_added_to_any_response() {
    let runtime = runtime();
    let response = runtime.oneshot(Method::GET, "/plain", &[], None).await;
    assert_eq!(response.header("x-one"), Some("1"));
    assert_eq!(response.header("x-two"), Some("2"));
    assert_eq!(response.body_string().await, "body");
    let created = runtime.oneshot(Method::GET, "/created", &[], None).await;
    assert_eq!(created.status(), 201);
}

#[tokio::test]
async fn several_set_cookie_headers_are_all_kept() {
    let response = runtime().oneshot(Method::GET, "/login", &[], None).await;
    assert_eq!(
        response.header_all("set-cookie"),
        ["sid=abc; Path=/", "theme=dark"]
    );
}

#[tokio::test]
async fn a_redirect_has_a_location_and_no_body() {
    let response = runtime().oneshot(Method::GET, "/go", &[], None).await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.header("location"), Some("/target"));
    assert_eq!(response.body_string().await, "");
}

#[test]
fn every_redirect_status_is_available() {
    use oas_rs::IntoResponse;
    for (redirect, status) in [
        (Redirect::found("/x"), 302),
        (Redirect::see_other("/x"), 303),
        (Redirect::temporary("/x"), 307),
        (Redirect::permanent("/x"), 308),
    ] {
        assert_eq!(redirect.into_response().status(), status);
    }
}

#[tokio::test]
async fn html_sets_its_content_type() {
    let response = runtime().oneshot(Method::GET, "/page", &[], None).await;
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(response.body_string().await, "<p>hi</p>");
}

#[test]
fn set_cookie_renders_every_attribute() {
    let cookie = SetCookie::new("sid", "abc")
        .path("/app")
        .domain("example.com")
        .max_age(Duration::from_secs(3600))
        .http_only()
        .secure()
        .same_site(SameSite::Lax);
    assert_eq!(
        cookie.to_string(),
        "sid=abc; Path=/app; Domain=example.com; Max-Age=3600; HttpOnly; Secure; SameSite=Lax"
    );
    assert_eq!(
        SetCookie::new("a", "b")
            .same_site(SameSite::None)
            .to_string(),
        "a=b; SameSite=None"
    );
    assert_eq!(
        SetCookie::new("a", "").max_age(Duration::ZERO).to_string(),
        "a=; Max-Age=0"
    );
}

#[test]
#[should_panic(expected = "cookie")]
fn an_invalid_cookie_name_is_rejected() {
    let _ = SetCookie::new("bad name", "x");
}

#[test]
#[should_panic(expected = "cookie")]
fn an_invalid_cookie_value_is_rejected() {
    let _ = SetCookie::new("a", "x;y");
}

#[tokio::test]
async fn cookies_are_parsed_from_one_or_many_headers() {
    let runtime = runtime();
    let one = runtime
        .oneshot(Method::GET, "/read", &[("cookie", "a=1; b=\"two\"")], None)
        .await;
    assert_eq!(one.body_string().await, "1|two|-");
    let many = runtime
        .oneshot(
            Method::GET,
            "/read",
            &[("cookie", "a=1"), ("cookie", "b=2")],
            None,
        )
        .await;
    assert_eq!(many.body_string().await, "1|2|-");
    let none = runtime.oneshot(Method::GET, "/read", &[], None).await;
    assert_eq!(none.body_string().await, "-|-|-");
}

#[tokio::test]
async fn malformed_cookie_pairs_are_skipped_not_fatal() {
    let response = runtime()
        .oneshot(
            Method::GET,
            "/read",
            &[("cookie", "junk; a=1;; =x; b=2")],
            None,
        )
        .await;
    assert_eq!(response.body_string().await, "1|2|-");
}
