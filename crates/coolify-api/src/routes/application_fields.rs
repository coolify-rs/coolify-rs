//! The request fields `create_application()` and `update_by_uuid()` share: setters of the
//! `Application` model, `applicationSettingsFromRequest()`, `docker_compose_domains`,
//! `checkIfDomainIsAlreadyUsedViaAPI` and the base64 checks.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use coolify_compat::Encrypter;
use coolify_compat::yaml::PhpValue;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::naming::strip_tags;
use coolify_jobs::applications::domains;
use coolify_jobs::applications::labels::{self, LabelInput};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

use crate::input::Input;
use crate::response::{ApiError, PhpJson};
use crate::routes::databases::input_bool;
use crate::serialize::php_truthy;
use crate::state::AppState;

/// A thrown exception rendered by Laravel's handler (`APP_DEBUG=false`).
pub(super) const SERVER_ERROR: ApiError =
    ApiError::Abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");

/// A column list where a later write replaces an earlier one (`$model->col = ...` twice).
pub(super) type Columns = Vec<(&'static str, ColumnValue)>;

pub(super) fn put(columns: &mut Columns, col: &'static str, value: ColumnValue) {
    columns.retain(|(k, _)| *k != col);
    columns.push((col, value));
}

/// `{"message": "Validation failed.", "errors": {field: message}}`.
pub(super) fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), Value::String(message.into()));
    ApiError::Validation(errors)
}

/// PHP string conversion of a request value; an array cannot convert ("Array to string").
/// A validation error with a message string instead of a list (`'errors' => [$field => '...']`).
pub(super) fn field_message(field: &str, message: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), Value::String(message.to_owned()));
    ApiError::Validation(errors)
}

pub(super) fn php_text(v: &Value) -> Result<Option<String>, ApiError> {
    Ok(match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        // `Connection::prepareBindings` binds booleans as integers.
        Value::Bool(b) => Some(i32::from(*b).to_string()),
        Value::Array(_) | Value::Object(_) => return Err(SERVER_ERROR),
    })
}

/// A value bound to a boolean column without a cast on set: PostgreSQL reads its text form.
pub(super) fn pg_bool(v: &Value) -> Result<bool, ApiError> {
    let text = match v {
        Value::Bool(b) => return Ok(*b),
        other => php_text(other)?.unwrap_or_default(),
    };
    let lower = text.trim().to_ascii_lowercase();
    let is_prefix = |word: &str, min: usize| lower.len() >= min && word.starts_with(&lower);
    if lower == "1" || is_prefix("true", 1) || is_prefix("yes", 1) || is_prefix("on", 2) {
        Ok(true)
    } else if lower == "0" || is_prefix("false", 1) || is_prefix("no", 1) || is_prefix("off", 2) {
        Ok(false)
    } else {
        Err(SERVER_ERROR)
    }
}

/// A boolean setting without a rule: the `boolean` cast decides whether it is dirty, and only a
/// dirty value reaches PostgreSQL. `None` leaves the column alone.
pub(super) fn unvalidated_flag(v: &Value, current: bool) -> Result<Option<bool>, ApiError> {
    if php_truthy(v) == current {
        return Ok(None);
    }
    pg_bool(v).map(Some)
}

/// A failed save: the query exception as Laravel's handler renders it.
pub(super) fn save_error(e: coolify_db::DbError) -> ApiError {
    match e {
        coolify_db::DbError::ReadOnly(reason) => ApiError::ReadOnly(reason),
        other => {
            tracing::error!(error = %other, "database error");
            SERVER_ERROR
        }
    }
}

/// `isBase64Encoded()` of a string: the decoded bytes when it is canonical base64.
pub(super) fn base64_bytes(s: &str) -> Option<Vec<u8>> {
    let engine = base64::engine::general_purpose::STANDARD;
    engine.decode(s).ok().filter(|d| engine.encode(d) == s)
}

/// `isBase64Encoded($value)` of a request value: `base64_decode` throws a `TypeError` on an array,
/// and the strict comparison fails for anything but a string.
fn base64_value(v: &Value) -> Result<Option<Vec<u8>>, ApiError> {
    match v {
        Value::Array(_) | Value::Object(_) => Err(SERVER_ERROR),
        Value::String(s) => Ok(base64_bytes(s)),
        _ => Ok(None),
    }
}

/// The `custom_nginx_configuration` checks of create and update. The column stores base64, so
/// the (canonical) request value is what the setter would store again.
pub(super) fn check_custom_nginx(input: &Input) -> Result<(), ApiError> {
    const MESSAGE: &str = "The custom_nginx_configuration should be base64 encoded.";
    let Some(v) = input
        .get("custom_nginx_configuration")
        .filter(|v| !v.is_null())
    else {
        return Ok(());
    };
    let bytes =
        base64_value(v)?.ok_or_else(|| field_error("custom_nginx_configuration", MESSAGE))?;
    if String::from_utf8(bytes).is_err() {
        return Err(field_error("custom_nginx_configuration", MESSAGE));
    }
    Ok(())
}

