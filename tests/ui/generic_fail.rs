use oas_rs::ApiSchema;

#[derive(ApiSchema)]
struct Page<T> {
    items: Vec<T>,
}

fn main() {}
