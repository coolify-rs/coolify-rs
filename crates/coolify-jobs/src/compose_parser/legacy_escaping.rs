//! `dockerComposeVariablesWithStoredValueBeforeExactEscaping()` (Coolify v4.6.0).

use coolify_compat::yaml::{Key, PhpValue};

use super::{env as cenv, php, validate};

/// `\A\$([A-Za-z_][A-Za-z0-9_]*)\z`: a bare `$NAME` reference.
fn bare_reference(value: &str) -> Option<String> {
    let name = value.strip_prefix('$')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(name.to_owned())
}

/// Variables whose stored value the Compose parser before v4.5.0 wrote straight into the
/// `environment:` list, so their containers got the value without the old .env escaping:
/// references with a default or under another name (`FOO=${FOO:-}`, `BAR=${FOO}`, `BAR=$FOO`) and
/// references in such a default. A plain `FOO=${FOO}` reference kept reading the escaped .env
/// value. `SERVICE_*` variables are left out.
pub fn variables_with_stored_value_before_exact_escaping(compose: Option<&str>) -> Vec<String> {
    let Some(compose) = compose.filter(|c| !c.trim().is_empty()) else {
        return Vec::new();
    };
    // `extractHardcodedEnvironmentVariables()`
    let Ok(yaml) = validate::parse_docker_compose_yaml(compose) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(services)) = php::get(&yaml, "services") else {
        return Vec::new();
    };
    let mut keys: Vec<String> = Vec::new();
    for (_, service) in services {
        let environment = php::collect(php::get(service, "environment"));
        if environment.is_empty() {
            continue;
        }
        for (key, value) in cenv::convert_to_key_value_collection(&environment) {
            let key = match key {
                Key::Str(s) => s,
                Key::Int(i) => i.to_string(),
            };
            let PhpValue::Str(value) = value else {
                continue;
            };
            if !value.starts_with('$') || cenv::replace_variables(&value) == key {
                continue;
            }
            let Some((content, _, _)) = cenv::extract_balanced_brace_content(&value, 0) else {
                if let Some(name) = bare_reference(&value) {
                    keys.push(name);
                }
                continue;
            };
            match cenv::split_on_operator_outside_nested(&content) {
                None => keys.push(content),
                Some((variable, _, default)) => {
                    keys.push(variable);
                    keys.extend(
                        cenv::extract_docker_compose_environment_variable_references(&default),
                    );
                }
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        if !key.starts_with("SERVICE_") && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_that_got_the_stored_value_before_exact_escaping() {
        let compose = r#"
services:
  app:
    image: nginx
    environment:
      - PLAIN=${PLAIN}
      - WITH_DEFAULT=${WITH_DEFAULT:-}
      - RENAMED=${SOURCE}
      - BARE=$BARE_SOURCE
      - NESTED=${OUTER:-${INNER}}
      - SERVICE_URL_APP=${SERVICE_URL_APP_3000:-x}
      - LITERAL=value
"#;
        assert_eq!(
            variables_with_stored_value_before_exact_escaping(Some(compose)),
            ["WITH_DEFAULT", "SOURCE", "BARE_SOURCE", "OUTER", "INNER"]
        );
        assert!(variables_with_stored_value_before_exact_escaping(None).is_empty());
        assert!(variables_with_stored_value_before_exact_escaping(Some("  ")).is_empty());
    }
}
