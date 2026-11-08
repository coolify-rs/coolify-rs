//! Compose variable helpers of Coolify v4.6.0: `dockerComposeVariablesWithStoredValueBeforeExactEscaping()`
//! and `dockerComposeBareReferenceDefault()`.

use coolify_compat::yaml::{Key, PhpValue};

use super::{env as cenv, php, validate};

/// `(?<!\$)\$(?:KEY(?![A-Za-z0-9_])|\{KEY\})`: a reference of `key` without a default.
fn has_bare_reference(compose: &str, key: &str) -> bool {
    let b = compose.as_bytes();
    compose.match_indices('$').any(|(i, _)| {
        if i > 0 && b[i - 1] == b'$' {
            return false;
        }
        let rest = &compose[i + 1..];
        if let Some(after) = rest.strip_prefix(key)
            && !after
                .bytes()
                .next()
                .is_some_and(|c| c == b'_' || c.is_ascii_alphanumeric())
        {
            return true;
        }
        rest.strip_prefix('{')
            .and_then(|r| r.strip_prefix(key))
            .is_some_and(|r| r.starts_with('}'))
    })
}

/// `dockerComposeBareReferenceDefault($dockerCompose, $key)`: the single default of a variable
/// that the Compose file also references without a default, otherwise `None`. A `?` operator
/// (required) means no default.
pub fn docker_compose_bare_reference_default(compose: &str, key: &str) -> Option<String> {
    if key.is_empty() || !has_bare_reference(compose, key) {
        return None;
    }
    let needle = format!("${{{key}");
    let mut defaults: Vec<String> = Vec::new();
    let mut offset = 0;
    while let Some(found) = compose[offset..].find(&needle) {
        let start = offset + found;
        offset = start + 2;
        if start > 0 && compose.as_bytes()[start - 1] == b'$' {
            continue;
        }
        let Some((content, _, _)) = cenv::extract_balanced_brace_content(&compose[start..], 0)
        else {
            continue;
        };
        let Some((variable, operator, default)) = cenv::split_on_operator_outside_nested(&content)
        else {
            continue;
        };
        if variable != key {
            continue;
        }
        if operator.contains('?') {
            return None;
        }
        if !defaults.contains(&default) {
            defaults.push(default);
        }
    }
    if defaults.len() == 1 {
        defaults.pop()
    } else {
        None
    }
}

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
    fn bare_references_get_the_single_compose_default() {
        let umami = "services:\n  db:\n    environment:\n      - POSTGRES_DB=${POSTGRES_DB:-umami}\n  app:\n    environment:\n      - DATABASE_URL=postgres://db/$POSTGRES_DB\n";
        assert_eq!(
            docker_compose_bare_reference_default(umami, "POSTGRES_DB").as_deref(),
            Some("umami")
        );
        // No bare reference: Compose applies the default itself.
        assert_eq!(
            docker_compose_bare_reference_default("x: ${A:-1}", "A"),
            None
        );
        // `${A}` counts as bare; `$AB` and `$$A` do not.
        assert_eq!(
            docker_compose_bare_reference_default("x: ${A:-1} y: ${A}", "A").as_deref(),
            Some("1")
        );
        assert_eq!(
            docker_compose_bare_reference_default("x: ${A:-1} y: $AB", "A"),
            None
        );
        assert_eq!(
            docker_compose_bare_reference_default("x: ${A:-1} y: $$A", "A"),
            None
        );
        // Two different defaults, or a required reference: none.
        assert_eq!(
            docker_compose_bare_reference_default("${A:-1} ${A:-2} $A", "A"),
            None
        );
        assert_eq!(
            docker_compose_bare_reference_default("${A:-1} ${A:?missing} $A", "A"),
            None
        );
        // The same default twice is one default.
        assert_eq!(
            docker_compose_bare_reference_default("${A:-1} ${A-1} $A", "A").as_deref(),
            Some("1")
        );
    }

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