/// `decodeBase64EncodedLabels()`: peels nested base64 while a layer holds only labels.
pub(super) fn decode_labels(value: &str) -> Option<String> {
    let mut decoded = base64_bytes(value)?;
    let mut labels = decoded.clone();
    while !decoded.is_empty() {
        let Some(next) = std::str::from_utf8(&decoded).ok().and_then(base64_bytes) else {
            break;
        };
        decoded = next;
        if let Ok(text) = std::str::from_utf8(&decoded) {
            let only_labels = text
                .split("\r\n")
                .flat_map(|l| l.split(['\n', '\r']))
                .filter(|l| !l.is_empty())
                .all(|l| l.contains('=') && !l.starts_with('='));
            if only_labels {
                labels.clone_from(&decoded);
            }
        }
    }
    String::from_utf8(labels).ok()
}

/// The `custom_labels` part of `validateDataApplications`: the request value becomes the
/// base64 of the decoded labels.
pub(super) fn normalize_custom_labels(input: &mut Input) -> Result<(), ApiError> {
    const MESSAGE: &str = "The custom_labels should be base64 encoded.";
    let Some(v) = input.get("custom_labels") else {
        return Ok(());
    };
    if base64_value(v)?.is_none() {
        return Err(field_error("custom_labels", MESSAGE));
    }
    let labels = decode_labels(v.as_str().unwrap_or_default())
        .ok_or_else(|| field_error("custom_labels", MESSAGE))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(labels);
    input
        .0
        .insert("custom_labels".into(), Value::String(encoded));
    Ok(())
}

/// `get_port_from_dockerfile()`.
pub(crate) fn port_from_dockerfile(dockerfile: &str) -> Option<i64> {
    for line in dockerfile.split('\n') {
        let line = crate::routes::applications::php_trim(line);
        if line.starts_with("EXPOSE") {
            let rest = crate::routes::applications::php_trim(&line.replace("EXPOSE", ""));
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            return Some(digits.parse().unwrap_or(0));
        }
    }
    None
}

/// The setter of `custom_network_aliases`.
fn network_aliases(v: Option<&str>) -> Option<String> {
    let value = v.filter(|s| !s.is_empty())?;
    let items: Vec<Value> = match serde_json::from_str::<Value>(value) {
        Ok(Value::String(s)) => s.split(',').map(|a| json!(a)).collect(),
        Ok(Value::Array(a)) => a,
        Ok(Value::Object(o)) => o.into_iter().map(|(_, v)| v).collect(),
        Ok(Value::Null) => Vec::new(),
        Ok(other) => vec![other],
        Err(_) => value.split(',').map(|a| json!(a)).collect(),
    };
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        let Some(alias) = item.as_str() else {
            continue;
        };
        let alias = crate::routes::applications::php_trim(alias).replace(' ', "-");
        if alias.is_empty() || alias == "0" || out.iter().any(|o| o.as_str() == Some(&alias)) {
            continue;
        }
        out.push(Value::String(alias));
    }
    (!out.is_empty()).then(|| coolify_compat::php_json::encode(&Value::Array(out)))
}

/// `parseWatchPaths()`, through the `watch_paths` setter.
fn watch_paths(v: &Value) -> Result<Option<String>, ApiError> {
    if !php_truthy(v) {
        return Ok(None);
    }
    let text = php_text(v)?.unwrap_or_default();
    let trim = crate::routes::applications::php_trim;
    let paths: Vec<String> = text
        .split('\n')
        .map(|p| {
            let p = trim(p);
            match p.strip_prefix('!') {
                Some(rest) => format!("!{}", trim(rest).trim_start_matches('/')),
                None => p.trim_start_matches('/').to_owned(),
            }
        })
        .filter(|p| !p.is_empty())
        .collect();
    Ok(Some(trim(&paths.join("\n"))))
}

/// `dockerfileLocation` / `dockerComposeLocation` setters for a non-empty value.
fn location(v: &str) -> String {
    if v == "/" {
        "/".to_owned()
    } else {
        // `Str::start` collapses repeated leading slashes.
        let v = v.strip_suffix('/').unwrap_or(v);
        format!("/{}", v.trim_start_matches('/'))
    }
}

const FILL_TEXT: &[&str] = &[
    "ports_exposes",
    "ports_mappings",
    "redirect",
    "static_image",
    "dockerfile_target_build",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "docker_registry_image_name",
    "docker_registry_image_tag",
    "health_check_type",
    "health_check_command",
    "health_check_path",
    "health_check_port",
    "health_check_host",
    "health_check_method",
    "health_check_scheme",
    "health_check_response_text",
    "limits_memory",
    "limits_memory_swap",
    "limits_memory_reservation",
    "limits_cpus",
    "limits_cpuset",
    "custom_labels",
    "custom_docker_run_options",
    "post_deployment_command",
    "post_deployment_command_container",
    "pre_deployment_command",
    "pre_deployment_command_container",
    "preview_url_template",
    "http_basic_auth_username",
    "http_basic_auth_hash_algorithm",
    "dockerfile",
    "custom_nginx_configuration",
    "build_pack",
];

const FILL_INT: &[&str] = &[
    "health_check_return_code",
    "health_check_interval",
    "health_check_timeout",
    "health_check_retries",
    "health_check_start_period",
    "limits_memory_swappiness",
    "limits_cpu_shares",
    "max_restart_count",
    "http_basic_auth_bcrypt_cost",
    "http_basic_auth_argon2id_memory_cost",
    "http_basic_auth_argon2id_time_cost",
];

