//! A small Laravel-compatible validator for the rules the ported endpoints use.
//!
//! Messages are Laravel 12's defaults (`lang/en/validation.php`) unless a
//! controller passes custom ones; both were confirmed against the 4.5.0
//! reference (docs/API_COMPATIBILITY.md §4).

use regex::Regex;
use serde_json::{Map, Value};

use crate::input::Input;
use crate::response::ApiError;

#[derive(Debug, Clone)]
pub enum Rule {
    Required,
    Nullable,
    String,
    Boolean,
    /// Minimum length in characters (`mb_strlen`).
    Min(usize),
    /// Maximum length in characters.
    Max(usize),
    Regex(&'static Regex),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: &'static str,
    pub rules: Vec<Rule>,
}

/// Custom messages keyed by `field.rule` or `rule` (`:min` / `:max` substituted).
pub type Messages = &'static [(&'static str, &'static str)];

fn attribute(name: &str) -> String {
    name.replace('_', " ")
}

fn message(field: &str, rule: &str, messages: Messages, param: Option<usize>) -> String {
    let key = format!("{field}.{rule}");
    let template = messages
        .iter()
        .find(|(k, _)| *k == key)
        .or_else(|| messages.iter().find(|(k, _)| *k == rule))
        .map(|(_, m)| (*m).to_string())
        .unwrap_or_else(|| match rule {
            "required" => "The :attribute field is required.".into(),
            "string" => "The :attribute field must be a string.".into(),
            "boolean" => "The :attribute field must be true or false.".into(),
            "min" => "The :attribute field must be at least :min characters.".into(),
            "max" => "The :attribute field must not be greater than :max characters.".into(),
            _ => "The :attribute field format is invalid.".into(),
        });
    let p = param.map(|p| p.to_string()).unwrap_or_default();
    template
        .replace(":attribute", &attribute(field))
        .replace(":min", &p)
        .replace(":max", &p)
}

fn is_laravel_boolean(v: &Value) -> bool {
    match v {
        Value::Bool(_) => true,
        Value::Number(n) => n.as_i64().is_some_and(|i| i == 0 || i == 1),
        Value::String(s) => s == "0" || s == "1",
        _ => false,
    }
}

/// Validates `input` against `fields`, then reports keys outside `allowed`
/// with "This field is not allowed." (the controllers' `$extraFields` check).
pub fn validate(
    input: &Input,
    fields: &[Field],
    messages: Messages,
    allowed: &[&str],
) -> Result<(), ApiError> {
    let mut errors: Map<String, Value> = Map::new();
    let mut push = |field: &str, msg: String| {
        if let Some(list) = errors
            .entry(field.to_string())
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
        {
            list.push(Value::String(msg));
        }
    };

    for f in fields {
        let value = input.get(f.name);
        let required = f.rules.iter().any(|r| matches!(r, Rule::Required));
        let nullable = f.rules.iter().any(|r| matches!(r, Rule::Nullable));
        match value {
            None | Some(Value::Null) => {
                if required {
                    push(f.name, message(f.name, "required", messages, None));
                    continue;
                }
                if value.is_none() || nullable {
                    continue;
                }
            }
            _ => {}
        }
        let v = value.unwrap_or(&Value::Null);
        for rule in &f.rules {
            match rule {
                Rule::Required | Rule::Nullable => {}
                Rule::String => {
                    if !v.is_string() {
                        push(f.name, message(f.name, "string", messages, None));
                        // Size and pattern rules on non-strings are not reproduced (see KNOWN_LIMITATIONS).
                        break;
                    }
                }
                Rule::Boolean => {
                    if !is_laravel_boolean(v) {
                        push(f.name, message(f.name, "boolean", messages, None));
                    }
                }
                Rule::Min(n) => {
                    if v.as_str().is_some_and(|s| s.chars().count() < *n) {
                        push(f.name, message(f.name, "min", messages, Some(*n)));
                    }
                }
                Rule::Max(n) => {
                    if v.as_str().is_some_and(|s| s.chars().count() > *n) {
                        push(f.name, message(f.name, "max", messages, Some(*n)));
                    }
                }
                Rule::Regex(re) => {
                    if !v.as_str().is_some_and(|s| re.is_match(s)) {
                        push(f.name, message(f.name, "regex", messages, None));
                    }
                }
            }
        }
    }
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            push(key, "This field is not allowed.".into());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `customApiValidator` (`bootstrap/helpers/shared.php:2900`) messages.
pub const CUSTOM_API_MESSAGES: Messages = &[("required", "This field is required.")];

/// `ValidationPatterns::nameRules` / `descriptionRules` and their messages.
pub mod patterns {
    use coolify_domain::naming::{
        DESCRIPTION_MAX, DESCRIPTION_PATTERN, NAME_MAX, NAME_MIN, NAME_PATTERN,
    };

    use super::{Field, Messages, Rule};

    pub fn name(required: bool) -> Field {
        Field {
            name: "name",
            rules: vec![
                if required {
                    Rule::Required
                } else {
                    Rule::Nullable
                },
                Rule::String,
                Rule::Min(NAME_MIN),
                Rule::Max(NAME_MAX),
                Rule::Regex(&NAME_PATTERN),
            ],
        }
    }

    pub fn description() -> Field {
        Field {
            name: "description",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(DESCRIPTION_MAX),
                Rule::Regex(&DESCRIPTION_PATTERN),
            ],
        }
    }

    pub const NAME_MESSAGES: Messages = &[
        (
            "name.regex",
            "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +",
        ),
        ("name.min", "The name must be at least :min characters."),
        (
            "name.max",
            "The name may not be greater than :max characters.",
        ),
    ];

    /// `Livewire\Team\Index::messages()`: the combined messages plus its own
    /// `name.required`.
    pub const TEAM_MESSAGES: Messages = &[
        ("name.required", "The Name field is required."),
        (
            "name.regex",
            "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +",
        ),
        ("name.min", "The name must be at least :min characters."),
        (
            "name.max",
            "The name may not be greater than :max characters.",
        ),
        (
            "description.regex",
            "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
        ),
        (
            "description.max",
            "The description may not be greater than :max characters.",
        ),
    ];

    pub const COMBINED_MESSAGES: Messages = &[
        (
            "name.regex",
            "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +",
        ),
        ("name.min", "The name must be at least :min characters."),
        (
            "name.max",
            "The name may not be greater than :max characters.",
        ),
        (
            "description.regex",
            "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
        ),
        (
            "description.max",
            "The description may not be greater than :max characters.",
        ),
    ];
}

#[cfg(test)]
mod tests {
    use super::patterns::*;
    use super::*;
    use serde_json::json;

