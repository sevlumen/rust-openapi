use oas_rs::App;

async fn hello() -> &'static str {
    "hello"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut app = App::new();
    app.openapi().title("Hello API").version("1.0.0");
    app.get("/", hello).tag("Health").summary("Say hello");
    app.swagger().path("/swagger");
    app.build()?.listen("0.0.0.0:8080").await
}
