//! `serializeApiResponse` (`bootstrap/helpers/api.php:45-102`).

use serde_json::{Map, Value};

/// PHP truthiness for JSON-representable values.
pub fn php_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Reorders one record: keys sorted, `created_at`/`updated_at` last (when
/// truthy), then `name`, `description`, `uuid` (when truthy) and `id` (when
/// not null) moved to the front, giving `id, uuid, description, name, …`.
pub fn serialize_record(record: Map<String, Value>) -> Map<String, Value> {
    let mut entries: Vec<(String, Value)> = record.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut m: Map<String, Value> = entries.into_iter().collect();
    for key in ["created_at", "updated_at"] {
        if m.get(key).is_some_and(php_truthy)
            && let Some(v) = m.shift_remove(key)
        {
            m.insert(key.to_string(), v);
        }
    }
    let mut front: Vec<(String, Value)> = Vec::new();
    for key in ["id", "uuid", "description", "name"] {
        let wanted = match m.get(key) {
            Some(v) if key == "id" => !v.is_null(),
            Some(v) => php_truthy(v),
            None => false,
        };
        if wanted && let Some(v) = m.shift_remove(key) {
            front.push((key.to_string(), v));
        }
    }
    front.into_iter().chain(m).collect()
}

pub fn serialize_value(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(serialize_record(m)),
        Value::Array(items) => Value::Array(items.into_iter().map(serialize_value_item).collect()),
        other => other,
    }
}

fn serialize_value_item(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(serialize_record(m)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(v: &Value) -> Vec<String> {
        v.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn team_order_observed_on_reference() {
        let team = json!({"id":4,"name":"Acme","description":"Fixture team","personal_team":false,
            "created_at":"x","updated_at":"y","show_boarding":false,"is_mcp_server_enabled":true,
            "is_build_server_fallback_enabled":true,"avatar_path":null,"avatar_storage_type":null,"avatar_s3_storage_id":null});
        assert_eq!(
            keys(&serialize_value(team)),
            vec![
                "id",
                "description",
                "name",
                "avatar_path",
                "avatar_s3_storage_id",
                "avatar_storage_type",
                "is_build_server_fallback_enabled",
                "is_mcp_server_enabled",
                "personal_team",
                "show_boarding",
                "created_at",
                "updated_at"
            ]
        );
    }

    #[test]
    fn falsy_description_stays_in_place() {
        // Observed: [{"id":1,"uuid":"…","name":"Renamed","description":""}]
        let p = json!([{"id":1,"name":"Renamed","description":"","uuid":"u"}]);
        let out = serialize_value(p);
        assert_eq!(keys(&out[0]), vec!["id", "uuid", "name", "description"]);
    }

    #[test]
    fn truthiness() {
        assert!(!php_truthy(&json!("0")));
        assert!(!php_truthy(&json!("")));
        assert!(!php_truthy(&json!(0)));
        assert!(php_truthy(&json!("a")));
        assert!(!php_truthy(&json!([])));
    }
}
