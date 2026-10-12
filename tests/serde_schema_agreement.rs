//! What Serde writes must be valid against the schema the derive generates,
//! checked by a real JSON Schema (draft 2020-12) validator.
#![allow(dead_code)]

use oas_rs::ApiSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn valid(schema: &Value, value: &Value) -> bool {
    jsonschema::draft202012::options()
        .build(schema)
        .expect("the generated schema is itself a valid JSON Schema")
        .is_valid(value)
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
struct WithMaps {
    scores: std::collections::BTreeMap<String, u32>,
    nested: std::collections::HashMap<String, Inner>,
}

#[derive(Serialize, Deserialize, ApiSchema)]
#[serde(deny_unknown_fields)]
struct Closed {
    a: u32,
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

#[test]
fn maps_and_closed_objects_serialize_into_their_schema() {
    check(&[WithMaps {
        scores: [("a".to_owned(), 1), ("b".to_owned(), 2)].into(),
        nested: [("x".to_owned(), Inner { id: 1, label: None })].into(),
    }]);
    check(&[Closed { a: 1 }]);
}

/// The validator must also say no, or the tests above prove nothing.
#[test]
fn the_validator_rejects_data_that_does_not_match() {
    let record = Inner::schema();
    assert!(!valid(&record, &json!({ "id": "not a number" })));
    assert!(!valid(&record, &json!({ "label": "id is required" })));
    assert!(!valid(&record, &json!([1])));
    assert!(!valid(&Unit::schema(), &json!("Third")));
    assert!(!valid(&Closed::schema(), &json!({ "a": 1, "b": 2 })));
    let maps = WithMaps::schema();
    assert!(!valid(
        &maps,
        &json!({ "scores": { "a": "x" }, "nested": {} })
    ));
    assert!(!valid(
        &maps,
        &json!({ "scores": {}, "nested": { "k": { "id": "bad" } } })
    ));
    assert!(!valid(&External::schema(), &json!({ "Number": "one" })));
    assert!(!valid(&Internal::schema(), &json!({ "kind": "Nope" })));
    assert!(!valid(&Adjacent::schema(), &json!({ "t": "Number" })));
}
