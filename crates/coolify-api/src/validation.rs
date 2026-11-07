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
    /// `present`: the key is in the request, whatever its value.
    Present,
    /// `filled`: when the key is in the request, its value is not empty (an implicit rule).
    Filled,
    /// `bail`: the first failure of the field stops its other rules.
    Bail,
    Nullable,
    String,
    Boolean,
    /// Minimum length in characters (`mb_strlen`).
    Min(usize),
    /// Maximum length in characters.
    Max(usize),
    Regex(&'static Regex),
    /// `not_regex:`.
    NotRegex(&'static Regex),
    /// `integer` (`filter_var($v, FILTER_VALIDATE_INT)`).
    Integer,
    /// `between:min,max` on a field with a numeric rule (size = the number).
    Between(i64, i64),
    /// `min:n` / `max:n` on a field with a numeric rule.
    MinNum(i64),
    MaxNum(i64),
    /// `url` (`Str::isUrl` with the default protocols).
    Url,
    /// `in:a,b,c` (string comparison).
    In(&'static [&'static str]),
    /// `numeric` (PHP `is_numeric`).
    Numeric,
    /// `array`.
    Array,
    /// A rule object: returns the failure message (`:attribute` substituted).
    Custom(fn(&Value) -> Option<&'static str>),
    /// A closure rule whose message is built from the value.
    CustomOwned(fn(&Value) -> Option<String>),
    /// A rule object already evaluated against the value (it holds state, such as the stored
    /// value `ManualWebhookSecret` compares with); runs like `CustomOwned`.
    Evaluated(Option<String>),
    /// Wildcard rules (`field.*`, `field.*.key`): given the value of the part before the first
    /// dot, the `(attribute, message)` failures in rule order. The field's name is only a label.
    Expand(fn(&Value) -> Vec<(String, String)>),
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

fn message_between(field: &str, messages: Messages, min: i64, max: i64) -> String {
    message(field, "between", messages, None)
        .replace(":min", &min.to_string())
        .replace(":max", &max.to_string())
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
            "present" => "The :attribute field must be present.".into(),
            "filled" => "The :attribute field must have a value.".into(),
            "string" => "The :attribute field must be a string.".into(),
            "boolean" => "The :attribute field must be true or false.".into(),
            "min" => "The :attribute field must be at least :min characters.".into(),
            "max" => "The :attribute field must not be greater than :max characters.".into(),
            "integer" => "The :attribute field must be an integer.".into(),
            "between" => "The :attribute field must be between :min and :max.".into(),
            "in" => "The selected :attribute is invalid.".into(),
            "min.numeric" => "The :attribute field must be at least :min.".into(),
            "max.numeric" => "The :attribute field must not be greater than :max.".into(),
            "url" => "The :attribute field must be a valid URL.".into(),
            "numeric" => "The :attribute field must be a number.".into(),
            "array" => "The :attribute field must be an array.".into(),
            _ => "The :attribute field format is invalid.".into(),
        });
    // `attribute:{field}` entries rename the field (Livewire `$validationAttributes`).
    let attr_key = format!("attribute:{field}");
    let name = messages
        .iter()
        .find(|(k, _)| *k == attr_key)
        .map_or_else(|| attribute(field), |(_, v)| (*v).to_owned());
    let template = template.replace(":attribute", &name);
    match param {
        Some(p) => template
            .replace(":min", &p.to_string())
            .replace(":max", &p.to_string()),
        None => template,
    }
}

/// PHP `filter_var($v, FILTER_VALIDATE_INT)` for JSON values.
pub(crate) fn is_php_int(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => {
            n.is_i64()
                || n.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() < 9.2e18)
        }
        Value::String(s) => {
            let t = s.trim();
            let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
            !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'))
                && digits.parse::<i64>().is_ok()
        }
        _ => false,
    }
}

/// PHP `is_numeric` value of a JSON value, if numeric.
fn php_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty() && !t.starts_with(['x', 'X']) && !t.contains("0x"))
                .then(|| t.parse::<f64>().ok())
                .flatten()
                .filter(|f| f.is_finite())
        }
        _ => None,
    }
}

/// Laravel `getSize` without a numeric rule: element count for arrays,
/// otherwise `mb_strlen` of the PHP string form.
fn php_size(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        other => php_string(other).chars().count(),
    }
}

