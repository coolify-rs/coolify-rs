//! `ServerTransferBundle`: the checks on a bundle before the import writes anything.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::{blank, dg};

/// `ServerTransferBundle::SCHEMA_VERSION`.
pub const SCHEMA_VERSION: i64 = coolify_jobs::server_transfer::SCHEMA_VERSION;

/// `ServerTransferBundle::ID_PATTERN`.
static ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}\z").expect("regex"));
/// `ValidationPatterns::DOCKER_NETWORK_PATTERN`.
static NETWORK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").expect("regex"));
/// `ValidationPatterns::CONTAINER_NAME_PATTERN`.
static CONTAINER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z").expect("regex"));

/// `ServerTransferBundle::MAX_ITEMS`.
const MAX_ITEMS: &[(&str, usize)] = &[
    ("projects", 1000),
    ("environments", 5000),
    ("applications", 5000),
    ("databases", 5000),
    ("services", 2000),
    ("previews", 10000),
    ("environment_variables", 50000),
    ("environment_variables_preview", 50000),
    ("shared_environment_variables", 20000),
    ("persistent_storages", 20000),
    ("file_storages", 20000),
    ("scheduled_tasks", 10000),
    ("scheduled_backups", 10000),
    ("volume_backups", 10000),
    ("tags", 20000),
    ("resource_links", 10000),
    ("destinations", 1000),
    ("private_keys", 1000),
    ("github_apps", 1000),
    ("gitlab_apps", 1000),
    ("gitea_apps", 1000),
    ("s3_storages", 1000),
    ("cloud_provider_tokens", 1000),
    ("integration_tokens", 1000),
    ("dns_zones", 1000),
    ("managed_dns_records", 10000),
    ("ssl_certificates", 10000),
    ("warnings", 1000),
    ("post_transfer_checklist", 1000),
];

fn max_items(key: &str) -> Option<usize> {
    MAX_ITEMS.iter().find(|(k, _)| *k == key).map(|(_, m)| *m)
}

#[derive(Debug)]
pub struct Validation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// `version_compare($a, $b, '>')` for dotted versions; a suffix sorts before the release.
pub fn version_greater(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<i64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+', '_'])
            .map(|p| {
                let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
                if digits.is_empty() || digits.len() != p.len() {
                    -1
                } else {
                    digits.parse().unwrap_or(-1)
                }
            })
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        match (pa.get(i), pb.get(i)) {
            (Some(x), Some(y)) if x != y => return x > y,
            (Some(x), None) => return *x >= 0,
            (None, Some(y)) => return *y < 0,
            _ => {}
        }
    }
    false
}

/// `ServerTransferBundle::validate`.
pub fn validate(bundle: &Map<String, Value>, instance_version: &str) -> Validation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let root = Value::Object(bundle.clone());

    match bundle.get("schema_version").filter(|v| !v.is_null()) {
        None => errors.push("Missing schema_version.".to_owned()),
        Some(v) => {
            if super::as_int(v) != SCHEMA_VERSION {
                errors.push(format!(
                    "Unsupported schema_version {}. Expected {SCHEMA_VERSION}.",
                    super::text(v)
                ));
            }
        }
    }
    match bundle.get("coolify_version").and_then(Value::as_str) {
        None | Some("") => errors.push(format!(
            "Missing coolify_version. Export the server again with Coolify {instance_version} or newer."
        )),
        Some(v) if version_greater(v, instance_version) => errors.push(format!(
            "This bundle was exported from Coolify {v}. Upgrade this instance (Coolify {instance_version}) to {v} or newer before you import it."
        )),
        Some(_) => {}
    }
    if bundle
        .get("export_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push("Missing export_id.".to_owned());
    }
    match bundle.get("server") {
        Some(s) if is_array(s) => {
            for f in ["uuid", "name", "ip", "port", "user"] {
                if s.get(f).is_none() {
                    errors.push(format!("Missing server.{f}."));
                }
            }
        }
        _ => errors.push("Missing server payload.".to_owned()),
    }
    match bundle.get("private_key") {
        Some(k) if is_array(k) => {
            if blank(dg(&root, "private_key.private_key")) {
                errors.push("Missing private_key.private_key material.".to_owned());
            }
        }
        _ => errors.push("Missing private_key payload.".to_owned()),
    }
    match bundle.get("destinations") {
        Some(d) if is_array(d) => {
            if len(d) == 0 {
                warnings.push(
                    "Bundle has no destinations; a default destination will be used.".to_owned(),
                );
            }
        }
        _ => errors.push("Missing destinations array.".to_owned()),
    }
    if !bundle.get("projects").is_some_and(is_array) {
        errors.push("Missing projects array.".to_owned());
    }
    errors.extend(content_errors(&root));
    Validation { errors, warnings }
}

