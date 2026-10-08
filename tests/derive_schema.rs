#![allow(dead_code)]

use oas_rs::{ApiSchema, App, Json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Who wrote it.
#[derive(Serialize, Deserialize, ApiSchema)]
struct Meta {
    created_by: String,
}

/// An account.
///
/// Accounts own everything else.
#[derive(Serialize, Deserialize, ApiSchema)]
struct Account {
    /// The id.
    id: u32,
    #[serde(skip)]
    secret: String,
    #[serde(skip_serializing)]
    password: String,
    #[serde(skip_deserializing)]
    created: String,
    #[serde(default)]
    nickname: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[api_schema(example = "ada", min_length = 2, max_length = 20, pattern = "^[a-z]+$")]
    login: String,
    #[api_schema(minimum = 0, maximum = 150, example = 36)]
    age: u32,
    #[api_schema(min_items = 1, max_items = 3)]
    roles: Vec<String>,
    #[api_schema(description = "explicit", deprecated)]
    old: String,
    #[serde(flatten)]
    meta: Meta,
}

#[test]
fn doc_comments_become_descriptions() {
    let schema = Account::schema();
    assert_eq!(
        schema["description"],
        "An account.\n\nAccounts own everything else."
    );
    assert_eq!(schema["properties"]["id"]["description"], "The id.");
}

#[test]
fn skipped_fields_disappear_and_directional_ones_are_marked() {
    let schema = Account::schema();
    let properties = schema["properties"].as_object().unwrap();
    assert!(!properties.contains_key("secret"));
    assert_eq!(properties["password"]["writeOnly"], true);
    assert_eq!(properties["created"]["readOnly"], true);
}

#[test]
fn defaulted_and_conditionally_skipped_fields_are_not_required() {
    let schema = Account::schema();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    for name in ["id", "password", "login", "age", "roles", "old"] {
        assert!(required.contains(&name), "{name} should be required");
    }
    for name in ["nickname", "tags", "created", "secret"] {
        assert!(!required.contains(&name), "{name} should not be required");
    }
}

#[test]
fn constraints_examples_and_flags_are_emitted() {
    let schema = Account::schema();
    let properties = &schema["properties"];
    assert_eq!(properties["login"]["example"], "ada");
    assert_eq!(properties["login"]["minLength"], 2);
    assert_eq!(properties["login"]["maxLength"], 20);
    assert_eq!(properties["login"]["pattern"], "^[a-z]+$");
    assert_eq!(properties["age"]["minimum"], 0);
    assert_eq!(properties["age"]["maximum"], 150);
    assert_eq!(properties["age"]["example"], 36);
    assert_eq!(properties["roles"]["minItems"], 1);
    assert_eq!(properties["roles"]["maxItems"], 3);
    assert_eq!(properties["old"]["description"], "explicit");
    assert_eq!(properties["old"]["deprecated"], true);
}

#[test]
fn a_flattened_field_is_merged_with_all_of() {
    let schema = Account::schema();
    let all_of = schema["allOf"].as_array().unwrap();
    assert_eq!(all_of.len(), 1);
    assert_eq!(all_of[0]["properties"]["created_by"]["type"], "string");
    assert!(
        !schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("meta")
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
enum Shape {
    Circle { radius: f64 },
    Square(f64),
    Point,
    Pair(u32, u32),
}

fn number() -> Value {
    json!({ "type": "number", "format": "double" })
}

fn int() -> Value {
    json!({ "type": "integer", "format": "int32" })
}

#[test]
fn externally_tagged_enums_use_one_of() {
    let schema = Shape::schema();
    assert_eq!(
        schema["oneOf"],
        json!([
            {
                "type": "object",
                "properties": {
                    "Circle": {
                        "type": "object",
                        "properties": { "radius": number() },
                        "required": ["radius"]
                    }
                },
                "required": ["Circle"]
            },
            {
                "type": "object",
                "properties": { "Square": number() },
                "required": ["Square"]
            },
            { "type": "string", "enum": ["Point"] },
            {
                "type": "object",
                "properties": {
                    "Pair": {
                        "type": "array",
                        "prefixItems": [int(), int()],
                        "minItems": 2,
                        "maxItems": 2
                    }
                },
                "required": ["Pair"]
            }
        ])
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Created { id: u32 },
    Deleted,
}

#[test]
fn internally_tagged_enums_put_the_tag_in_each_object() {
    let schema = Event::schema();
    assert_eq!(
        schema["oneOf"],
        json!([
            {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "enum": ["created"] },
                    "id": int()
                },
                "required": ["type", "id"]
            },
            {
                "type": "object",
                "properties": { "type": { "type": "string", "enum": ["deleted"] } },
                "required": ["type"]
            }
        ])
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "t", content = "c")]
enum Message {
    Text(String),
    Ping,
}

#[test]
fn adjacently_tagged_enums_separate_tag_and_content() {
    let schema = Message::schema();
    assert_eq!(
        schema["oneOf"],
        json!([
            {
                "type": "object",
                "properties": {
                    "t": { "type": "string", "enum": ["Text"] },
                    "c": { "type": "string" }
                },
                "required": ["t", "c"]
            },
            {
                "type": "object",
                "properties": { "t": { "type": "string", "enum": ["Ping"] } },
                "required": ["t"]
            }
        ])
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(untagged)]
enum Either {
    Number(u32),
    Text(String),
    Nothing,
}

#[test]
fn untagged_enums_list_the_payloads() {
    let schema = Either::schema();
    assert_eq!(
        schema["oneOf"],
        json!([int(), { "type": "string" }, { "type": "null" }])
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
enum Plain {
    One,
    Two,
}

#[test]
fn unit_only_enums_keep_the_plain_string_enum() {
    assert_eq!(
        Plain::schema(),
        json!({ "type": "string", "enum": ["One", "Two"] })
    );
}

async fn account() -> Json<Account> {
    unreachable!()
}

async fn shape() -> Json<Shape> {
    unreachable!()
}

#[test]
fn in_a_document_nested_types_are_referenced() {
    let mut app = App::new();
    app.get("/account", account);
    app.get("/shape", shape);
    let doc = app.openapi_document();
    let account = &doc["components"]["schemas"]["Account"];
    assert_eq!(
        account["allOf"][0],
        json!({ "$ref": "#/components/schemas/Meta" })
    );
    assert!(doc["components"]["schemas"]["Meta"].is_object());
    assert!(doc["components"]["schemas"]["Shape"]["oneOf"].is_array());
}
