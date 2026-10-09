use crate::*;

/// The small set of type schemas that can be inferred without runtime
/// reflection. Applications can implement this trait for their own scalar
/// path types.
pub trait ApiSchema {
    /// The complete, self-contained schema (nested types inlined).
    fn schema() -> Value;

    /// The schema for use inside an OpenAPI document: a type that has a name
    /// registers its definition with `registry` and returns a `$ref` to it.
    /// The default returns [`schema`](Self::schema), so a hand-written impl
    /// keeps being inlined.
    ///
    /// A wrapper type (`Box`, a map, your own `Page<T>`) must forward to the
    /// inner type's `schema_with`, not `schema()`: calling `schema()` expands
    /// the inner type in place, which duplicates it in the document and never
    /// terminates for a type that contains itself.
    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        let _ = registry;
        Self::schema()
    }
}

/// Collects the named schemas of an OpenAPI document (`components.schemas`).
///
/// Used by `#[derive(ApiSchema)]`; a hand-written [`ApiSchema`] impl can call
/// [`define`](Self::define) from its own `schema_with` to get a `$ref` too.
pub struct SchemaRegistry {
    inline: bool,
    definitions: std::collections::BTreeMap<String, Value>,
    owners: HashMap<String, &'static str>,
    conflicts: Vec<String>,
    /// Types being inlined right now (recursion guard in inline mode).
    stack: Vec<&'static str>,
}

impl SchemaRegistry {
    pub(crate) fn document() -> Self {
        Self {
            inline: false,
            definitions: Default::default(),
            owners: HashMap::new(),
            conflicts: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// A registry that never produces `$ref`s: every type is expanded in
    /// place. A type that contains itself is cut off with a plain object.
    pub fn inline() -> Self {
        Self {
            inline: true,
            ..Self::document()
        }
    }

    /// Returns the schema for type `T` under `name`. In a document registry
    /// it records `build`'s result once and returns a `$ref`; a second,
    /// different type with the same name is reported as a conflict. Types are
    /// told apart by `std::any::type_name`, and `name` should consist of
    /// letters, digits, `.`, `-` and `_` (OpenAPI component names).
    pub fn define<T: ?Sized>(
        &mut self,
        name: &str,
        build: impl FnOnce(&mut SchemaRegistry) -> Value,
    ) -> Value {
        let owner = std::any::type_name::<T>();
        if self.inline {
            if self.stack.contains(&owner) {
                return json!({ "type": "object" });
            }
            self.stack.push(owner);
            let schema = build(self);
            self.stack.pop();
            return schema;
        }
        match self.owners.get(name) {
            Some(existing) if *existing != owner => {
                if !self.conflicts.iter().any(|conflict| conflict == name) {
                    self.conflicts.push(name.to_owned());
                }
            }
            Some(_) => {}
            None => {
                self.owners.insert(name.to_owned(), owner);
                // `owners` above already stops a type that contains itself.
                self.definitions.insert(name.to_owned(), Value::Null);
                let schema = build(self);
                self.definitions.insert(name.to_owned(), schema);
            }
        }
        json!({ "$ref": format!("#/components/schemas/{name}") })
    }

    pub(crate) fn definitions(&self) -> &std::collections::BTreeMap<String, Value> {
        &self.definitions
    }

    pub(crate) fn conflicts(&self) -> &[String] {
        &self.conflicts
    }
}

#[doc(hidden)]
pub trait OpenApiQuery: Sized {
    fn parameters() -> Vec<Value>;

    fn parse(query: &str) -> Result<Self, ApiError>
    where
        Self: DeserializeOwned,
    {
        parse_query(query, &Self::parameters())
    }
}

pub trait QueryValue: Sized {
    fn parse_query_value(value: &str) -> Result<Self, ApiError>;
}

impl<T> QueryValue for T
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    fn parse_query_value(value: &str) -> Result<Self, ApiError> {
        value
            .parse::<Self>()
            .map_err(|error| ApiError::bad_request(error.to_string()))
    }
}

#[doc(hidden)]
pub fn parse_query_value<T: QueryValue>(value: &str) -> Result<T, ApiError> {
    T::parse_query_value(value)
}

impl ApiSchema for String {
    fn schema() -> Value {
        json!({ "type": "string" })
    }
}

impl ApiSchema for u32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl ApiSchema for u64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl ApiSchema for i32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl ApiSchema for i64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl ApiSchema for bool {
    fn schema() -> Value {
        json!({ "type": "boolean" })
    }
}

#[cfg(feature = "uuid")]
impl ApiSchema for uuid::Uuid {
    fn schema() -> Value {
        json!({ "type": "string", "format": "uuid" })
    }
}

impl<T: ApiSchema> ApiSchema for Option<T> {
    /// The inner schema: an optional query parameter is just absent.
    fn schema() -> Value {
        T::schema()
    }

    /// Nullable: serde writes `None` as `null` (and reads `null` back).
    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        // `anyOf`, not `oneOf`: when `T` itself admits `null` (an untagged
        // enum with a unit variant, `Value`, another `Option`) a `null` would
        // match both alternatives and `oneOf` would reject it.
        json!({ "anyOf": [T::schema_with(registry), { "type": "null" }] })
    }
}