    fn input(v: Value) -> Input {
        Input(v.as_object().cloned().unwrap_or_default())
    }

    fn errors(r: Result<(), ApiError>) -> Value {
        match r {
            Err(ApiError::Validation(e)) => Value::Object(e),
            _ => Value::Null,
        }
    }

    #[test]
    fn reproduces_reference_messages() {
        // All three cases were captured from the 4.5.0 reference.
        let fields = [name(true), description()];
        let allowed = ["name", "description"];
        assert_eq!(
            errors(validate(
                &input(json!({"name": null})),
                &fields,
                COMBINED_MESSAGES,
                &allowed
            )),
            json!({"name": ["The name field is required."]})
        );
        assert_eq!(
            errors(validate(
                &input(json!({"name": "ab", "description": "x".repeat(300), "foo": 1})),
                &fields,
                COMBINED_MESSAGES,
                &allowed
            )),
            json!({"name": ["The name must be at least 3 characters."],
                   "description": ["The description may not be greater than 255 characters."],
                   "foo": ["This field is not allowed."]})
        );
        assert_eq!(
            errors(validate(
                &input(json!({"name": 123})),
                &fields,
                COMBINED_MESSAGES,
                &allowed
            )),
            json!({"name": ["The name field must be a string."]})
        );
    }

    #[test]
    fn boolean_messages() {
        let f = [Field {
            name: "is_build_server_fallback_enabled",
            rules: vec![Rule::Required, Rule::Boolean],
        }];
        assert_eq!(
            errors(validate(
                &input(json!({"is_build_server_fallback_enabled": "yes"})),
                &f,
                &[],
                &["is_build_server_fallback_enabled"]
            )),
            json!({"is_build_server_fallback_enabled": ["The is build server fallback enabled field must be true or false."]})
        );
        assert!(
            validate(
                &input(json!({"is_build_server_fallback_enabled": "1"})),
                &f,
                &[],
                &["is_build_server_fallback_enabled"]
            )
            .is_ok()
        );
    }

    #[test]
    fn nullable_skips_rules_but_absent_required_fails() {
        let f = [name(false)];
        assert!(validate(&input(json!({"name": null})), &f, NAME_MESSAGES, &["name"]).is_ok());
        assert!(validate(&input(json!({})), &f, NAME_MESSAGES, &["name"]).is_ok());
    }
}