fn is_array(v: &Value) -> bool {
    v.is_array() || v.is_object()
}

fn len(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => 0,
    }
}

/// `array_filter($value, 'is_array')` with the keys kept.
fn items(v: Option<&Value>) -> Vec<(String, &Value)> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .filter(|(_, x)| is_array(x))
            .map(|(i, x)| (i.to_string(), x))
            .collect(),
        Some(Value::Object(o)) => o
            .iter()
            .filter(|(_, x)| is_array(x))
            .map(|(k, x)| (k.clone(), x))
            .collect(),
        _ => Vec::new(),
    }
}

/// `ServerTransferBundle::walk`.
fn walk(node: &Value, path: &str, errors: &mut Vec<String>, counts: &mut BTreeMap<String, usize>) {
    let entries: Vec<(String, bool, &Value)> = match node {
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), false, v))
            .collect(),
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), true, v)).collect(),
        _ => return,
    };
    for (key, is_string_key, value) in entries {
        // PHP turns a numeric object key into an integer key.
        let is_string_key = is_string_key && key.parse::<i64>().is_err();
        let at = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if is_string_key {
            if key == "uuid" || key.ends_with("_uuid") {
                check_id(errors, &at, value);
            } else if key.ends_with("_uuids") {
                match value {
                    Value::Array(a) => {
                        for (i, id) in a.iter().enumerate() {
                            check_id(errors, &format!("{at}.{i}"), id);
                        }
                    }
                    Value::Object(o) => {
                        for (i, id) in o {
                            check_id(errors, &format!("{at}.{i}"), id);
                        }
                    }
                    other => check_id(errors, &format!("{at}.0"), other),
                }
                continue;
            }
            if max_items(&key).is_some() && is_array(value) {
                let n = match value {
                    Value::Array(a) => a.len(),
                    Value::Object(o) => o.values().map(len).sum(),
                    _ => 0,
                };
                *counts.entry(key.clone()).or_default() += n;
            }
        }
        if is_array(value) {
            walk(value, &at, errors, counts);
        }
    }
}

fn check_id(errors: &mut Vec<String>, at: &str, value: &Value) {
    if value.is_null() || value.as_str() == Some("") {
        return;
    }
    if !value.as_str().is_some_and(|s| ID.is_match(s)) {
        errors.push(format!(
            "{at} is not a valid id. An id may only contain letters, digits, hyphens, and underscores (at most 64 characters)."
        ));
    }
}

/// `nullable|string|max:N|regex` with Laravel's messages.
fn check_string(
    errors: &mut Vec<String>,
    at: &str,
    value: Option<&Value>,
    max: Option<usize>,
    pattern: Option<&Regex>,
) {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return;
    };
    let Some(s) = value.as_str() else {
        errors.push(format!("The {at} field must be a string."));
        return;
    };
    if let Some(m) = max
        && s.chars().count() > m
    {
        errors.push(format!(
            "The {at} field must not be greater than {m} characters."
        ));
    }
    if let Some(p) = pattern
        && !p.is_match(s)
    {
        errors.push(format!("The {at} field format is invalid."));
    }
}

