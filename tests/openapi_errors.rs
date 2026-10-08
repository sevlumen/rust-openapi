use oas_rs::{ApiSchema, App, Json, Path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
}

async fn plain() -> &'static str {
    "ok"
}

async fn by_id(Path(_id): Path<u32>) -> &'static str {
    "ok"
}

async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

fn document(configure: impl FnOnce(&mut App)) -> Value {
    let mut app = App::new();
    configure(&mut app);
    app.openapi_document()
}

const PROBLEM_REF: &str = "#/components/schemas/Problem";

fn error_ref(doc: &Value, path: &str, method: &str, status: &str) -> Option<Value> {
    doc["paths"][path][method]["responses"][status]["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .map(|reference| json!(reference))
}

#[test]
fn routes_without_inputs_document_no_errors() {
    let doc = document(|app| {
        app.get("/plain", plain);
    });
    let responses = doc["paths"]["/plain"]["get"]["responses"]
        .as_object()
        .unwrap();
    assert_eq!(responses.keys().collect::<Vec<_>>(), ["200"]);
    assert!(doc.get("components").is_none());
}

#[test]
fn a_path_parameter_documents_400() {
    let doc = document(|app| {
        app.get("/items/{id}", by_id);
    });
    assert_eq!(
        error_ref(&doc, "/items/{id}", "get", "400"),
        Some(json!(PROBLEM_REF))
    );
    assert!(doc["paths"]["/items/{id}"]["get"]["responses"]["413"].is_null());
}

#[test]
fn a_json_body_documents_400_and_413() {
    let doc = document(|app| {
        app.post("/items", create);
    });
    for status in ["400", "413"] {
        assert_eq!(
            error_ref(&doc, "/items", "post", status),
            Some(json!(PROBLEM_REF)),
            "{status}"
        );
    }
}

#[test]
fn security_documents_401_and_default_security_applies() {
    let doc = document(|app| {
        app.openapi().bearer_auth("Bearer");
        app.get("/secret", plain).security(["Bearer"]);
        app.get("/open", plain).public();
        app.get("/inherits", plain);
        app.openapi().default_security(["Bearer"]);
    });
    assert_eq!(
        error_ref(&doc, "/secret", "get", "401"),
        Some(json!(PROBLEM_REF))
    );
    assert_eq!(
        error_ref(&doc, "/inherits", "get", "401"),
        Some(json!(PROBLEM_REF))
    );
    assert!(doc["paths"]["/open"]["get"]["responses"]["401"].is_null());
}

#[test]
fn the_problem_schema_matches_api_error() {
    let doc = document(|app| {
        app.get("/items/{id}", by_id);
    });
    let problem = &doc["components"]["schemas"]["Problem"];
    assert_eq!(problem["type"], "object");
    let properties = problem["properties"].as_object().unwrap();
    for key in ["type", "title", "status", "detail"] {
        assert!(properties.contains_key(key), "{key}");
    }
}

#[test]
fn error_documentation_can_be_turned_off() {
    let doc = document(|app| {
        app.openapi().document_errors(false);
        app.post("/items", create);
    });
    let responses = doc["paths"]["/items"]["post"]["responses"]
        .as_object()
        .unwrap();
    assert_eq!(responses.keys().collect::<Vec<_>>(), ["200"]);
    assert!(doc["components"]["schemas"].get("Problem").is_none());
}

#[test]
fn a_path_capture_without_an_extractor_documents_no_400() {
    let doc = document(|app| {
        app.get("/items/{id}", plain);
    });
    assert!(doc["paths"]["/items/{id}"]["get"]["responses"]["400"].is_null());
    assert!(doc.get("components").is_none());
}

#[test]
fn body_routes_document_415() {
    let doc = document(|app| {
        app.post("/items", create);
    });
    assert!(doc["paths"]["/items"]["post"]["responses"]["415"].is_object());
}

#[tokio::test]
async fn documented_media_type_matches_what_the_server_sends() {
    let mut app = App::new();
    app.post("/items", create);
    let doc = app.openapi_document();
    let runtime = app.build().unwrap();
    for (status, body, content_type) in [
        ("400", Some("{bad"), "application/json"),
        ("415", Some("{}"), "text/plain"),
    ] {
        let response = runtime
            .oneshot(
                oas_rs::Method::POST,
                "/items",
                &[("content-type", content_type)],
                body.map(|text| bytes::Bytes::from_static(text.as_bytes())),
            )
            .await;
        assert_eq!(response.status().as_u16().to_string(), status);
        let sent = response.header("content-type").unwrap().to_owned();
        let documented = doc["paths"]["/items"]["post"]["responses"][status]["content"]
            .as_object()
            .unwrap();
        assert!(documented.contains_key(&sent), "{status}: {sent}");
    }
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Problem {
    mine: String,
}

async fn my_problem() -> Json<Problem> {
    unreachable!()
}

#[test]
fn a_user_type_named_problem_only_conflicts_when_errors_are_documented() {
    let mut quiet = App::new();
    quiet.get("/p", my_problem);
    assert!(quiet.build().is_ok(), "no route documents framework errors");

    let mut noisy = App::new();
    noisy.get("/p", my_problem);
    noisy.post("/items", create);
    assert!(matches!(
        noisy.build().err(),
        Some(oas_rs::BuildError::SchemaNameConflict { .. })
    ));

    let mut off = App::new();
    off.openapi().document_errors(false);
    off.get("/p", my_problem);
    off.post("/items", create);
    assert!(off.build().is_ok());
}
