use oas_rs::ApiSchema;

#[derive(ApiSchema)]
struct Split {
    #[serde(rename(serialize = "fullName", deserialize = "full_name"))]
    name: String,
}

fn main() {}