fn check_path(errors: &mut Vec<String>, at: &str, value: Option<&Value>) {
    let Some(value) = value.filter(|v| !v.is_null() && v.as_str() != Some("")) else {
        return;
    };
    let Some(s) = value.as_str() else {
        errors.push(format!("{at} must be a string."));
        return;
    };
    if let Err(e) = coolify_jobs::compose_parser::validate::validate_shell_safe_path(s, at) {
        errors.push(e);
    }
}

fn check_cron(errors: &mut Vec<String>, at: &str, value: Option<&Value>) {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return;
    };
    if !value
        .as_str()
        .is_some_and(coolify_compat::cron::validate_cron_expression)
    {
        errors.push(format!("{at} is not a valid cron expression or frequency."));
    }
}

/// `parse_url`, reduced to what `validateApplicationDomains` reads.
fn split_url(url: &str) -> Option<(String, String, Option<i64>)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let port = authority[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse().ok());
        return Some((scheme.to_owned(), authority[..=end].to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Some((scheme.to_owned(), h.to_owned(), Some(p.parse().ok()?))),
        None => Some((scheme.to_owned(), authority.to_owned(), None)),
    }
}

/// `ValidationPatterns::validateApplicationDomains`.
fn check_domains(errors: &mut Vec<String>, at: &str, value: Option<&Value>) {
    let Some(value) = value.filter(|v| !blank(Some(v))) else {
        return;
    };
    let Some(s) = value.as_str() else {
        errors.push(format!("{at}: The domains field must be a string."));
        return;
    };
    for url in s.split(',').map(str::trim).filter(|u| !u.is_empty()) {
        if url.chars().any(|c| "`$;&|<>()\\\r\n".contains(c)) {
            errors.push(format!("{at}: Invalid URL: {url}"));
            continue;
        }
        // `isValidDomainUrl`: an http or https URL that `parse_url` can read.
        let parsed = split_url(url).filter(|(scheme, host, port)| {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
                && !host.is_empty()
                && port.is_none_or(|p| (1..=65535).contains(&p))
        });
        let Some((_, host, _)) = parsed else {
            errors.push(format!("{at}: Invalid URL: {url}"));
            continue;
        };
        let unwrapped = host.trim_matches(['[', ']']);
        if !unwrapped.contains('.') && unwrapped.parse::<std::net::IpAddr>().is_err() {
            errors.push(format!(
                "{at}: Invalid URL: {url}. The hostname must be a fully qualified domain name."
            ));
        }
    }
}

fn check_storages(errors: &mut Vec<String>, at: &str, resource: &Value) {
    for (i, v) in items(resource.get("persistent_storages")) {
        let p = format!("{at}.persistent_storages.{i}");
        check_path(errors, &format!("{p}.mount_path"), v.get("mount_path"));
        check_path(errors, &format!("{p}.host_path"), v.get("host_path"));
    }
    for (i, v) in items(resource.get("file_storages")) {
        let p = format!("{at}.file_storages.{i}");
        check_path(errors, &format!("{p}.fs_path"), v.get("fs_path"));
        check_path(errors, &format!("{p}.mount_path"), v.get("mount_path"));
    }
}

fn check_tasks(errors: &mut Vec<String>, at: &str, resource: &Value) {
    for (i, task) in items(resource.get("scheduled_tasks")) {
        let p = format!("{at}.scheduled_tasks.{i}");
        let cmd_at = format!("{p}.command");
        match task.get("command") {
            None | Some(Value::Null) => errors.push(format!("The {cmd_at} field is required.")),
            Some(Value::String(s)) if s.trim().is_empty() => {
                errors.push(format!("The {cmd_at} field is required."));
            }
            Some(Value::String(_)) => {}
            Some(_) => errors.push(format!("The {cmd_at} field must be a string.")),
        }
        check_string(
            errors,
            &format!("{p}.container"),
            task.get("container"),
            Some(255),
            Some(&CONTAINER),
        );
        let empty = Value::String(String::new());
        check_cron(
            errors,
            &format!("{p}.frequency"),
            Some(task.get("frequency").unwrap_or(&empty)),
        );
    }
}

