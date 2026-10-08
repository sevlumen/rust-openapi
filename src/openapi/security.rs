use serde_json::{Value, json};

/// Where an API key is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApiKeyLocation {
    Header,
    Query,
    Cookie,
}

impl ApiKeyLocation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
            Self::Cookie => "cookie",
        }
    }
}

/// An OpenAPI security scheme, declared once under `components.securitySchemes`
/// and referenced by name from routes.
///
/// This only *describes* authentication in the generated document (and makes
/// Swagger UI show its Authorize button); it does not enforce anything.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecurityScheme {
    Http {
        scheme: String,
        bearer_format: Option<String>,
    },
    ApiKey {
        location: ApiKeyLocation,
        name: String,
    },
}

impl SecurityScheme {
    /// HTTP `Authorization: Bearer <token>`.
    pub fn bearer() -> Self {
        Self::Http {
            scheme: "bearer".to_owned(),
            bearer_format: None,
        }
    }

    /// HTTP bearer with a documented token format such as `"JWT"`.
    pub fn bearer_with_format(format: impl Into<String>) -> Self {
        Self::Http {
            scheme: "bearer".to_owned(),
            bearer_format: Some(format.into()),
        }
    }

    /// HTTP basic authentication.
    pub fn basic() -> Self {
        Self::Http {
            scheme: "basic".to_owned(),
            bearer_format: None,
        }
    }

    /// An API key carried in a header, query parameter or cookie called `name`.
    pub fn api_key(location: ApiKeyLocation, name: impl Into<String>) -> Self {
        Self::ApiKey {
            location,
            name: name.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Http {
                scheme,
                bearer_format,
            } => {
                let mut value = json!({ "type": "http", "scheme": scheme });
                if let Some(format) = bearer_format {
                    value["bearerFormat"] = json!(format);
                }
                value
            }
            Self::ApiKey { location, name } => {
                json!({ "type": "apiKey", "in": location.as_str(), "name": name })
            }
        }
    }
}

/// Builds one OpenAPI security requirement object: every listed scheme is
/// required together (logical AND).
pub(crate) fn requirement_json(schemes: &[String]) -> Value {
    let mut object = serde_json::Map::new();
    for scheme in schemes {
        object.insert(scheme.clone(), json!([]));
    }
    Value::Object(object)
}
