use std::sync::{Arc, Mutex};

use http::{HeaderValue, Request};
use oas_rs::{ApiError, App, Header, HeaderSpec, IntoResponse, Method, Next, RequestBody};

async fn hello() -> &'static str {
    "hello"
}

struct User(String);

impl HeaderSpec for User {
    const NAME: &'static str = "x-user";
    fn parse(value: &str) -> Result<Self, ApiError> {
        Ok(User(value.to_owned()))
    }
}

async fn whoami(Header(user): Header<User>) -> String {
    user.0
}

fn recorder(log: &Arc<Mutex<Vec<String>>>, name: &'static str) -> impl oas_rs::Middleware {
    let log = Arc::clone(log);
    move |request: Request<RequestBody>, next: Next| {
        let log = Arc::clone(&log);
        async move {
            log.lock().unwrap().push(format!("{name}:in"));
            let response = next.run(request).await;
            log.lock().unwrap().push(format!("{name}:out"));
            response
        }
    }
}

#[tokio::test]
async fn layers_wrap_in_registration_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new();
    app.get("/", hello);
    app.layer(recorder(&log, "a"));
    app.layer(recorder(&log, "b"));
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
    assert_eq!(*log.lock().unwrap(), ["a:in", "b:in", "b:out", "a:out"]);
}

#[tokio::test]
async fn a_layer_can_answer_without_calling_next() {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(|_request: Request<RequestBody>, _next: Next| async {
        ApiError::new(http::StatusCode::FORBIDDEN, "Forbidden", "nope").into_response()
    });
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn a_layer_can_modify_the_request_and_the_response() {
    let mut app = App::new();
    app.get("/me", whoami);
    app.layer(|mut request: Request<RequestBody>, next: Next| async move {
        request
            .headers_mut()
            .insert("x-user", HeaderValue::from_static("alice"));
        let mut response = next.run(request).await;
        response
            .headers_mut()
            .insert("x-layer", HeaderValue::from_static("seen"));
        response
    });
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/me", &[], None).await;
    assert_eq!(response.header("x-layer"), Some("seen"));
    assert_eq!(response.body_string().await, "alice");
}

#[tokio::test]
async fn layers_see_404_405_options_and_head() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new();
    app.get("/", hello);
    let counter = Arc::clone(&seen);
    app.layer(move |request: Request<RequestBody>, next: Next| {
        let counter = Arc::clone(&counter);
        async move {
            counter.lock().unwrap().push(request.method().to_string());
            next.run(request).await
        }
    });
    let runtime = app.build().unwrap();

    assert_eq!(
        runtime
            .oneshot(Method::GET, "/missing", &[], None)
            .await
            .status(),
        404
    );
    assert_eq!(
        runtime.oneshot(Method::POST, "/", &[], None).await.status(),
        405
    );
    assert_eq!(
        runtime
            .oneshot(Method::OPTIONS, "/", &[], None)
            .await
            .status(),
        204
    );
    let head = runtime.oneshot(Method::HEAD, "/", &[], None).await;
    assert_eq!(head.status(), 200);
    assert_eq!(head.body_string().await, "");
    assert_eq!(*seen.lock().unwrap(), ["GET", "POST", "OPTIONS", "HEAD"]);
}

#[tokio::test]
async fn no_layers_behaves_as_before() {
    let mut app = App::new();
    app.get("/", hello);
    let runtime = app.build().unwrap();
    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "hello");
}
