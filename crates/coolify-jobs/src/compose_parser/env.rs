//! Environment variables in Compose files: `convertToKeyValueCollection()`, the `${VAR}`
//! reference helpers (bootstrap/helpers/services.php and shared.php) and the magic `SERVICE_*`
//! variables (`parseServiceEnvironmentVariable()`, `parseCommandFromMagicEnvVariable()`,
//! `generateEnvValue()`, `generateDockerComposeServiceName()`).

use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue};

use super::php::{self, Arr};

/// `convertToKeyValueCollection($environment)` for the merged `environment` + `build.args`
/// collection.
pub fn convert_to_key_value_collection(environment: &[(Key, PhpValue)]) -> Arr {
    let mut out: Arr = Vec::new();
    if php::is_associative(environment) {
        for (key, value) in environment {
            match key {
                Key::Int(_) => {
                    let text = php::to_string(value);
                    match text.split_once('=') {
                        Some((k, v)) => php::put_str(&mut out, k, PhpValue::Str(v.to_owned())),
                        None => {
                            if let PhpValue::Str(s) = value
                                && !s.is_empty()
                            {
                                let existing = php::get_key(&out, &Key::from_php_string(s))
                                    .cloned()
                                    .unwrap_or(PhpValue::Null);
                                php::put_str(&mut out, s, existing);
                            }
                        }
                    }
                }
                Key::Str(_) => php::put(&mut out, key.clone(), value.clone()),
            }
        }
        return out;
    }
    for (_, value) in environment {
        if let PhpValue::Str(s) = value {
            let (k, v) = match s.split_once('=') {
                Some((k, v)) => (k, v),
                None => (s.as_str(), ""),
            };
            if !php::empty_str(k) {
                php::put_str(&mut out, k, PhpValue::Str(v.to_owned()));
            }
        }
    }
    out
}

/// `extractBalancedBraceContent($str, $startPos)`: `(content, start, end)`.
pub fn extract_balanced_brace_content(s: &str, start_pos: usize) -> Option<(String, usize, usize)> {
    let b = s.as_bytes();
    if start_pos >= b.len() {
        return None;
    }
    let open = b[start_pos..].iter().position(|c| *c == b'{')? + start_pos;
    let mut depth = 1;
    let mut pos = open + 1;
    while pos < b.len() && depth > 0 {
        match b[pos] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        pos += 1;
    }
    if depth != 0 {
        return None;
    }
    let content = String::from_utf8_lossy(&b[open + 1..pos - 1]).into_owned();
    Some((content, open, pos - 1))
}

