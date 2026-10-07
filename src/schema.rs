use crate::*;

/// The small set of type schemas that can be inferred without runtime
/// reflection. Applications can implement this trait for their own scalar
/// path types.
pub trait ApiSchema {
    fn schema() -> Value;
}

#[doc(hidden)]
pub trait OpenApiQuery: Sized {
    fn parameters() -> Vec<Value>;

    fn parse(query: &str) -> Result<Self, ApiError>
    where
        Self: DeserializeOwned,
    {
        parse_query(query)
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
    fn schema() -> Value {
        T::schema()
    }
}

impl<T: ApiSchema> ApiSchema for Vec<T> {
    fn schema() -> Value {
        json!({ "type": "array", "items": T::schema() })
    }
}

impl<T: ApiSchema + ?Sized> ApiSchema for &T {
    fn schema() -> Value {
        T::schema()
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
}

impl<V: ApiSchema> ApiSchema for std::collections::HashMap<String, V> {
    fn schema() -> Value {
        json!({ "type": "object", "additionalProperties": V::schema() })
    }
}

impl<V: ApiSchema> ApiSchema for std::collections::BTreeMap<String, V> {
    fn schema() -> Value {
        json!({ "type": "object", "additionalProperties": V::schema() })
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