/// The query parameters a flattened struct contributes: the properties of its
/// object schema (following `allOf` for nested flattens), each required when
/// the schema says so and the flattened field is not optional.
#[doc(hidden)]
pub fn query_parameters_from_schema(schema: &Value, optional: bool) -> Vec<Value> {
    let mut parameters = Vec::new();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            parameters.push(json!({
                "in": "query",
                "name": name,
                "required": !optional && required.contains(&name.as_str()),
                "schema": property,
            }));
        }
    }
    if let Some(parts) = schema["allOf"].as_array() {
        for part in parts {
            parameters.extend(query_parameters_from_schema(part, optional));
        }
    }
    // An optional flatten is `anyOf [T, {"type": "object"}]`: the fields of
    // `T` are parameters too, and none of them is required.
    if let Some(alternatives) = schema["anyOf"].as_array() {
        for alternative in alternatives {
            parameters.extend(query_parameters_from_schema(alternative, true));
        }
    }
    parameters
}

/// Drops later parameters whose name an earlier one already used.
#[doc(hidden)]
pub fn dedup_parameters(parameters: &mut Vec<Value>) {
    let mut seen = std::collections::HashSet::new();
    parameters.retain(|parameter| {
        parameter["name"]
            .as_str()
            .is_none_or(|name| seen.insert(name.to_owned()))
    });
}

/// Merges the schema of a `#[serde(flatten)]` field (or an internally tagged
/// newtype payload) into the object `schema` being built. A plain map becomes
/// the object's `additionalProperties`; anything else goes into `all_of`,
/// loosened to "this or any object" when the field is optional.
#[doc(hidden)]
pub fn flatten_schema(
    schema: &mut Map<String, Value>,
    all_of: &mut Vec<Value>,
    flattened: Value,
    optional: bool,
) {
    let pure_map = flattened.as_object().is_some_and(|object| {
        object.contains_key("additionalProperties")
            && ["properties", "$ref", "allOf", "oneOf", "anyOf"]
                .iter()
                .all(|key| !object.contains_key(*key))
    });
    if pure_map {
        if let Some(extra) = flattened.get("additionalProperties") {
            schema.insert("additionalProperties".to_owned(), extra.clone());
        }
        return;
    }
    // `additionalProperties: false` from `deny_unknown_fields` on the
    // flattened struct would forbid the outer struct's own fields.
    let mut flattened = flattened;
    if let Some(object) = flattened.as_object_mut()
        && object.get("additionalProperties") == Some(&Value::Bool(false))
    {
        object.remove("additionalProperties");
    }
    if optional {
        all_of.push(json!({ "anyOf": [flattened, { "type": "object" }] }));
    } else {
        all_of.push(flattened);
    }
}

impl<T: ApiSchema> ApiSchema for Vec<T> {
    fn schema() -> Value {
        json!({ "type": "array", "items": T::schema() })
    }

    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        json!({ "type": "array", "items": T::schema_with(registry) })
    }
}

impl<T: ApiSchema + ?Sized> ApiSchema for &T {
    fn schema() -> Value {
        T::schema()
    }

    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        T::schema_with(registry)
    }
}

macro_rules! integer_schema {
    ($($ty:ty => $format:literal),* $(,)?) => {$(
        impl ApiSchema for $ty {
            fn schema() -> Value {
                json!({ "type": "integer", "format": $format })
            }
        }
    )*};
}

integer_schema! {
    i8 => "int32", i16 => "int32", u8 => "int32", u16 => "int32",
    isize => "int64", usize => "int64",
}

impl ApiSchema for f32 {
    fn schema() -> Value {
        json!({ "type": "number", "format": "float" })
    }
}

impl ApiSchema for f64 {
    fn schema() -> Value {
        json!({ "type": "number", "format": "double" })
    }
}

impl<T: ApiSchema + ?Sized> ApiSchema for Box<T> {
    fn schema() -> Value {
        T::schema()
    }

    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        T::schema_with(registry)
    }
}

impl<V: ApiSchema> ApiSchema for std::collections::HashMap<String, V> {
    fn schema() -> Value {
        json!({ "type": "object", "additionalProperties": V::schema() })
    }

    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        json!({ "type": "object", "additionalProperties": V::schema_with(registry) })
    }
}

impl<V: ApiSchema> ApiSchema for std::collections::BTreeMap<String, V> {
    fn schema() -> Value {
        json!({ "type": "object", "additionalProperties": V::schema() })
    }

    fn schema_with(registry: &mut SchemaRegistry) -> Value {
        json!({ "type": "object", "additionalProperties": V::schema_with(registry) })
    }
}

impl ApiSchema for Value {
    fn schema() -> Value {
        json!({})
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenApiRequest {
    pub(crate) path_schemas: Vec<Value>,
    pub(crate) parameters: Vec<Value>,
    pub(crate) request_body: Option<Value>,
}

impl OpenApiRequest {
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.path_schemas.append(&mut other.path_schemas);
        self.parameters.append(&mut other.parameters);
        if self.request_body.is_none() {
            self.request_body = other.request_body;
        }
    }
}
