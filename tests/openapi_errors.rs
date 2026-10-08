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
    doc["paths"][path][method]["responses"][status]["content"]["application/problem+json"]["schema"]
        ["$ref"]
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