pub(super) const WEBHOOK_SECRETS: [&str; 4] = [
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
];

/// The decrypted value of an `encrypted` column.
pub(super) fn decrypted(
    enc: &Encrypter,
    a: &ApplicationRow,
    key: &str,
) -> Result<Option<String>, ApiError> {
    a.str(key)
        .map(|c| enc.decrypt_string(c).map_err(|_| SERVER_ERROR))
        .transpose()
}

/// `$application->fill($request->only($allowedFields))` with the model's setters and what its
/// `saving` hook does to these columns. `setter_pack` is `$this->build_pack` when the location
/// setters run. `current` is the stored row on update: a column that stays null is not dirty, so
/// the hook does not turn it into ''.
pub(super) fn fill_columns(
    input: &Input,
    enc: &Encrypter,
    name: Option<String>,
    setter_pack: Option<&str>,
    current: Option<&ApplicationRow>,
) -> Result<Columns, ApiError> {
    let mut columns: Columns = Vec::new();
    if let Some(name) = name {
        columns.push(("name", ColumnValue::Text(Some(name))));
    }
    if input.has("description") {
        columns.push((
            "description",
            ColumnValue::Text(Some(strip_tags(input.str("description")))),
        ));
    }
    for key in FILL_TEXT {
        if let Some(v) = input.get(key) {
            // The `ports_mappings` setter stores '' as null.
            let text = php_text(v)?.filter(|s| !(*key == "ports_mappings" && s.is_empty()));
            columns.push((key, ColumnValue::Text(text)));
        }
    }
    for key in FILL_INT {
        if let Some(v) = input.get(key) {
            let n = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .or_else(|| v.as_f64().map(|f| f as i64));
            columns.push((key, n.map_or(ColumnValue::Text(None), ColumnValue::Int)));
        }
    }
    for key in ["health_check_enabled", "is_http_basic_auth_enabled"] {
        if let Some(v) = input.get(key) {
            columns.push((
                key,
                if v.is_null() {
                    ColumnValue::Text(None)
                } else {
                    ColumnValue::Bool(input_bool(input, key))
                },
            ));
        }
    }
    // `encrypted` casts: a new ciphertext only when the plain value changes.
    for key in WEBHOOK_SECRETS
        .into_iter()
        .chain(["http_basic_auth_password"])
    {
        let Some(v) = input.get(key) else {
            continue;
        };
        let plain = php_text(v)?;
        if let Some(a) = current
            && decrypted(enc, a, key)? == plain
        {
            continue;
        }
        columns.push((
            key,
            ColumnValue::Text(plain.map(|p| enc.encrypt_string(&p))),
        ));
    }
    if let Some(v) = input.get("watch_paths") {
        columns.push(("watch_paths", ColumnValue::Text(watch_paths(v)?)));
    }
    if input.has("custom_network_aliases") {
        columns.push((
            "custom_network_aliases",
            ColumnValue::Text(network_aliases(input.str("custom_network_aliases"))),
        ));
    }
    let stays_null = |key: &str| {
        current.is_some_and(|a| a.str(key).is_none()) && input.get(key).is_some_and(Value::is_null)
    };
    let trim = crate::routes::applications::php_trim;
    for key in ["git_repository", "git_branch", "git_commit_sha"] {
        if let Some(v) = input.str(key) {
            columns.push((key, ColumnValue::Text(Some(trim(v)))));
        }
    }
    // `saving` stores `str($value)->trim()`, so a null command becomes ''.
    for key in ["install_command", "build_command", "start_command"] {
        if input.has(key) && !stays_null(key) {
            let v = trim(input.str(key).unwrap_or_default());
            columns.push((key, ColumnValue::Text(Some(v))));
        }
    }
    // `baseDirectory` setter: '/'.ltrim($value, '/').
    if input.has("base_directory") {
        let v = input.str("base_directory").unwrap_or_default();
        let v = trim(&format!("/{}", v.trim_start_matches('/')));
        columns.push(("base_directory", ColumnValue::Text(Some(v))));
    }
    // `publishDirectory` setter: '/'.ltrim or null; the `saving` hook then sets the trimmed
    // Stringable, which is truthy even when empty, so null ends up as '/'.
    if input.has("publish_directory") && !stays_null("publish_directory") {
        let v = input
            .get("publish_directory")
            .filter(|v| php_truthy(v))
            .and_then(Value::as_str)
            .map(|v| format!("/{}", trim(v).trim_start_matches('/')))
            .unwrap_or_else(|| "/".into());
        columns.push(("publish_directory", ColumnValue::Text(Some(v))));
    }
    if input.has("dockerfile_location") {
        let v = match input.str("dockerfile_location") {
            Some(v) => Some(location(v)),
            None => (setter_pack == Some("dockerfile")).then(|| "/Dockerfile".to_owned()),
        };
        columns.push(("dockerfile_location", ColumnValue::Text(v)));
    }
    if input.has("docker_compose_location") {
        let v = input
            .str("docker_compose_location")
            .map_or_else(|| "/docker-compose.yaml".to_owned(), location);
        columns.push(("docker_compose_location", ColumnValue::Text(Some(v))));
    }
    Ok(columns)
}

