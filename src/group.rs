use crate::*;

/// Joins a group prefix and a route path without doubled or missing slashes.
fn join(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let path = path.trim_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{path}"),
        (false, true) => format!("/{prefix}"),
        (false, false) => format!("/{prefix}/{path}"),
    }
}

/// Routes registered through a group share a path prefix, and layers added to
/// the group apply only to requests under that prefix. Obtain one with
/// [`App::group`].
///
/// Every registration and per-route method returns the group itself, so a
/// chain such as `g.get("/a", h).get("/b", h2)` keeps the prefix for both
/// routes; returning the `App` instead would silently drop the prefix for the
/// second one. (`group` returns the closure's result, like [`App::group`].)
pub struct Group<'a, S = ()> {
    pub(crate) app: &'a mut App<S>,
    pub(crate) prefix: String,
}

impl<S: Send + Sync + 'static> Group<'_, S> {
    /// A nested group whose prefix is this group's prefix plus `prefix`.
    pub fn group<R>(&mut self, prefix: &str, f: impl FnOnce(&mut Group<'_, S>) -> R) -> R {
        let mut nested = Group {
            app: &mut *self.app,
            prefix: join(&self.prefix, prefix),
        };
        f(&mut nested)
    }

    pub fn get<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.get(&join(&self.prefix, path), handler);
        self
    }

    pub fn post<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.post(&join(&self.prefix, path), handler);
        self
    }

    pub fn put<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.put(&join(&self.prefix, path), handler);
        self
    }

    pub fn patch<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.patch(&join(&self.prefix, path), handler);
        self
    }

    pub fn delete<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.delete(&join(&self.prefix, path), handler);
        self
    }

    pub fn head<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.head(&join(&self.prefix, path), handler);
        self
    }

    pub fn options<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Handler<S, A>,
    {
        self.app.options(&join(&self.prefix, path), handler);
        self
    }

    /// See [`App::raw`].
    pub fn raw<H>(&mut self, method: Method, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.app.raw(method, &join(&self.prefix, path), handler);
        self
    }

    /// See [`App::raw_get`].
    pub fn raw_get<H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.app.raw_get(&join(&self.prefix, path), handler);
        self
    }

    /// See [`App::static_text`].
    pub fn static_text(&mut self, path: &str, body: &'static str) -> &mut Self {
        self.app.static_text(&join(&self.prefix, path), body);
        self
    }

    /// See [`App::static_json`].
    pub fn static_json(&mut self, path: &str, body: Bytes) -> &mut Self {
        self.app.static_json(&join(&self.prefix, path), body);
        self
    }

    /// Registers a layer for this group: it applies to requests under the
    /// group's prefix only (see [`App::layer_for`]).
    pub fn layer(&mut self, middleware: impl Middleware) -> &mut Self {
        self.app.layer_for(&self.prefix, middleware);
        self
    }

    /// Registers a layer for a path below the group's prefix.
    pub fn layer_for(&mut self, prefix: &str, middleware: impl Middleware) -> &mut Self {
        self.app.layer_for(&join(&self.prefix, prefix), middleware);
        self
    }

    // Per-route forwarders: they act on the route registered last.

    /// See [`App::tag`].
    pub fn tag(&mut self, tag: impl Into<String>) -> &mut Self {
        self.app.tag(tag);
        self
    }

    /// See [`App::summary`].
    pub fn summary(&mut self, summary: impl Into<String>) -> &mut Self {
        self.app.summary(summary);
        self
    }

    /// See [`App::operation_id`].
    pub fn operation_id(&mut self, operation_id: impl Into<String>) -> &mut Self {
        self.app.operation_id(operation_id);
        self
    }

    /// See [`App::security`].
    pub fn security<I, T>(&mut self, schemes: I) -> &mut Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.app.security(schemes);
        self
    }

    /// See [`App::public`].
    pub fn public(&mut self) -> &mut Self {
        self.app.public();
        self
    }

    /// See [`App::body_limit`].
    pub fn body_limit(&mut self, limit: usize) -> &mut Self {
        self.app.body_limit(limit);
        self
    }

    /// See [`App::route_layer`].
    pub fn route_layer(&mut self, middleware: impl Middleware) -> &mut Self {
        self.app.route_layer(middleware);
        self
    }
}
