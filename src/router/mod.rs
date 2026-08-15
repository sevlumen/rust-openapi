use super::*;

pub type ErasedZeroHandler = Box<dyn Fn() -> HandlerFuture + Send + Sync>;
type ErasedNoParamsHandler<S> =
    Box<dyn Fn(&mut Request<Bytes>, &Arc<S>) -> HandlerFuture + Send + Sync>;
type ErasedHandler<S> =
    Box<dyn Fn(&mut Request<Bytes>, &Params, &Arc<S>) -> HandlerFuture + Send + Sync>;
type ErasedRawHandler = Box<dyn Fn(Request<Incoming>) -> HandlerFuture + Send + Sync>;

pub(crate) enum HandlerKind<S> {
    Zero(ErasedZeroHandler),
    TypedNoParams(ErasedNoParamsHandler<S>),
    Typed(ErasedHandler<S>),
    Raw(ErasedRawHandler),
    // Static payloads are cold registration data; keep them out of every
    // route plan's inline layout while retaining a zero-allocation request
    // path for the static response itself.
    Static(Box<StaticResponse>),
}

pub(crate) trait CaptureProvider: Copy {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        capture_names: Option<&Arc<[String]>>,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture;
}

#[derive(Clone, Copy)]
pub(crate) struct StaticCaptures;

impl CaptureProvider for StaticCaptures {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        _capture_names: Option<&Arc<[String]>>,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture {
        debug_assert!(matches!(mode, CaptureMode::None));
        handler(request, Params::empty(), state)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DynamicCaptures(pub(crate) CaptureSet);

impl CaptureProvider for DynamicCaptures {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        capture_names: Option<&Arc<[String]>>,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture {
        let path = normalize_request_path(request.uri().path());
        let params = match mode {
            CaptureMode::None => Params::empty().clone(),
            CaptureMode::Borrowed => Params::from_match(&[], self.0, path, false),
            CaptureMode::Materialized => {
                let names = capture_names.expect("materialized captures have names");
                match Params::from_materialized_names(Arc::clone(names), self.0, path) {
                    Ok(params) => params,
                    Err(error) => return HandlerFuture::from_response_future(future::ready(error)),
                }
            }
        };
        handler(request, &params, state)
    }
}

pub(crate) enum StaticResponse {
    Text {
        body: Bytes,
        content_length: HeaderValue,
    },
    Json {
        body: Bytes,
    },
}

impl StaticResponse {
    pub(crate) fn text(body: &'static str) -> Self {
        Self::text_bytes(Bytes::from_static(body.as_bytes()))
    }

    pub(crate) fn text_bytes(body: Bytes) -> Self {
        Self::Text {
            content_length: HeaderValue::from_str(&body.len().to_string())
                .expect("static text length is a valid header value"),
            body,
        }
    }

    pub(crate) fn json(body: Bytes) -> Self {
        Self::Json { body }
    }

    pub(crate) fn to_response(&self) -> HttpResponse {
        let mut response = Response::new(ResponseBody::full(match self {
            Self::Text { body, .. } | Self::Json { body } => body.clone(),
        }));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, self.content_type());
        if let Self::Text { content_length, .. } = self {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, content_length.clone());
        }
        response
    }

    fn content_type(&self) -> HeaderValue {
        match self {
            Self::Text { .. } => HeaderValue::from_static("text/plain; charset=utf-8"),
            Self::Json { .. } => HeaderValue::from_static("application/json"),
        }
    }
}

pub(crate) struct RoutePlan<S> {
    pub(crate) handler: HandlerKind<S>,
    pub(crate) body_limit: u32,
    pub(crate) capture_mode: CaptureMode,
    pub(crate) body_mode: BodyMode,
}