fn check_backups(errors: &mut Vec<String>, at: &str, database: &Value) {
    for (i, backup) in items(database.get("scheduled_backups")) {
        let p = format!("{at}.scheduled_backups.{i}");
        check_cron(errors, &format!("{p}.frequency"), backup.get("frequency"));
        check_path(
            errors,
            &format!("{p}.databases_to_backup"),
            backup.get("databases_to_backup"),
        );
    }
}

/// A single port or port range with an optional protocol: `\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?`.
static CONTAINER_PORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?\n?\z").expect("regex"));

/// `ServerTransferBundle::checkDatabasePorts`: older databases were saved before the UI checked
/// their ports, so any public port in 1-65535, spaces around mappings, empty entries and
/// container-only ports pass. Values that are not ports are refused: they reach the compose file.
fn check_database_ports(errors: &mut Vec<String>, at: &str, database: &Value) {
    let Some(attributes) = database.get("attributes").and_then(Value::as_object) else {
        return;
    };
    // `check(..., ['nullable', 'integer', 'between:1,65535'])` with the path as the attribute.
    if let Some(port) = attributes.get("public_port").filter(|v| !v.is_null()) {
        let field = format!("{at}.public_port");
        if !crate::validation::is_php_int(port) {
            errors.push(format!("The {field} field must be an integer."));
        }
        let size = crate::validation::numeric_rule_size(port);
        if !(1.0..=65535.0).contains(&size) {
            errors.push(format!("The {field} field must be between 1 and 65535."));
        }
    }
    let Some(mappings) = attributes.get("ports_mappings").filter(|v| !v.is_null()) else {
        return;
    };
    let Some(mappings) = mappings.as_str() else {
        errors.push(format!("{at}.ports_mappings must be a string."));
        return;
    };
    for mapping in mappings.split(',') {
        let mapping = mapping.trim_matches([' ', '\t']);
        if mapping.is_empty()
            || CONTAINER_PORT.is_match(mapping)
            || crate::ui_databases::PORT_MAPPINGS.is_match(mapping)
        {
            continue;
        }
        errors.push(format!(
            "{at}.ports_mappings must be a comma-separated list of port mappings (e.g. 5432:5432,127.0.0.1:8080:80/udp)."
        ));
        return;
    }
}

