use oas_rs::ApiSchema;

#[derive(ApiSchema)]
#[api_schema(name = "Item Dto/x")]
struct Invalid {
    id: u32,
}

fn main() {}
