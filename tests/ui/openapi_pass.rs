use oas_rs::OpenApi;

#[derive(OpenApi)]
struct UserQuery {
    page: u32,
    active: bool,
    tag: Option<String>,
}

fn main() {
    let _ = <UserQuery as oas_rs::OpenApiQuery>::parameters();
    let _ = <UserQuery as oas_rs::OpenApiSchema>::schema();
}
