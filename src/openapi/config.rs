use std::fmt::{self, Display};

use crate::{App, Method, SecurityScheme, normalize_path};

#[derive(Clone)]
pub(crate) struct OpenApiConfig {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) description: Option<String>,
    /// Declared schemes in declaration order; redeclaring a name replaces it.
    pub(crate) security_schemes: Vec<(String, SecurityScheme)>,
    /// Document-wide security requirements (each entry is an AND group; the
    /// entries are alternatives).
    pub(crate) default_security: Vec<Vec<String>>,
    /// Whether routes document the errors the framework itself can return.
    pub(crate) document_errors: bool,
}

#[cfg(any(test, feature = "swagger"))]
#[derive(Clone)]
pub(crate) struct SwaggerConfig {
    pub(crate) path: String,
}

/// An error raised while compiling the application builder into a runtime.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    RouteConflict {
        path: String,
        method: Method,
    },
    InvalidGeneratedPath {
        path: String,
    },
    TooManyCaptures {
        path: String,
        captures: usize,
        max: usize,
    },
    /// A route or the document defaults reference a security scheme that was
    /// never declared with [`OpenApiOptions::security_scheme`] and friends.
    UnknownSecurityScheme {
        name: String,
    },
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
            Self::TooManyCaptures {
                path,
                captures,
                max,
            } => write!(
                formatter,
                "route {path} has {captures} path captures; the maximum is {max}"
            ),
            Self::UnknownSecurityScheme { name } => {
                write!(formatter, "security scheme {name:?} is not declared")
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

    /// Whether every route documents the error responses the framework can
    /// produce for it (`400` for bad parameters or bodies, `413` for a body
    /// over the limit, `401` for secured routes), all pointing at the shared
    /// `Problem` schema. On by default.
    pub fn document_errors(self, enabled: bool) -> Self {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .document_errors = enabled;
        self.app.invalidate_openapi_cache();
        self
    }

    /// Declares a security scheme under `components.securitySchemes`. Routes
    /// and [`default_security`](Self::default_security) refer to it by `name`.
    /// Redeclaring a name replaces the earlier scheme.
    pub fn security_scheme(self, name: impl Into<String>, scheme: SecurityScheme) -> Self {
        let name = name.into();
        let config = self
            .app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized");
        match config.security_schemes.iter_mut().find(|(n, _)| *n == name) {
            Some(existing) => existing.1 = scheme,
            None => config.security_schemes.push((name, scheme)),
        }
        self.app.invalidate_openapi_cache();
        self
    }

    /// Declares an HTTP bearer-token scheme named `name`.
    pub fn bearer_auth(self, name: impl Into<String>) -> Self {
        self.security_scheme(name, SecurityScheme::bearer())
    }

    /// Declares an API-key scheme named `name` that reads `key_name` from
    /// `location`, e.g. `api_key("TenantId", ApiKeyLocation::Header,
    /// "X-Tenant-Id")` (an example, not a built-in).
    pub fn api_key(
        self,
        name: impl Into<String>,
        location: crate::ApiKeyLocation,
        key_name: impl Into<String>,
    ) -> Self {
        self.security_scheme(name, SecurityScheme::api_key(location, key_name))
    }

    /// Adds a document-wide security requirement: every listed scheme is
    /// required together. Calling it again adds an alternative requirement.
    pub fn default_security<I, T>(self, schemes: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.app
            .openapi_config
            .as_mut()
            .expect("OpenAPI options are initialized")
            .default_security
            .push(schemes.into_iter().map(Into::into).collect());
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
