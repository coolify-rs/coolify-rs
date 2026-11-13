//! `App\Services\ImageScan\SecurityAction`: one action that fixes relevant findings of a scanned
//! image (spec 15.5).

use serde_json::{Map, Value, json};

use super::SecurityActionType;

/// One image of a merged service update (`parts`): the owner (morph type and key), the scanned
/// image, and the candidate ref.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionPart {
    pub owner_type: String,
    /// `int|string` as stored.
    pub owner_id: Value,
    pub image: Option<String>,
    pub reference: Option<String>,
}

impl ActionPart {
    pub fn to_value(&self) -> Value {
        json!({
            "owner_type": self.owner_type,
            "owner_id": self.owner_id,
            "image": self.image,
            "ref": self.reference,
        })
    }

    /// `(string) $part['owner_id']`.
    pub fn owner_id_string(&self) -> String {
        value_string(&self.owner_id)
    }
}

/// One action.
///
/// `parts`: an update_image that merges the images of several applications or databases of a
/// service lists each image it updates. A button for it must update every part. Empty for an
/// action of one image, which uses `image` and `reference`.
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityAction {
    pub action_type: SecurityActionType,
    /// The text of spec 15.5.
    pub sentence: String,
    /// The tag (update_image, pull_latest), the new base image (change_base), or the package.
    pub target: Option<String>,
    /// Text for the copy button: `FROM {new}` or `{package} {fixed_in}`.
    pub copy: Option<String>,
    /// The IDs of the findings it fixes.
    pub fixed_ids: Vec<String>,
    /// Coolify can do it with a button (update_image, pull_latest, rebuild). Callers still
    /// authorize.
    pub button: bool,
    /// The scanned image it belongs to.
    pub image: Option<String>,
    /// The candidate image to switch to (update_image, pull_latest).
    pub reference: Option<String>,
    pub parts: Vec<ActionPart>,
}

impl SecurityAction {
    /// `toArray()`, in upstream's key order.
    pub fn to_value(&self) -> Value {
        json!({
            "type": self.action_type.value(),
            "sentence": self.sentence,
            "target": self.target,
            "copy": self.copy,
            "fixed_ids": self.fixed_ids,
            "button": self.button,
            "image": self.image,
            "ref": self.reference,
            "parts": self.parts.iter().map(ActionPart::to_value).collect::<Vec<_>>(),
        })
    }

    /// `SecurityAction::fromArray($data)`: `None` for data that is not an action.
    pub fn from_value(data: &Value) -> Option<Self> {
        let empty = Map::new();
        let data = data.as_object().unwrap_or(&empty);
        let action_type = SecurityActionType::from_value(
            &data.get("type").map(value_string).unwrap_or_default(),
        )?;
        let isset = |key: &str| data.get(key).filter(|v| !v.is_null()).map(value_string);
        let parts = match data.get("parts") {
            Some(Value::Array(parts)) => parts.iter().collect::<Vec<_>>(),
            Some(Value::Object(parts)) => parts.values().collect(),
            _ => Vec::new(),
        };
        Some(Self {
            action_type,
            sentence: data.get("sentence").map(value_string).unwrap_or_default(),
            target: isset("target"),
            copy: isset("copy"),
            fixed_ids: string_list(data.get("fixed_ids")),
            button: php_bool(data.get("button")),
            image: isset("image"),
            reference: isset("ref"),
            parts: parts
                .into_iter()
                .filter(|part| part.is_object())
                .map(|part| ActionPart {
                    owner_type: part.get("owner_type").map(value_string).unwrap_or_default(),
                    owner_id: part.get("owner_id").cloned().unwrap_or(Value::Null),
                    image: part.get("image").and_then(Value::as_str).map(str::to_owned),
                    reference: part.get("ref").and_then(Value::as_str).map(str::to_owned),
                })
                .collect(),
        })
    }
}

/// `(string) $value` of a decoded JSON value.
pub(crate) fn value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".to_owned(),
        _ => String::new(),
    }
}

/// `(bool) $value` of a decoded JSON value.
pub(crate) fn php_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// `(int) $value` of a decoded JSON value.
pub(crate) fn php_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => {
            let digits: String = s
                .trim_start()
                .chars()
                .enumerate()
                .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
                .map(|(_, c)| c)
                .collect();
            digits.parse().unwrap_or(0)
        }
        Some(Value::Bool(true)) => 1,
        _ => 0,
    }
}

/// `array_values(array_map('strval', (array) $value))`.
pub(crate) fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(value_string).collect(),
        Some(Value::Object(items)) => items.values().map(value_string).collect(),
        Some(scalar) => vec![value_string(scalar)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_round_trips_and_bad_data_is_none() {
        let action = SecurityAction {
            action_type: SecurityActionType::UpdateImage,
            sentence: "Update to 1.28.".to_owned(),
            target: Some("1.28".to_owned()),
            copy: None,
            fixed_ids: vec!["CVE-2026-0001".to_owned()],
            button: true,
            image: Some("nginx:1.27".to_owned()),
            reference: Some("nginx:1.28".to_owned()),
            parts: vec![ActionPart {
                owner_type: r"App\Models\ServiceApplication".to_owned(),
                owner_id: json!(4),
                image: Some("nginx:1.27".to_owned()),
                reference: Some("nginx:1.28".to_owned()),
            }],
        };
        let value = action.to_value();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec![
                "type",
                "sentence",
                "target",
                "copy",
                "fixed_ids",
                "button",
                "image",
                "ref",
                "parts"
            ]
        );
        assert_eq!(SecurityAction::from_value(&value), Some(action));
        assert_eq!(SecurityAction::from_value(&json!({"type": "nope"})), None);
        assert_eq!(SecurityAction::from_value(&json!("x")), None);
        let loose = SecurityAction::from_value(
            &json!({"type": "rebuild", "fixed_ids": [1, "a"], "button": 1, "parts": [5]}),
        )
        .unwrap();
        assert_eq!(loose.fixed_ids, vec!["1", "a"]);
        assert!(loose.button);
        assert!(loose.parts.is_empty());
        assert_eq!(loose.target, None);
    }
}
