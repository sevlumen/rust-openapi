use oas_rs::ApiSchema;

#[derive(ApiSchema)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
struct Split {
    full_name: String,
}

fn main() {}
