use std::path::PathBuf;

use oas_rs::{App, AppRuntime, Method, ServeDir};

struct Site {
    root: PathBuf,
}

impl Site {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("oas-rs-static-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("public/sub")).unwrap();
        std::fs::write(root.join("public/index.html"), "<h1>home</h1>").unwrap();
        std::fs::write(root.join("public/app.css"), "body{}").unwrap();
        std::fs::write(root.join("public/sub/page.txt"), "page").unwrap();
        std::fs::write(root.join("public/sub/index.html"), "<p>sub</p>").unwrap();
        std::fs::write(root.join("public/.secret"), "hidden").unwrap();
        std::fs::write(root.join("public/data.unknown"), "?").unwrap();
        std::fs::write(root.join("outside.txt"), "outside").unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(root.join("public/big.bin"), big).unwrap();
        Site { root }
    }

    fn public(&self) -> PathBuf {
        self.root.join("public")
    }

    fn runtime(&self, serve: ServeDir) -> AppRuntime {
        let mut app = App::new();
        app.get("/api/ping", ping);
        app.layer(serve);
        app.build().unwrap()
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn ping() -> &'static str {
    "pong"
}

async fn get(runtime: &AppRuntime, uri: &str) -> oas_rs::TestResponse {
    runtime.oneshot(Method::GET, uri, &[], None).await
}

#[tokio::test]
async fn files_are_served_with_type_length_and_validators() {
    let site = Site::new("basic");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = get(&runtime, "/assets/app.css").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.header("content-type"),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(response.header("content-length"), Some("6"));
    assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
    assert!(response.header("etag").unwrap().starts_with("W/\""));
    assert!(response.header("last-modified").unwrap().ends_with("GMT"));
    assert_eq!(response.body_string().await, "body{}");
    let unknown = get(&runtime, "/assets/data.unknown").await;
    assert_eq!(
        unknown.header("content-type"),
        Some("application/octet-stream")
    );
}

#[tokio::test]
async fn directories_serve_their_index_file() {
    let site = Site::new("index");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = get(&runtime, "/assets/").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "<h1>home</h1>");
    let sub = get(&runtime, "/assets/sub/").await;
    assert_eq!(sub.body_string().await, "<p>sub</p>");
    let nested = get(&runtime, "/assets/sub/page.txt").await;
    assert_eq!(nested.body_string().await, "page");
}