/// `BOOLEAN_APPLICATION_SETTING_FIELDS` and the other `APPLICATION_SETTING_FIELDS`, in order.
const SETTING_FIELDS: &[(&str, bool)] = &[
    ("is_git_submodules_enabled", true),
    ("is_git_lfs_enabled", true),
    ("is_git_shallow_clone_enabled", true),
    ("disable_build_cache", true),
    ("inject_build_args_to_dockerfile", true),
    ("include_source_commit_in_build", true),
    ("is_env_sorting_enabled", true),
    ("redact_all_env_values_in_logs", true),
    ("include_coolify_default_variables", true),
    ("is_pr_deployments_public_enabled", true),
    ("stop_grace_period", false),
    ("rolling_update_drain_seconds", false),
    ("docker_images_to_keep", false),
    ("is_gzip_enabled", true),
    ("is_stripprefix_enabled", true),
    ("is_permanent_redirect_enabled", true),
    ("traefik_certificate_resolver", false),
    ("is_container_label_readonly_enabled", true),
    ("is_raw_compose_deployment_enabled", true),
    ("is_log_drain_enabled", true),
    ("is_gpu_enabled", true),
    ("gpu_driver", false),
    ("gpu_count", false),
    ("gpu_device_ids", false),
    ("gpu_options", false),
    ("is_consistent_container_name_enabled", true),
    ("custom_internal_name", false),
    ("custom_container_name_prefix", false),
];