/// `ServerTransferBundle::contentErrors`.
fn content_errors(bundle: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let mut counts = BTreeMap::new();
    walk(bundle, "", &mut errors, &mut counts);
    for (key, count) in &counts {
        let max = max_items(key).unwrap_or(usize::MAX);
        if *count > max {
            errors.push(format!(
                "The bundle has {count} {key}. A bundle can have at most {max} {key}."
            ));
        }
    }
    // Stop before the field checks: a bundle with a bad id or too many items is not imported.
    if !errors.is_empty() {
        return errors;
    }
    for (i, d) in items(bundle.get("destinations")) {
        check_string(
            &mut errors,
            &format!("destinations.{i}.network"),
            d.get("network"),
            Some(255),
            Some(&NETWORK),
        );
    }
    for key in ["github_apps", "gitlab_apps", "gitea_apps"] {
        for (i, source) in items(bundle.get(key)) {
            for field in ["html_url", "api_url"] {
                let at = format!("{key}.{i}.{field}");
                let Some(value) = source.get(field).filter(|v| !v.is_null()) else {
                    continue;
                };
                let Some(s) = value.as_str() else {
                    errors.push(format!("The {at} field must be a string."));
                    errors.push(format!("The {at} field must be a valid URL."));
                    continue;
                };
                if !crate::laravel_url::is_url(s) {
                    errors.push(format!("The {at} field must be a valid URL."));
                }
                let scheme_ok = split_url(s).is_some_and(|(sc, host, _)| {
                    matches!(sc.as_str(), "http" | "https") && !host.is_empty()
                });
                if !scheme_ok {
                    errors.push(format!("The {at} must use the http or https scheme."));
                }
            }
        }
    }
    for (i, c) in items(bundle.get("ssl_certificates")) {
        let p = format!("ssl_certificates.{i}");
        check_path(
            &mut errors,
            &format!("{p}.configuration_dir"),
            c.get("configuration_dir"),
        );
        check_path(&mut errors, &format!("{p}.mount_path"), c.get("mount_path"));
    }
    for (i, b) in items(bundle.get("volume_backups")) {
        check_cron(
            &mut errors,
            &format!("volume_backups.{i}.frequency"),
            b.get("frequency"),
        );
    }
    for (p, project) in items(bundle.get("projects")) {
        for (e, env) in items(project.get("environments")) {
            let at = format!("projects.{p}.environments.{e}");
            for (a, app) in items(env.get("applications")) {
                let aat = format!("{at}.applications.{a}");
                check_domains(
                    &mut errors,
                    &format!("{aat}.fqdn"),
                    dg(app, "attributes.fqdn"),
                );
                check_storages(&mut errors, &aat, app);
                check_tasks(&mut errors, &aat, app);
                for (r, preview) in items(app.get("previews")) {
                    let pat = format!("{aat}.previews.{r}");
                    check_domains(&mut errors, &format!("{pat}.fqdn"), preview.get("fqdn"));
                    check_storages(&mut errors, &pat, preview);
                }
            }
            for (d, db) in items(env.get("databases")) {
                let dat = format!("{at}.databases.{d}");
                check_database_ports(&mut errors, &dat, db);
                check_storages(&mut errors, &dat, db);
                check_backups(&mut errors, &dat, db);
            }
            for (s, service) in items(env.get("services")) {
                let sat = format!("{at}.services.{s}");
                check_tasks(&mut errors, &sat, service);
                for (a, sa) in items(service.get("applications")) {
                    let aat = format!("{sat}.applications.{a}");
                    let fqdn = dg(sa, "attributes.fqdn").or_else(|| sa.get("fqdn"));
                    check_domains(&mut errors, &format!("{aat}.fqdn"), fqdn);
                    check_storages(&mut errors, &aat, sa);
                }
                for (d, sd) in items(service.get("databases")) {
                    let dat = format!("{sat}.databases.{d}");
                    check_storages(&mut errors, &dat, sd);
                    check_backups(&mut errors, &dat, sd);
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn port_errors(attributes: Value) -> Vec<String> {
        let mut errors = Vec::new();
        check_database_ports(&mut errors, "db", &json!({ "attributes": attributes }));
        errors
    }

    #[test]
    fn database_ports_accept_old_values() {
        assert!(port_errors(json!({ "public_port": 5432, "ports_mappings": " 5432:5432 , ,6379/udp,127.0.0.1:8080:80/udp" })).is_empty());
        assert!(port_errors(json!({ "public_port": null, "ports_mappings": null })).is_empty());
        assert!(port_errors(json!({ "public_port": "65535", "ports_mappings": "" })).is_empty());
        assert!(port_errors(json!({})).is_empty());
    }

    #[test]
    fn database_ports_refuse_values_that_are_not_ports() {
        assert_eq!(
            port_errors(json!({ "public_port": 0 })),
            ["The db.public_port field must be between 1 and 65535."]
        );
        assert_eq!(
            port_errors(json!({ "public_port": "abc" })),
            // `getSize` of a non-numeric string is its length, so `between` passes.
            ["The db.public_port field must be an integer."]
        );
        assert_eq!(
            port_errors(json!({ "ports_mappings": "5432:5432,$(id)" })),
            [
                "db.ports_mappings must be a comma-separated list of port mappings (e.g. 5432:5432,127.0.0.1:8080:80/udp)."
            ]
        );
        assert_eq!(
            port_errors(json!({ "ports_mappings": 5432 })),
            ["db.ports_mappings must be a string."]
        );
    }
}
