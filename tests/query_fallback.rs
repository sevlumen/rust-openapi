use oas_rs::{ApiSchema, App, Method, Query};
use serde::Deserialize;

/// `default` disables the direct query parser, so this takes the serde
/// fallback.
#[derive(Deserialize, ApiSchema)]
struct Login {
    user: String,
    password: String,
    #[serde(default)]
    remember: bool,
    #[serde(default)]
    limit: u32,
    #[serde(default)]
    ratio: f64,
}

async fn login(Query(query): Query<Login>) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        query.user, query.password, query.remember, query.limit, query.ratio
    )
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/login", login);
    app.build().unwrap()
}

async fn get(query: &str) -> (u16, String) {
    let response = runtime()
        .oneshot(Method::GET, &format!("/login?{query}"), &[], None)
        .await;
    (response.status().as_u16(), response.body_string().await)
}

#[tokio::test]
async fn string_fields_keep_text_that_looks_like_a_number_or_bool() {
    for (query, expected) in [
        ("user=bob&password=123456", "bob|123456|false|0|0"),
        ("user=true&password=x", "true|x|false|0|0"),
        ("user=007&password=1e3", "007|1e3|false|0|0"),
        (
            "user=a&password=b&remember=true&limit=5&ratio=1.5",
            "a|b|true|5|1.5",
        ),
    ] {
        let (status, body) = get(query).await;
        assert_eq!(status, 200, "{query}: {body}");
        assert_eq!(body, expected, "{query}");
    }
}

#[tokio::test]
async fn a_plus_in_a_query_stays_a_plus_and_percent_encoding_decodes() {
    let (status, body) = get("user=a%2Bb&password=c+d%20e").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "a+b|c+d e|false|0|0");
}

#[tokio::test]
async fn bad_numbers_missing_fields_and_bad_percent_encoding_are_bad_requests() {
    for query in [
        "user=a&password=b&limit=lots",
        "user=a&password=b&remember=maybe",
        "user=a",
        "user=a&password=%zz",
    ] {
        let (status, _) = get(query).await;
        assert_eq!(status, 400, "{query}");
    }
}