/// `applicationSettingsFromRequest()`.
pub(super) fn settings_from_request(input: &Input) -> Vec<(&'static str, Value)> {
    let mut settings: Vec<(&'static str, Value)> = Vec::new();
    for (field, boolean) in SETTING_FIELDS {
        let Some(v) = input.get(field) else {
            continue;
        };
        settings.push((
            field,
            if *boolean {
                Value::Bool(input_bool(input, field))
            } else {
                v.clone()
            },
        ));
    }
    if let Some((_, prefix)) = settings
        .iter_mut()
        .find(|(k, _)| *k == "custom_container_name_prefix")
        && !prefix.is_array()
        && !prefix.is_object()
    {
        // `str($prefix)->slug()->value() ?: null`.
        let text = match &*prefix {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(true) => "1".into(),
            _ => String::new(),
        };
        let slug = coolify_jobs::databases::slug(&text);
        *prefix = if slug.is_empty() || slug == "0" {
            Value::Null
        } else {
            Value::String(slug)
        };
    }
    let name_filled = setting(&settings, "custom_internal_name").is_some_and(value_filled);
    if name_filled && setting(&settings, "is_consistent_container_name_enabled").is_none() {
        settings.push(("is_consistent_container_name_enabled", Value::Bool(true)));
    }
    settings
}

pub(super) fn setting<'a>(settings: &'a [(&'static str, Value)], key: &str) -> Option<&'a Value> {
    settings.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

/// Laravel `filled()`.
fn value_filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// The columns of `$application->settings->fill($settings)`.
pub(super) fn settings_columns(settings: &[(&'static str, Value)]) -> Result<Columns, ApiError> {
    let mut columns: Columns = Vec::new();
    for (key, v) in settings {
        let value = match (*key, v) {
            (_, Value::Bool(b)) if SETTING_FIELDS.iter().any(|(k, b)| k == key && *b) => {
                ColumnValue::Bool(*b)
            }
            ("stop_grace_period" | "rolling_update_drain_seconds" | "docker_images_to_keep", v) => {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .map_or(ColumnValue::Text(None), ColumnValue::Int)
            }
            (_, v) => ColumnValue::Text(php_text(v)?),
        };
        put(&mut columns, key, value);
    }
    Ok(columns)
}

/// `containerNamePrefixValidationResponse()` + `ApplicationSetting::isContainerNamePrefixInUse()`.
pub(super) async fn check_container_name_prefix(
    state: &AppState,
    settings: &[(&'static str, Value)],
    server_id: i64,
    ignore_application: Option<i64>,
) -> Result<(), ApiError> {
    let list_error = |message: &str| {
        let mut errors = Map::new();
        errors.insert("custom_container_name_prefix".into(), json!([message]));
        ApiError::Validation(errors)
    };
    let prefix = match setting(settings, "custom_container_name_prefix") {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::String(s)) => s,
        Some(_) => {
            return Err(list_error(
                "The custom container name prefix field must be a string.",
            ));
        }
    };
    if prefix.trim().is_empty() {
        return Ok(());
    }
    let in_use = apps::list_for_server(&state.db, server_id)
        .await?
        .iter()
        .any(|a| {
            Some(a.id) != ignore_application
                && (a.uuid == *prefix
                    || a.setting_str("custom_container_name_prefix") == Some(prefix)
                    || a.setting_str("custom_internal_name") == Some(prefix))
        });
    if in_use {
        return Err(list_error(
            "This container name prefix is already in use by another application.",
        ));
    }
    Ok(())
}

/// `customInternalNameValidationResponse()`.
pub(super) fn check_custom_internal_name(
    settings: &[(&'static str, Value)],
) -> Result<(), ApiError> {
    let filled = setting(settings, "custom_internal_name").is_some_and(value_filled);
    let enabled = setting(settings, "is_consistent_container_name_enabled")
        .is_none_or(|v| v.as_bool().unwrap_or(false));
    if filled && !enabled {
        let mut errors = Map::new();
        errors.insert(
            "custom_internal_name".into(),
            json!([
                "Set is_consistent_container_name_enabled to true to use custom_internal_name. Coolify ignores the custom internal name while consistent container naming is turned off."
            ]),
        );
        return Err(ApiError::Validation(errors));
    }
    Ok(())
}

/// `ManualWebhookSecret`, with the stored value on update.
pub(super) fn webhook_secret_failure(v: &Value, current: Option<&str>) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => (current != Some(s.as_str()) && s.chars().count() < 16)
            .then(|| "The :attribute must be at least 16 characters.".to_owned()),
        _ => Some("The :attribute must be a string.".to_owned()),
    }
}

/// `ApplicationSetting::parseGpuOptions()`: whether the value is a flat mapping.
pub(super) fn gpu_options_valid(options: &str) -> bool {
    let options = crate::routes::applications::php_trim(options);
    if options.is_empty() {
        return true;
    }
    if let Ok(PhpValue::Array(items)) = coolify_compat::yaml_parse::parse(&options, 128) {
        let is_list = items
            .iter()
            .enumerate()
            .all(|(i, (k, _))| matches!(k, coolify_compat::yaml::Key::Int(n) if *n == i as i64));
        if !items.is_empty() && !is_list {
            return items.iter().all(|(_, v)| {
                matches!(
                    v,
                    PhpValue::Bool(_) | PhpValue::Int(_) | PhpValue::Float(_) | PhpValue::Str(_)
                )
            });
        }
    }
    options
        .split("\r\n")
        .flat_map(|l| {
            l.split([
                '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}',
            ])
        })
        .map(crate::routes::applications::php_trim)
        .filter(|p| !p.is_empty())
        .all(|p| match p.split_once('=') {
            Some((k, _)) => !crate::routes::applications::php_trim(k).is_empty(),
            None => false,
        })
}

/// The `docker_compose_domains.*` rules, in rule order: the item (`array:name,domain,redirect`),
/// then each key.
pub(super) fn compose_domain_rules(v: &Value) -> Vec<(String, String)> {
    compose_domain_rules_with(v, false)
}

/// The rules of `update_preview_by_pull_request_id`: `distinct` on the names, no custom message.
pub(super) fn compose_domain_rules_distinct(v: &Value) -> Vec<(String, String)> {
    compose_domain_rules_with(v, true)
}

fn compose_domain_rules_with(v: &Value, distinct: bool) -> Vec<(String, String)> {
    let items: Vec<(String, &Value)> = match v {
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        _ => return Vec::new(),
    };
    let mut errors = Vec::new();
    for (k, item) in &items {
        let ok = match item {
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o
                .keys()
                .all(|k| ["name", "domain", "redirect"].contains(&k.as_str())),
            _ => false,
        };
        if !ok {
            errors.push((
                format!("docker_compose_domains.{k}"),
                if distinct {
                    // `array:name,domain,redirect` with Laravel's own message.
                    format!("The docker_compose_domains.{k} field must be an array.")
                } else {
                    "An item in the docker_compose_domains array has invalid fields. Only name, domain, and redirect fields are supported.".to_owned()
                },
            ));
        }
    }
    for (k, item) in &items {
        let attr = format!("docker_compose_domains.{k}.name");
        match item.get("name") {
            None | Some(Value::Null) => {
                errors.push((attr.clone(), format!("The {attr} field is required.")));
            }
            Some(Value::String(name)) => {
                let duplicate = items.iter().any(|(other, i)| {
                    other != k && i.get("name").and_then(Value::as_str) == Some(name)
                });
                if distinct && duplicate {
                    errors.push((
                        attr.clone(),
                        format!("The {attr} field has a duplicate value."),
                    ));
                }
            }
            Some(other) => {
                errors.push((attr.clone(), format!("The {attr} field must be a string.")));
                if other.as_array().is_some_and(Vec::is_empty) {
                    errors.push((attr.clone(), format!("The {attr} field is required.")));
                }
            }
        }
    }
    for (k, item) in &items {
        let attr = format!("docker_compose_domains.{k}.domain");
        match item.get("domain") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                if s.chars().count() > 2048 {
                    errors.push((
                        attr.clone(),
                        format!("The {attr} field must not be greater than 2048 characters."),
                    ));
                }
                for e in domains::validate_domains(s) {
                    errors.push((attr.clone(), e));
                }
            }
            Some(_) => {
                errors.push((attr.clone(), format!("The {attr} field must be a string.")));
                errors.push((
                    attr.clone(),
                    "The domains field must be a string.".to_owned(),
                ));
            }
        }
    }
    for (k, item) in &items {
        let attr = format!("docker_compose_domains.{k}.redirect");
        match item.get("redirect") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                if !["www", "non-www", "both"].contains(&s.as_str()) {
                    errors.push((attr.clone(), format!("The selected {attr} is invalid.")));
                }
            }
            Some(_) => {
                errors.push((attr.clone(), format!("The {attr} field must be a string.")));
                errors.push((attr.clone(), format!("The selected {attr} is invalid.")));
            }
        }
    }
    errors
}

/// The `noindex_domains.*` rule (`string`).
pub(super) fn noindex_item_rules(v: &Value) -> Vec<(String, String)> {
    let items: Vec<(String, &Value)> = match v {
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter(|(_, v)| !v.is_string())
        .map(|(k, _)| {
            (
                format!("noindex_domains.{k}"),
                format!("The noindex_domains.{k} field must be a string."),
            )
        })
        .collect()
}

/// `domainConflictKey()`.
fn conflict_key(domain: &str) -> String {
    let d = domain.strip_suffix('/').unwrap_or(domain);
    let lower = d.to_ascii_lowercase();
    if lower.starts_with("https://") {
        d[8..].to_owned()
    } else if lower.starts_with("http://") {
        d[7..].to_owned()
    } else {
        d.to_owned()
    }
}

/// `checkIfDomainIsAlreadyUsedViaAPI($domains, $teamId, $uuid)`: the conflicts, in upstream order.
pub(super) async fn domain_conflicts(
    state: &AppState,
    team_id: i64,
    urls: &[String],
    exclude_uuid: Option<&str>,
) -> Result<Vec<Value>, ApiError> {
    let wanted: Vec<String> = urls
        .iter()
        .map(|d| conflict_key(d.strip_suffix('/').unwrap_or(d)))
        .collect();
    let mut conflicts = Vec::new();
    for (uuid, name, fqdn, build_pack, compose) in
        apps::domain_rows_of_team(&state.db, team_id).await?
    {
        if Some(uuid.as_str()) == exclude_uuid {
            continue;
        }
        // `applicationDomainEntries()`.
        let mut entries: Vec<(String, Option<String>)> = fqdn
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .filter(|d| !d.is_empty())
            .map(|d| (d.trim_end_matches('/').to_owned(), None))
            .collect();
        if build_pack.as_deref() == Some("dockercompose")
            && let Some(Value::Object(services)) = compose
                .as_deref()
                .and_then(|c| serde_json::from_str::<Value>(c).ok())
        {
            for (service, config) in &services {
                let text = config
                    .get("domain")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for d in text.split(',').filter(|d| !d.is_empty()) {
                    entries.push((d.trim_end_matches('/').to_owned(), Some(service.clone())));
                }
            }
        }
        for (naked, service) in entries {
            if !wanted.contains(&conflict_key(&naked)) {
                continue;
            }
            let suffix = service
                .as_deref()
                .map_or(String::new(), |s| format!(" (service: {s})"));
            let mut conflict = Map::new();
            conflict.insert("domain".into(), json!(naked));
            conflict.insert("resource_name".into(), json!(name));
            conflict.insert("resource_uuid".into(), json!(uuid));
            conflict.insert("resource_type".into(), json!("application"));
            conflict.insert(
                "message".into(),
                json!(format!(
                    "Domain {naked} is already in use by application '{name}'{suffix}"
                )),
            );
            if let Some(s) = service {
                conflict.insert("service_name".into(), json!(s));
            }
            conflicts.push(Value::Object(conflict));
        }
    }
    for (uuid, fqdn, service) in apps::service_domain_rows_of_team(&state.db, team_id).await? {
        if Some(uuid.as_str()) == exclude_uuid {
            continue;
        }
        for d in fqdn
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .filter(|d| !d.is_empty())
        {
            let naked = d.strip_suffix('/').unwrap_or(d);
            if wanted.contains(&conflict_key(naked)) {
                conflicts.push(json!({
                    "domain": naked,
                    "resource_name": service,
                    "resource_uuid": uuid,
                    "resource_type": "service",
                    "message": format!("Domain {naked} is already in use by service '{service}'"),
                }));
            }
        }
    }
    if let Some(domain) = apps::instance_fqdn(&state.db)
        .await?
        .filter(|d| !d.is_empty())
    {
        let naked = domain.strip_suffix('/').unwrap_or(&domain);
        if wanted.contains(&conflict_key(naked)) {
            conflicts.push(json!({
                "domain": naked,
                "resource_name": "Coolify Instance",
                "resource_uuid": null,
                "resource_type": "instance",
                "message": format!("Domain {naked} is already in use by this Coolify instance"),
            }));
        }
    }
    Ok(conflicts)
}

/// The 409 answer of a domain conflict.
pub(super) fn conflict_response(conflicts: Vec<Value>) -> Response {
    PhpJson(
        StatusCode::CONFLICT,
        json!({
            "message": "Domain conflicts detected. Use force_domain_override=true to proceed.",
            "conflicts": conflicts,
            "warning": "Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior.",
        }),
    )
    .into_response()
}

/// The URL checks of `docker_compose_domains` (the same block in every create type and in
/// update). `duplicate_suffix` is what follows the duplicate message in that copy of the block.
pub(super) async fn check_compose_domain_urls(
    state: &AppState,
    team_id: i64,
    input: &Input,
    duplicate_suffix: &str,
    exclude_uuid: Option<&str>,
) -> Result<Option<Response>, ApiError> {
    let items = compose_items(input);
    let urls: Vec<String> = items
        .iter()
        .filter_map(|i| i.get("domain"))
        .filter_map(Value::as_str)
        .flat_map(|d| domains::domain_list(Some(d)))
        .collect();
    let force = input_bool(input, "force_domain_override");
    let mut errors: Vec<Value> = Vec::new();
    for url in &urls {
        if !domains::is_valid_domain_url(url) {
            errors.push(json!(format!("Invalid URL: {url}")));
        }
    }
    let mut duplicates: Vec<&str> = Vec::new();
    for (i, url) in urls.iter().enumerate() {
        if urls[..i].contains(url) && !duplicates.contains(&url.as_str()) {
            duplicates.push(url);
        }
    }
    if !duplicates.is_empty() && !force {
        errors.push(json!(format!(
            "The current request contains conflicting URLs: {} Use force_domain_override=true to proceed.{duplicate_suffix}",
            duplicates.join(", ")
        )));
    }
    if !errors.is_empty() {
        let mut m = Map::new();
        m.insert("docker_compose_domains".into(), Value::Array(errors));
        return Err(ApiError::Validation(m));
    }
    if !urls.is_empty() && !force {
        let conflicts = domain_conflicts(state, team_id, &urls, exclude_uuid).await?;
        if !conflicts.is_empty() {
            return Ok(Some(conflict_response(conflicts)));
        }
    }
    Ok(None)
}

/// `collect($request->docker_compose_domains)`.
pub(super) fn compose_items(input: &Input) -> Vec<&Value> {
    match input.get("docker_compose_domains") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    }
}

/// One entry of the `$dockerComposeDomainsJson` of create.
pub(super) fn compose_entries_for_create(input: &Input) -> Map<String, Value> {
    let mut entries = Map::new();
    for item in compose_items(input) {
        let mut entry = Map::new();
        entry.insert(
            "domain".into(),
            item.get("domain").cloned().unwrap_or(Value::Null),
        );
        if let Some(r) = redirect_of(item.get("redirect")) {
            entry.insert("redirect".into(), json!(r));
        }
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            entries.insert(name.to_owned(), Value::Object(entry));
        }
    }
    entries
}

pub(super) fn redirect_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
        .filter(|r| ["www", "non-www", "both"].contains(r))
}

/// `normalizeDockerComposeDomainPorts()`: the JSON of `docker_compose_domains` and of
/// `domain_port_overrides`.
pub(super) fn normalize_compose_domains(
    entries: Map<String, Value>,
    existing: &BTreeMap<String, i64>,
) -> (String, Option<String>) {
    let all: Vec<String> = entries
        .values()
        .filter_map(|e| e.get("domain").and_then(Value::as_str))
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect();
    let (fqdn, overrides) = domains::normalize_port_overrides(Some(&all.join(",")), existing);
    let mut out = Map::new();
    for (name, mut entry) in entries {
        let domain = domains::domain_list(entry.get("domain").and_then(Value::as_str))
            .iter()
            .map(|d| domains::without_port(d))
            .collect::<Vec<_>>()
            .join(",");
        if let Some(e) = entry.as_object_mut() {
            e.insert("domain".into(), Value::String(domain));
        }
        out.insert(name, entry);
    }
    (
        coolify_compat::php_json::encode(&Value::Object(out)),
        overrides.map(|o| domains::overrides_json(fqdn.as_deref(), &o)),
    )
}

/// The stored `domain_port_overrides`.
pub(super) fn stored_overrides(a: &ApplicationRow) -> BTreeMap<String, i64> {
    let parsed = match a.raw.get("domain_port_overrides") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    let Some(Value::Object(o)) = parsed else {
        return BTreeMap::new();
    };
    o.iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

/// `HasNoindexDomains::setNoindexDomains()` given the resource's `fqdn` and `docker_compose_domains`.
pub(super) fn noindex_json(
    values: &[Value],
    fqdn: Option<&str>,
    compose_domains: Option<&str>,
) -> String {
    let normalize = |d: &str| domains::without_port(&domains::normalize_url(d));
    let mut current: Vec<String> = domains::domain_list(fqdn);
    if let Some(Value::Object(services)) =
        compose_domains.and_then(|c| serde_json::from_str::<Value>(c).ok())
    {
        for entry in services.values() {
            let text = entry
                .as_str()
                .or_else(|| entry.get("domain").and_then(Value::as_str));
            current.extend(domains::domain_list(text));
        }
    }
    let current: Vec<String> = current.iter().map(|d| normalize(d)).collect();
    let mut out: Vec<Value> = Vec::new();
    for v in values {
        let Some(d) = v.as_str().filter(|d| !d.trim().is_empty()) else {
            continue;
        };
        let d = normalize(d);
        if current.contains(&d) && !out.iter().any(|o| o.as_str() == Some(&d)) {
            out.push(Value::String(d));
        }
    }
    coolify_compat::php_json::encode(&Value::Array(out))
}

/// The values of the `noindex_domains` request field, as `collect()` iterates them.
pub(super) fn noindex_values(input: &Input) -> Vec<Value> {
    match input.get("noindex_domains") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => o.values().cloned().collect(),
        _ => Vec::new(),
    }
}

/// `generateLabelsApplication($application)` joined with newlines, as `custom_labels` stores it.
pub(crate) fn generated_labels(
    a: &ApplicationRow,
    server: &servers::ServerRow,
    enc: &Encrypter,
) -> Result<String, ApiError> {
    let input = LabelInput::new(a, server)
        .with_basic_auth(a, server, enc)
        .map_err(|e| {
            tracing::error!(error = %e, "basic auth password could not be decrypted");
            SERVER_ERROR
        })?;
    Ok(labels::application(&input).join("\n"))
}

/// `Server::isProxyShouldRun()`.
pub(super) fn proxy_should_run(server: &servers::ServerRow) -> bool {
    let settings = server.settings.clone().unwrap_or_default();
    let proxy_type = server
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("NONE");
    proxy_type != "NONE" && !crate::routes::servers::effective_role_is_build(&settings)
}

/// `parseDockerfileInterval()`.
fn dockerfile_interval(something: &str) -> i64 {
    let value: String = something.chars().filter(char::is_ascii_digit).collect();
    let unit: String = something.chars().filter(|c| !c.is_ascii_digit()).collect();
    let n: f64 = value.parse().unwrap_or(0.0);
    (match unit.as_str() {
        "ns" => n / 1_000_000_000.0,
        "us" | "µs" => n / 1_000_000.0,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => n,
    }) as i64
}

/// `Application::parseHealthcheckFromDockerfile($dockerfile)`: the columns it saves.
pub(crate) fn healthcheck_from_dockerfile(a: &ApplicationRow, dockerfile: Option<&str>) -> Columns {
    let trim = crate::routes::applications::php_trim;
    let text = trim(dockerfile.unwrap_or_default());
    let has = text.contains("HEALTHCHECK");
    let mut columns: Columns = Vec::new();
    if !has && a.bool("custom_healthcheck_found") {
        columns.push(("custom_healthcheck_found", ColumnValue::Bool(false)));
        columns.push(("health_check_interval", ColumnValue::Int(5)));
        columns.push(("health_check_timeout", ColumnValue::Int(5)));
        columns.push(("health_check_retries", ColumnValue::Int(10)));
        columns.push(("health_check_start_period", ColumnValue::Int(5)));
        return columns;
    }
    if !has || a.bool("health_check_enabled") {
        return columns;
    }
    let trim_cont = |s: &str| s.trim_matches(['\\', ' ']).to_owned();
    let mut command: Option<String> = None;
    for line in text.split('\n') {
        let line = trim(line);
        if line.starts_with("HEALTHCHECK") {
            command
                .get_or_insert_with(String::new)
                .push_str(&trim_cont(&line));
            continue;
        }
        if let Some(c) = command.as_mut() {
            if line.contains('\\') {
                c.push(' ');
                c.push_str(&trim_cont(&line));
            } else if !c.is_empty() {
                c.push(' ');
                c.push_str(&line);
                break;
            }
        }
    }
    let Some(command) = command.filter(|c| !c.is_empty()) else {
        return columns;
    };
    let capture = |pattern: &str| {
        regex::Regex::new(pattern)
            .ok()
            .and_then(|r| r.captures(&command).map(|c| c[1].to_owned()))
    };
    for (pattern, col) in [
        (r"--interval=([0-9]+[a-zµ]*)", "health_check_interval"),
        (r"--timeout=([0-9]+[a-zµ]*)", "health_check_timeout"),
        (
            r"--start-period=([0-9]+[a-zµ]*)",
            "health_check_start_period",
        ),
    ] {
        if let Some(v) = capture(pattern) {
            columns.push((col, ColumnValue::Int(dockerfile_interval(&v))));
        }
    }
    if let Some(v) = capture(r"--retries=(\d+)") {
        columns.push((
            "health_check_retries",
            ColumnValue::Int(v.parse().unwrap_or(0)),
        ));
    }
    columns.push(("custom_healthcheck_found", ColumnValue::Bool(true)));
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_labels_decode() {
        let once = base64::engine::general_purpose::STANDARD.encode("a=1\nb=2");
        let twice = base64::engine::general_purpose::STANDARD.encode(&once);
        assert_eq!(decode_labels(&once).as_deref(), Some("a=1\nb=2"));
        assert_eq!(decode_labels(&twice).as_deref(), Some("a=1\nb=2"));
        assert_eq!(decode_labels("not base64!"), None);
    }

    #[test]
    fn aliases() {
        assert_eq!(
            network_aliases(Some("a b, c ,a b")).as_deref(),
            Some(r#"["a-b","c"]"#)
        );
        assert_eq!(network_aliases(Some("5")), None);
        assert_eq!(network_aliases(None), None);
    }

    #[test]
    fn gpu_options() {
        assert!(gpu_options_valid("a=1\nb=2"));
        assert!(gpu_options_valid("a: 1"));
        assert!(!gpu_options_valid("just text"));
        assert!(!gpu_options_valid("a: [1]"));
    }

    #[test]
    fn dockerfile_ports() {
        assert_eq!(port_from_dockerfile("EXPOSE 8080"), Some(8080));
        assert_eq!(port_from_dockerfile("RlJPTQ=="), None);
    }
}