/// `splitOnOperatorOutsideNested($content)`: `(variable, operator, default)`.
pub fn split_on_operator_outside_nested(content: &str) -> Option<(String, String, String)> {
    const OPERATORS: [&str; 4] = [":-", "-", ":?", "?"];
    let b = content.as_bytes();
    let mut depth: i64 = 0;
    for i in 0..b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ if depth == 0 => {
                for op in OPERATORS {
                    if b[i..].starts_with(op.as_bytes()) {
                        return Some((
                            content[..i].to_owned(),
                            op.to_owned(),
                            content[i + op.len()..].to_owned(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// `replaceVariables($variable)`.
pub fn replace_variables(variable: &str) -> String {
    if variable.starts_with("${") {
        if let Some((content, _, _)) = extract_balanced_brace_content(variable, 0) {
            return content;
        }
        let before = php::before(variable, "}");
        return php::replace_first(&php::replace_first(&before, "$", ""), "{", "");
    }
    if variable.starts_with('{') && variable.ends_with('}') {
        return if variable.len() >= 2 {
            variable[1..variable.len() - 1].to_owned()
        } else {
            String::new()
        };
    }
    if variable.starts_with('{') {
        if let Some((content, _, _)) = extract_balanced_brace_content(&format!("${variable}"), 0) {
            return content;
        }
        return php::before(&php::replace_first(variable, "{", ""), "}");
    }
    if variable.starts_with('$') {
        return php::replace_first(variable, "$", "");
    }
    variable.to_owned()
}

/// `extractDockerComposePassthroughKeys($environment)`.
pub fn extract_docker_compose_passthrough_keys(environment: Option<&PhpValue>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (key, value) in php::collect(environment) {
        let found = match key {
            Key::Int(_) => match &value {
                PhpValue::Str(s) if !s.contains('=') => Some(s.clone()),
                _ => None,
            },
            Key::Str(k) => (value == PhpValue::Null).then_some(k),
        };
        if let Some(f) = found.filter(|f| !php::empty_str(f))
            && !out.contains(&f)
        {
            out.push(f);
        }
    }
    out
}

static BRACED_REFERENCE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?<!\$)\$\{([A-Za-z_][A-Za-z0-9_]*)")
        .unwrap_or_else(|_| unreachable!())
});
static BARE_REFERENCE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?<!\$)\$(?!\{)([A-Za-z_][A-Za-z0-9_]*)")
        .unwrap_or_else(|_| unreachable!())
});

/// `extractDockerComposeEnvironmentVariableReferences($value)`.
pub fn extract_docker_compose_environment_variable_references(value: &str) -> Vec<String> {
    if !value.contains('$') {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for r in [&*BRACED_REFERENCE, &*BARE_REFERENCE] {
        for c in r.captures_iter(value).flatten() {
            if let Some(m) = c.get(1)
                && !out.iter().any(|o| o == m.as_str())
            {
                out.push(m.as_str().to_owned());
            }
        }
    }
    out
}

/// `dockerComposeEnvironmentVariableIsRequired($dockerCompose, $key)`.
pub fn docker_compose_environment_variable_is_required(compose: Option<&str>, key: &str) -> bool {
    let Some(compose) = compose.filter(|c| !c.trim().is_empty()) else {
        return false;
    };
    let pattern = format!(r"(?<!\$)\$\{{{}(?::\?|\?)", fancy_regex::escape(key));
    fancy_regex::Regex::new(&pattern).is_ok_and(|r| r.is_match(compose).unwrap_or(false))
}

/// `parseServiceEnvironmentVariable($key)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceVariable {
    pub service_name: String,
    pub port: Option<String>,
    pub has_port: bool,
}

/// `parseServiceEnvironmentVariable($key)`.
pub fn parse_service_environment_variable(key: &str) -> ServiceVariable {
    let last = php::after_last(key, "_");
    let has_port = !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit());
    let name = |rest: &str| {
        if has_port {
            php::before_last(rest, "_").to_lowercase()
        } else {
            rest.to_lowercase()
        }
    };
    let service_name = if key.starts_with("SERVICE_URL_") {
        name(&php::after(key, "SERVICE_URL_"))
    } else if key.starts_with("SERVICE_FQDN_") {
        name(&php::after(key, "SERVICE_FQDN_"))
    } else {
        String::new()
    };
    ServiceVariable {
        service_name,
        port: has_port.then_some(last),
        has_port,
    }
}

/// `parseCommandFromMagicEnvVariable($key)`.
pub fn parse_command_from_magic_env_variable(key: &str) -> String {
    let count = key.matches('_').count();
    let fqdn_or_url = key.starts_with("SERVICE_FQDN") || key.starts_with("SERVICE_URL");
    match count {
        2 => php::before_last(&php::after(key, "SERVICE_"), "_"),
        3 if fqdn_or_url => php::before(&php::after(key, "SERVICE_"), "_"),
        3 => php::before_last(&php::after(key, "SERVICE_"), "_"),
        _ => String::new(),
    }
}

/// `generateEnvValue($command, $resource)`. `jwt_secret` is the value of `SERVICE_PASSWORD_JWT`
/// (the Supabase tokens need it); `now` is the current unix time.
pub fn generate_env_value(command: &str, jwt_secret: Option<&str>, now: i64) -> Option<String> {
    Some(match command {
        "PASSWORD" => php::str_password(32, false),
        "PASSWORD_64" => php::str_password(64, false),
        "PASSWORDWITHSYMBOLS" => php::str_password(32, true),
        "PASSWORDWITHSYMBOLS_64" => php::str_password(64, true),
        "BASE64_64" => php::str_random(64),
        "BASE64_128" => php::str_random(128),
        "BASE64" | "BASE64_32" => php::str_random(32),
        "REALBASE64_64" => php::random_base64(64),
        "REALBASE64_128" => php::random_base64(128),
        "REALBASE64" | "REALBASE64_32" => php::random_base64(32),
        "HEX_32" => php::random_hex(16),
        "HEX_64" => php::random_hex(32),
        "HEX_128" => php::random_hex(64),
        "USER" => php::str_random(16),
        "LOWERCASEUSER" => php::str_random(16).to_lowercase(),
        "SUPABASEANON" => supabase_jwt(jwt_secret?, "anon", now),
        "SUPABASESERVICE" => supabase_jwt(jwt_secret?, "service_role", now),
        _ => return None,
    })
}

/// The HS256 token of `generateEnvValue('SUPABASEANON'|'SUPABASESERVICE')` (lcobucci/jwt):
/// issued by `supabase` at the current minute, valid for 100 years.
fn supabase_jwt(secret: &str, role: &str, now: i64) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let iat = now - now.rem_euclid(60);
    let exp = add_years(iat, 100);
    let header = b64(br#"{"typ":"JWT","alg":"HS256"}"#);
    let payload =
        b64(format!(r#"{{"iss":"supabase","iat":{iat},"exp":{exp},"role":"{role}"}}"#).as_bytes());
    let signing_input = format!("{header}.{payload}");
    // HMAC-SHA256
    let key = secret.as_bytes();
    let mut k = if key.len() > 64 {
        sha2::Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = sha2::Sha256::new()
        .chain_update(&ipad)
        .chain_update(signing_input.as_bytes())
        .finalize();
    let mac = sha2::Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    format!("{signing_input}.{}", b64(&mac))
}

/// `$date->modify('+N year')` on a unix timestamp (UTC).
fn add_years(ts: i64, years: i32) -> i64 {
    use chrono::{Datelike, TimeZone, Utc};
    let Some(dt) = Utc.timestamp_opt(ts, 0).single() else {
        return ts;
    };
    let target = dt
        .with_year(dt.year() + years)
        .or_else(|| (dt + chrono::Duration::days(1)).with_year(dt.year() + years))
        .unwrap_or(dt);
    target.timestamp()
}

/// `generateDockerComposeServiceName($services, $pullRequestId)`.
pub fn generate_docker_compose_service_name(
    services: &[(Key, PhpValue)],
    pull_request_id: i64,
) -> Arr {
    let mut out: Arr = Vec::new();
    for (name, _) in services {
        let name = name.as_string();
        let key = format!(
            "SERVICE_NAME_{}",
            php::upper(&name.replace(['-', '.'], "_"))
        );
        php::put_str(
            &mut out,
            &key,
            PhpValue::Str(super::volumes::add_preview_deployment_suffix(
                &name,
                pull_request_id,
            )),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_variables() {
        assert_eq!(
            parse_service_environment_variable("SERVICE_URL_MY_APP_3000"),
            ServiceVariable {
                service_name: "my_app".into(),
                port: Some("3000".into()),
                has_port: true
            }
        );
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_FQDN_APP_3000"),
            "FQDN"
        );
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_BASE64_64_APP"),
            "BASE64_64"
        );
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_PASSWORD_WEB"),
            "PASSWORD"
        );
        assert_eq!(
            parse_command_from_magic_env_variable("SERVICE_URL_MY_APP_80"),
            ""
        );
    }

    #[test]
    fn references() {
        assert_eq!(
            split_on_operator_outside_nested("A:-${B:-c}"),
            Some(("A".into(), ":-".into(), "${B:-c}".into()))
        );
        assert_eq!(
            extract_docker_compose_environment_variable_references("${A} $B $$C ${D:-x}"),
            vec!["A".to_owned(), "D".to_owned(), "B".to_owned()]
        );
        assert_eq!(replace_variables("${VAR}"), "VAR");
        assert_eq!(replace_variables("$VAR"), "VAR");
        assert!(docker_compose_environment_variable_is_required(
            Some("x: ${REQ:?msg}"),
            "REQ"
        ));
    }
}
