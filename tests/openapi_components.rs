use oas_rs::{ApiSchema, App, BuildError, Created, Json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, ApiSchema)]
struct Item {
    name: String,
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(rename_all = "snake_case")]
enum Status {
    Open,
    Closed,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Order {
    first: Item,
    all: Vec<Item>,
    maybe: Option<Item>,
    status: Status,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Node {
    label: String,
    children: Vec<Node>,
}

async fn get_item() -> Json<Item> {
    Json(Item { name: "a".into() })
}

async fn put_item(Json(item): Json<Item>) -> Created<Item> {
    Created(item)
}

async fn get_order() -> Json<Order> {
    unreachable!()
}

async fn get_node() -> Json<Node> {
    unreachable!()
}

fn document(configure: impl FnOnce(&mut App)) -> Value {
    let mut app = App::new();
    configure(&mut app);
    app.openapi_document()
}

fn item_ref() -> Value {
    json!({ "$ref": "#/components/schemas/Item" })
}

fn json_schema(doc: &Value, path: &str, method: &str, status: &str) -> Value {
    doc["paths"][path][method]["responses"][status]["content"]["application/json"]["schema"].clone()
}

#[test]
fn a_response_type_becomes_a_component_reference() {
    let doc = document(|app| {
        app.get("/item", get_item);
    });
    assert_eq!(json_schema(&doc, "/item", "get", "200"), item_ref());
    let item = &doc["components"]["schemas"]["Item"];
    assert_eq!(item["type"], "object");
    assert_eq!(item["properties"]["name"], json!({ "type": "string" }));
    assert_eq!(item["required"], json!(["name"]));
}

#[test]
fn request_and_response_share_one_definition() {
    let doc = document(|app| {
        app.put("/item", put_item);
    });
    assert_eq!(
        doc["paths"]["/item"]["put"]["requestBody"]["content"]["application/json"]["schema"],
        item_ref()
    );
    assert_eq!(json_schema(&doc, "/item", "put", "201"), item_ref());
    // Item, Problem (400/413 from the body) and nothing else.
    assert_eq!(doc["components"]["schemas"].as_object().unwrap().len(), 2);
    assert!(doc["components"]["schemas"]["Item"].is_object());
    assert!(doc["components"]["schemas"]["Problem"].is_object());
}

#[test]
fn nested_types_are_referenced_not_inlined() {
    let doc = document(|app| {
        app.get("/order", get_order);
        app.get("/item", get_item);
    });
    let order = &doc["components"]["schemas"]["Order"];
    assert_eq!(order["properties"]["first"], item_ref());
    assert_eq!(
        order["properties"]["all"],
        json!({ "type": "array", "items": item_ref() })
    );
    assert_eq!(
        order["properties"]["maybe"],
        json!({ "anyOf": [item_ref(), { "type": "null" }] })
    );
    assert_eq!(
        order["properties"]["status"],
        json!({ "$ref": "#/components/schemas/Status" })
    );
    assert_eq!(order["required"], json!(["first", "all", "status"]));
    assert_eq!(
        doc["components"]["schemas"]["Status"],
        json!({ "type": "string", "enum": ["open", "closed"] })
    );
    // Defined once even though two routes reach it.
    assert!(doc["components"]["schemas"]["Item"].is_object());
}

#[test]
fn a_recursive_type_references_itself() {
    let doc = document(|app| {
        app.get("/node", get_node);
    });
    assert_eq!(
        doc["components"]["schemas"]["Node"]["properties"]["children"],
        json!({ "type": "array", "items": { "$ref": "#/components/schemas/Node" } })
    );
}

#[test]
fn calling_schema_directly_still_returns_the_inline_definition() {
    let inline = <Order as ApiSchema>::schema();
    assert_eq!(inline["properties"]["first"]["type"], "object");
    assert_eq!(
        inline["properties"]["status"],
        json!({ "type": "string", "enum": ["open", "closed"] })
    );
    // A recursive type must not overflow the stack when inlined.
    let node = <Node as ApiSchema>::schema();
    assert_eq!(node["type"], "object");
}

mod first {
    use super::*;
    #[derive(Serialize, Deserialize, ApiSchema)]
    pub struct Thing {
        pub a: String,
    }
}

mod second {
    use super::*;
    #[derive(Serialize, Deserialize, ApiSchema)]
    pub struct Thing {
        pub b: u32,
    }
}

async fn first_thing() -> Json<first::Thing> {
    unreachable!()
}

async fn second_thing() -> Json<second::Thing> {
    unreachable!()
}

#[test]
fn two_different_types_with_one_name_fail_the_build() {
    let mut app = App::new();
    app.get("/first", first_thing);
    app.get("/second", second_thing);
    let error = app.build().expect_err("conflicting schema names");
    assert_eq!(
        error,
        BuildError::SchemaNameConflict {
            name: "Thing".to_owned()
        }
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[api_schema(name = "ItemDto")]
struct Renamed {
    id: u32,
}

async fn get_renamed() -> Json<Renamed> {
    unreachable!()
}

#[test]
fn api_schema_name_overrides_the_component_name() {
    let doc = document(|app| {
        app.get("/renamed", get_renamed);
    });
    assert_eq!(
        json_schema(&doc, "/renamed", "get", "200"),
        json!({ "$ref": "#/components/schemas/ItemDto" })
    );
    assert!(doc["components"]["schemas"]["ItemDto"].is_object());
}

struct Manual;

impl ApiSchema for Manual {
    fn schema() -> Value {
        json!({ "type": "string", "format": "custom" })
    }
}

impl Serialize for Manual {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("m")
    }
}

async fn get_manual() -> Json<Manual> {
    Json(Manual)
}

#[test]
fn a_hand_written_schema_is_still_inlined() {
    let doc = document(|app| {
        app.get("/manual", get_manual);
    });
    assert_eq!(
        json_schema(&doc, "/manual", "get", "200"),
        json!({ "type": "string", "format": "custom" })
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Tree {
    left: Option<Box<Tree>>,
    boxed: Box<Item>,
    by_name: std::collections::HashMap<String, Item>,
    ordered: std::collections::BTreeMap<String, Item>,
}

async fn get_tree() -> Json<Tree> {
    unreachable!()
}

#[test]
fn box_and_map_nesting_uses_references_and_recursion_terminates() {
    let doc = document(|app| {
        app.get("/tree", get_tree);
    });
    let tree = &doc["components"]["schemas"]["Tree"]["properties"];
    assert_eq!(
        tree["left"],
        json!({ "anyOf": [{ "$ref": "#/components/schemas/Tree" }, { "type": "null" }] })
    );
    assert_eq!(tree["boxed"], item_ref());
    assert_eq!(
        tree["by_name"],
        json!({ "type": "object", "additionalProperties": item_ref() })
    );
    assert_eq!(
        tree["ordered"],
        json!({ "type": "object", "additionalProperties": item_ref() })
    );
    // Inline use of a Box-recursive type must not overflow the stack either.
    assert_eq!(<Tree as ApiSchema>::schema()["type"], "object");
}
