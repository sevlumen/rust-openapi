use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use http::{Request, StatusCode};
use oas_rs::{ApiError, App, IntoResponse, Method, Next, RequestBody};

async fn ok() -> &'static str {
    "ok"
}

/// A layer that rejects with 401 unless `x-token: yes` is present.
fn auth(
    request: Request<RequestBody>,
    next: Next,
) -> impl Future<Output = oas_rs::HttpResponse> + Send + 'static {
    async move {
        let allowed = request
            .headers()
            .get("x-token")
            .and_then(|value| value.to_str().ok())
            == Some("yes");
        if allowed {
            next.run(request).await
        } else {
            ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized", "token").into_response()
        }
    }
}

async fn status(runtime: &oas_rs::AppRuntime, method: Method, uri: &str) -> u16 {
    runtime
        .oneshot(method, uri, &[], None)
        .await
        .status()
        .as_u16()
}

fn app_with_scoped_auth(prefix: &str) -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/admin", ok);
    app.get("/admin/users", ok);
    app.get("/administrator", ok);
    app.get("/public", ok);
    app.get("/tenants/{id}/data", ok);
    app.layer_for(prefix, auth);
    app.build().unwrap()
}

#[tokio::test]
async fn a_prefix_layer_covers_the_prefix_and_what_is_below_it() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::GET, "/admin").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/admin/").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/admin/users").await, 401);
    // Not found under the prefix is still protected: no route-existence leak.
    assert_eq!(status(&runtime, Method::GET, "/admin/nope").await, 401);
}

#[tokio::test]
async fn a_prefix_layer_does_not_cover_other_paths_or_longer_names() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::GET, "/administrator").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/public").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/missing").await, 404);
}

#[tokio::test]
async fn captures_in_a_prefix_match_any_single_segment() {
    let runtime = app_with_scoped_auth("/tenants/{id}");
    assert_eq!(status(&runtime, Method::GET, "/tenants/42/data").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/tenants/abc").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/tenants").await, 404); // outside the scope
    assert_eq!(status(&runtime, Method::GET, "/public").await, 200);
}

#[tokio::test]
async fn the_root_prefix_is_global() {
    let runtime = app_with_scoped_auth("/");
    assert_eq!(status(&runtime, Method::GET, "/public").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/missing").await, 401);
}

#[tokio::test]
async fn a_prefix_layer_runs_for_405_and_options_under_the_prefix() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::POST, "/admin/users").await, 401);
    assert_eq!(status(&runtime, Method::OPTIONS, "/admin/users").await, 401);
    assert_eq!(status(&runtime, Method::POST, "/public").await, 405);
}

/// Security parity: wherever the router serves a request, the scope applies.
/// Multi-segment prefixes matter here: an empty segment from a repeated slash
/// must not make a scoped request look like it is outside its prefix.
#[tokio::test]
async fn a_scope_is_never_narrower_than_routing() {
    fn build(prefix: Option<&str>) -> oas_rs::AppRuntime {
        let mut app = App::new();
        app.get("/admin/x", ok);
        app.get("/admin/{id}", ok);
        app.get("/api/v1/items", ok);
        app.get("/api/{version}/items", ok);
        if let Some(prefix) = prefix {
            app.layer_for(prefix, auth);
        }
        app.build().unwrap()
    }
    let plain = build(None);

    let cases: [(&str, &[&str]); 2] = [
        (
            "/admin",
            &[
                "/admin/x",
                "/admin/x/",
                "/admin//x",
                "/admin//x//",
                "/admin/y",
                "/admin/%78",
            ],
        ),
        (
            "/api/v1",
            &[
                "/api/v1/items",
                "/api/v1/items/",
                "/api//v1/items",
                "/api/v1//items",
                "/api///v1///items//",
                "/api/v1/%69tems",
            ],
        ),
    ];
    for (prefix, uris) in cases {
        let guarded = build(Some(prefix));
        let mut exercised = 0;
        for uri in uris {
            let served = status(&plain, Method::GET, uri).await;
            let protected = status(&guarded, Method::GET, uri).await;
            if served == 200 {
                exercised += 1;
                assert_eq!(
                    protected, 401,
                    "{uri} is served by the router but not guarded by {prefix}"
                );
            }
        }
        assert!(
            exercised >= 4,
            "the router served too few variants of {prefix} to prove parity"
        );
    }
}

#[tokio::test]
async fn global_and_scoped_layers_run_in_registration_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let make = |name: &'static str, log: &Arc<Mutex<Vec<String>>>| {
        let log = Arc::clone(log);
        move |request: Request<RequestBody>, next: Next| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(name.to_owned());
                next.run(request).await
            }
        }
    };
    let mut app = App::new();
    app.get("/admin/users", ok);
    app.get("/other", ok);
    app.layer(make("global-1", &log));
    app.layer_for("/admin", make("admin", &log));
    app.layer(make("global-2", &log));
    let runtime = app.build().unwrap();

    status(&runtime, Method::GET, "/admin/users").await;
    assert_eq!(*log.lock().unwrap(), ["global-1", "admin", "global-2"]);
    log.lock().unwrap().clear();
    status(&runtime, Method::GET, "/other").await;
    assert_eq!(*log.lock().unwrap(), ["global-1", "global-2"]);
}

/// Over a real connection, odd request targets (leading `//`, repeated slashes,
/// encodings) must never reach a guarded handler without a token, whatever the
/// server decides to do with them (guard, 404 or reject).
#[tokio::test]
async fn odd_request_targets_never_reach_a_guarded_route_over_tcp() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    let mut app = App::new();
    app.get("/admin/x", ok);
    app.get("/admin/{id}", ok);
    app.layer_for("/admin", auth);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });

    for target in [
        "/admin/x",
        "//admin/x",
        "///admin/x",
        "/admin//x",
        "//admin//x//",
        "/admin/x/",
        "/admin/%78",
        "/%61dmin/x",
        "/admin/.%2e/admin/x",
    ] {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out).await;
        assert!(
            !out.starts_with("HTTP/1.1 200"),
            "{target} reached the handler without a token: {out}"
        );
    }
}

#[tokio::test]
async fn route_layer_affects_only_that_route_and_method() {
    let mut app = App::new();
    app.get("/export", ok).route_layer(auth);
    app.post("/export", ok);
    app.get("/export/{id}", ok);
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/export").await, 401);
    assert_eq!(status(&runtime, Method::HEAD, "/export").await, 401); // HEAD falls back to GET
    assert_eq!(status(&runtime, Method::POST, "/export").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/export/7").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/export/").await, 401); // trailing slash: same route
}

#[tokio::test]
async fn route_layer_uses_the_routes_capture_pattern() {
    let mut app = App::new();
    app.get("/items/{id}", ok).route_layer(auth);
    app.get("/items", ok);
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/items/9").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/items").await, 200);
}
