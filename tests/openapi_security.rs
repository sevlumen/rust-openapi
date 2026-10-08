use oas_rs::{ApiKeyLocation, App, BuildError, SecurityScheme};
use serde_json::{Value, json};

async fn ok() -> &'static str {
    "ok"
}

fn app_with(configure: impl FnOnce(&mut App)) -> Value {
    let mut app = App::new();
    app.openapi().title("Firmware API").version("1.0.0");
    configure(&mut app);
    app.openapi_document()
}

#[test]
fn no_security_configuration_leaves_the_document_unchanged() {
    let doc = app_with(|app| {
        app.get("/firmwares", ok);
    });
    assert!(doc.get("components").is_none());
    assert!(doc.get("security").is_none());
    assert!(doc["paths"]["/firmwares"]["get"].get("security").is_none());
}

#[test]
fn bearer_scheme_is_declared_under_components() {
    let doc = app_with(|app| {
        app.openapi().bearer_auth("BearerAuth");
    });
    assert_eq!(
        doc["components"]["securitySchemes"]["BearerAuth"],
        json!({ "type": "http", "scheme": "bearer" })
    );
}

#[test]
fn api_key_scheme_uses_the_example_tenant_header() {
    // `TenantId` / `X-Tenant-Id` are only an example of a custom API-key header.
    let doc = app_with(|app| {
        app.openapi()
            .api_key("TenantId", ApiKeyLocation::Header, "X-Tenant-Id");
    });
    assert_eq!(
        doc["components"]["securitySchemes"]["TenantId"],
        json!({ "type": "apiKey", "in": "header", "name": "X-Tenant-Id" })
    );
}

#[test]
fn general_scheme_builder_supports_bearer_format_and_basic() {
    let doc = app_with(|app| {
        app.openapi()
            .security_scheme("Jwt", SecurityScheme::bearer_with_format("JWT"))
            .security_scheme("Basic", SecurityScheme::basic())
            .security_scheme(
                "Session",
                SecurityScheme::api_key(ApiKeyLocation::Cookie, "sid"),
            );
    });
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(
        schemes["Jwt"],
        json!({ "type": "http", "scheme": "bearer", "bearerFormat": "JWT" })
    );
    assert_eq!(
        schemes["Basic"],
        json!({ "type": "http", "scheme": "basic" })
    );
    assert_eq!(
        schemes["Session"],
        json!({ "type": "apiKey", "in": "cookie", "name": "sid" })
    );
}

#[test]
fn default_security_applies_globally() {
    let doc = app_with(|app| {
        app.get("/firmwares", ok);
        app.openapi()
            .bearer_auth("BearerAuth")
            .default_security(["BearerAuth"]);
    });
    assert_eq!(doc["security"], json!([{ "BearerAuth": [] }]));
    assert!(doc["paths"]["/firmwares"]["get"].get("security").is_none());
}

#[test]
fn route_security_requires_all_listed_schemes() {
    let doc = app_with(|app| {
        app.openapi().bearer_auth("BearerAuth").api_key(
            "TenantId",
            ApiKeyLocation::Header,
            "X-Tenant-Id",
        );
        app.post("/firmwares", ok)
            .security(["BearerAuth", "TenantId"]);
    });
    assert_eq!(
        doc["paths"]["/firmwares"]["post"]["security"],
        json!([{ "BearerAuth": [], "TenantId": [] }])
    );
}

#[test]
fn repeated_security_calls_add_alternatives() {
    let doc = app_with(|app| {
        app.openapi().bearer_auth("BearerAuth").api_key(
            "ApiKey",
            ApiKeyLocation::Header,
            "X-Api-Key",
        );
        app.get("/x", ok)
            .security(["BearerAuth"])
            .security(["ApiKey"]);
    });
    assert_eq!(
        doc["paths"]["/x"]["get"]["security"],
        json!([{ "BearerAuth": [] }, { "ApiKey": [] }])
    );
}

#[test]
fn public_overrides_the_default_security() {
    let doc = app_with(|app| {
        app.openapi()
            .bearer_auth("BearerAuth")
            .default_security(["BearerAuth"]);
        app.get("/health", ok).public();
    });
    assert_eq!(doc["paths"]["/health"]["get"]["security"], json!([]));
}

#[test]
fn unknown_route_scheme_fails_the_build() {
    let mut app = App::new();
    app.openapi()
        .title("t")
        .version("1")
        .bearer_auth("BearerAuth");
    app.get("/x", ok).security(["Nope"]);
    match app.build() {
        Err(BuildError::UnknownSecurityScheme { name }) => assert_eq!(name, "Nope"),
        other => panic!(
            "expected UnknownSecurityScheme, got {:?}",
            other.map(|_| ())
        ),
    }
}

#[test]
fn unknown_default_scheme_fails_the_build() {
    let mut app = App::new();
    app.openapi()
        .title("t")
        .version("1")
        .default_security(["Missing"]);
    app.get("/x", ok);
    assert!(matches!(
        app.build(),
        Err(BuildError::UnknownSecurityScheme { .. })
    ));
}

#[cfg(feature = "test-util")]
#[tokio::test]
async fn served_openapi_json_reflects_security_schemes() {
    let mut app = App::new();
    app.get("/firmwares", ok);
    app.openapi()
        .title("t")
        .version("1")
        .bearer_auth("BearerAuth");
    let response = app
        .oneshot(oas_rs::Method::GET, "/openapi.json", &[], None)
        .await;
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_str(&response.body_string().await).unwrap();
    assert_eq!(
        body["components"]["securitySchemes"]["BearerAuth"]["scheme"],
        "bearer"
    );
}
