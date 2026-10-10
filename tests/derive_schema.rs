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
    // Untagged payloads can overlap (a `u32` also matches a number), and
    // `oneOf` requires exactly one match, so the schema uses `anyOf`.
    assert!(schema.get("oneOf").is_none());
    assert_eq!(
        schema["anyOf"],
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
fn in_a_document_nested_types_are_referenced_but_flattened_ones_are_merged() {
    let mut app = App::new();
    app.get("/account", account);
    app.get("/shape", shape);
    let doc = app.openapi_document();
    let account = &doc["components"]["schemas"]["Account"];
    // A flattened struct is merged into the object, so it is inlined (a
    // `$ref` to a component that also forbids unknown properties could never
    // validate the outer struct's own fields).
    assert_eq!(
        account["allOf"][0]["properties"]["created_by"]["type"],
        "string"
    );
    assert!(doc["components"]["schemas"]["Shape"]["oneOf"].is_array());
}

use std::collections::HashMap;

#[derive(Serialize, Deserialize, ApiSchema)]
struct FlatMap {
    id: u32,
    #[serde(flatten)]
    rest: HashMap<String, String>,
}

#[test]
fn a_flattened_map_becomes_additional_properties_of_the_object() {
    let schema = FlatMap::schema();
    assert_eq!(schema["additionalProperties"], json!({ "type": "string" }));
    assert!(schema.get("allOf").is_none());
    assert_eq!(schema["properties"]["id"]["type"], "integer");
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Inner {
    x: String,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct FlatOpt {
    id: u32,
    #[serde(flatten)]
    extra: Option<Inner>,
}

#[test]
fn a_flattened_option_does_not_require_the_inner_fields() {
    let schema = FlatOpt::schema();
    let entry = &schema["allOf"][0];
    let alternatives = entry["anyOf"].as_array().unwrap();
    assert_eq!(alternatives.len(), 2);
    assert_eq!(alternatives[0]["required"], json!(["x"]));
    assert_eq!(alternatives[1], json!({ "type": "object" }));
}

#[derive(Serialize, Deserialize, Default, ApiSchema)]
#[serde(default)]
struct Defaulted {
    name: String,
    count: u32,
}

#[test]
fn a_container_default_makes_every_field_optional() {
    let schema = Defaulted::schema();
    assert!(schema.get("required").is_none(), "{schema}");
    let parameters = <Defaulted as oas_rs::OpenApiQuery>::parameters();
    assert!(parameters.iter().all(|p| p["required"] == false));
    let parsed = <Defaulted as oas_rs::OpenApiQuery>::parse("").unwrap();
    assert_eq!(parsed.count, 0);
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct OptField {
    q: Option<String>,
}

#[test]
fn an_option_field_is_nullable_but_not_required() {
    let schema = OptField::schema_for_document();
    assert_eq!(
        schema["properties"]["q"],
        json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
    );
    assert!(schema.get("required").is_none());
}

trait DocumentSchema {
    fn schema_for_document() -> Value;
}

impl<T: ApiSchema> DocumentSchema for T {
    fn schema_for_document() -> Value {
        let mut registry = oas_rs::SchemaRegistry::inline();
        T::schema_with(&mut registry)
    }
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Holds {
    Map(HashMap<String, u32>),
    Wrapped(Inner),
}

#[test]
fn an_internally_tagged_map_variant_is_one_satisfiable_object() {
    let schema = Holds::schema();
    assert_eq!(
        schema["oneOf"][0],
        json!({
            "type": "object",
            "properties": { "type": { "type": "string", "enum": ["map"] } },
            "required": ["type"],
            "additionalProperties": { "type": "integer", "format": "int32" }
        })
    );
    assert!(schema["oneOf"][1]["allOf"].is_array());
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(transparent)]
struct Wrapper {
    inner: u32,
}

#[test]
fn a_transparent_struct_has_its_field_schema() {
    assert_eq!(
        Wrapper::schema(),
        json!({ "type": "integer", "format": "int32" })
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Examples {
    #[api_schema(example = ["a", "b"])]
    tags: Vec<String>,
    /**
     * Block doc.
     * Second line.
     */
    note: String,
}

#[test]
fn array_examples_and_block_doc_comments_work() {
    let schema = Examples::schema();
    assert_eq!(schema["properties"]["tags"]["example"], json!(["a", "b"]));
    assert_eq!(
        schema["properties"]["note"]["description"],
        "Block doc.\nSecond line."
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: u32,
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(rename_all_fields = "camelCase")]
enum Fields {
    V { long_name: u32 },
}

#[derive(Serialize, Deserialize, ApiSchema)]
enum Mixed {
    A {
        v: u32,
    },
    #[serde(untagged)]
    B {
        w: u32,
    },
}

#[test]
fn strictness_field_renames_and_untagged_variants_are_honored() {
    assert_eq!(Strict::schema()["additionalProperties"], false);
    let fields = Fields::schema();
    assert!(fields["oneOf"][0]["properties"]["V"]["properties"]["longName"].is_object());
    let mixed = Mixed::schema();
    assert!(mixed["anyOf"][0]["properties"]["A"].is_object());
    assert!(mixed["anyOf"][1]["properties"]["w"].is_object());
    assert!(mixed["anyOf"][1]["properties"].get("B").is_none());
}

#[allow(non_upper_case_globals)]
mod hygiene {
    use super::*;

    const registry: u32 = 1;
    const property: u32 = 2;
    const schema: u32 = 3;
    const query: u32 = 4;
    const parameters: u32 = 5;
    const required: u32 = 7;
    const properties: u32 = 8;
    const all_of: u32 = 9;

    #[derive(Serialize, Deserialize, ApiSchema)]
    pub struct Hygienic {
        /// Doc.
        #[api_schema(example = 1)]
        pub a: u32,
        pub b: Option<String>,
    }

    #[derive(Serialize, Deserialize, ApiSchema)]
    #[serde(tag = "t")]
    pub enum HygienicEnum {
        One { x: u32 },
        Two,
    }

    pub fn used() -> u32 {
        registry + property + schema + query + parameters + required + properties + all_of
    }
}

#[test]
fn generated_code_does_not_collide_with_user_constants() {
    assert_eq!(hygiene::used(), 39);
    assert_eq!(hygiene::Hygienic::schema()["properties"]["a"]["example"], 1);
    assert!(hygiene::HygienicEnum::schema()["oneOf"].is_array());
    assert!(<hygiene::Hygienic as oas_rs::OpenApiQuery>::parse("a=1").is_ok());
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct SameRename {
    #[serde(rename(serialize = "fullName", deserialize = "fullName"))]
    name: String,
}

#[test]
fn a_rename_with_equal_directions_is_honored() {
    assert!(SameRename::schema()["properties"].get("fullName").is_some());
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(deny_unknown_fields)]
struct Strict2 {
    a: u32,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct FlatStrict {
    id: u32,
    #[serde(flatten)]
    inner: Strict2,
}

#[test]
fn flattening_a_deny_unknown_fields_struct_does_not_forbid_the_outer_fields() {
    let schema = FlatStrict::schema();
    assert!(
        schema["allOf"][0].get("additionalProperties").is_none(),
        "{schema}"
    );
    // On its own the strict struct still forbids unknown properties.
    assert_eq!(Strict2::schema()["additionalProperties"], false);
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(untagged)]
enum MaybeNull {
    Number(u32),
    Nothing,
}

#[test]
fn an_option_of_something_that_admits_null_still_accepts_null() {
    // `null` matches both `MaybeNull::Nothing` and the `null` alternative, so
    // the combinator must be `anyOf`.
    let schema =
        <Option<MaybeNull> as ApiSchema>::schema_with(&mut oas_rs::SchemaRegistry::inline());
    assert!(
        schema.get("anyOf").is_some() && schema.get("oneOf").is_none(),
        "{schema}"
    );
}

macro_rules! rename_probe {
    ($module:ident, $rule:literal) => {
        mod $module {
            use super::*;

            #[derive(Serialize, Deserialize, ApiSchema, Default)]
            #[serde(rename_all = $rule)]
            #[allow(non_snake_case)]
            pub struct Fields {
                pub a__b: u8,
                pub _lead: u8,
                pub plain_word: u8,
                pub x: u8,
                pub HTTP_server: u8,
            }

            #[derive(Serialize, Deserialize, ApiSchema)]
            #[allow(non_camel_case_types, dead_code)]
            #[serde(rename_all = $rule)]
            pub enum Variants {
                Foo_Bar,
                lower_snake,
                HTTPServer,
                Plain,
                X,
            }

            pub fn serde_field_names() -> Vec<String> {
                let value = serde_json::to_value(Fields::default()).unwrap();
                let mut names: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
                names.sort();
                names
            }

            pub fn schema_field_names() -> Vec<String> {
                let schema = Fields::schema();
                let mut names: Vec<String> = schema["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect();
                names.sort();
                names
            }

            pub fn serde_variant_names() -> Vec<String> {
                let all = [
                    Variants::Foo_Bar,
                    Variants::lower_snake,
                    Variants::HTTPServer,
                    Variants::Plain,
                    Variants::X,
                ];
                let mut names: Vec<String> = all
                    .iter()
                    .map(|variant| {
                        serde_json::to_value(variant)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                    .collect();
                names.sort();
                names
            }

            pub fn schema_variant_names() -> Vec<String> {
                let schema = Variants::schema();
                let mut names: Vec<String> = schema["enum"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect();
                names.sort();
                names
            }
        }
    };
}

rename_probe!(r_lower, "lowercase");
rename_probe!(r_upper, "UPPERCASE");
rename_probe!(r_pascal, "PascalCase");
rename_probe!(r_camel, "camelCase");
rename_probe!(r_snake, "snake_case");
rename_probe!(r_screaming, "SCREAMING_SNAKE_CASE");
rename_probe!(r_kebab, "kebab-case");
rename_probe!(r_screaming_kebab, "SCREAMING-KEBAB-CASE");

#[test]
fn rename_all_produces_exactly_the_names_serde_writes() {
    macro_rules! check {
        ($($module:ident),*) => {$(
            assert_eq!(
                $module::schema_field_names(),
                $module::serde_field_names(),
                "fields under {}",
                stringify!($module)
            );
            assert_eq!(
                $module::schema_variant_names(),
                $module::serde_variant_names(),
                "variants under {}",
                stringify!($module)
            );
        )*};
    }
    check!(
        r_lower,
        r_upper,
        r_pascal,
        r_camel,
        r_snake,
        r_screaming,
        r_kebab,
        r_screaming_kebab
    );
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "kind")]
enum MixedTagging {
    Circle {
        radius: u32,
    },
    // Accepts `{"kind":"Circle","radius":1}` too, so the tagged alternative
    // and this one can both match one value.
    #[serde(untagged)]
    Anything(serde_json::Value),
}

#[test]
fn a_mixed_tagged_and_untagged_enum_uses_any_of() {
    let schema = MixedTagging::schema();
    // `oneOf` demands exactly one match; a tagged value that the untagged
    // variant also accepts would match two alternatives and be rejected.
    assert!(schema.get("oneOf").is_none(), "{schema}");
    assert_eq!(
        schema["anyOf"].as_array().map(Vec::len),
        Some(2),
        "{schema}"
    );
}
