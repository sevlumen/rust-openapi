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
