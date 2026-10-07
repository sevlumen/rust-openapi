use oas_rs::{ApiSchema, OpenApiQuery};
use serde::Deserialize;

#[derive(ApiSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Filter {
    page_size: u32,
    #[serde(rename = "q")]
    search: Option<String>,
    r#type: String,
}

#[derive(ApiSchema)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
enum Status {
    InProgress,
    Done,
    #[serde(rename = "WONT")]
    WontFix,
}

#[derive(ApiSchema, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(dead_code)]
struct Shouty {
    user_id: u64,
}

#[test]
fn struct_fields_use_serde_wire_names_in_schema_and_parameters() {
    let schema = Filter::schema();
    let properties = schema["properties"].as_object().unwrap();
    for name in ["pageSize", "q", "type"] {
        assert!(properties.contains_key(name), "missing property {name}");
    }
    assert_eq!(properties.len(), 3);
    let mut required: Vec<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    required.sort();
    assert_eq!(required, ["pageSize", "type"]);

    let names: Vec<_> = Filter::parameters()
        .iter()
        .map(|parameter| parameter["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["pageSize", "q", "type"]);
}

#[test]
fn direct_query_parser_reads_renamed_keys() {
    let filter = Filter::parse("pageSize=3&q=rust&type=a%20b").unwrap();
    assert_eq!(filter.page_size, 3);
    assert_eq!(filter.search.as_deref(), Some("rust"));
    assert_eq!(filter.r#type, "a b");
    assert!(Filter::parse("page_size=3&type=a").is_err());
}

#[test]
fn unit_enum_becomes_a_string_enum_with_renames() {
    assert_eq!(
        Status::schema(),
        serde_json::json!({
            "type": "string",
            "enum": ["in-progress", "done", "WONT"]
        })
    );
}

#[test]
fn rename_all_screaming_snake_case() {
    assert!(
        Shouty::schema()["properties"]
            .as_object()
            .unwrap()
            .contains_key("USER_ID")
    );
}
