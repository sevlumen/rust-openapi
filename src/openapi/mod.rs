mod config;

#[cfg(any(test, feature = "swagger"))]
pub use config::SwaggerOptions;
pub use config::{BuildError, OpenApiOptions};

pub(crate) use config::OpenApiConfig;
#[cfg(any(test, feature = "swagger"))]
pub(crate) use config::SwaggerConfig;
