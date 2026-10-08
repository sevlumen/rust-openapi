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

#[derive(Deserialize, ApiSchema)]
struct Paging {
    page: Option<u32>,
    size: u32,
}

#[derive(Deserialize, ApiSchema)]
struct Filter {
    q: String,
    #[serde(flatten)]
    paging: Paging,
}

async fn filter(Query(filter): Query<Filter>) -> String {
    format!(
        "{}|{:?}|{}",
        filter.q, filter.paging.page, filter.paging.size
    )
}

#[tokio::test]
async fn flattened_query_structs_parse_numbers_and_are_documented() {
    let mut app = App::new();
    app.get("/filter", filter);
    let doc = app.openapi_document();
    let parameters = doc["paths"]["/filter"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = parameters
        .iter()
        .map(|parameter| parameter["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["q", "page", "size"]);
    let required: Vec<bool> = parameters
        .iter()
        .map(|parameter| parameter["required"].as_bool().unwrap())
        .collect();
    assert_eq!(required, [true, false, true]);
    assert_eq!(parameters[2]["schema"]["type"], "integer");

    let runtime = app.build().unwrap();
    let response = runtime
        .oneshot(Method::GET, "/filter?q=rust&page=2&size=10", &[], None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "rust|Some(2)|10");
}

#[derive(Deserialize, ApiSchema)]
struct Size {
    size: u32,
    #[serde(default)]
    exact: bool,
}

#[derive(Deserialize, ApiSchema)]
struct Sized {
    q: String,
    #[serde(flatten)]
    paging: Size,
}

async fn sized(Query(query): Query<Sized>) -> String {
    format!("{}|{}|{}", query.q, query.paging.size, query.paging.exact)
}

#[tokio::test]
async fn a_string_field_next_to_a_flattened_number_keeps_its_text() {
    let mut app = App::new();
    app.get("/sized", sized);
    let runtime = app.build().unwrap();
    for (query, expected) in [
        ("q=123&size=10", "123|10|false"),
        ("q=true&size=1&exact=true", "true|1|true"),
        ("q=007&size=3", "007|3|false"),
        ("q=1e3&size=4&exact=false", "1e3|4|false"),
    ] {
        let response = runtime
            .oneshot(Method::GET, &format!("/sized?{query}"), &[], None)
            .await;
        assert_eq!(response.status(), 200, "{query}");
        assert_eq!(response.body_string().await, expected, "{query}");
    }
    let bad = runtime
        .oneshot(Method::GET, "/sized?q=a&size=ten", &[], None)
        .await;
    assert_eq!(bad.status(), 400);
}

#[derive(Deserialize, ApiSchema)]
struct Inner {
    a: u32,
}

#[derive(Deserialize, ApiSchema)]
struct Mid {
    #[serde(flatten)]
    inner: Inner,
    b: String,
}

#[derive(Deserialize, ApiSchema)]
struct Outer {
    #[serde(flatten)]
    mid: Option<Mid>,
    c: u32,
}

async fn outer(Query(query): Query<Outer>) -> String {
    match query.mid {
        Some(mid) => format!("{}|{}|{}", mid.inner.a, mid.b, query.c),
        None => format!("none|{}", query.c),
    }
}

#[tokio::test]
async fn nested_optional_flattens_are_documented_as_optional_parameters() {
    let mut app = App::new();
    app.get("/outer", outer);
    let doc = app.openapi_document();
    let parameters = doc["paths"]["/outer"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let find = |name: &str| {
        parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap_or_else(|| panic!("parameter {name} is missing: {parameters:?}"))
    };
    assert_eq!(find("c")["required"], true);
    // Inside an optional flatten nothing is required.
    assert_eq!(find("a")["required"], false);
    assert_eq!(find("b")["required"], false);
    assert_eq!(parameters.len(), 3);

    let runtime = app.build().unwrap();
    let full = runtime
        .oneshot(Method::GET, "/outer?a=1&b=x&c=3", &[], None)
        .await;
    assert_eq!(full.body_string().await, "1|x|3");
}
