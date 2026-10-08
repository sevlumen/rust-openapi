use super::*;

/// Mutable route-registration builder.
pub struct App<S = ()> {
    pub(crate) state: Arc<S>,
    pub(crate) max_body_size: u32,
    pub(crate) plans: Vec<RoutePlan<S>>,
    pub(crate) metadata: Vec<RouteMetadata>,
    pub(crate) static_routes: HashMap<String, RouteSet>,
    pub(crate) dynamic_routes: DynamicRouteTrie,
    pub(crate) last_route: Option<usize>,
    pub(crate) openapi_config: Option<OpenApiConfig>,
    pub(crate) openapi_bytes: Option<Bytes>,
    pub(crate) route_error: Option<BuildError>,
    pub(crate) middleware: Vec<ScopedLayer>,
    /// Named schemas of the routes registered so far (`components.schemas`).
    pub(crate) schemas: SchemaRegistry,
    #[cfg(any(test, feature = "swagger"))]
    pub(crate) swagger_config: Option<SwaggerConfig>,
    #[cfg(any(test, feature = "swagger"))]
    pub(crate) swagger_bytes: Option<Bytes>,
}

impl App<()> {
    pub fn new() -> Self {
        Self {
            state: Arc::new(()),
            max_body_size: encode_body_limit(DEFAULT_MAX_BODY_SIZE),
            plans: Vec::new(),
            metadata: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: DynamicRouteTrie::default(),
            last_route: None,
            openapi_config: None,
            openapi_bytes: None,
            route_error: None,
            middleware: Vec::new(),
            schemas: SchemaRegistry::document(),
            #[cfg(any(test, feature = "swagger"))]
            swagger_config: None,
            #[cfg(any(test, feature = "swagger"))]
            swagger_bytes: None,
        }
    }

    pub fn with_state<T: Send + Sync + 'static>(self, state: T) -> App<T> {
        assert!(
            self.plans.is_empty(),
            "with_state must be configured before routes"
        );
        App {
            state: Arc::new(state),
            max_body_size: self.max_body_size,
            plans: Vec::new(),
            metadata: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: DynamicRouteTrie::default(),
            last_route: None,
            openapi_config: self.openapi_config,
            openapi_bytes: self.openapi_bytes,
            route_error: self.route_error,
            middleware: self.middleware,
            schemas: SchemaRegistry::document(),
            #[cfg(any(test, feature = "swagger"))]
            swagger_config: self.swagger_config,
            #[cfg(any(test, feature = "swagger"))]
            swagger_bytes: self.swagger_bytes,
        }
    }
}

