use oas_rs::ApiSchema;

#[derive(ApiSchema)]
enum Kind {
    #[api_schema(description = "nope")]
    A,
}

fn main() {}
