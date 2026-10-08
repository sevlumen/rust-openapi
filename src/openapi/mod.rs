#[cfg(any(test, feature = "swagger"))]
use crate::*;
mod config;
mod security;

#[cfg(any(test, feature = "swagger"))]
pub use config::SwaggerOptions;
pub use config::{BuildError, OpenApiOptions};
pub(crate) use security::requirement_json;
pub use security::{ApiKeyLocation, SecurityScheme};

pub(crate) use config::OpenApiConfig;
#[cfg(any(test, feature = "swagger"))]
pub(crate) use config::SwaggerConfig;

#[cfg(any(test, feature = "swagger"))]
pub(crate) fn swagger_html(openapi_path: Option<&str>) -> Bytes {
    let openapi_path = openapi_path.unwrap_or("/openapi.json");
    let encoded_path = serde_json::to_string(openapi_path).unwrap();
    Bytes::from(format!(
        "<!doctype html><html><head><title>Swagger UI</title><link rel=\"stylesheet\" href=\"https://unpkg.com/swagger-ui-dist@5.17.14/swagger-ui.css\" integrity=\"sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn\" crossorigin=\"anonymous\"></head><body><div id=\"swagger-ui\">Loading Swagger UI…</div><script src=\"https://unpkg.com/swagger-ui-dist@5.17.14/swagger-ui-bundle.js\" integrity=\"sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep\" crossorigin=\"anonymous\"></script><script>window.onload=()=>window.ui=SwaggerUIBundle({{url:{encoded_path},dom_id:'#swagger-ui'}});</script></body></html>"
    ))
}
