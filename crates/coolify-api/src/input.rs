//! Request input, as Laravel controllers see it.
//!
//! * `validateIncomingRequest` (`bootstrap/helpers/api.php:434`): JSON content
//!   type, decodable and truthy JSON, non-empty object.
//! * Global `TrimStrings` and `ConvertEmptyStringsToNull` middleware apply to
//!   JSON and query input.
//! * `$request->all()` = JSON body + query parameters (body wins).

use axum::http::{HeaderMap, Uri, header};
use serde_json::{Map, Value};

use crate::response::ApiError;

/// Normalized request input in body-then-query key order.
#[derive(Debug, Clone, Default)]
pub struct Input(pub Map<String, Value>);

impl Input {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    /// Laravel `filled`: present and not blank.
    pub fn filled(&self, key: &str) -> bool {
        match self.0.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(_) => true,
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    /// `Some(None)` for explicit null, `Some(Some(s))` for strings, `None` if absent.
    /// Non-string values never reach this point (validation rejects them).
    pub fn patch_str(&self, key: &str) -> Option<Option<&str>> {
        self.0.get(key).map(Value::as_str)
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("/json") || ct.contains("+json"))
}

/// `validateIncomingRequest` followed by input normalization.
pub fn validated_json(headers: &HeaderMap, uri: &Uri, body: &[u8]) -> Result<Input, ApiError> {
    if !is_json(headers) {
        return Err(ApiError::InvalidRequest(
            "Content-Type must be application/json.",
        ));
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) if crate::serialize::php_truthy(&v) || matches!(v, Value::Object(_)) => v,
        _ => return Err(ApiError::InvalidRequest("Invalid JSON.")),
    };
    let body_map = to_map(parsed);
    if body_map.is_empty() {
        return Err(ApiError::InvalidRequest("Empty JSON."));
    }
    Ok(normalize(body_map, uri))
}

/// Input without `validateIncomingRequest` (e.g. `PATCH /team`).
pub fn lenient_json(headers: &HeaderMap, uri: &Uri, body: &[u8]) -> Input {
    let body_map = if is_json(headers) {
        serde_json::from_slice::<Value>(body)
            .map(to_map)
            .unwrap_or_default()
    } else {
        Map::new()
    };
    normalize(body_map, uri)
}

fn to_map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Value::Null => Map::new(),
        // `(array) json_decode(...)` of a scalar.
        scalar => [("0".to_owned(), scalar)].into_iter().collect(),
    }
}

fn normalize(mut body: Map<String, Value>, uri: &Uri) -> Input {
    for (k, v) in parse_query(uri.query().unwrap_or("")) {
        body.entry(k).or_insert(Value::String(v));
    }
    Input(body.into_iter().map(|(k, v)| (k, clean(v))).collect())
}

/// TrimStrings + ConvertEmptyStringsToNull, recursively.
fn clean(v: Value) -> Value {
    match v {
        Value::String(s) => {
            let t = trim_laravel(&s);
            if t.is_empty() {
                Value::Null
            } else {
                Value::String(t.to_string())
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(clean).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, clean(v))).collect()),
        other => other,
    }
}

/// Laravel's TrimStrings: Unicode whitespace plus BOM / zero-width chars.
fn trim_laravel(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{FEFF}' | '\u{200B}' | '\u{200E}'))
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                    16,
                ) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query flag parsing for `?force=true`-style parameters.
pub fn query_param(uri: &Uri, key: &str) -> Option<String> {
    parse_query(uri.query().unwrap_or(""))
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn json_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        h
    }

    fn uri(s: &str) -> Uri {
        s.parse().unwrap_or_default()
    }

    #[test]
    fn mirrors_validate_incoming_request() {
        let u = uri("/api/v1/projects");
        assert!(matches!(
            validated_json(&HeaderMap::new(), &u, b"{\"a\":1}"),
            Err(ApiError::InvalidRequest(
                "Content-Type must be application/json."
            ))
        ));
        assert!(matches!(
            validated_json(&json_headers(), &u, b"{bad"),
            Err(ApiError::InvalidRequest("Invalid JSON."))
        ));
        assert!(matches!(
            validated_json(&json_headers(), &u, b"[]"),
            Err(ApiError::InvalidRequest("Invalid JSON."))
        ));
        assert!(matches!(
            validated_json(&json_headers(), &u, b"{}"),
            Err(ApiError::InvalidRequest("Empty JSON."))
        ));
        assert!(validated_json(&json_headers(), &u, b"{\"name\":\"x\"}").is_ok());
    }

    #[test]
    fn trims_and_nulls_and_merges_query() {
        let i = validated_json(
            &json_headers(),
            &uri("/x?foo=bar&name=ignored"),
            br#"{"name":"  ab  ","description":"   ","n":[" a "]}"#,
        )
        .unwrap_or_default();
        assert_eq!(i.str("name"), Some("ab"));
        assert_eq!(i.get("description"), Some(&Value::Null));
        assert_eq!(i.get("n"), Some(&serde_json::json!(["a"])));
        assert_eq!(i.str("foo"), Some("bar"));
        let keys: Vec<&String> = i.keys().collect();
        assert_eq!(keys, vec!["name", "description", "n", "foo"]);
    }

    #[test]
    fn decodes_query() {
        assert_eq!(
            query_param(&uri("/x?a=b%20c+d&e"), "a").as_deref(),
            Some("b c d")
        );
        assert_eq!(query_param(&uri("/x?e"), "e").as_deref(), Some(""));
    }
}
