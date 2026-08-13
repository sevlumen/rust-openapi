use oas_rs::{App, Json, OpenApi, Path};
use uuid::Uuid;

#[derive(serde::Serialize, OpenApi)]
struct User {
    id: Uuid,
}

async fn get_user(Path(id): Path<Uuid>) -> Json<User> {
    Json(User { id })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut app = App::new().title("Users API").version("1.0.0");
    app.get("/users/{id}", get_user)
        .tag("Users")
        .summary("Get user");
    app.openapi("/openapi.json").swagger("/swagger");
    app.listen("0.0.0.0:8080").await
}
