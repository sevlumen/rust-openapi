//! One type, two schemas when the wire formats differ: a field with
//! `skip_serializing_if` and no `default` may be missing from a response but
//! not from a request body Serde will deserialize.
use oas_rs::{ApiSchema, App, Json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn valid(schema: &Value, document: &Value, value: &Value) -> bool {
    // Resolve `$ref`s against the document's components with a real validator.
    let mut root = json!({ "components": document["components"].clone() });
    root["$ref"] = schema["$ref"].clone();
    if schema.get("$ref").is_none() {
        root = schema.clone();
        root["components"] = document["components"].clone();
    }
    jsonschema::draft202012::options()
        .build(&root)
        .unwrap()
        .is_valid(value)
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Gap {
    id: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Plain {
    id: u32,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Outer {
    gap: Gap,
    plain: Plain,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Node {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<Node>,
}

async fn echo_gap(Json(gap): Json<Gap>) -> Json<Gap> {
    Json(gap)
}
async fn echo_outer(Json(outer): Json<Outer>) -> Json<Outer> {
    Json(outer)
}
async fn echo_plain(Json(plain): Json<Plain>) -> Json<Plain> {
    Json(plain)
}
async fn echo_node(Json(node): Json<Node>) -> Json<Node> {
    Json(node)
}

fn document() -> Value {
    let mut app = App::new();
    app.post("/gap", echo_gap);
    app.post("/outer", echo_outer);
    app.post("/plain", echo_plain);
    app.post("/node", echo_node);
    app.openapi_document()
}

fn request_schema(document: &Value, path: &str) -> Value {
    document["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"].clone()
}

fn response_schema(document: &Value, path: &str) -> Value {
    document["paths"][path]["post"]["responses"]["200"]["content"]["application/json"]["schema"]
        .clone()
}

#[test]
fn a_request_requires_a_field_that_a_response_may_omit() {
    let document = document();
    let request = request_schema(&document, "/gap");
    let response = response_schema(&document, "/gap");
    assert_ne!(request, response, "request and response share one schema");
    // Serde writes `{"id":1}` for an empty list, so a response may omit it...
    assert!(valid(&response, &document, &json!({ "id": 1 })));
    // ...but would not read it back, so a request must carry it.
    assert!(serde_json::from_value::<Gap>(json!({ "id": 1 })).is_err());
    assert!(!valid(&request, &document, &json!({ "id": 1 })));
    assert!(valid(&request, &document, &json!({ "id": 1, "tags": [] })));
}

#[test]
fn a_type_without_a_gap_keeps_one_schema() {
    let document = document();
    assert_eq!(
        request_schema(&document, "/plain"),
        response_schema(&document, "/plain")
    );
    assert!(document["components"]["schemas"]["PlainInput"].is_null());
}

#[test]
fn the_difference_propagates_to_the_types_that_contain_it() {
    let document = document();
    let request = request_schema(&document, "/outer");
    assert_ne!(request, response_schema(&document, "/outer"));
    assert!(!valid(
        &request,
        &document,
        &json!({ "gap": { "id": 1 }, "plain": { "id": 2 } })
    ));
    assert!(valid(
        &request,
        &document,
        &json!({ "gap": { "id": 1, "tags": [] }, "plain": { "id": 2 } })
    ));
}

#[test]
fn a_type_that_contains_itself_terminates_and_stays_consistent() {
    let document = document();
    let request = request_schema(&document, "/node");
    assert!(!valid(&request, &document, &json!({})));
    assert!(!valid(&request, &document, &json!({ "children": [{}] })));
    assert!(valid(
        &request,
        &document,
        &json!({ "children": [{ "children": [] }] })
    ));
}

#[test]
fn schema_without_a_registry_is_unchanged() {
    // `ApiSchema::schema()` keeps describing what the type writes.
    assert!(valid(&Gap::schema(), &json!({}), &json!({ "id": 1 })));
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "kind")]
enum Event {
    Created {
        id: u32,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        note: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    Gone,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Wrapped {
    one: Option<Gap>,
    many: Vec<Gap>,
    by_name: std::collections::BTreeMap<String, Gap>,
}

async fn echo_event(Json(event): Json<Event>) -> Json<Event> {
    Json(event)
}
async fn echo_wrapped(Json(wrapped): Json<Wrapped>) -> Json<Wrapped> {
    Json(wrapped)
}

fn more() -> Value {
    let mut app = App::new();
    app.post("/event", echo_event);
    app.post("/wrapped", echo_wrapped);
    app.openapi_document()
}

#[test]
fn an_enum_variant_with_a_gap_is_stricter_in_a_request() {
    let document = more();
    let request = request_schema(&document, "/event");
    let response = response_schema(&document, "/event");
    assert_ne!(request, response);
    let missing = json!({ "kind": "Created", "id": 1 });
    assert!(serde_json::from_value::<Event>(missing.clone()).is_err());
    assert!(valid(&response, &document, &missing));
    assert!(!valid(&request, &document, &missing));
    assert!(valid(
        &request,
        &document,
        &json!({ "kind": "Created", "id": 1, "tags": [] })
    ));
    assert!(valid(&request, &document, &json!({ "kind": "Gone" })));
}

#[test]
fn wrappers_forward_the_request_schema() {
    let document = more();
    let request = request_schema(&document, "/wrapped");
    let body =
        |gap: Value| json!({ "one": gap.clone(), "many": [gap.clone()], "by_name": { "a": gap } });
    assert!(!valid(&request, &document, &body(json!({ "id": 1 }))));
    assert!(valid(
        &request,
        &document,
        &body(json!({ "id": 1, "tags": [] }))
    ));
    assert!(valid(
        &request,
        &document,
        &json!({ "one": null, "many": [], "by_name": {} })
    ));
}

#[test]
fn a_response_keeps_describing_what_is_written() {
    let document = more();
    let response = response_schema(&document, "/wrapped");
    let written = serde_json::to_value(Wrapped {
        one: Some(Gap {
            id: 1,
            tags: vec![],
        }),
        many: vec![Gap {
            id: 2,
            tags: vec![],
        }],
        by_name: Default::default(),
    })
    .unwrap();
    assert!(valid(&response, &document, &written));
}
