//! What Serde writes must be valid against the schema the derive generates.
//! A tiny validator for the keywords the derive emits keeps this free of a
//! JSON Schema dependency; keywords it does not know are ignored.
#![allow(dead_code)]

use oas_rs::ApiSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn valid(schema: &Value, value: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return true; // `true` / unknown form accepts everything
    };
    if let Some(types) = schema.get("type") {
        let names: Vec<&str> = match types {
            Value::String(name) => vec![name.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let matches = |name: &str| match name {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            "null" => value.is_null(),
            _ => true,
        };
        if !names.is_empty() && !names.iter().any(|name| matches(name)) {
            return false;
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(value)
    {
        return false;
    }
    if let Some(object) = value.as_object() {
        if let Some(Value::Array(required)) = schema.get("required")
            && required
                .iter()
                .filter_map(Value::as_str)
                .any(|name| !object.contains_key(name))
        {
            return false;
        }
        if let Some(Value::Object(properties)) = schema.get("properties") {
            for (name, inner) in properties {
                if let Some(field) = object.get(name)
                    && !valid(inner, field)
                {
                    return false;
                }
            }
            if schema.get("additionalProperties") == Some(&json!(false))
                && object.keys().any(|key| !properties.contains_key(key))
            {
                return false;
            }
        }
    }
    if let Some(list) = value.as_array() {
        if let Some(items) = schema.get("items")
            && !list.iter().all(|item| valid(items, item))
        {
            return false;
        }
        if let Some(Value::Array(prefix)) = schema.get("prefixItems") {
            if list.len() != prefix.len() {
                return false;
            }
            if !prefix
                .iter()
                .zip(list)
                .all(|(inner, item)| valid(inner, item))
            {
                return false;
            }
        }
    }
    if let Some(Value::Array(all)) = schema.get("allOf")
        && !all.iter().all(|inner| valid(inner, value))
    {
        return false;
    }
    if let Some(Value::Array(any)) = schema.get("anyOf")
        && !any.iter().any(|inner| valid(inner, value))
    {
        return false;
    }
    if let Some(Value::Array(one)) = schema.get("oneOf")
        && one.iter().filter(|inner| valid(inner, value)).count() != 1
    {
        return false;
    }
    true
}

fn check<T: Serialize + ApiSchema>(values: &[T]) {
    let schema = T::schema();
    for value in values {
        let json = serde_json::to_value(value).unwrap();
        assert!(
            valid(&schema, &json),
            "{json} is not valid against {schema}"
        );
    }
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Inner {
    id: u32,
    label: Option<String>,
}

#[derive(Serialize, Deserialize, ApiSchema)]
struct Record {
    name: String,
    count: u64,
    ratio: f64,
    ok: bool,
    tags: Vec<String>,
    inner: Inner,
    maybe: Option<Inner>,
    #[serde(default)]
    note: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize, Deserialize, ApiSchema)]
enum Unit {
    First,
    Second,
}

#[derive(Serialize, Deserialize, ApiSchema)]
enum External {
    Empty,
    Number(u32),
    Pair(u8, String),
    Named { a: u32, b: Option<String> },
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "kind")]
enum Internal {
    Empty,
    Named { a: u32 },
    Wrapped(Inner),
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "t", content = "c")]
enum Adjacent {
    Empty,
    Number(u32),
    Named { a: u32 },
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(untagged)]
enum Untagged {
    Number(u32),
    Text(String),
    Nothing,
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(tag = "kind")]
enum Mixed {
    Circle {
        radius: u32,
    },
    #[serde(untagged)]
    Anything(serde_json::Value),
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(rename_all = "camelCase")]
struct Renamed {
    first_name: String,
    #[serde(rename = "ID")]
    id: u32,
    nested_list: Vec<Inner>,
}

#[test]
fn structs_serialize_into_their_schema() {
    let inner = || Inner { id: 1, label: None };
    check(&[
        Record {
            name: "a".into(),
            count: 3,
            ratio: 0.5,
            ok: true,
            tags: vec![],
            inner: inner(),
            maybe: None,
            note: String::new(),
            kind: "k".into(),
        },
        Record {
            name: "b".into(),
            count: 0,
            ratio: -1.0,
            ok: false,
            tags: vec!["t".into()],
            inner: Inner {
                id: 2,
                label: Some("l".into()),
            },
            maybe: Some(inner()),
            note: "n".into(),
            kind: "m".into(),
        },
    ]);
    check(&[Renamed {
        first_name: "ada".into(),
        id: 7,
        nested_list: vec![inner()],
    }]);
}

#[test]
fn every_enum_tagging_form_serializes_into_its_schema() {
    check(&[Unit::First, Unit::Second]);
    check(&[
        External::Empty,
        External::Number(1),
        External::Pair(2, "p".into()),
        External::Named { a: 3, b: None },
        External::Named {
            a: 4,
            b: Some("s".into()),
        },
    ]);
    check(&[
        Internal::Empty,
        Internal::Named { a: 1 },
        Internal::Wrapped(Inner { id: 2, label: None }),
    ]);
    check(&[
        Adjacent::Empty,
        Adjacent::Number(1),
        Adjacent::Named { a: 2 },
    ]);
    check(&[
        Untagged::Number(1),
        Untagged::Text("t".into()),
        Untagged::Nothing,
    ]);
}

#[test]
fn a_mixed_enum_accepts_a_value_both_a_tagged_and_the_untagged_variant_match() {
    // Serialized by the untagged variant, but also a valid `Circle`.
    check(&[
        Mixed::Anything(json!({ "kind": "Circle", "radius": 1 })),
        Mixed::Anything(json!("loose")),
        Mixed::Circle { radius: 2 },
    ]);
}