pub(crate) enum CaptureMode {
    None,
    Borrowed,
    Materialized,
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub(crate) enum BodyMode {
    None,
    Buffered,
    Incoming,
}

pub(crate) struct RouteMetadata {
    pub(crate) builtin: bool,
    pub(crate) method: Method,
    pub(crate) template: String,
    pub(crate) segments: Vec<Segment>,
    pub(crate) capture_names: Option<Arc<[String]>>,
    pub(crate) operation: Operation,
}

#[derive(Clone)]
pub(crate) enum Segment {
    Static(String),
    Capture(String),
}

pub(crate) struct DynamicRouteTrie {
    nodes: Vec<DynamicRouteNode>,
}

#[derive(Default)]
pub(crate) struct DynamicRouteNode {
    static_children: HashMap<String, NodeId>,
    capture_child: Option<NodeId>,
    routes: RouteSet,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NodeId(u32);

impl NodeId {
    fn new(index: usize) -> Self {
        assert!(
            index <= u32::MAX as usize,
            "dynamic trie exceeds u32::MAX nodes"
        );
        Self(index as u32)
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteId(u32);

impl RouteId {
    const NONE: Self = Self(u32::MAX);

    fn new(index: usize) -> Self {
        assert!(index <= u32::MAX as usize, "route count exceeds u32::MAX");
        Self(index as u32)
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

pub(crate) struct RouteSet {
    routes: [RouteId; 7],
    method_mask: u8,
}

impl Default for RouteSet {
    fn default() -> Self {
        Self {
            routes: [RouteId::NONE; 7],
            method_mask: 0,
        }
    }
}

impl RouteSet {
    pub(crate) fn insert(&mut self, method: Method, route: usize) {
        let slot = method_slot(&method).expect("unsupported route method");
        let route = RouteId::new(route);
        assert_eq!(
            self.routes[slot],
            RouteId::NONE,
            "duplicate route pattern and method"
        );
        self.routes[slot] = route;
        self.method_mask |= 1_u8 << slot;
    }

    pub(crate) fn route(&self, method: &Method) -> Option<RouteId> {
        method_slot(method)
            .and_then(|slot| (self.routes[slot] != RouteId::NONE).then_some(self.routes[slot]))
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn allowed_methods(&self) -> String {
        let mut allowed = String::with_capacity(48);
        for (index, name) in METHOD_NAMES.iter().enumerate() {
            if self.method_mask & (1_u8 << index) == 0 {
                continue;
            }
            if !allowed.is_empty() {
                allowed.push_str(", ");
            }
            allowed.push_str(name);
        }
        allowed
    }

    #[cfg(test)]
    pub(crate) fn remove(&mut self, method: &Method) {
        if let Some(slot) = method_slot(method) {
            self.routes[slot] = RouteId::NONE;
            self.method_mask &= !(1_u8 << slot);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.method_mask == 0
    }
}

const METHOD_NAMES: [&str; 7] = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

fn method_slot(method: &Method) -> Option<usize> {
    match method.as_str() {
        "DELETE" => Some(0),
        "GET" => Some(1),
        "HEAD" => Some(2),
        "OPTIONS" => Some(3),
        "PATCH" => Some(4),
        "POST" => Some(5),
        "PUT" => Some(6),
        _ => None,
    }
}

pub(crate) struct DynamicPathMatch<'a> {
    pub(crate) routes: &'a RouteSet,
    pub(crate) captures: CaptureSet,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CaptureSet {
    pub(crate) packed: [u64; MAX_CAPTURE_PARAMS],
    pub(crate) count: u8,
}

impl CaptureSet {
    pub(crate) fn range(&self, index: usize) -> Option<CaptureRange> {
        (index < self.count as usize).then(|| {
            let packed = self.packed[index];
            CaptureRange {
                start: (packed >> 32) as usize,
                end: packed as u32 as usize,
            }
        })
    }

    pub(crate) fn with_capture(mut self, index: usize, range: CaptureRange) -> Self {
        debug_assert!(range.start <= u32::MAX as usize);
        debug_assert!(range.end <= u32::MAX as usize);
        self.packed[index] = ((range.start as u32 as u64) << 32) | range.end as u32 as u64;
        self.count = (index + 1) as u8;
        self
    }
}

#[derive(Debug)]
pub(crate) enum RouteFailure {
    Options(String),
    MethodNotAllowed(String),
}

pub(crate) fn route_index(routes: &RouteSet, method: &Method) -> Option<RouteId> {
    routes.route(method).or_else(|| {
        (method == Method::HEAD)
            .then(|| routes.route(&Method::GET))
            .flatten()
    })
}

pub(crate) fn resolve_route_set(
    method: &Method,
    routes: &RouteSet,
) -> Result<RouteId, RouteFailure> {
    let index = route_index(routes, method);
    if index.is_none() && *method == Method::OPTIONS {
        return Err(RouteFailure::Options(routes.allowed_methods()));
    }
    let Some(index) = index else {
        return Err(RouteFailure::MethodNotAllowed(routes.allowed_methods()));
    };
    Ok(index)
}

impl Default for DynamicRouteTrie {
    fn default() -> Self {
        Self {
            nodes: vec![DynamicRouteNode::default()],
        }
    }
}

impl DynamicRouteTrie {
    #[cfg(test)]
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn insert(&mut self, segments: &[Segment], method: Method, route: usize) {
        let mut node_index = 0;
        for segment in segments {
            let next = match segment {
                Segment::Static(value) => {
                    if let Some(index) = self.nodes[node_index].static_children.get(value) {
                        *index
                    } else {
                        let index = NodeId::new(self.nodes.len());
                        self.nodes.push(DynamicRouteNode::default());
                        self.nodes[node_index]
                            .static_children
                            .insert(value.clone(), index);
                        index
                    }
                }
                Segment::Capture(_) => {
                    if let Some(index) = self.nodes[node_index].capture_child {
                        index
                    } else {
                        let index = NodeId::new(self.nodes.len());
                        self.nodes.push(DynamicRouteNode::default());
                        self.nodes[node_index].capture_child = Some(index);
                        index
                    }
                }
            };
            node_index = next.index();
        }
        self.nodes[node_index].routes.insert(method, route);
    }

    pub(crate) fn find(&self, path: &str) -> Option<DynamicPathMatch<'_>> {
        let (node_index, captures) =
            self.find_node(0, PathParts::new(path), CaptureSet::default())?;
        let routes = &self.nodes[node_index].routes;
        Some(DynamicPathMatch { routes, captures })
    }

    pub(crate) fn find_node(
        &self,
        node_index: usize,
        mut parts: PathParts<'_>,
        captures: CaptureSet,
    ) -> Option<(usize, CaptureSet)> {
        let node = &self.nodes[node_index];
        let Some(part) = parts.next() else {
            return (!node.routes.is_empty()).then_some((node_index, captures));
        };

        // Static branches have precedence over captures, but the capture
        // branch remains available if the static branch fails deeper down.
        if let Some(&child) = node.static_children.get(part.value)
            && let Some(found) = self.find_node(child.index(), parts, captures)
        {
            return Some(found);
        }

        if (captures.count as usize) < MAX_CAPTURE_PARAMS
            && valid_percent_encoding(part.value)
            && let Some(child) = node.capture_child
        {
            let captured = captures.with_capture(
                captures.count as usize,
                CaptureRange {
                    start: part.start,
                    end: part.end,
                },
            );
            if let Some(found) = self.find_node(child.index(), parts, captured) {
                return Some(found);
            }
        }
        None
    }
}
