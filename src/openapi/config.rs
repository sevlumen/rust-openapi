use std::fmt::{self, Display};

use crate::{App, Method, normalize_path};

#[derive(Clone)]
pub(crate) struct OpenApiConfig {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) description: Option<String>,
}

#[cfg(any(test, feature = "swagger"))]
#[derive(Clone)]
pub(crate) struct SwaggerConfig {
    pub(crate) path: String,
}

/// An error raised while compiling the application builder into a runtime.
#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    RouteConflict { path: String, method: Method },
    InvalidGeneratedPath { path: String },
}

impl Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RouteConflict { path, method } => {
                write!(formatter, "generated route conflicts with {method} {path}")
            }
            Self::InvalidGeneratedPath { path } => {
                write!(formatter, "generated route must be static: {path}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Mutable OpenAPI configuration returned by [`App::openapi`].
pub struct OpenApiOptions<'a, S> {
    pub(crate) app: &'a mut App<S>,
}

impl<S: Send + Sync + 'static> OpenApiOptions<'_, S> {
    pub fn path(self, path: impl Into<String>) -> Self {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .path = normalize_path(&path.into());
        self.app.invalidate_openapi_cache();
        self
    }

    pub fn title(self, title: impl Into<String>) -> Self {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .title = title.into();
        self.app.invalidate_openapi_cache();
        self
    }

    pub fn version(self, version: impl Into<String>) -> Self {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .version = version.into();
        self.app.invalidate_openapi_cache();
        self
    }

    pub fn description(self, description: impl Into<String>) -> Self {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .description = Some(description.into());
        self.app.invalidate_openapi_cache();
        self
    }
}

/// Mutable Swagger configuration returned by [`App::swagger`].
#[cfg(any(test, feature = "swagger"))]
pub struct SwaggerOptions<'a, S> {
    pub(crate) app: &'a mut App<S>,
}

#[cfg(any(test, feature = "swagger"))]
impl<S: Send + Sync + 'static> SwaggerOptions<'_, S> {
    pub fn path(self, path: impl Into<String>) -> Self {
        self.app.swagger_config = Some(SwaggerConfig {
            path: normalize_path(&path.into()),
        });
        self.app.swagger_bytes = None;
        self
    }
}
