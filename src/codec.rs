use crate::*;

/// The serde fallback for query structs the direct parser cannot handle. Each
/// value is parsed into its own field's type (`serde_urlencoded`), so a
/// `String` field keeps `123456` or `true` as text. A `+` is a literal plus in
/// a query string here (not a space), and invalid percent-encoding is a `400`.
///
/// `parameters` are the struct's declared query parameters (names and schema
/// types). They matter for the second attempt: serde's `flatten` buffers
/// values as text and cannot turn them into numbers, so a flattened struct is
/// retried with values converted to JSON numbers/booleans *only where a
/// parameter declares that type*; every other value stays a string.
pub(crate) fn parse_query<T: DeserializeOwned>(
    query: &str,
    parameters: &[Value],
) -> Result<T, ApiError> {
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        percent_decode(key)?;
        percent_decode(value)?;
    }
    match serde_urlencoded::from_str(&query.replace('+', "%2B")) {
        Ok(value) => Ok(value),
        Err(error) => parse_query_typed(query, parameters)
            .map_err(|_| ApiError::bad_request(error.to_string())),
    }
}

/// For query structs with `#[serde(flatten)]`, generated code calls this
/// directly: serde buffers flattened values as text, cannot parse numbers out
/// of them, and an `Option` flatten swallows that failure as `None`, so the
/// text attempt is not safe there and values are typed from the start.
pub fn parse_query_guided<T: DeserializeOwned>(
    query: &str,
    parameters: &[Value],
) -> Result<T, ApiError> {
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        percent_decode(key)?;
        percent_decode(value)?;
    }
    parse_query_typed(query, parameters)
}

/// The `items` schema of an array parameter, looking through the
/// `oneOf`/`anyOf` that a nullable (`Option`) field produces.
fn array_items(schema: &Value) -> Option<&Value> {
    if schema["type"] == "array" {
        return Some(&schema["items"]);
    }
    ["oneOf", "anyOf"]
        .iter()
        .filter_map(|key| schema[*key].as_array())
        .flatten()
        .find_map(array_items)
}

/// A query value as JSON of the declared scalar type (integers, numbers and
/// booleans when they parse; otherwise the text).
fn scalar(kind: Option<&str>, value: String) -> Value {
    match (kind, value.as_str()) {
        (Some("boolean"), "true") => Value::Bool(true),
        (Some("boolean"), "false") => Value::Bool(false),
        (Some("integer"), text) => text
            .parse::<i64>()
            .map(|number| json!(number))
            .or_else(|_| text.parse::<u64>().map(|number| json!(number)))
            .unwrap_or(Value::String(value)),
        (Some("number"), text) => match text.parse::<f64>() {
            Ok(parsed) if parsed.is_finite() => json!(parsed),
            _ => Value::String(value),
        },
        _ => Value::String(value),
    }
}

/// The JSON type a parameter schema declares, looking through the
/// `oneOf`/`anyOf` that a nullable (`Option`) field produces.
fn declared_type(schema: &Value) -> Option<&str> {
    if let Some(kind) = schema["type"].as_str() {
        return Some(kind);
    }
    ["oneOf", "anyOf"]
        .iter()
        .filter_map(|key| schema[*key].as_array())
        .flatten()
        .filter_map(declared_type)
        .find(|kind| *kind != "null")
}

/// Second attempt: values become numbers or booleans where the declared
/// parameter type says so (everything else, and every value of an unknown
/// parameter, stays a string). Without declared parameters (a hand-written
/// `OpenApiQuery`) any numeric-looking value is converted.
fn parse_query_typed<T: DeserializeOwned>(
    query: &str,
    parameters: &[Value],
) -> Result<T, ApiError> {
    let declared = |name: &str| {
        parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .and_then(|parameter| declared_type(&parameter["schema"]))
    };
    let array_items = |name: &str| {
        parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .and_then(|parameter| array_items(&parameter["schema"]))
    };
    let mut object = Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        // A repeated key is an array when the parameter is declared as one.
        if let Some(items) = array_items(&key) {
            let item = scalar(declared_type(items), value);
            match object
                .entry(key)
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                Value::Array(values) => values.push(item),
                other => *other = Value::Array(vec![item]),
            }
            continue;
        }
        let kind = if parameters.is_empty() {
            Some("any")
        } else {
            declared(&key)
        };
        let number = |text: &str| -> Option<Value> {
            text.parse::<i64>()
                .map(|number| json!(number))
                .or_else(|_| text.parse::<u64>().map(|number| json!(number)))
                .ok()
        };
        let json_value = match (kind, value.as_str()) {
            (Some("boolean" | "any"), "true") => Value::Bool(true),
            (Some("boolean" | "any"), "false") => Value::Bool(false),
            (Some("integer" | "any"), text) if number(text).is_some() => {
                number(text).unwrap_or(Value::Null)
            }
            (Some("number" | "any"), text) => match text.parse::<f64>() {
                Ok(parsed) if parsed.is_finite() => number(text).unwrap_or_else(|| json!(parsed)),
                _ => Value::String(value),
            },
            _ => Value::String(value),
        };
        object.insert(key, json_value);
    }
    serde_json::from_value(Value::Object(object))
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

pub(crate) fn percent_decode(value: &str) -> Result<String, ApiError> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        // Copy the run of literal bytes up to the next escape in one go.
        let run = bytes[index..]
            .iter()
            .position(|&byte| byte == b'%')
            .unwrap_or(bytes.len() - index);
        output.extend_from_slice(&bytes[index..index + run]);
        index += run;
        if index == bytes.len() {
            break;
        }
        if index + 2 >= bytes.len() {
            return Err(ApiError::bad_request("invalid percent encoding"));
        }
        let high = hex(bytes[index + 1])
            .ok_or_else(|| ApiError::bad_request("invalid percent encoding"))?;
        let low = hex(bytes[index + 2])
            .ok_or_else(|| ApiError::bad_request("invalid percent encoding"))?;
        output.push(high * 16 + low);
        index += 3;
    }
    String::from_utf8(output)
        .map_err(|_| ApiError::bad_request("invalid UTF-8 in percent encoding"))
}

#[doc(hidden)]
pub fn decode_query_component(value: &str) -> Result<Cow<'_, str>, ApiError> {
    if value.as_bytes().contains(&b'%') {
        Ok(Cow::Owned(percent_decode(value)?))
    } else {
        Ok(Cow::Borrowed(value))
    }
}

pub(crate) fn valid_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || hex(bytes[index + 1]).is_none()
                || hex(bytes[index + 2]).is_none()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

pub(crate) fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