#[tokio::test]
async fn head_has_the_headers_but_no_body() {
    let site = Site::new("head");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = runtime
        .oneshot(Method::HEAD, "/assets/app.css", &[], None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-length"), Some("6"));
    assert_eq!(response.body_string().await, "");
}

#[tokio::test]
async fn conditional_requests_get_304() {
    let site = Site::new("conditional");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let first = get(&runtime, "/assets/app.css").await;
    let etag = first.header("etag").unwrap().to_owned();
    let modified = first.header("last-modified").unwrap().to_owned();

    let by_etag = runtime
        .oneshot(
            Method::GET,
            "/assets/app.css",
            &[("if-none-match", etag.as_str())],
            None,
        )
        .await;
    assert_eq!(by_etag.status(), 304);
    assert_eq!(by_etag.body_string().await, "");

    let by_date = runtime
        .oneshot(
            Method::GET,
            "/assets/app.css",
            &[("if-modified-since", modified.as_str())],
            None,
        )
        .await;
    assert_eq!(by_date.status(), 304);

    let stale = runtime
        .oneshot(
            Method::GET,
            "/assets/app.css",
            &[("if-none-match", "W/\"other\"")],
            None,
        )
        .await;
    assert_eq!(stale.status(), 200);
}

#[tokio::test]
async fn large_files_stream_completely() {
    let site = Site::new("big");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = get(&runtime, "/assets/big.bin").await;
    assert_eq!(response.header("content-length"), Some("200000"));
    let bytes = response.body_bytes().await;
    assert_eq!(bytes.len(), 200_000);
    assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
}

#[tokio::test]
async fn unknown_files_and_other_routes_fall_through_to_the_app() {
    let site = Site::new("fallthrough");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    assert_eq!(get(&runtime, "/assets/missing.css").await.status(), 404);
    assert_eq!(get(&runtime, "/api/ping").await.body_string().await, "pong");
    // A prefix is whole segments: `/assetsx` is not under `/assets`.
    assert_eq!(get(&runtime, "/assetsx/app.css").await.status(), 404);
    // Only GET and HEAD are served.
    let post = runtime
        .oneshot(Method::POST, "/assets/app.css", &[], None)
        .await;
    assert_ne!(post.status(), 200);
}

#[tokio::test]
async fn path_traversal_and_hidden_files_are_not_served() {
    let site = Site::new("traversal");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    for uri in [
        "/assets/../outside.txt",
        "/assets/%2e%2e/outside.txt",
        "/assets/%2E%2E%2Foutside.txt",
        "/assets/sub/../../outside.txt",
        "/assets/..%5Coutside.txt",
        "/assets/sub/..%2f..%2foutside.txt",
        "/assets/%252e%252e/outside.txt",
        "/assets/.secret",
        "/assets/%2esecret",
        "/assets/app.css%00.txt",
    ] {
        let response = get(&runtime, uri).await;
        let status = response.status().as_u16();
        let body = response.body_string().await;
        assert_ne!(status, 200, "{uri} must not be served");
        assert!(
            !body.contains("outside") && !body.contains("hidden"),
            "{uri}: {body}"
        );
    }
    let allowed = site.runtime(ServeDir::new("/assets", site.public()).allow_dotfiles(true));
    assert_eq!(
        get(&allowed, "/assets/.secret").await.body_string().await,
        "hidden"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_directory_is_not_followed() {
    let site = Site::new("symlink");
    std::os::unix::fs::symlink(
        site.root.join("outside.txt"),
        site.public().join("link.txt"),
    )
    .unwrap();
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = get(&runtime, "/assets/link.txt").await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn the_cache_control_header_and_index_file_are_configurable() {
    let site = Site::new("config");
    let runtime = site.runtime(
        ServeDir::new("/assets", site.public())
            .cache_control("public, max-age=3600")
            .index_file("app.css"),
    );
    let response = get(&runtime, "/assets/").await;
    assert_eq!(
        response.header("cache-control"),
        Some("public, max-age=3600")
    );
    assert_eq!(response.body_string().await, "body{}");
}

#[test]
#[should_panic(expected = "ServeDir")]
fn a_missing_directory_is_rejected_at_construction() {
    let _ = ServeDir::new("/assets", "/definitely/not/here/oas-rs");
}

#[tokio::test]
async fn serving_from_the_root_prefix_works() {
    let site = Site::new("root");
    let runtime = site.runtime(ServeDir::new("/", site.public()));
    assert_eq!(
        get(&runtime, "/app.css").await.body_string().await,
        "body{}"
    );
    assert_eq!(
        get(&runtime, "/").await.body_string().await,
        "<h1>home</h1>"
    );
    // Routes registered by the app still win when no file matches.
    assert_eq!(get(&runtime, "/api/ping").await.body_string().await, "pong");
}

#[tokio::test]
async fn a_directory_without_a_trailing_slash_redirects_so_relative_links_work() {
    let site = Site::new("redirect");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    for (uri, location) in [
        ("/assets", "/assets/"),
        ("/assets/sub", "/assets/sub/"),
        ("/assets/sub?v=1", "/assets/sub/?v=1"),
    ] {
        let response = get(&runtime, uri).await;
        assert_eq!(response.status(), 308, "{uri}");
        assert_eq!(response.header("location"), Some(location), "{uri}");
    }
}

#[tokio::test]
async fn files_say_they_do_not_support_ranges() {
    let site = Site::new("ranges");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = runtime
        .oneshot(
            Method::GET,
            "/assets/app.css",
            &[("range", "bytes=0-1")],
            None,
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("accept-ranges"), Some("none"));
}

#[tokio::test]
async fn windows_short_names_do_not_reach_hidden_files() {
    // On NTFS `.secret` may also be reachable as `SECRET~1`; the check runs on
    // the resolved name. Elsewhere this simply is not found.
    let site = Site::new("shortname");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    for uri in ["/assets/SECRET~1", "/assets/secret~1"] {
        let response = get(&runtime, uri).await;
        let body = response.body_string().await;
        assert!(!body.contains("hidden"), "{uri}: {body}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_to_a_hidden_file_is_not_served() {
    let site = Site::new("dotlink");
    std::os::unix::fs::symlink(
        site.public().join(".secret"),
        site.public().join("plain.txt"),
    )
    .unwrap();
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let response = get(&runtime, "/assets/plain.txt").await;
    assert_eq!(response.status(), 404);
    let allowed = site.runtime(ServeDir::new("/assets", site.public()).allow_dotfiles(true));
    assert_eq!(
        get(&allowed, "/assets/plain.txt").await.body_string().await,
        "hidden"
    );
}

#[tokio::test]
async fn a_rewrite_within_the_same_second_and_size_changes_the_etag() {
    use std::time::{Duration, SystemTime};
    let site = Site::new("etag");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    let path = site.public().join("v.txt");
    let base = SystemTime::now() - Duration::from_secs(3600);
    std::fs::write(&path, "aaaa").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(base)
        .unwrap();
    let first = get(&runtime, "/assets/v.txt").await;
    let first_tag = first.header("etag").unwrap().to_owned();

    // Same size, same second, different content.
    std::fs::write(&path, "bbbb").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(base + Duration::from_millis(5))
        .unwrap();
    let second = get(&runtime, "/assets/v.txt").await;
    let second_tag = second.header("etag").unwrap().to_owned();
    assert_ne!(
        first_tag, second_tag,
        "a stale validator would give a wrong 304"
    );
    let stale = runtime
        .oneshot(
            Method::GET,
            "/assets/v.txt",
            &[("if-none-match", first_tag.as_str())],
            None,
        )
        .await;
    assert_eq!(stale.status(), 200);
    assert_eq!(stale.body_string().await, "bbbb");
}

#[tokio::test]
async fn a_directory_redirect_never_becomes_a_protocol_relative_url() {
    let site = Site::new("redirect_open");
    let runtime = site.runtime(ServeDir::new("/assets", site.public()));
    for uri in ["//assets/sub", "///assets//sub", "//assets/sub?x=1"] {
        let response = get(&runtime, uri).await;
        if response.status() == 308 {
            let location = response.header("location").unwrap().to_owned();
            assert!(location.starts_with("/assets/sub/"), "{uri} -> {location}");
            assert!(!location.starts_with("//"), "{uri} -> {location}");
        }
    }
}