/// The subject `preg_match` sees, or `None` for non-string non-numeric values.
fn regex_subject(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn php_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn is_laravel_boolean(v: &Value) -> bool {
    match v {
        Value::Bool(_) => true,
        Value::Number(n) => n.as_i64().is_some_and(|i| i == 0 || i == 1),
        Value::String(s) => s == "0" || s == "1",
        _ => false,
    }
}

/// Laravel's `getSize` under a numeric rule: the number, the size of an array, else the string length.
pub(crate) fn numeric_rule_size(v: &Value) -> f64 {
    match v {
        Value::Array(a) => a.len() as f64,
        Value::Object(o) => o.len() as f64,
        _ => php_number(v).unwrap_or_else(|| php_string(v).chars().count() as f64),
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
    let errors: std::cell::RefCell<Map<String, Value>> = std::cell::RefCell::new(Map::new());
    let push = |field: &str, msg: String| {
        if let Some(list) = errors
            .borrow_mut()
            .entry(field.to_string())
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
        {
            list.push(Value::String(msg));
        }
    };

    for f in fields {
        if let [Rule::Expand(expand)] = f.rules.as_slice() {
            let parent = f.name.split('.').next().unwrap_or_default();
            if let Some(v) = input.get(parent) {
                for (attribute, msg) in expand(v) {
                    push(&attribute, msg);
                }
            }
            continue;
        }
        let value = input.get(f.name);
        let nullable = f.rules.iter().any(|r| matches!(r, Rule::Nullable));
        // Missing key, or null on a nullable field: only implicit rules run.
        let implicit_only = match value {
            None => true,
            Some(Value::Null) => nullable,
            _ => false,
        };
        let v = value.unwrap_or(&Value::Null);
        let bail = f.rules.iter().any(|r| matches!(r, Rule::Bail));
        // Laravel evaluates rules in declared order and stops after a failed
        // implicit rule (`required`); other failures do not stop it, unless the field has `bail`.
        for rule in &f.rules {
            if bail && errors.borrow().contains_key(f.name) {
                break;
            }
            if let Rule::Present = rule {
                if value.is_none() {
                    push(f.name, message(f.name, "present", messages, None));
                    break;
                }
                continue;
            }
            if let Rule::Required = rule {
                // validateRequired: null, or an empty array (`count() < 1`).
                let empty_array = v.as_array().is_some_and(Vec::is_empty)
                    || v.as_object().is_some_and(Map::is_empty);
                if v.is_null() || empty_array {
                    push(f.name, message(f.name, "required", messages, None));
                    break;
                }
                continue;
            }
            if let Rule::Filled = rule {
                // validateFilled: a key in the request must pass validateRequired.
                let empty = match v {
                    Value::Null => true,
                    Value::String(s) => s.trim().is_empty(),
                    Value::Array(a) => a.is_empty(),
                    Value::Object(o) => o.is_empty(),
                    _ => false,
                };
                if value.is_some() && empty {
                    push(f.name, message(f.name, "filled", messages, None));
                    break;
                }
                continue;
            }
            if implicit_only {
                continue;
            }
            match rule {
                Rule::Required | Rule::Nullable | Rule::Present | Rule::Filled | Rule::Bail => {}
                Rule::String => {
                    if !v.is_string() {
                        push(f.name, message(f.name, "string", messages, None));
                    }
                }
                Rule::Boolean => {
                    if !is_laravel_boolean(v) {
                        push(f.name, message(f.name, "boolean", messages, None));
                    }
                }
                Rule::Min(n) => {
                    if php_size(v) < *n {
                        push(f.name, message(f.name, "min", messages, Some(*n)));
                    }
                }
                Rule::Max(n) => {
                    if php_size(v) > *n {
                        push(f.name, message(f.name, "max", messages, Some(*n)));
                    }
                }
                // validateRegex / validateNotRegex: values that are neither
                // strings nor numbers fail both.
                Rule::Regex(re) => {
                    if !regex_subject(v).is_some_and(|s| re.is_match(&s)) {
                        push(f.name, message(f.name, "regex", messages, None));
                    }
                }
                Rule::NotRegex(re) => {
                    if regex_subject(v).is_none_or(|s| re.is_match(&s)) {
                        push(f.name, message(f.name, "not_regex", messages, None));
                    }
                }
                Rule::Integer => {
                    if !is_php_int(v) {
                        push(f.name, message(f.name, "integer", messages, None));
                    }
                }
                Rule::Between(min, max) => {
                    let size = numeric_rule_size(v);
                    if size < *min as f64 || size > *max as f64 {
                        push(f.name, message_between(f.name, messages, *min, *max));
                    }
                }
                Rule::MinNum(n) | Rule::MaxNum(n) => {
                    let size = numeric_rule_size(v);
                    let (failed, rule) = match rule {
                        Rule::MinNum(_) => (size < *n as f64, "min.numeric"),
                        _ => (size > *n as f64, "max.numeric"),
                    };
                    if failed {
                        let msg = message(f.name, rule, messages, None)
                            .replace(":min", &n.to_string())
                            .replace(":max", &n.to_string());
                        push(f.name, msg);
                    }
                }
                Rule::Url => {
                    if !v.as_str().is_some_and(crate::laravel_url::is_url) {
                        push(f.name, message(f.name, "url", messages, None));
                    }
                }
                Rule::In(options) => {
                    if !options.contains(&php_string(v).as_str()) {
                        push(f.name, message(f.name, "in", messages, None));
                    }
                }
                Rule::Numeric => {
                    let numeric = match v {
                        Value::Number(_) => true,
                        Value::String(s) => coolify_compat::yaml::php_is_numeric(s),
                        _ => false,
                    };
                    if !numeric {
                        push(f.name, message(f.name, "numeric", messages, None));
                    }
                }
                Rule::Array => {
                    if !(v.is_array() || v.is_object()) {
                        push(f.name, message(f.name, "array", messages, None));
                    }
                }
                Rule::Custom(check) => {
                    if let Some(msg) = check(v) {
                        push(f.name, msg.replace(":attribute", &attribute(f.name)));
                    }
                }
                Rule::CustomOwned(check) => {
                    if let Some(msg) = check(v) {
                        push(f.name, msg.replace(":attribute", &attribute(f.name)));
                    }
                }
                Rule::Evaluated(failure) => {
                    if let Some(msg) = failure {
                        push(f.name, msg.replace(":attribute", &attribute(f.name)));
                    }
                }
                Rule::Expand(_) => {}
            }
        }
    }
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            push(key, "This field is not allowed.".into());
        }
    }
    let errors = errors.into_inner();
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
