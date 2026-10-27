//! `json_encode` with PHP's default flags, as used by Laravel `JsonResponse`.
//!
//! Observed on the 4.5.0 reference: `/` is escaped as `\/` and non-ASCII is
//! written as lowercase `\uXXXX` (UTF-16 code units). Reproducing this lets
//! differential tests compare response bodies byte-for-byte.

use std::fmt::Write as _;

use serde_json::Value;

pub fn encode(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// `json_encode($v, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)`, as Laravel's
/// exception handler renders `abort()` responses for JSON clients.
pub fn encode_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(&mut out, value, 0);
    out
}

fn write_pretty(out: &mut String, v: &Value, depth: usize) {
    let pad = |n: usize| "    ".repeat(n);
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_pretty(out, item, depth + 1);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (i, (k, item)) in map.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                out.push_str(&encode(&Value::String(k.clone())).replace("\\/", "/"));
                out.push_str(": ");
                write_pretty(out, item, depth + 1);
                out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(_) => out.push_str("[]"),
        Value::Object(_) => out.push_str("{}"),
        Value::String(_) => out.push_str(&encode(v).replace("\\/", "/")),
        other => out.push_str(&encode(other)),
    }
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7F => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_print_like_laravel_abort() {
        // Bytes from the reference (`abort(403, ...)` with Accept: application/json).
        assert_eq!(
            encode_pretty(&json!({"message": "Missing required permissions: deploy"})),
            "{\n    \"message\": \"Missing required permissions: deploy\"\n}"
        );
        assert_eq!(
            encode_pretty(&json!({"a": [1, {"b": "x/y"}], "c": [], "d": {}})),
            "{\n    \"a\": [\n        1,\n        {\n            \"b\": \"x/y\"\n        }\n    ],\n    \"c\": [],\n    \"d\": {}\n}"
        );
    }

    #[test]
    fn matches_php_default_flags() {
        // Bytes observed from the 4.5.0 reference.
        assert_eq!(
            encode(&json!({"uuid":"x","name":"staging","description":"Stage é/ü"})),
            r#"{"uuid":"x","name":"staging","description":"Stage \u00e9\/\u00fc"}"#
        );
        assert_eq!(
            encode(&json!({"message":"Not found.","docs":"https://coolify.io/docs"})),
            r#"{"message":"Not found.","docs":"https:\/\/coolify.io\/docs"}"#
        );
    }

    #[test]
    fn escapes_controls_and_astral() {
        assert_eq!(encode(&json!("a\"b\\c\n\u{1}")), r#""a\"b\\c\n\u0001""#);
        // PHP writes astral characters as surrogate pairs.
        assert_eq!(encode(&json!("😀")), r#""\ud83d\ude00""#);
        assert_eq!(encode(&json!([1, true, null, -2.5])), "[1,true,null,-2.5]");
    }
}
