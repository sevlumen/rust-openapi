//! Seeded pseudo-random property tests: no panics on hostile input, and the
//! security property that a scoped layer never covers less than the router
//! serves. Deterministic (fixed seeds), so a failure reproduces.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use bytes::Bytes;
use http::Request;
use oas_rs::{ApiSchema, App, AppRuntime, Cookies, Json, Method, Next, Query, RequestBody};
use serde::{Deserialize, Serialize};

/// xorshift64*: small, fast and good enough to drive the cases.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
    tags: Vec<String>,
}

#[derive(Deserialize, ApiSchema)]
struct Search {
    #[allow(dead_code)]
    q: Option<String>,
    #[allow(dead_code)]
    page: Option<u32>,
}

async fn route() -> &'static str {
    "route"
}

async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

async fn search(Query(_search): Query<Search>) -> &'static str {
    "search"
}

async fn cookies(_cookies: Cookies) -> &'static str {
    "cookies"
}

fn runtime(layer_hits: &Arc<AtomicUsize>) -> AppRuntime {
    let mut app = App::new();
    app.get("/api/admin/users", route);
    app.get("/api/admin/{section}/x", route);
    app.get("/api/{section}", route);
    app.get("/public", route);
    app.post("/items", create);
    app.get("/search", search);
    app.get("/cookies", cookies);
    let hits = Arc::clone(layer_hits);
    app.layer_for(
        "/api/admin",
        move |request: Request<RequestBody>, next: Next| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                next.run(request).await
            }
        },
    );
    app.build().unwrap()
}

fn path_from(rng: &mut Rng) -> String {
    const SEGMENTS: &[&str] = &[
        "api",
        "admin",
        "users",
        "x",
        "a",
        "b",
        "public",
        "",
        ".",
        "..",
        "%61dmin",
        "%2e%2e",
        "%2F",
        "%252e",
        "ADMIN",
        "admin%20",
        "%00",
        "%",
        "%zz",
        "ü",
        "api%2fadmin",
    ];
    let mut path = String::new();
    for _ in 0..(1 + rng.below(6)) {
        path.push('/');
        if rng.below(8) == 0 {
            path.push('/'); // repeated slash
        }
        path.push_str(rng.pick(SEGMENTS));
    }
    if rng.below(5) == 0 {
        path.push('/');
    }
    path
}

#[tokio::test]
async fn a_scoped_layer_runs_whenever_the_router_serves_something_under_its_prefix() {
    let hits = Arc::new(AtomicUsize::new(0));
    let runtime = runtime(&hits);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut checked = 0;
    for _ in 0..20_000 {
        let path = path_from(&mut rng);
        let Ok(uri) = path.parse::<http::Uri>() else {
            continue;
        };
        let before = hits.load(Ordering::SeqCst);
        let response = runtime
            .oneshot(Method::GET, &uri.to_string(), &[], None)
            .await;
        let served = response.status() == 200;
        let ran = hits.load(Ordering::SeqCst) > before;
        // A route that is under /api/admin once the router has decoded and
        // normalized the path must have passed through the layer.
        if served {
            let decoded = percent_decode(&path);
            let under_admin = decoded
                .split('/')
                .filter(|part| !part.is_empty())
                .take(2)
                .collect::<Vec<_>>()
                == ["api", "admin"];
            if under_admin {
                assert!(ran, "the layer did not run for served path {path:?}");
            }
        }
        checked += 1;
    }
    assert!(checked > 10_000);
}

/// Decodes `%XX` the way the router's captures are decoded, leaving anything
/// invalid as it is.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len() + 0
            && let Ok(value) = u8::from_str_radix(&text[index + 1..index + 3], 16)
        {
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn hostile_paths_queries_headers_and_bodies_never_panic() {
    let hits = Arc::new(AtomicUsize::new(0));
    let runtime = runtime(&hits);
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    const QUERY_PARTS: &[&str] = &[
        "q",
        "page",
        "=",
        "&",
        "%",
        "%zz",
        "+",
        "a",
        "1",
        "-1",
        "99999999999999999999",
        "%00",
        "ü",
        "true",
        "1e3",
        "&&",
        "=&=",
    ];
    const COOKIES: &[&str] = &[
        "a=1", "b", "=x", ";;", "a=\"q\"", "name=ü", "x=y=z", " ", "a=1; b=2", "\"", "=", ";",
    ];
    const BODIES: &[&str] = &[
        "{}",
        "{\"name\":\"a\",\"tags\":[]}",
        "{\"name\":1}",
        "[",
        "null",
        "\u{0}",
        "{\"name\":\"a\",\"tags\":[\"b\"]",
        "{\"name\":\"\\ud800\",\"tags\":[]}",
        "",
        "{\"name\":\"x\",\"tags\":[null]}",
    ];
    for round in 0..8_000 {
        let mut query = String::new();
        for _ in 0..rng.below(6) {
            query.push_str(rng.pick(QUERY_PARTS));
        }
        let uri = format!("/search?{query}");
        if uri.parse::<http::Uri>().is_ok() {
            let response = runtime.oneshot(Method::GET, &uri, &[], None).await;
            assert!(response.status().as_u16() < 600, "{uri}");
        }
        let mut cookie = String::new();
        for _ in 0..(1 + rng.below(4)) {
            if !cookie.is_empty() {
                cookie.push_str("; ");
            }
            cookie.push_str(rng.pick(COOKIES));
        }
        if http::HeaderValue::from_str(&cookie).is_ok() {
            let response = runtime
                .oneshot(Method::GET, "/cookies", &[("cookie", &cookie)], None)
                .await;
            assert_eq!(response.status(), 200, "{cookie:?}");
        }
        let body = rng.pick(BODIES);
        let content_type = if round % 7 == 0 {
            "text/plain"
        } else {
            "application/json"
        };
        let response = runtime
            .oneshot(
                Method::POST,
                "/items",
                &[("content-type", content_type)],
                Some(Bytes::from(body.to_owned())),
            )
            .await;
        let status = response.status().as_u16();
        assert!(
            matches!(status, 200 | 400 | 415 | 422),
            "{status} for {body:?}"
        );
        let path = path_from(&mut rng);
        if let Ok(uri) = path.parse::<http::Uri>() {
            let response = runtime
                .oneshot(Method::GET, &uri.to_string(), &[], None)
                .await;
            assert!(response.status().as_u16() < 600, "{path}");
        }
    }
}
