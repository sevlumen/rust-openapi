use oas_rs::{ApiSchema, App, Json, Path};
use uuid::Uuid;

#[derive(serde::Serialize, ApiSchema)]
struct User {
    id: Uuid,
}

async fn get_user(Path(id): Path<Uuid>) -> Json<User> {
    Json(User { id })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut app = App::new();
    app.openapi().title("Users API").version("1.0.0");
    app.get("/users/{id}", get_user)
        .tag("Users")
        .summary("Get user");
    app.swagger().path("/swagger");
    app.build()?.listen("0.0.0.0:8080").await
}
