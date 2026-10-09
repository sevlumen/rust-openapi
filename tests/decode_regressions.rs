//! Regressions from the 0.8.2 adversarial review: the same struct must decode
//! the same way through `Query` and `Form`, and optional extractors must be
//! optional.

use bytes::Bytes;
use oas_rs::{ApiSchema, App, AppRuntime, Form, Method, Query};
use serde::{Deserialize, Deserializer, de::Error as _};

fn upper<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let text = String::deserialize(deserializer)?;
    if text.is_empty() {
        return Err(D::Error::custom("empty not allowed"));
    }
    Ok(text.to_uppercase())
}

#[derive(Deserialize, ApiSchema)]
struct Custom {
    #[serde(deserialize_with = "upper")]
    name: String,
}

#[derive(Deserialize, ApiSchema)]
struct Aliased {
    #[serde(alias = "p")]
    page: u32,
}

#[derive(Deserialize, ApiSchema)]
#[serde(deny_unknown_fields)]
struct Deny {
    a: u32,
}

#[derive(Deserialize, ApiSchema)]
struct Tags {
    tags: Vec<String>,
    opt_tags: Option<Vec<u32>>,
}

#[derive(Deserialize, ApiSchema)]
struct Plain {
    a: u32,
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

async fn custom_q(Query(v): Query<Custom>) -> String {
    v.name
}
async fn custom_f(Form(v): Form<Custom>) -> String {
    v.name
}
async fn aliased_q(Query(v): Query<Aliased>) -> String {
    v.page.to_string()
}
async fn aliased_f(Form(v): Form<Aliased>) -> String {
    v.page.to_string()
}
async fn deny_q(Query(v): Query<Deny>) -> String {
    v.a.to_string()
}
async fn deny_f(Form(v): Form<Deny>) -> String {
    v.a.to_string()
}
async fn tags_q(Query(v): Query<Tags>) -> String {
    format!("{:?}|{:?}", v.tags, v.opt_tags)
}
async fn plain_f(Form(v): Form<Plain>) -> String {
    v.a.to_string()
}
async fn filter_f(Form(v): Form<Filter>) -> String {
    format!("{}|{:?}|{}", v.q, v.paging.page, v.paging.size)
}
async fn opt_q(query: Option<Query<Plain>>) -> String {
    query.map_or("none".to_owned(), |Query(v)| v.a.to_string())
}
async fn opt_f(form: Option<Form<Plain>>) -> String {
    form.map_or("none".to_owned(), |Form(v)| v.a.to_string())
}

fn runtime() -> AppRuntime {
    let mut app = App::new();
    app.get("/custom", custom_q);
    app.post("/custom", custom_f);
    app.get("/aliased", aliased_q);
    app.post("/aliased", aliased_f);
    app.get("/deny", deny_q);
    app.post("/deny", deny_f);
    app.get("/tags", tags_q);
    app.post("/plain", plain_f);
    app.post("/filter", filter_f);
    app.get("/optq", opt_q);
    app.post("/optf", opt_f);
    app.build().unwrap()
}

const FORM: &str = "application/x-www-form-urlencoded";

async fn get(runtime: &AppRuntime, uri: &str) -> (u16, String) {
    let response = runtime.oneshot(Method::GET, uri, &[], None).await;
    (response.status().as_u16(), response.body_string().await)
}

async fn post(runtime: &AppRuntime, uri: &str, body: &'static str) -> (u16, String) {
    let response = runtime
        .oneshot(
            Method::POST,
            uri,
            &[("content-type", FORM)],
            Some(Bytes::from_static(body.as_bytes())),
        )
        .await;
    (response.status().as_u16(), response.body_string().await)
}

#[tokio::test]
async fn query_and_form_agree_for_deserialize_with_alias_and_deny_unknown_fields() {
    let runtime = runtime();
    // `deserialize_with` runs for both (and its validation applies).
    assert_eq!(get(&runtime, "/custom?name=abc").await, (200, "ABC".into()));
    assert_eq!(
        post(&runtime, "/custom", "name=abc").await,
        (200, "ABC".into())
    );
    assert_eq!(get(&runtime, "/custom?name=").await.0, 400);
    assert_eq!(post(&runtime, "/custom", "name=").await.0, 400);
    // `alias` is accepted by both.
    assert_eq!(get(&runtime, "/aliased?p=3").await, (200, "3".into()));
    assert_eq!(post(&runtime, "/aliased", "p=3").await, (200, "3".into()));
    assert_eq!(get(&runtime, "/aliased?page=4").await, (200, "4".into()));
    // `deny_unknown_fields` is honored by both.
    assert_eq!(get(&runtime, "/deny?a=1").await, (200, "1".into()));
    assert_eq!(get(&runtime, "/deny?a=1&zzz=2").await.0, 400);
    assert_eq!(post(&runtime, "/deny", "a=1&zzz=2").await.0, 400);
}

#[tokio::test]
async fn a_form_with_a_flattened_struct_decodes_numbers() {
    let runtime = runtime();
    assert_eq!(
        post(&runtime, "/filter", "q=rust&page=2&size=10").await,
        (200, "rust|Some(2)|10".into())
    );
    assert_eq!(
        post(&runtime, "/filter", "q=123&size=10").await,
        (200, "123|None|10".into())
    );
    assert_eq!(post(&runtime, "/filter", "q=a&size=ten").await.0, 400);
}

#[tokio::test]
async fn a_repeated_form_key_keeps_the_last_value() {
    let runtime = runtime();
    assert_eq!(post(&runtime, "/plain", "a=1&a=2").await, (200, "2".into()));
}

#[tokio::test]
async fn a_vec_query_field_collects_repeated_keys() {
    let runtime = runtime();
    assert_eq!(
        get(&runtime, "/tags?tags=a&tags=b").await,
        (200, "[\"a\", \"b\"]|None".into())
    );
    assert_eq!(
        get(&runtime, "/tags?tags=a&opt_tags=1&opt_tags=2").await,
        (200, "[\"a\"]|Some([1, 2])".into())
    );
    // Missing required array, or an item of the wrong type.
    assert_eq!(get(&runtime, "/tags").await.0, 400);
    assert_eq!(get(&runtime, "/tags?tags=a&opt_tags=x").await.0, 400);
}

#[tokio::test]
async fn optional_extractors_are_none_when_nothing_was_sent() {
    let runtime = runtime();
    assert_eq!(get(&runtime, "/optq").await, (200, "none".into()));
    assert_eq!(get(&runtime, "/optq?a=5").await, (200, "5".into()));
    // Sent but invalid is still an error, not None.
    assert_eq!(get(&runtime, "/optq?a=x").await.0, 400);
    let none = runtime.oneshot(Method::POST, "/optf", &[], None).await;
    assert_eq!(none.body_string().await, "none");
    assert_eq!(post(&runtime, "/optf", "a=7").await, (200, "7".into()));
}

#[test]
#[should_panic(expected = "more than one Path")]
fn two_path_extractors_in_one_handler_are_rejected_at_registration() {
    use oas_rs::Path;
    let mut app = App::new();
    app.get(
        "/a/{x}/{y}",
        |Path(_x): Path<u32>, Path(_y): Path<String>| async { "no" },
    );
}

// Compiles: `Option<Option<T>>` is a legal (if odd) JSON field.
#[derive(Deserialize, ApiSchema)]
#[allow(dead_code)]
struct Nested {
    inner: Option<Option<u32>>,
}

#[test]
fn a_json_media_type_suffix_is_matched_case_insensitively() {
    use oas_rs::Json;
    async fn echo(Json(v): Json<serde_json::Value>) -> Json<serde_json::Value> {
        Json(v)
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut app = App::new();
        app.post("/j", echo);
        let runtime = app.build().unwrap();
        for content_type in [
            "application/vnd.api+JSON",
            "APPLICATION/JSON",
            "application/Problem+Json",
        ] {
            let response = runtime
                .oneshot(
                    Method::POST,
                    "/j",
                    &[("content-type", content_type)],
                    Some(Bytes::from_static(b"{}")),
                )
                .await;
            assert_eq!(response.status(), 200, "{content_type}");
        }
    });
}
