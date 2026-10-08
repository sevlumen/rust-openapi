use crate::*;

/// The serde fallback for query structs the direct parser cannot handle. Each
/// value is parsed into its own field's type (`serde_urlencoded`), so a
/// `String` field keeps `123456` or `true` as text. A `+` is a literal plus in
/// a query string here (not a space), and invalid percent-encoding is a `400`.
pub(crate) fn parse_query<T: DeserializeOwned>(query: &str) -> Result<T, ApiError> {
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        percent_decode(key)?;
        percent_decode(value)?;
    }
    match serde_urlencoded::from_str(&query.replace('+', "%2B")) {
        Ok(value) => Ok(value),
        Err(error) => {
            // serde's `flatten` buffers values as text and cannot turn them
            // into numbers, so a flattened struct needs the second attempt
            // below, where numeric-looking values are real JSON numbers.
            parse_query_coercing(query).map_err(|_| ApiError::bad_request(error.to_string()))
        }
    }
}

/// Second attempt: numbers and booleans become JSON numbers and booleans.
fn parse_query_coercing<T: DeserializeOwned>(query: &str) -> Result<T, ApiError> {
    let mut object = Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        let json_value = match value.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            text => {
                if let Ok(number) = text.parse::<i64>() {
                    json!(number)
                } else if let Ok(number) = text.parse::<f64>() {
                    json!(number)
                } else {
                    Value::String(value)
                }
            }
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
