use oas_rs::{App, BearerAuth, Cors, ErrorFormat, ErrorInfo, RateLimit, RequestId, Trace};

#[test]
fn types_holding_secrets_or_closures_have_a_debug_that_prints_no_state() {
    let bearer = format!("{:?}", BearerAuth::static_token("s3cret-token"));
    assert!(bearer.starts_with("BearerAuth"), "{bearer}");
    assert!(!bearer.contains("s3cret"), "{bearer}");

    let format = ErrorFormat::new(|info: &ErrorInfo| info.respond(serde_json::json!({})));
    assert!(format!("{format:?}").starts_with("ErrorFormat"));
    assert!(format!("{:?}", Cors::new()).starts_with("Cors"));
    assert!(
        format!(
            "{:?}",
            RateLimit::new(10, std::time::Duration::from_secs(60))
        )
        .starts_with("RateLimit")
    );
    assert!(format!("{:?}", RequestId::new()).starts_with("RequestId"));
    assert!(format!("{:?}", Trace::stderr()).starts_with("Trace"));
}

#[test]
fn an_app_and_its_runtime_can_be_debug_printed() {
    let mut app = App::new();
    app.get("/", || async { "ok" });
    assert!(format!("{app:?}").starts_with("App"));
    let runtime = app.build().unwrap();
    assert!(format!("{runtime:?}").starts_with("AppRuntime"));
}

mod secrets {
    use oas_rs::{App, Cookies, Headers, Method, SetCookie};

    async fn show_headers(headers: Headers) -> String {
        format!("{headers:?}")
    }

    async fn show_cookies(cookies: Cookies) -> String {
        format!("{cookies:?}")
    }

    async fn get(path: &str, headers: &[(&str, &str)]) -> String {
        let mut app = App::new();
        app.get("/headers", show_headers);
        app.get("/cookies", show_cookies);
        let runtime = app.build().unwrap();
        runtime
            .oneshot(Method::GET, path, headers, None)
            .await
            .body_string()
            .await
    }

    #[tokio::test]
    async fn headers_debug_hides_credentials_but_keeps_everything_else() {
        let shown = get(
            "/headers",
            &[
                ("authorization", "Bearer s3cr3t-token"),
                ("cookie", "sid=s3cr3t-session"),
                ("proxy-authorization", "Basic s3cr3t-proxy"),
                ("x-api-key", "s3cr3t-key"),
                // Names that do not look secret are hidden all the same.
                ("x-credential", "s3cr3t-credential"),
                ("x-access-code", "s3cr3t-code"),
                ("x-request-id", "request-42"),
                ("user-agent", "curl/8.0"),
                // Harmless-looking names that can carry secrets or personal data.
                ("referer", "https://example.com/reset?token=s3cr3t-referer"),
                ("sec-websocket-protocol", "auth-s3cr3t-subprotocol"),
                ("sec-ch-api-key", "s3cr3t-client-hint"),
                ("x-forwarded-for", "203.0.113.77"),
                ("x-real-ip", "203.0.113.78"),
            ],
        )
        .await;
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(
            !shown.contains("203.0.113"),
            "client addresses leaked: {shown}"
        );
        // The names stay, so the output is still useful for debugging.
        for name in [
            "authorization",
            "cookie",
            "x-api-key",
            "x-credential",
            "x-access-code",
            "x-request-id",
        ] {
            assert!(shown.contains(name), "{name} missing from {shown}");
        }
        // Known harmless headers keep their values.
        assert!(
            shown.contains("request-42") && shown.contains("curl/8.0"),
            "{shown}"
        );
    }

    #[tokio::test]
    async fn cookies_debug_shows_names_not_values() {
        let shown = get("/cookies", &[("cookie", "sid=s3cr3t-session; theme=dark")]).await;
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(!shown.contains("dark"), "{shown}");
        assert!(shown.contains("sid") && shown.contains("theme"), "{shown}");
    }

    #[test]
    fn set_cookie_debug_hides_the_value_but_keeps_the_attributes() {
        let cookie = SetCookie::new("sid", "s3cr3t-session")
            .path("/app")
            .http_only()
            .secure();
        let shown = format!("{cookie:?}");
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(shown.contains("sid") && shown.contains("/app"), "{shown}");
    }
}