impl Default for App<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync + 'static> App<S> {
    /// Enable OpenAPI and return its configuration builder.
    ///
    /// The default document path is `/openapi.json`, with title and version
    /// taken from the package that owns this framework configuration.
    pub fn openapi(&mut self) -> OpenApiOptions<'_, S> {
        if self.openapi_config.is_none() {
            self.openapi_config = Some(OpenApiConfig {
                path: "/openapi.json".to_owned(),
                title: env!("CARGO_PKG_NAME").to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                description: None,
                security_schemes: Vec::new(),
                default_security: Vec::new(),
                document_errors: true,
            });
        }
        self.invalidate_openapi_cache();
        OpenApiOptions { app: self }
    }

    /// Enable Swagger UI and return its configuration builder.
    ///
    /// Swagger is independent from OpenAPI generation. Its default path is
    /// `/docs`, and it points at the configured OpenAPI document when one is
    /// enabled.
    #[cfg(any(test, feature = "swagger"))]
    pub fn swagger(&mut self) -> SwaggerOptions<'_, S> {
        if self.swagger_config.is_none() {
            self.swagger_config = Some(SwaggerConfig {
                path: "/docs".to_owned(),
            });
        }
        self.swagger_bytes = None;
        SwaggerOptions { app: self }
    }

    pub(crate) fn invalidate_openapi_cache(&mut self) {
        self.openapi_bytes = None;
        #[cfg(any(test, feature = "swagger"))]
        {
            self.swagger_bytes = None;
        }
    }

    /// Set the maximum body size collected by buffered typed extractors for
    /// **every** buffered route: those already registered and those registered
    /// later. Use [`body_limit`](Self::body_limit) to change one route only.
    ///
    /// Raw `Incoming` handlers remain streaming and are not affected.
    pub fn max_body_size(&mut self, limit: usize) -> &mut Self {
        let limit = encode_body_limit(limit);
        self.max_body_size = limit;
        for plan in &mut self.plans {
            if matches!(plan.body_mode, BodyMode::Buffered) {
                plan.body_limit = limit;
            }
        }
        self
    }

    /// Sets the body-size limit of the last registered route, overriding the
    /// application-wide limit for that route only. It has no effect on routes
    /// without a buffered body (raw handlers, handlers that read no body).
    ///
    /// Call it right after registering the route. A later call to
    /// [`max_body_size`](Self::max_body_size) resets every buffered route,
    /// including this one.
    pub fn body_limit(&mut self, limit: usize) -> &mut Self {
        let limit = encode_body_limit(limit);
        if let Some(index) = self.last_route
            && let Some(plan) = self.plans.get_mut(index)
            && matches!(plan.body_mode, BodyMode::Buffered)
        {
            plan.body_limit = limit;
        }
        self
    }

    /// Freeze route registration into an immutable runtime.
    ///
    /// The returned runtime exposes serving and request execution only; route
    /// registration methods remain available on the builder type.
    /// Every scheme referenced by the document defaults or a route must have
    /// been declared; otherwise Swagger UI's Authorize button would be broken.
    fn validate_schemas(&self) -> Result<(), BuildError> {
        if let Some(name) = self.schemas.conflicts().first() {
            return Err(BuildError::SchemaNameConflict { name: name.clone() });
        }
        let documents_errors = self
            .openapi_config
            .as_ref()
            .is_none_or(|config| config.document_errors);
        let default_secured = self
            .openapi_config
            .as_ref()
            .is_some_and(|config| !config.default_security.is_empty());
        if documents_errors
            && self.schemas.definitions().contains_key("Problem")
            && self.metadata.iter().any(|metadata| {
                !metadata.builtin && !framework_errors(metadata, default_secured).is_empty()
            })
        {
            return Err(BuildError::SchemaNameConflict {
                name: "Problem".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_security(&self) -> Result<(), BuildError> {
        let Some(config) = &self.openapi_config else {
            return Ok(());
        };
        let declared = |name: &String| config.security_schemes.iter().any(|(n, _)| n == name);
        let route_groups = self
            .metadata
            .iter()
            .filter(|metadata| !metadata.builtin)
            .filter_map(|metadata| metadata.operation.security.as_ref())
            .flatten();
        match config
            .default_security
            .iter()
            .chain(route_groups)
            .flatten()
            .find(|name| !declared(name))
        {
            Some(name) => Err(BuildError::UnknownSecurityScheme { name: name.clone() }),
            None => Ok(()),
        }
    }

    pub fn build(mut self) -> Result<AppRuntime<S>, BuildError> {
        if let Some(error) = self.route_error.take() {
            return Err(error);
        }
        self.validate_schemas()?;
        self.validate_security()?;
        self.prepare_openapi();
        #[cfg(any(test, feature = "swagger"))]
        self.prepare_swagger();
        self.install_generated_routes()?;
        Ok(AppRuntime {
            inner: Arc::new(RuntimeInner {
                state: self.state,
                plans: self.plans.into_boxed_slice(),
                capture_names: self
                    .metadata
                    .iter()
                    .map(|metadata| metadata.capture_names.clone())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                static_routes: self.static_routes,
                dynamic_routes: self.dynamic_routes,
                middleware: self.middleware.into_boxed_slice(),
            }),
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            tcp_nodelay: true,
            header_read_timeout: Some(DEFAULT_HEADER_READ_TIMEOUT),
            max_connections: None,
            connection_error_observer: None,
            #[cfg(feature = "http2")]
            http2_max_concurrent_streams: None,
            #[cfg(feature = "http2")]
            h2c: false,
            #[cfg(feature = "tls")]
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        })
    }

    pub fn get<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::GET, path, handler);
        self
    }

    /// Register a GET route whose text response is prepared at startup.
    ///
    /// This is the lowest-overhead route for health checks, readiness probes,
    /// and other immutable text responses. It does not create or poll a
    /// handler future per request.
    pub fn static_text(&mut self, path: &str, body: &'static str) -> &mut Self {
        self.add_static_route(path, StaticResponse::text(body));
        self
    }

    /// Register a GET route whose JSON bytes are prepared by the application.
    ///
    /// The bytes are reference-counted and cloned by handle, so serialization
    /// is not repeated during request dispatch.
    pub fn static_json(&mut self, path: &str, body: Bytes) -> &mut Self {
        self.add_static_route(path, StaticResponse::json(body));
        self
    }

    pub fn head<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::HEAD, path, handler);
        self
    }

    pub fn post<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::POST, path, handler);
        self
    }

    pub fn put<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::PUT, path, handler);
        self
    }

    pub fn patch<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::PATCH, path, handler);
        self
    }

    pub fn delete<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::DELETE, path, handler);
        self
    }

    pub fn options<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.add_route(Method::OPTIONS, path, handler);
        self
    }

    /// Register a handler that receives Hyper's streaming `Incoming` body
    /// directly. The framework does not collect or size-limit this body; the
    /// handler owns cancellation and any upload limits.
    pub fn raw<H>(&mut self, method: Method, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.add_raw_route(method, path, handler);
        self
    }

    /// Register a GET handler that receives Hyper's streaming `Incoming`
    /// body directly.
    pub fn raw_get<H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.raw(Method::GET, path, handler)
    }

    /// Registers routes under a shared path prefix, with layers scoped to it.
    /// The closure receives a [`Group`]; layers added to it (and routes it
    /// registers) apply under `prefix` only.
    ///
    /// ```
    /// # use oas_rs::App;
    /// # async fn list() -> &'static str { "users" }
    /// # let mut app = App::new();
    /// app.group("/admin", |g| {
    ///     g.get("/users", list).tag("admin"); // registered as /admin/users
    /// });
    /// ```
    pub fn group<R>(&mut self, prefix: &str, f: impl FnOnce(&mut Group<'_, S>) -> R) -> R {
        let mut group = Group {
            app: self,
            prefix: prefix.to_owned(),
        };
        f(&mut group)
    }

    /// Registers a global layer. Layers run before routing; the first one
    /// registered is the outermost. See [`Middleware`].
    pub fn layer(&mut self, middleware: impl Middleware) -> &mut Self {
        self.middleware.push(ScopedLayer {
            scope: Scope::All,
            layer: Arc::new(middleware),
        });
        self
    }

    /// Scopes a layer to the last registered route's path **pattern** and
    /// method (a `HEAD` request also matches a `GET` route). Layers run before
    /// routing, so it cannot tell which route will be chosen: it also covers
    /// requests a more specific sibling route would serve (a layer on
    /// `GET /items/{id}` runs for `GET /items/new`), and `HEAD` matches a `GET`
    /// route's layer even when an explicit `HEAD` route exists. Other methods on
    /// the same path are not affected, and automatic `OPTIONS` answers do not
    /// match a method-scoped layer.
    ///
    /// **Does nothing if no route has been registered yet**: call it right
    /// after registering the route.
    pub fn route_layer(&mut self, middleware: impl Middleware) -> &mut Self {
        if let Some(index) = self.last_route {
            let metadata = &self.metadata[index];
            self.middleware.push(ScopedLayer {
                scope: Scope::Route {
                    method: metadata.method.clone(),
                    segments: Scope::parse(&metadata.template),
                },
                layer: Arc::new(middleware),
            });
        }
        self
    }

    /// Registers a layer that applies only to requests whose path starts with
    /// `prefix`, which may contain `{capture}` segments. A prefix matches whole
    /// segments (`/admin` covers `/admin` and `/admin/x`, not `/administrator`)
    /// and also covers paths with no route under it (a `404`), so an
    /// authentication layer does not reveal which routes exist. A segment
    /// containing `%` is also compared percent-decoded (a `{capture}` route
    /// decodes its segment), matching is case-sensitive, and a scope is never
    /// narrower than what the router serves.
    ///
    /// # Panics
    ///
    /// If `prefix` is malformed (`{id`, `{}`, `x{id}`), like a route template.
    pub fn layer_for(&mut self, prefix: &str, middleware: impl Middleware) -> &mut Self {
        self.middleware.push(ScopedLayer {
            scope: Scope::Prefix(Scope::parse(prefix)),
            layer: Arc::new(middleware),
        });
        self
    }

    pub fn tag(&mut self, tag: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.metadata[index].operation.tag = Some(tag.into());
            self.invalidate_openapi_cache();
        }
        self
    }

    pub fn summary(&mut self, summary: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.metadata[index].operation.summary = Some(summary.into());
            self.invalidate_openapi_cache();
        }
        self
    }

    /// Requires every listed security scheme for the last registered route.
    /// Calling it again adds an alternative requirement. The schemes must be
    /// declared through [`OpenApiOptions`] or `build()` fails.
    pub fn security<I, T>(&mut self, schemes: I) -> &mut Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        if let Some(index) = self.last_route {
            self.metadata[index]
                .operation
                .security
                .get_or_insert_with(Vec::new)
                .push(schemes.into_iter().map(Into::into).collect());
            self.invalidate_openapi_cache();
        }
        self
    }

    /// Marks the last registered route as requiring no authentication,
    /// overriding any document-wide default security.
    pub fn public(&mut self) -> &mut Self {
        if let Some(index) = self.last_route {
            self.metadata[index].operation.security = Some(Vec::new());
            self.invalidate_openapi_cache();
        }
        self
    }

    pub fn operation_id(&mut self, operation_id: impl Into<String>) -> &mut Self {
        let operation_id = operation_id.into();
        assert!(
            self.metadata.iter().all(|metadata| {
                metadata.operation.operation_id.as_deref() != Some(operation_id.as_str())
            }),
            "duplicate operation id"
        );
        if let Some(index) = self.last_route {
            self.metadata[index].operation.operation_id = Some(operation_id);
            self.invalidate_openapi_cache();
        }
        self
    }

    pub fn openapi_document(&self) -> Value {
        let mut paths = Map::new();
        let document_errors = self
            .openapi_config
            .as_ref()
            .is_none_or(|config| config.document_errors);
        let default_secured = self
            .openapi_config
            .as_ref()
            .is_some_and(|config| !config.default_security.is_empty());
        let mut uses_problem = false;
        for metadata in self.metadata.iter() {
            if metadata.builtin {
                continue;
            }
            let method = metadata.method.as_str().to_ascii_lowercase();
            let mut operation = Map::new();
            if let Some(tag) = &metadata.operation.tag {
                operation.insert("tags".to_owned(), json!([tag]));
            }
            if let Some(summary) = &metadata.operation.summary {
                operation.insert("summary".to_owned(), json!(summary));
            }
            if let Some(operation_id) = &metadata.operation.operation_id {
                operation.insert("operationId".to_owned(), json!(operation_id));
            }
            if let Some(security) = &metadata.operation.security {
                operation.insert(
                    "security".to_owned(),
                    Value::Array(
                        security
                            .iter()
                            .map(|group| requirement_json(group))
                            .collect(),
                    ),
                );
            }
            let mut parameters = metadata.operation.request.parameters.clone();
            let mut path_schema_index = 0;
            parameters.extend(
                metadata
                    .segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => {
                            let schema = metadata
                                .operation
                                .request
                                .path_schemas
                                .get(path_schema_index)
                                .cloned()
                                .unwrap_or_else(|| json!({ "type": "string" }));
                            path_schema_index += 1;
                            Some(json!({
                                "in": "path",
                                "name": name,
                                "required": true,
                                "schema": schema
                            }))
                        }
                        Segment::Static(_) => None,
                    })
                    .collect::<Vec<_>>(),
            );
            if !parameters.is_empty() {
                operation.insert("parameters".to_owned(), Value::Array(parameters));
            }
            if let Some(request_body) = &metadata.operation.request.request_body {
                operation.insert("requestBody".to_owned(), request_body.clone());
            }
            let status = metadata.operation.response_status.as_u16().to_string();
            let mut response = Map::new();
            response.insert(
                "description".to_owned(),
                Value::String(
                    match metadata.operation.response_status {
                        StatusCode::NO_CONTENT => "No Content",
                        StatusCode::NOT_MODIFIED => "Not Modified",
                        StatusCode::CREATED => "Created",
                        _ => "Success",
                    }
                    .to_owned(),
                ),
            );
            if let Some(schema) = &metadata.operation.response_schema {
                response.insert(
                    "content".to_owned(),
                    json!({ "application/json": { "schema": schema } }),
                );
            }
            let mut responses = Map::new();
            responses.insert(status, Value::Object(response));
            if document_errors {
                for (code, description) in framework_errors(metadata, default_secured) {
                    uses_problem = true;
                    responses.insert(
                        code.to_owned(),
                        json!({
                            "description": description,
                            "content": { "application/json": {
                                "schema": { "$ref": PROBLEM_REF }
                            } }
                        }),
                    );
                }
            }
            operation.insert("responses".to_owned(), Value::Object(responses));
            paths
                .entry(metadata.template.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            paths
                .get_mut(&metadata.template)
                .and_then(Value::as_object_mut)
                .unwrap()
                .insert(method, Value::Object(operation));
        }
        let config = self.openapi_config.as_ref();
        let mut info = Map::new();
        info.insert(
            "title".to_owned(),
            json!(config.map_or(env!("CARGO_PKG_NAME"), |config| config.title.as_str())),
        );
        info.insert(
            "version".to_owned(),
            json!(config.map_or(env!("CARGO_PKG_VERSION"), |config| config.version.as_str())),
        );
        if let Some(description) = config.and_then(|config| config.description.as_deref()) {
            info.insert("description".to_owned(), json!(description));
        }
        let mut document = json!({
            "openapi": "3.1.0",
            "info": Value::Object(info),
            "paths": paths,
        });
        let mut components = Map::new();
        let mut schemas: Map<String, Value> = self
            .schemas
            .definitions()
            .iter()
            .map(|(name, schema)| (name.clone(), schema.clone()))
            .collect();
        if uses_problem {
            schemas
                .entry("Problem".to_owned())
                .or_insert_with(problem_schema);
        }
        if !schemas.is_empty() {
            components.insert("schemas".to_owned(), Value::Object(schemas));
        }
        if let Some(config) = config {
            if !config.security_schemes.is_empty() {
                let schemes: Map<String, Value> = config
                    .security_schemes
                    .iter()
                    .map(|(name, scheme)| (name.clone(), scheme.to_json()))
                    .collect();
                components.insert("securitySchemes".to_owned(), Value::Object(schemes));
            }
            if !config.default_security.is_empty() {
                document["security"] = Value::Array(
                    config
                        .default_security
                        .iter()
                        .map(|group| requirement_json(group))
                        .collect(),
                );
            }
        }
        if !components.is_empty() {
            document["components"] = Value::Object(components);
        }
        document
    }

    /// Temporary compatibility helper for the in-tree tests. Production
    /// serving is exposed by [`AppRuntime`]; this helper is hidden from the
    /// normal API surface and will move to `oas-test`.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-util"))]
    pub async fn oneshot(
        &mut self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Bytes>,
    ) -> TestResponse {
        self.prepare_openapi();
        #[cfg(any(test, feature = "swagger"))]
        self.prepare_swagger();
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            if !name.is_empty() {
                builder = builder.header(*name, *value);
            }
        }
        let request = builder
            .body(body.unwrap_or_default())
            .expect("valid test request");
        let path = normalize_request_path(request.uri().path());
        #[cfg(any(test, feature = "swagger"))]
        let response = if self
            .openapi_config
            .as_ref()
            .is_some_and(|config| config.path == path)
        {
            self.openapi_bytes
                .clone()
                .map(|body| response_json_bytes(StatusCode::OK, body))
        } else if self
            .swagger_config
            .as_ref()
            .is_some_and(|config| config.path == path)
        {
            self.swagger_bytes
                .clone()
                .map(|body| response_text(StatusCode::OK, body))
        } else {
            None
        };
        #[cfg(not(any(test, feature = "swagger")))]
        let response = self
            .openapi_config
            .as_ref()
            .is_some_and(|config| config.path == path)
            .then(|| {
                self.openapi_bytes
                    .clone()
                    .map(|body| response_json_bytes(StatusCode::OK, body))
            })
            .flatten();
        let response = match response {
            Some(response) => response,
            None => self.handle(request).await,
        };
        TestResponse {
            response: Some(response),
        }
    }

    fn add_static_route(&mut self, path: &str, response: StaticResponse) {
        let template = normalize_path(path);
        let segments = parse_template(&template);
        assert!(
            segments
                .iter()
                .all(|segment| matches!(segment, Segment::Static(_))),
            "static response routes cannot contain captures"
        );
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != Method::GET || metadata.template != template),
            "duplicate route"
        );

        let index = self.plans.len();
        self.static_routes
            .entry(template.clone())
            .or_default()
            .insert(Method::GET, index);
        self.plans.push(RoutePlan {
            handler: HandlerKind::Static(Box::new(response)),
            body_limit: 0,
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::None,
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method: Method::GET,
            template,
            segments,
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                security: None,
                response_status: StatusCode::OK,
                response_schema: None,
                request: OpenApiRequest::default(),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn add_route<H, A>(&mut self, method: Method, path: &str, handler: H)
    where
        H: Handler<S, A>,
    {
        let template = normalize_path(path);
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != method || metadata.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        if self.record_capture_limit_error(&template, &segments) {
            return;
        }
        let capture_names = if H::NEEDS_PARAMS {
            Some(
                segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => Some(name.clone()),
                        Segment::Static(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .into(),
            )
        } else {
            None
        };
        let capture_mode = if capture_names.is_some() {
            CaptureMode::Materialized
        } else if segments
            .iter()
            .any(|segment| matches!(segment, Segment::Capture(_)))
        {
            CaptureMode::Borrowed
        } else {
            CaptureMode::None
        };
        let handler = match handler.zero_handler() {
            Some(zero_handler) => HandlerKind::Zero(zero_handler),
            None if !H::NEEDS_CAPTURE => {
                HandlerKind::TypedNoParams(Box::new(move |request, state| {
                    handler.call(request, Params::empty(), state)
                }))
            }
            None => HandlerKind::Typed(Box::new(move |request, params, state| {
                handler.call(request, params, state)
            })),
        };
        let index = self.plans.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .insert(method.clone(), index);
        } else {
            self.dynamic_routes.insert(&segments, method.clone(), index);
        }
        self.plans.push(RoutePlan {
            handler,
            body_limit: if H::NEEDS_BODY { self.max_body_size } else { 0 },
            capture_mode,
            body_mode: if H::NEEDS_BODY {
                BodyMode::Buffered
            } else {
                BodyMode::None
            },
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method,
            template,
            segments,
            capture_names,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                security: None,
                response_status: <H::Response as ResponseMetadata>::status_code(),
                response_schema: <H::Response as ResponseMetadata>::response_schema_with(
                    &mut self.schemas,
                ),
                request: H::openapi_request_with(&mut self.schemas),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn add_raw_route<H>(&mut self, method: Method, path: &str, handler: H)
    where
        H: RawHandler<S>,
    {
        let template = normalize_path(path);
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != method || metadata.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        if self.record_capture_limit_error(&template, &segments) {
            return;
        }
        let handler = HandlerKind::Raw(Box::new(move |request| handler.call(request)));
        let index = self.plans.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .insert(method.clone(), index);
        } else {
            self.dynamic_routes.insert(&segments, method.clone(), index);
        }
        self.plans.push(RoutePlan {
            handler,
            body_limit: 0,
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::Incoming,
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method,
            template,
            segments,
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                security: None,
                response_status: <H::Response as ResponseMetadata>::status_code(),
                response_schema: <H::Response as ResponseMetadata>::response_schema_with(
                    &mut self.schemas,
                ),
                request: OpenApiRequest::default(),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn record_capture_limit_error(&mut self, path: &str, segments: &[Segment]) -> bool {
        let captures = segments
            .iter()
            .filter(|segment| matches!(segment, Segment::Capture(_)))
            .count();
        if captures <= MAX_CAPTURE_PARAMS {
            return false;
        }
        if self.route_error.is_none() {
            self.route_error = Some(BuildError::TooManyCaptures {
                path: path.to_owned(),
                captures,
                max: MAX_CAPTURE_PARAMS,
            });
        }
        true
    }

    fn prepare_openapi(&mut self) {
        if self.openapi_config.is_some() && self.openapi_bytes.is_none() {
            self.openapi_bytes = Some(Bytes::from(self.openapi_document().to_string()));
        }
    }

    #[cfg(any(test, feature = "swagger"))]
    fn prepare_swagger(&mut self) {
        if self.swagger_config.is_some() && self.swagger_bytes.is_none() {
            self.swagger_bytes = Some(swagger_html(
                self.openapi_config
                    .as_ref()
                    .map(|config| config.path.as_str()),
            ));
        }
    }

    fn install_generated_routes(&mut self) -> Result<(), BuildError> {
        if let Some(config) = self.openapi_config.clone()
            && let Some(body) = self.openapi_bytes.clone()
        {
            self.add_generated_static_route(config.path, StaticResponse::json(body))?;
        }
        #[cfg(any(test, feature = "swagger"))]
        if let Some(config) = self.swagger_config.clone()
            && let Some(body) = self.swagger_bytes.clone()
        {
            self.add_generated_static_route(config.path, StaticResponse::text_bytes(body))?;
        }
        Ok(())
    }

    fn add_generated_static_route(
        &mut self,
        path: String,
        response: StaticResponse,
    ) -> Result<(), BuildError> {
        let segments = parse_template(&path);
        if !segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            return Err(BuildError::InvalidGeneratedPath { path });
        }
        if self
            .static_routes
            .get(&path)
            .and_then(|routes| routes.route(&Method::GET))
            .is_some()
        {
            return Err(BuildError::RouteConflict {
                path,
                method: Method::GET,
            });
        }
        let index = self.plans.len();
        self.static_routes
            .entry(path.clone())
            .or_default()
            .insert(Method::GET, index);
        self.plans.push(RoutePlan {
            handler: HandlerKind::Static(Box::new(response)),
            body_limit: 0,
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::None,
        });
        self.metadata.push(RouteMetadata {
            builtin: true,
            method: Method::GET,
            template: path,
            segments,
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                security: None,
                response_status: StatusCode::OK,
                response_schema: None,
                request: OpenApiRequest::default(),
            },
        });
        Ok(())
    }

    #[cfg(any(test, feature = "test-util"))]
    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let is_head = request.method() == Method::HEAD;
        let path = normalize_request_path(request.uri().path());
        if let Some(routes) = self.static_routes.get(path) {
            return match resolve_route_set(request.method(), routes) {
                Ok(index) => {
                    self.handle_matched(request, index, StaticCaptures, is_head)
                        .await
                }
                Err(RouteFailure::Options(allow)) => options_response(&allow),
                Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
            };
        }
        let Some(path_match) = self.dynamic_routes.find(path) else {
            return not_found();
        };
        match resolve_route_set(request.method(), path_match.routes) {
            Ok(index) => {
                self.handle_matched(
                    request,
                    index,
                    DynamicCaptures(path_match.captures),
                    is_head,
                )
                .await
            }
            Err(RouteFailure::Options(allow)) => options_response(&allow),
            Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    async fn handle_matched<C: CaptureProvider>(
        &self,
        request: Request<Bytes>,
        index: RouteId,
        captures: C,
        is_head: bool,
    ) -> HttpResponse {
        let plan = &self.plans[index.index()];
        match &plan.handler {
            HandlerKind::Zero(handler) => return maybe_head_flag(is_head, handler().await),
            HandlerKind::Static(response) => {
                return maybe_head_flag(is_head, response.to_response());
            }
            HandlerKind::TypedNoParams(_) | HandlerKind::Typed(_) | HandlerKind::Raw(_) => {}
        }
        let mut request = request;
        if matches!(plan.body_mode, BodyMode::Buffered)
            && request.body().len() > plan.body_limit as usize
        {
            return payload_too_large_response();
        }
        let response = match &plan.handler {
            HandlerKind::TypedNoParams(handler) => handler(&mut request, &self.state).await,
            HandlerKind::Typed(handler) => {
                captures
                    .invoke(
                        &plan.capture_mode,
                        &mut request,
                        self.metadata[index.index()].capture_names.as_ref(),
                        handler,
                        &self.state,
                    )
                    .await
            }
            HandlerKind::Raw(_) => unreachable!("raw routes require Incoming request bodies"),
            HandlerKind::Zero(_) | HandlerKind::Static(_) => {
                unreachable!("fast handlers returned before typed dispatch")
            }
        };
        maybe_head_flag(is_head, response)
    }
}

#[derive(Clone, Default)]
pub(crate) struct Operation {
    pub(crate) tag: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) operation_id: Option<String>,
    pub(crate) response_status: StatusCode,
    pub(crate) response_schema: Option<Value>,
    pub(crate) request: OpenApiRequest,
    /// `None` inherits the document default; `Some(vec![])` is explicitly
    /// public; otherwise each entry is an AND group and entries are alternatives.
    pub(crate) security: Option<Vec<Vec<String>>>,
}

/// The error responses the framework itself can produce for a route, from
/// what the route declares: `400` for typed parameters or a JSON body, `413`
/// and `415` for a body, `401` for declared security (not for a layer that
/// enforces it without `.security(..)`). Raw routes and custom extractors
/// declare nothing, so they get none.
fn framework_errors(
    metadata: &RouteMetadata,
    default_secured: bool,
) -> Vec<(&'static str, &'static str)> {
    let request = &metadata.operation.request;
    let mut errors = Vec::new();
    if !request.parameters.is_empty()
        || !request.path_schemas.is_empty()
        || request.request_body.is_some()
    {
        errors.push(("400", "Bad Request"));
    }
    if request.request_body.is_some() {
        errors.push(("413", "Payload Too Large"));
        errors.push(("415", "Unsupported Media Type"));
    }
    let secured = match &metadata.operation.security {
        Some(requirements) => !requirements.is_empty(),
        None => default_secured,
    };
    if secured {
        errors.push(("401", "Unauthorized"));
    }
    errors.sort();
    errors
}

const PROBLEM_REF: &str = "#/components/schemas/Problem";

/// The body of every error the framework produces (see `ApiError`).
fn problem_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type", "title", "status", "detail"],
        "properties": {
            "type": { "type": "string", "examples": ["about:blank"] },
            "title": { "type": "string" },
            "status": { "type": "integer" },
            "detail": { "type": "string" }
        }
    })
}
