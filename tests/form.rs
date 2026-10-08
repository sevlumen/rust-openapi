use bytes::Bytes;
use oas_rs::{ApiSchema, App, Form, Json, Method};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Signup {
    name: String,
    age: u32,
    note: Option<String>,
}

async fn signup(Form(form): Form<Signup>) -> Json<Signup> {
    Json(form)
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.post("/signup", signup);
    app.build().unwrap()
}

async fn post(content_type: &str, body: &'static str) -> (u16, String) {
    let response = runtime()
        .oneshot(
            Method::POST,
            "/signup",
            &[("content-type", content_type)],
            Some(Bytes::from_static(body.as_bytes())),
        )
        .await;
    (response.status().as_u16(), response.body_string().await)
}

const FORM: &str = "application/x-www-form-urlencoded";

#[tokio::test]
async fn a_form_is_decoded_with_plus_as_space() {
    let (status, body) = post(FORM, "name=Ada+Lovelace&age=36&note=a%2Bb%20c").await;
    assert_eq!(status, 200, "{body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["name"], "Ada Lovelace");
    assert_eq!(value["age"], 36);
    assert_eq!(value["note"], "a+b c");
}

#[tokio::test]
async fn optional_fields_may_be_missing() {
    let (status, body) = post(FORM, "name=Bob&age=7").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"note\":null"), "{body}");
}

#[tokio::test]
async fn a_charset_parameter_is_accepted() {
    let (status, _) = post(
        "application/x-www-form-urlencoded; charset=UTF-8",
        "name=A&age=1",
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn other_media_types_are_unsupported() {
    let (status, _) = post("application/json", "{\"name\":\"A\",\"age\":1}").await;
    assert_eq!(status, 415);
    let (status, _) = post("multipart/form-data", "name=A&age=1").await;
    assert_eq!(status, 415);
}

#[tokio::test]
async fn missing_required_fields_and_bad_numbers_are_bad_requests() {
    let (status, _) = post(FORM, "name=A").await;
    assert_eq!(status, 400);
    let (status, _) = post(FORM, "name=A&age=old").await;
    assert_eq!(status, 400);
}

#[test]
fn the_form_is_documented_as_urlencoded() {
    let mut app = App::new();
    app.post("/signup", signup);
    let doc = app.openapi_document();
    let content = &doc["paths"]["/signup"]["post"]["requestBody"]["content"];
    let schema = &content["application/x-www-form-urlencoded"]["schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["age"]["type"], "integer");
    assert_eq!(schema["required"], serde_json::json!(["name", "age"]));
}
