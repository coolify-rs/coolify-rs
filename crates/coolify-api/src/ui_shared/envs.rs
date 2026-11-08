//! `Project\Shared\EnvironmentVariable\{All, Add, Show, ShowHardcoded}`.

use std::collections::HashSet;
use std::sync::LazyLock;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::applications as apps;
use coolify_db::database_parts as parts;
use coolify_db::records::Record;
use coolify_db::resource_envs::{self as re, ManagedQuery, NewEnv, flag};
use coolify_db::servers::ColumnValue as C;
use coolify_jobs::compose_parser::env as cenv;
use coolify_jobs::compose_parser::{php, service_parser, validate};
use coolify_jobs::databases::env::{Owner, escape_env_variables, resolve_references};
use coolify_jobs::databases::variables as jobvars;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::dev_view::{self, Parsed};
use super::{Class, Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;
type PI = Path<(String, String, i64)>;

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/envs";
    Router::new()
        .route(b, get(list).post(add))
        .route(&format!("{b}/available"), get(available))
        .route(&format!("{b}/dev"), get(dev_view))
        .route(&format!("{b}/bulk"), post(bulk))
        .route(&format!("{b}/settings"), post(settings))
        .route(&format!("{b}/resolve"), post(resolve_hardcoded))
        .route(
            &format!("{b}/{{id}}"),
            get(values).patch(update).delete(destroy),
        )
        .route(&format!("{b}/{{id}}/copy"), get(copy_value))
        .route(&format!("{b}/{{id}}/lock"), post(lock))
        .route(
            &format!("{b}/{{id}}/compose-default"),
            post(compose_default),
        )
}

static KEY_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!()));
static SHARED_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
        .unwrap_or_else(|_| unreachable!())
});
const KEY_MESSAGE: &str = "The key must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.";

fn is_magic(key: &str) -> bool {
    key.starts_with("SERVICE_FQDN")
        || key.starts_with("SERVICE_URL")
        || key.starts_with("SERVICE_NAME")
}

/// `isProtectedEnvironmentVariable`.
fn is_protected(key: &str) -> bool {
    key.starts_with("SERVICE_FQDN_")
        || key.starts_with("SERVICE_URL_")
        || key.starts_with("SERVICE_NAME_")
}

/// `isSharedReference`.
/// The audit event of `$environmentVariable->delete()`.
pub(crate) async fn audit_deleted(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    r: &Record,
) {
    let key = text(r, "key").unwrap_or_default();
    let mut changes = Map::new();
    for (k, v) in [
        ("key", r.get("key").cloned()),
        ("is_preview", r.get("is_preview").cloned()),
        ("is_shown_once", r.get("is_shown_once").cloned()),
        ("is_multiline", r.get("is_multiline").cloned()),
        ("version", r.get("version").cloned()),
        ("is_literal", r.get("is_literal").cloned()),
        ("is_required", r.get("is_required").cloned()),
        ("is_shared", r.get("is_shared").cloned()),
        ("resourceable_type", r.get("resourceable_type").cloned()),
        ("is_runtime", r.get("is_runtime").cloned()),
        ("is_buildtime", r.get("is_buildtime").cloned()),
        ("comment", r.get("comment").cloned()),
        (
            "preserve_compose_null",
            r.get("preserve_compose_null").cloned(),
        ),
    ] {
        changes.insert(k.to_owned(), json!({"old": v, "new": null}));
    }
    audit_env(
        state,
        ctx,
        meta,
        res,
        "deleted",
        text(r, "uuid").unwrap_or_default(),
        key,
        vec![],
        Value::Object(changes),
    )
    .await;
}

pub(crate) fn is_shared_reference(value: Option<&str>) -> bool {
    value.is_some_and(|v| SHARED_REFERENCE.is_match(v.trim()))
}

fn owner(res: &Res) -> Owner<'_> {
    Owner {
        morph: &res.morph,
        id: res.id,
        team_id: res.team_id,
        project_id: res.project_id,
        environment_id: res.environment_id,
    }
}

fn text<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn row_id(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

/// The `value` accessor: `trim(decrypt($value))`.
fn plain(state: &AppState, r: &Record) -> Result<Option<String>, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    match text(r, "value").filter(|c| !c.is_empty()) {
        Some(c) => Ok(Some(
            enc.decrypt_maybe_serialized(c)
                .map_err(|_| ApiError::Internal)?
                .trim()
                .to_owned(),
        )),
        None => Ok(None),
    }
}

/// `real_value`.
async fn real_value(
    state: &AppState,
    res: &Res,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    match value.filter(|v| !v.is_empty()) {
        Some(v) => Ok(Some(
            resolve_references(&state.db, enc, &owner(res), res.server_id, v)
                .await
                .map_err(|_| ApiError::Internal)?,
        )),
        None => Ok(value.map(str::to_owned)),
    }
}

/// `displayRealValue()`.
async fn display_real_value(
    state: &AppState,
    res: &Res,
    value: Option<&str>,
    legacy: bool,
    is_literal: bool,
    is_multiline: bool,
) -> Result<Option<String>, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    jobvars::display_real_value_of(
        &state.db,
        enc,
        &owner(res),
        res.server_id,
        value,
        legacy,
        is_literal,
        is_multiline,
    )
    .await
    .map_err(|_| ApiError::Internal)
}

/// One assignment of `extractHardcodedEnvironmentVariables`.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub key: String,
    pub value: Option<String>,
    pub comment: Option<String>,
    pub service_name: String,
    pub is_passthrough: bool,
}

/// `extractHardcodedEnvironmentVariables($dockerComposeRaw)`.
pub fn extract_hardcoded(raw: &str) -> Vec<Assignment> {
    let mut out = Vec::new();
    if raw.trim().is_empty() {
        return out;
    }
    let Ok(yaml) = validate::parse_docker_compose_yaml(raw) else {
        return out;
    };
    let PhpValue::Array(top) = &yaml else {
        return out;
    };
    let services = top
        .iter()
        .find(|(k, _)| matches!(k, Key::Str(s) if s == "services"));
    let Some((_, PhpValue::Array(services))) = services else {
        return out;
    };
    let comments = service_parser::extract_yaml_environment_comments(raw);
    for (service_key, service) in services {
        let name = match service_key {
            Key::Str(s) => s.clone(),
            Key::Int(i) => i.to_string(),
        };
        let PhpValue::Array(items) = service else {
            continue;
        };
        let Some((_, environment)) = items
            .iter()
            .find(|(k, _)| matches!(k, Key::Str(s) if s == "environment"))
        else {
            continue;
        };
        let collected = php::collect(Some(environment));
        if collected.is_empty() {
            continue;
        }
        let passthrough = cenv::extract_docker_compose_passthrough_keys(Some(environment));
        for (key, value) in cenv::convert_to_key_value_collection(&collected) {
            let key = match key {
                Key::Str(s) => s,
                Key::Int(i) => i.to_string(),
            };
            out.push(Assignment {
                value: match &value {
                    PhpValue::Null => None,
                    v => Some(php::to_string(v)),
                },
                comment: comments.get(&key).cloned(),
                service_name: name.clone(),
                is_passthrough: passthrough.contains(&key),
                key,
            });
        }
    }
    out
}

/// `directComposeReference`: `(key, default, operator, required)`.
fn direct_reference(value: Option<&str>) -> Option<String> {
    let value = value?;
    if let Some(rest) = value.strip_prefix('$')
        && !rest.is_empty()
        && rest
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    {
        return Some(rest.to_owned());
    }
    let (content, start, end) = cenv::extract_balanced_brace_content(value, 0)?;
    if start != 1 || end + 1 != value.len() {
        return None;
    }
    Some(match cenv::split_on_operator_outside_nested(&content) {
        Some((variable, _, _)) => variable,
        None => content,
    })
}

/// `isSelfReferencingComposeVariable`.
fn is_self_reference(a: &Assignment) -> bool {
    a.is_passthrough || direct_reference(a.value.as_deref()).as_deref() == Some(a.key.as_str())
}

/// `hardcodedEnvironmentVariableKeys`.
fn hardcoded_keys(assignments: &[Assignment]) -> Vec<String> {
    let editable: HashSet<&str> = assignments
        .iter()
        .filter(|a| is_self_reference(a))
        .map(|a| a.key.as_str())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for a in assignments {
        if editable.contains(a.key.as_str()) || is_protected(&a.key) || out.contains(&a.key) {
            continue;
        }
        out.push(a.key.clone());
    }
    out
}

/// `dockerComposeEnvironmentVariableInfo`.
fn compose_info(assignments: &[Assignment], key: &str) -> Option<Value> {
    let mut services: Vec<String> = Vec::new();
    let mut default: Option<String> = None;
    let mut operator: Option<String> = None;
    let mut required = false;
    for a in assignments {
        let passthrough = a.is_passthrough && a.key == key;
        let references = a
            .value
            .as_deref()
            .map(cenv::extract_docker_compose_environment_variable_references)
            .unwrap_or_default();
        if !passthrough && !references.iter().any(|r| r == key) {
            continue;
        }
        if !a.service_name.is_empty() && !services.contains(&a.service_name) {
            services.push(a.service_name.clone());
        }
        let Some(value) = a.value.as_deref() else {
            continue;
        };
        let mut offset = 0;
        while let Some(found) = value.get(offset..).and_then(|v| v.find("${")) {
            let start = offset + found;
            if start > 0 && value.as_bytes()[start - 1] == b'$' {
                offset = start + 2;
                continue;
            }
            let Some((content, _, _)) = cenv::extract_balanced_brace_content(&value[start..], 0)
            else {
                break;
            };
            if let Some((variable, op, def)) = cenv::split_on_operator_outside_nested(&content)
                && variable == key
            {
                if operator.is_none() {
                    operator = Some(op.clone());
                }
                if op.contains('-') && default.is_none() {
                    default = Some(def);
                }
                required = required || op.contains('?');
            }
            offset = start + 2;
        }
    }
    if services.is_empty() {
        return None;
    }
    Some(
        json!({"services": services, "default": default, "operator": operator, "required": required}),
    )
}

/// `isEnvironmentVariableUsedInDockerCompose`.
fn used_in_compose(assignments_source: Option<&str>, key: &str) -> bool {
    compose_use(assignments_source, key).is_some()
}

/// The reason of `isEnvironmentVariableUsedInDockerCompose`, when the file uses the key.
pub(crate) fn compose_use(assignments_source: Option<&str>, key: &str) -> Option<String> {
    let compose = assignments_source.filter(|c| !c.is_empty())?;
    let Ok(PhpValue::Array(top)) = validate::parse_docker_compose_yaml(compose) else {
        return None;
    };
    let Some((_, PhpValue::Array(services))) = top
        .iter()
        .find(|(k, _)| matches!(k, Key::Str(s) if s == "services"))
    else {
        return None;
    };
    let reference = format!("${key}");
    for (_, service) in services {
        let PhpValue::Array(items) = service else {
            continue;
        };
        let Some((_, PhpValue::Array(envs))) = items
            .iter()
            .find(|(k, _)| matches!(k, Key::Str(s) if s == "environment"))
        else {
            continue;
        };
        for (k, _) in envs {
            if matches!(k, Key::Str(s) if s == key) {
                return Some(format!(
                    "Environment variable '{key}' is used directly in the Docker Compose file."
                ));
            }
        }
        for (_, v) in envs {
            if let PhpValue::Str(s) = v
                && s.contains(&reference)
            {
                return Some(format!(
                    "Environment variable '{key}' is referenced in the Docker Compose file."
                ));
            }
        }
    }
    None
}

fn show_preview(res: &Res) -> bool {
    res.is_application()
        && res
            .str("git_repository")
            .is_some_and(|v| !v.trim().is_empty())
        && res.str("dockerfile").is_none_or(|v| v.trim().is_empty())
}

/// `method_exists($resource, 'redactsAllEnvValuesInLogs')`: applications, services, standalone
/// databases.
fn has_log_redaction(res: &Res) -> bool {
    matches!(
        res.class,
        Class::Application | Class::Service | Class::Database
    )
}

fn redacts_all(res: &Res) -> bool {
    let v = if res.is_application() {
        res.setting("redact_all_env_values_in_logs")
    } else {
        res.raw.get("redact_all_env_values_in_logs")
    };
    v.and_then(Value::as_bool).unwrap_or(true)
}

fn includes_defaults(res: &Res) -> bool {
    let v = if res.is_application() {
        res.setting("include_coolify_default_variables")
    } else {
        res.raw.get("include_coolify_default_variables")
    };
    v.and_then(Value::as_bool) != Some(false)
}

#[derive(Debug, Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    search: String,
    #[serde(default)]
    filters: String,
    #[serde(default)]
    services: String,
    #[serde(default)]
    env: String,
    #[serde(default)]
    sort: String,
    page: Option<i64>,
    per_page: Option<i64>,
}

fn csv(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `managedEnvironmentVariableRow`.
fn managed_row(res: &Res, r: &Record, can_view_compose: bool, assignments: &[Assignment]) -> Value {
    let key = text(r, "key").unwrap_or_default();
    let mut info = compose_info(assignments, key);
    if !can_view_compose && let Some(Value::Object(m)) = info.as_mut() {
        m.insert("default".into(), Value::Null);
    }
    json!({
        "kind": "managed",
        "row_id": format!("environment-{}", row_id(r)),
        "scope": if flag(r, "is_preview") { "preview" } else { "production" },
        "id": row_id(r),
        "uuid": r.get("uuid"),
        "key": key,
        "comment": r.get("comment"),
        "is_preview": flag(r, "is_preview"),
        "is_multiline": flag(r, "is_multiline"),
        "is_literal": flag(r, "is_literal"),
        "is_runtime": flag(r, "is_runtime"),
        "is_buildtime": flag(r, "is_buildtime"),
        "is_required": flag(r, "is_required"),
        "is_shown_once": flag(r, "is_shown_once"),
        "is_shared": flag(r, "is_shared"),
        "is_magic": is_magic(key),
        "is_redis_credential": res.type_name == "standalone-redis" && (key == "REDIS_PASSWORD" || key == "REDIS_USERNAME"),
        "compose_info": info,
    })
}

/// `getEnvironmentVariablePageRowsProperty` with what the page reads around it.
async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    Query(q): Query<ListQuery>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let filters = csv(&q.filters);
    let service_filters = csv(&q.services);
    let environment_filter = if q.env.is_empty() {
        "all"
    } else {
        q.env.as_str()
    };
    let alphabetical = res.setting_bool("is_env_sorting_enabled");
    let preview = show_preview(&res);
    let can = ctx.is_team_admin();
    let compose_source = if res.is_compose() || res.compose_raw().is_some() {
        res.compose_raw().unwrap_or_default()
    } else {
        ""
    };
    let assignments = extract_hardcoded(compose_source);
    let shows_hardcoded = res.is_compose();
    let hidden_keys = if shows_hardcoded {
        hardcoded_keys(&assignments)
    } else {
        Vec::new()
    };
    let includes_hardcoded = !filters.iter().any(|f| f == "user")
        && !filters
            .iter()
            .any(|f| ["buildtime", "runtime", "multiline", "literal"].contains(&f.as_str()));
    let search = q.search.trim().to_owned();
    let mut rows: Vec<Value> = Vec::new();
    let mut scopes: Vec<bool> = Vec::new();
    if environment_filter == "all" || environment_filter == "production" {
        scopes.push(false);
    }
    if preview && (environment_filter == "all" || environment_filter == "preview") {
        scopes.push(true);
    }
    for is_preview in scopes {
        // `missingRequiredEnvironmentVariableIds`.
        let mut missing = Vec::new();
        for r in re::scoped(&state.db, &res.morph, res.id, is_preview, false).await? {
            if flag(&r, "is_required") {
                let v = plain(&state, &r)?;
                let real = real_value(&state, &res, v.as_deref()).await?;
                if real.as_deref().unwrap_or_default().is_empty() {
                    missing.push(row_id(&r));
                }
            }
        }
        let managed = re::managed(
            &state.db,
            &res.morph,
            res.id,
            &ManagedQuery {
                is_preview,
                hidden_keys: &hidden_keys,
                missing_required_ids: &missing,
                search: &search,
                filters: &filters,
                services_filtered: !service_filters.is_empty(),
                sort: &q.sort,
                alphabetical,
            },
        )
        .await?;
        rows.extend(
            managed
                .iter()
                .map(|r| managed_row(&res, r, can, &assignments)),
        );
        if includes_hardcoded && shows_hardcoded {
            let mut hard: Vec<&Assignment> = assignments
                .iter()
                .filter(|a| !is_self_reference(a))
                .filter(|a| !is_protected(&a.key))
                .filter(|a| {
                    search.is_empty() || a.key.to_lowercase().contains(&search.to_lowercase())
                })
                .filter(|a| service_filters.is_empty() || service_filters.contains(&a.service_name))
                .collect();
            if alphabetical {
                hard.sort_by(|a, b| a.key.cmp(&b.key));
            }
            let scope = if is_preview { "preview" } else { "production" };
            for (index, a) in hard.iter().enumerate() {
                let references = a
                    .value
                    .as_deref()
                    .map(cenv::extract_docker_compose_environment_variable_references)
                    .unwrap_or_default();
                rows.push(json!({
                    "kind": "hardcoded",
                    "row_id": format!("hardcoded-{scope}-{}-{}-{index}", a.key, a.service_name),
                    "scope": scope,
                    "key": a.key,
                    "value": if can { a.value.clone() } else { None },
                    "is_value_hidden": !can,
                    "comment": a.comment,
                    "service_name": a.service_name,
                    "compose_type": if references.is_empty() { "literal" } else { "derived" },
                    "references": references,
                }));
            }
        }
    }
    let mut service_options: Vec<String> = assignments
        .iter()
        .map(|a| a.service_name.clone())
        .filter(|s| !s.is_empty())
        .collect();
    service_options.sort();
    service_options.dedup();
    let total = rows.len() as i64;
    let per_page = q.per_page.unwrap_or(10).clamp(1, 100);
    let last_page = ((total + per_page - 1) / per_page).max(1);
    let page = q.page.unwrap_or(1).clamp(1, last_page);
    let start = ((page - 1) * per_page) as usize;
    let page_rows: Vec<Value> = rows
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .collect();
    Ok(ok(json!({
        "rows": page_rows,
        "total": total,
        "page": page,
        "last_page": last_page,
        "per_page": per_page,
        "service_options": service_options,
        "show_preview": preview,
        "can_manage": can,
        "is_application": res.is_application(),
        "build_pack": res.str("build_pack"),
        "is_env_sorting_enabled": alphabetical,
        "use_build_secrets": res.setting_bool("use_build_secrets"),
        "include_coolify_default_variables": includes_defaults(&res),
        "has_log_redaction": has_log_redaction(&res),
        "redact_all_env_values_in_logs": redacts_all(&res),
    }))
    .into_response())
}

/// `availableSharedVariables`.
async fn available(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let team = re::shared_keys(&state.db, res.team_id, "team", None).await?;
    let project = match res.project_id {
        Some(id) => {
            re::shared_keys(&state.db, res.team_id, "project", Some(("project_id", id))).await?
        }
        None => Vec::new(),
    };
    let environment = match res.environment_id {
        Some(id) => {
            re::shared_keys(
                &state.db,
                res.team_id,
                "environment",
                Some(("environment_id", id)),
            )
            .await?
        }
        None => Vec::new(),
    };
    // The route of a database page has no application or service uuid: no server variables.
    let server = match (res.class, res.server_id) {
        (Class::Database, _) | (_, None) => Vec::new(),
        (_, Some(id)) => {
            re::shared_keys(&state.db, res.team_id, "server", Some(("server_id", id))).await?
        }
    };
    let vault_keys_url =
        coolify_db::secret_managers::link_with_token(&state.db, &res.morph, res.id)
            .await?
            .map(|_| format!("/ui/resources/{kind}/{uuid}/secret-manager/autocomplete"));
    Ok(ok(json!({
        "team": team,
        "project": project,
        "environment": environment,
        "server": server,
        // `hasSecretManagerSource()`: the keys are fetched only when a reference is typed.
        "vault_keys_url": vault_keys_url,
    }))
    .into_response())
}

fn env_changes_for_create(n: &NewEnv<'_>) -> Value {
    let mut m = Map::new();
    for (k, v) in [
        ("is_runtime", json!(n.is_runtime)),
        ("is_buildtime", json!(n.is_buildtime)),
        ("key", json!(n.key)),
        ("is_multiline", json!(n.is_multiline)),
        ("is_literal", json!(n.is_literal)),
        ("is_preview", json!(n.is_preview)),
        ("comment", json!(n.comment)),
        ("resourceable_type", json!(n.morph)),
        ("is_shared", json!(n.is_shared)),
    ] {
        m.insert(k.into(), json!({"old": null, "new": v}));
    }
    Value::Object(m)
}

/// What the `version` update of the `created` hook reports as changed: the model still has no
/// original attributes then, so every attribute set on it counts, in the order it was set.
pub(crate) const ADDED_FIELDS: &[&str] = &[
    "key",
    "value",
    "is_multiline",
    "is_literal",
    "is_preview",
    "comment",
    "resourceable_id",
    "resourceable_type",
    "is_shared",
    "uuid",
    "created_at",
    "id",
    "version",
];
/// `BULK_FIELDS` with the flags a `# coolify:` line set: `is_runtime` and `is_buildtime` have
/// defaults in `$attributes`, so they keep their place at the start and count only when they
/// change; `is_literal` is set by `fill()` after `is_multiline`.
fn bulk_fields(settings: &[(String, bool)]) -> Vec<&'static str> {
    let mut fields: Vec<&'static str> = Vec::new();
    for (column, default) in [("is_runtime", true), ("is_buildtime", true)] {
        if settings.iter().any(|(a, v)| a == column && *v != default) {
            fields.push(column);
        }
    }
    for field in BULK_FIELDS {
        fields.push(field);
        if *field == "is_multiline" && settings.iter().any(|(a, _)| a == "is_literal") {
            fields.push("is_literal");
        }
    }
    fields
}

const BULK_FIELDS: &[&str] = &[
    "key",
    "value",
    "comment",
    "is_multiline",
    "is_preview",
    "resourceable_id",
    "resourceable_type",
    "is_shared",
    "uuid",
    "created_at",
    "id",
    "version",
];
const PREVIEW_COPY_FIELDS: &[&str] = &[
    "key",
    "value",
    "is_multiline",
    "is_literal",
    "is_required",
    "is_shown_once",
    "comment",
    "resourceable_type",
    "resourceable_id",
    "is_preview",
    "is_shared",
    "uuid",
    "created_at",
    "id",
    "version",
];

/// `auditChanges('updated', $fields)` of that update.
fn version_changes(n: &NewEnv<'_>, fields: &[&str]) -> Value {
    let mut m = Map::new();
    for f in fields {
        let new = match *f {
            "key" => json!(n.key),
            "is_multiline" => json!(n.is_multiline),
            "is_literal" => json!(n.is_literal),
            "is_runtime" => json!(n.is_runtime),
            "is_buildtime" => json!(n.is_buildtime),
            "is_required" => json!(n.is_required),
            "is_shown_once" => json!(n.is_shown_once),
            "comment" => json!(n.comment),
            "resourceable_type" => json!(n.morph),
            "is_preview" => json!(n.is_preview),
            "is_shared" => json!(n.is_shared),
            "version" => json!(n.version),
            _ => continue,
        };
        m.insert((*f).to_owned(), json!({"old": null, "new": new}));
    }
    Value::Object(m)
}

/// `getChanges()` lists attributes in the order of the table's columns.
fn column_order(changed: &mut [&str]) {
    const COLUMNS: &[&str] = &[
        "key",
        "value",
        "is_preview",
        "is_shown_once",
        "is_multiline",
        "version",
        "is_literal",
        "uuid",
        "order",
        "is_required",
        "is_shared",
        "resourceable_type",
        "resourceable_id",
        "is_runtime",
        "is_buildtime",
        "comment",
        "uses_legacy_escaping",
        "preserve_compose_null",
    ];
    changed.sort_by_key(|c| COLUMNS.iter().position(|x| x == c));
}

#[allow(clippy::too_many_arguments)]
async fn audit_env(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    action: &str,
    uuid: &str,
    key: &str,
    changed: Vec<&str>,
    changes: Value,
) {
    crate::audit::mutation(
        state,
        ctx,
        meta,
        "environment_variable",
        action,
        json!({
            "team_id": res.team_id,
            "environment_variable_uuid": uuid,
            "environment_variable_name": key,
            "changed_fields": changed,
        }),
        changes,
    )
    .await;
}

/// `new EnvironmentVariable`, `save()` and the model's `created` hook: the preview copy of an
/// application's variable and the `version` update of both.
pub(crate) async fn create_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    n: &NewEnv<'_>,
    version_fields: &[&str],
) -> Result<Record, ApiError> {
    let now = state.now();
    let row = re::create(&state.db, n, now).await?;
    let uuid = text(&row, "uuid").unwrap_or_default().to_owned();
    audit_env(
        state,
        ctx,
        meta,
        res,
        "created",
        &uuid,
        n.key,
        vec![],
        env_changes_for_create(n),
    )
    .await;
    if res.is_application()
        && !n.is_preview
        && re::by_key(&state.db, &res.morph, res.id, true, n.key)
            .await?
            .is_none()
        && coolify_db::environment_variables::application_copies_to_preview(
            state.db.pool(),
            res.id,
            n.key,
        )
        .await?
    {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let value = match n.value_encrypted {
            Some(c) => Some(
                enc.decrypt_maybe_serialized(c)
                    .map_err(|_| ApiError::Internal)?,
            ),
            None => None,
        };
        let copy_cipher = value.as_deref().map(|v| enc.encrypt_serialized(v));
        let copy = NewEnv {
            value_encrypted: copy_cipher.as_deref(),
            is_preview: true,
            order: None,
            ..n.clone()
        };
        let preview = re::create(&state.db, &copy, now).await?;
        let preview_uuid = text(&preview, "uuid").unwrap_or_default().to_owned();
        audit_env(
            state,
            ctx,
            meta,
            res,
            "created",
            &preview_uuid,
            n.key,
            vec![],
            env_changes_for_create(&copy),
        )
        .await;
        audit_env(
            state,
            ctx,
            meta,
            res,
            "updated",
            &preview_uuid,
            n.key,
            PREVIEW_COPY_FIELDS.to_vec(),
            version_changes(&copy, PREVIEW_COPY_FIELDS),
        )
        .await;
    }
    audit_env(
        state,
        ctx,
        meta,
        res,
        "updated",
        &uuid,
        n.key,
        version_fields.to_vec(),
        version_changes(n, version_fields),
    )
    .await;
    Ok(row)
}

fn field_errors(errors: Map<String, Value>) -> Response {
    let first = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
        .unwrap_or("The given data was invalid.")
        .to_owned();
    let count: usize = errors
        .values()
        .map(|v| v.as_array().map_or(0, Vec::len))
        .sum();
    let message = match count.saturating_sub(1) {
        0 => first,
        1 => format!("{first} (and 1 more error)"),
        n => format!("{first} (and {n} more errors)"),
    };
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

fn validate_fields(key: &str, comment: Option<&str>) -> Map<String, Value> {
    let mut errors = Map::new();
    if key.is_empty() {
        errors.insert("key".into(), json!(["The key field is required."]));
    } else {
        let mut list = Vec::new();
        if key.chars().count() > 255 {
            list.push("The key may not be greater than 255 characters.");
        }
        if !KEY_PATTERN.is_match(key) {
            list.push(KEY_MESSAGE);
        }
        if !list.is_empty() {
            errors.insert("key".into(), json!(list));
        }
    }
    if comment.is_some_and(|c| c.chars().count() > 256) {
        errors.insert(
            "comment".into(),
            json!(["The comment field must not be greater than 256 characters."]),
        );
    }
    errors
}

fn bool_of(body: &Value, key: &str, default: bool) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// `$resource->environment_variables()->create(['key' => $key, 'value' => $value])`.
pub(crate) async fn create_plain(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    key: &str,
    value: &str,
) -> Result<(), ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let cipher = enc.encrypt_serialized(value);
    let n = NewEnv {
        morph: &res.morph,
        resource_id: res.id,
        key,
        value_encrypted: Some(&cipher),
        comment: None,
        is_multiline: false,
        is_literal: false,
        is_runtime: true,
        is_buildtime: true,
        is_required: false,
        is_shown_once: false,
        is_preview: false,
        is_shared: false,
        order: None,
        version: &state.version,
    };
    create_model(state, ctx, meta, res, &n, &["key", "value"]).await?;
    Ok(())
}

/// `Add::submit` then `All::submit($data)` (`handleSingleSubmit`).
async fn add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let comment = body.get("comment").and_then(Value::as_str);
    let errors = validate_fields(&key, comment);
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if re::by_key(&state.db, &res.morph, res.id, false, &key)
        .await?
        .is_some()
    {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            "Environment variable already exists.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let order = re::max_order(&state.db, &res.morph, res.id).await? + 1;
    let value = body
        .get("value")
        .and_then(Value::as_str)
        .map(|v| v.trim().to_owned());
    let cipher = value.as_deref().map(|v| enc.encrypt_serialized(v));
    let n = NewEnv {
        morph: &res.morph,
        resource_id: res.id,
        key: &key,
        value_encrypted: cipher.as_deref(),
        comment,
        is_multiline: bool_of(&body, "is_multiline", false),
        is_literal: bool_of(&body, "is_literal", false),
        is_runtime: bool_of(&body, "is_runtime", true),
        is_buildtime: bool_of(&body, "is_buildtime", true),
        is_required: false,
        is_shown_once: false,
        is_preview: false,
        is_shared: is_shared_reference(value.as_deref()),
        order: Some(order),
        version: &state.version,
    };
    create_model(&state, &ctx, &meta, &res, &n, ADDED_FIELDS).await?;
    Ok(ok(json!({"message": "Environment variable added."})).into_response())
}

/// `formatEnvironmentVariables`.
fn format_dev(state: &AppState, rows: &[Record], hide: bool) -> Result<String, ApiError> {
    let mut lines = Vec::new();
    for r in rows {
        let key = text(r, "key").unwrap_or_default();
        if is_protected(key) {
            continue;
        }
        // `formatEnvVariableForDevView`.
        let value = if hide { None } else { plain(state, r)? };
        let flags = [
            ("literal", flag(r, "is_literal")),
            ("buildtime", flag(r, "is_buildtime")),
            ("runtime", flag(r, "is_runtime")),
        ];
        let v = dev_view::DevVariable {
            key,
            value: value.as_deref(),
            is_shown_once: flag(r, "is_shown_once"),
            is_multiline: flag(r, "is_multiline"),
            flags: &flags,
        };
        lines.push(dev_view::format_variable(&v, !hide, dev_view::ENV_FLAGS));
    }
    Ok(lines.join("\n"))
}

/// `getDevView`.
async fn dev_view(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let alphabetical = res.setting_bool("is_env_sorting_enabled");
    let hide = !ctx.is_team_admin();
    let prod = re::scoped(&state.db, &res.morph, res.id, false, alphabetical).await?;
    let variables = format_dev(&state, &prod, hide)?;
    let preview = if show_preview(&res) {
        let rows = re::scoped(&state.db, &res.morph, res.id, true, alphabetical).await?;
        Some(format_dev(&state, &rows, hide)?)
    } else {
        None
    };
    Ok(ok(json!({"variables": variables, "variables_preview": preview})).into_response())
}

/// `parseEnvFormatToArray`: `(key, value, comment)`; a later line wins, at the place of the first.
pub fn parse_env_format(contents: &str) -> Vec<(String, String, Option<String>)> {
    let mut out: Vec<(String, String, Option<String>)> = Vec::new();
    static UNQUOTED_COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+#").unwrap_or_else(|_| unreachable!()));
    for line in contents.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let mut remainder = "";
        let value: String;
        let first = rest.chars().next();
        if first == Some('"') || first == Some('\'') {
            let q = first.unwrap_or('"');
            let inner = &rest[1..];
            match inner.find(q) {
                Some(pos) => {
                    value = inner[..pos].to_owned();
                    remainder = &inner[pos + 1..];
                }
                None => value = inner.to_owned(),
            }
        } else if let Some(m) = UNQUOTED_COMMENT.find(rest) {
            remainder = &rest[m.start()..];
            value = rest[..m.start()].trim_end().to_owned();
        } else {
            value = rest.to_owned();
        }
        let comment = remainder
            .find('#')
            .map(|p| remainder[p + 1..].trim().to_owned())
            .filter(|c| !c.is_empty());
        match out.iter_mut().find(|(k, _, _)| k == key) {
            Some(slot) => {
                slot.1 = value;
                slot.2 = comment;
            }
            None => out.push((key.to_owned(), value, comment)),
        }
    }
    out
}

/// `normalizeEnvironmentVariables`.
fn normalize(parsed: Vec<Parsed>) -> Result<Vec<Parsed>, String> {
    let mut out: Vec<Parsed> = Vec::new();
    for mut p in parsed {
        let k = p.key.trim().to_owned();
        if !KEY_PATTERN.is_match(&k) {
            return Err(KEY_MESSAGE.to_owned());
        }
        if out.iter().any(|x| x.key == k) {
            return Err(format!(
                "Duplicate environment variable key after normalization: {k}."
            ));
        }
        p.key = k;
        out.push(p);
    }
    Ok(out)
}

/// `deleteRemovedVariables`: the number deleted, or `Err(key)` when Compose still uses one.
async fn delete_removed(
    state: &AppState,
    res: &Res,
    is_preview: bool,
    variables: &[Parsed],
) -> Result<Result<usize, String>, ApiError> {
    let rows = re::scoped(&state.db, &res.morph, res.id, is_preview, false).await?;
    let doomed: Vec<&Record> = rows
        .iter()
        .filter(|r| {
            let key = text(r, "key").unwrap_or_default();
            !variables.iter().any(|v| v.key == key) && !is_protected(key)
        })
        .collect();
    if doomed.is_empty() {
        return Ok(Ok(0));
    }
    if res.is_compose() {
        for r in &doomed {
            let key = text(r, "key").unwrap_or_default();
            if used_in_compose(res.str("docker_compose"), key) {
                return Ok(Err(key.to_owned()));
            }
        }
    }
    let ids: Vec<i64> = doomed.iter().map(|r| row_id(r)).collect();
    re::delete_ids(&state.db, &ids).await?;
    Ok(Ok(ids.len()))
}

/// `updateOrCreateVariables`.
async fn update_or_create(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    is_preview: bool,
    variables: &[Parsed],
) -> Result<usize, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut count = 0;
    for parsed in variables {
        let (key, value, comment) = (&parsed.key, &parsed.value, &parsed.comment);
        if is_magic(key) {
            continue;
        }
        match re::by_key(&state.db, &res.morph, res.id, is_preview, key).await? {
            Some(found) => {
                let current = plain(state, &found)?;
                // A value the view could not show is not overwritten by its placeholder.
                if flag(&found, "is_shown_once")
                    || flag(&found, "is_multiline")
                    || dev_view::format_value(current.as_deref()).is_none()
                {
                    continue;
                }
                let trimmed = value.trim();
                let mut values: Vec<(&'static str, C)> = Vec::new();
                let mut changed: Vec<&str> = Vec::new();
                let mut changes = Map::new();
                if current.as_deref() != Some(value.as_str()) {
                    values.push(("value", C::Text(Some(enc.encrypt_serialized(trimmed)))));
                    if current.as_deref() != Some(trimmed) {
                        changed.push("value");
                    }
                }
                if let Some(c) = comment
                    && text(&found, "comment") != Some(c.as_str())
                {
                    values.push(("comment", C::Text(Some(c.clone()))));
                    changed.push("comment");
                    changes.insert(
                        "comment".into(),
                        json!({"old": found.get("comment"), "new": c}),
                    );
                }
                // `$found->fill($data['settings'])`: flags change only when a "# coolify:" line sets them.
                for (attribute, new) in &parsed.settings {
                    let column = match attribute.as_str() {
                        "is_literal" => "is_literal",
                        "is_buildtime" => "is_buildtime",
                        "is_runtime" => "is_runtime",
                        _ => continue,
                    };
                    if *new != flag(&found, column) {
                        values.push((column, C::Bool(*new)));
                        changed.push(column);
                        changes.insert(
                            column.into(),
                            json!({"old": flag(&found, column), "new": new}),
                        );
                    }
                }
                let shared = is_shared_reference(Some(trimmed));
                if shared != flag(&found, "is_shared") && !values.is_empty() {
                    values.push(("is_shared", C::Bool(shared)));
                    changed.push("is_shared");
                    changes.insert(
                        "is_shared".into(),
                        json!({"old": flag(&found, "is_shared"), "new": shared}),
                    );
                }
                if !values.is_empty() {
                    parts::update_row(
                        &state.db,
                        "environment_variables",
                        row_id(&found),
                        &values,
                        state.now(),
                    )
                    .await?;
                    if !changed.is_empty() {
                        column_order(&mut changed);
                        audit_env(
                            state,
                            ctx,
                            meta,
                            res,
                            "updated",
                            text(&found, "uuid").unwrap_or_default(),
                            key,
                            changed,
                            Value::Object(changes),
                        )
                        .await;
                    }
                    count += 1;
                }
            }
            None => {
                let cipher = enc.encrypt_serialized(value.trim());
                let n = NewEnv {
                    morph: &res.morph,
                    resource_id: res.id,
                    key,
                    value_encrypted: Some(&cipher),
                    comment: comment.as_deref(),
                    is_multiline: false,
                    is_literal: parsed.setting("is_literal").unwrap_or(false),
                    is_runtime: parsed.setting("is_runtime").unwrap_or(true),
                    is_buildtime: parsed.setting("is_buildtime").unwrap_or(true),
                    is_required: false,
                    is_shown_once: false,
                    is_preview,
                    is_shared: is_shared_reference(Some(value.trim())),
                    order: None,
                    version: &state.version,
                };
                let fields = bulk_fields(&parsed.settings);
                create_model(state, ctx, meta, res, &n, &fields).await?;
                count += 1;
            }
        }
    }
    Ok(count)
}

/// `updateOrder`.
async fn update_order(
    state: &AppState,
    res: &Res,
    is_preview: bool,
    variables: &[Parsed],
) -> Result<(), ApiError> {
    for (i, v) in variables.iter().enumerate() {
        if let Some(found) = re::by_key(&state.db, &res.morph, res.id, is_preview, &v.key).await? {
            re::set_order(&state.db, row_id(&found), i as i64 + 1, state.now()).await?;
        }
    }
    Ok(())
}

/// `All::submit()` of the developer view (`handleBulkSubmit`, `updateOrder`).
async fn bulk(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let contents = body
        .get("variables")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Parsed before the save, so that an invalid "# coolify:" line changes nothing.
    let variables = match dev_view::parse(contents, dev_view::ENV_FLAGS).and_then(normalize) {
        Ok(v) => v,
        Err(m) => return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m)),
    };
    let preview = show_preview(&res);
    let preview_variables = if preview {
        let contents = body
            .get("variables_preview")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match dev_view::parse(contents, dev_view::ENV_FLAGS).and_then(normalize) {
            Ok(v) => v,
            Err(m) => {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("Preview deployments: {m}"),
                ));
            }
        }
    } else {
        Vec::new()
    };
    let mut changes_made = false;
    let mut error: Option<String> = None;
    match delete_removed(&state, &res, false, &variables).await? {
        Ok(n) => changes_made |= n > 0,
        Err(key) => error = Some(key),
    }
    changes_made |= update_or_create(&state, &ctx, &meta, &res, false, &variables).await? > 0;
    if preview {
        match delete_removed(&state, &res, true, &preview_variables).await? {
            Ok(n) => changes_made |= n > 0,
            Err(key) => error = Some(key),
        }
        changes_made |=
            update_or_create(&state, &ctx, &meta, &res, true, &preview_variables).await? > 0;
    }
    update_order(&state, &res, false, &variables).await?;
    if preview {
        update_order(&state, &res, true, &preview_variables).await?;
    }
    let message = (changes_made && error.is_none()).then_some("Environment variables updated.");
    Ok(ok(json!({
        "message": message,
        "error": error.map(|key| format!("Cannot delete environment variable '{key}' <br><br>Please remove it from the Docker Compose file first.")),
    }))
    .into_response())
}

/// `instantSave`, `saveCoolifyDefaultVariables` and `saveLogRedactionSetting`.
async fn settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut message = "Environment variable settings updated.";
    for k in [
        "is_env_sorting_enabled",
        "use_build_secrets",
        "include_coolify_default_variables",
        "redact_all_env_values_in_logs",
    ] {
        if let Some(b) = body.get(k).and_then(Value::as_bool) {
            let current = if res.is_application() {
                res.setting(k)
            } else {
                res.raw.get(k)
            }
            .and_then(Value::as_bool);
            if current != Some(b) {
                values.push((k, C::Bool(b)));
            }
            if k == "redact_all_env_values_in_logs" {
                message = "Log redaction setting updated.";
            }
        }
    }
    let names: Vec<&str> = values.iter().map(|(k, _)| *k).collect();
    if res.is_application() {
        apps::update_settings(&state.db, res.id, &values, state.now()).await?;
        if !names.is_empty() {
            crate::audit::log(
                &state,
                &ctx,
                &meta,
                "ui.application.settings_updated",
                json!({
                    "team_id": res.team_id,
                    "application_uuid": res.uuid,
                    "application_name": res.name,
                    "changed_fields": names,
                }),
                "info",
            )
            .await;
        }
    } else {
        parts::update_row(&state.db, res.table, res.id, &values, state.now()).await?;
        if !names.is_empty() {
            let resource = res.morph.rsplit('\\').next().map(snake).unwrap_or_default();
            let mut changes = Map::new();
            for (k, v) in &values {
                if let C::Bool(b) = v {
                    changes.insert((*k).to_owned(), json!({"old": res.raw.get(*k), "new": b}));
                }
            }
            crate::audit::mutation(
                &state,
                &ctx,
                &meta,
                &resource,
                "updated",
                json!({
                    "team_id": res.team_id,
                    format!("{resource}_uuid"): res.uuid,
                    format!("{resource}_name"): res.name,
                    "changed_fields": names,
                }),
                Value::Object(changes),
            )
            .await;
        }
    }
    Ok(ok(json!({"message": message})).into_response())
}

/// `Str::snake`.
pub fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `legacyEscapingChangesValue`.
fn legacy_changes_value(res: &Res, r: &Record, value: Option<&str>) -> bool {
    if !flag(r, "uses_legacy_escaping") {
        return false;
    }
    let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
        return false;
    };
    if value.contains("{{secrets.") {
        return false;
    }
    let key = text(r, "key").unwrap_or_default();
    let redis = [
        "standalone-redis",
        "standalone-keydb",
        "standalone-dragonfly",
    ]
    .contains(&res.type_name.as_str());
    if key == "REDIS_PASSWORD" && redis && value.contains('$') {
        return true;
    }
    if (value.starts_with('{') || value.starts_with('['))
        && serde_json::from_str::<Value>(value).is_ok()
    {
        return value.contains('$') || (!res.is_application() && value.contains('\n'));
    }
    let app_or_service = res.is_application() || res.is_service();
    if flag(r, "is_literal") || flag(r, "is_multiline") {
        return if app_or_service {
            value.contains('\'')
        } else {
            true
        };
    }
    escape_env_variables(value) != value || (app_or_service && value.contains(" #"))
}

/// `Show::loadValues`.
async fn values(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, id)): PI,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    let value = plain(&state, &r)?;
    let is_shared = is_shared_reference(value.as_deref());
    let real = if is_shared {
        display_real_value(
            &state,
            &res,
            value.as_deref(),
            flag(&r, "uses_legacy_escaping"),
            flag(&r, "is_literal"),
            flag(&r, "is_multiline"),
        )
        .await?
    } else {
        None
    };
    let really_required = flag(&r, "is_required")
        && if is_shared {
            real.as_deref().unwrap_or_default().trim().is_empty()
        } else {
            value.as_deref().unwrap_or_default().trim().is_empty()
        };
    let resolved = real_value(&state, &res, value.as_deref()).await?;
    let legacy = legacy_changes_value(&res, &r, resolved.as_deref());
    let locked = flag(&r, "is_shown_once");
    Ok(ok(json!({
        "value": if locked { None } else { value },
        "real_value": if locked { None } else { real },
        "is_shared": is_shared,
        "is_really_required": really_required,
        "legacy_escaping_changes_value": legacy,
    }))
    .into_response())
}

/// `resolveReferencedValue` of a value of this resource.
async fn resolve_referenced(
    state: &AppState,
    res: &Res,
    is_preview: bool,
    is_literal: bool,
    value: Option<String>,
) -> Result<Option<String>, ApiError> {
    let Some(v) = value.as_deref().filter(|v| !v.trim().is_empty()) else {
        return Ok(value);
    };
    if is_literal || !v.starts_with('$') {
        return Ok(value);
    }
    let referenced = v[1..].trim_matches(|c| c == '{' || c == '}');
    if let Some(found) = re::by_key(&state.db, &res.morph, res.id, is_preview, referenced).await?
        && !flag(&found, "is_shown_once")
        && let Some(p) = plain(state, &found)?
    {
        return Ok(Some(p));
    }
    Ok(value)
}

/// `Show::copyValue`.
async fn copy_value(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid, id)): PI,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    if flag(&r, "is_shown_once") || !ctx.is_team_admin() {
        return Ok(ok(json!({"value": null})).into_response());
    }
    let value = plain(&state, &r)?;
    let referenced = resolve_referenced(
        &state,
        &res,
        flag(&r, "is_preview"),
        flag(&r, "is_literal"),
        value,
    )
    .await?;
    let shown =
        display_real_value(&state, &res, referenced.as_deref(), false, false, false).await?;
    Ok(ok(json!({"value": shown})).into_response())
}

/// `ShowHardcoded::copyValue`.
async fn resolve_hardcoded(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(ok(json!({"value": null})).into_response());
    }
    let value = body.get("value").and_then(Value::as_str).map(str::to_owned);
    let referenced = resolve_referenced(
        &state,
        &res,
        bool_of(&body, "is_preview", false),
        false,
        value,
    )
    .await?;
    Ok(ok(json!({"value": referenced})).into_response())
}

/// `Show::lock`.
async fn lock(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    if !flag(&r, "is_shown_once") {
        parts::update_row(
            &state.db,
            "environment_variables",
            id,
            &[("is_shown_once", C::Bool(true))],
            state.now(),
        )
        .await?;
        audit_env(
            &state,
            &ctx,
            &meta,
            &res,
            "updated",
            text(&r, "uuid").unwrap_or_default(),
            text(&r, "key").unwrap_or_default(),
            vec!["is_shown_once"],
            json!({"is_shown_once": {"old": false, "new": true}}),
        )
        .await;
    }
    Ok(ok(json!({"message": null})).into_response())
}

/// `Show::submit`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    save(&state, &ctx, &meta, &res, &r, &body).await
}

/// `Show::useComposeDefault`.
async fn compose_default(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    let assignments = extract_hardcoded(res.compose_raw().unwrap_or_default());
    let info = compose_info(&assignments, text(&r, "key").unwrap_or_default());
    let usable = info.as_ref().is_some_and(|i| {
        !i.get("default").is_none_or(Value::is_null)
            && i.get("required").and_then(Value::as_bool) != Some(true)
    });
    if flag(&r, "is_shown_once") || !usable {
        return Ok(ok(json!({"message": null})).into_response());
    }
    let mut body = body;
    if let Value::Object(m) = &mut body {
        m.insert("value".into(), Value::Null);
    }
    save(&state, &ctx, &meta, &res, &r, &body).await
}

async fn save(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    r: &Record,
    body: &Value,
) -> ApiResult {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let stored_key = text(r, "key").unwrap_or_default().to_owned();
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // `serverComposeInfo`: applications and services only.
    if matches!(res.class, Class::Application | Class::Service) {
        let assignments = extract_hardcoded(res.compose_raw().unwrap_or_default());
        if compose_info(&assignments, &stored_key).is_some() && key != stored_key {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Compose-linked environment variable names cannot be changed.",
            ));
        }
    }
    let locked = flag(r, "is_shown_once");
    let current = plain(state, r)?;
    let new_value = if locked {
        current.clone()
    } else {
        body.get("value")
            .and_then(Value::as_str)
            .map(|v| v.trim().to_owned())
    };
    if flag(r, "is_required") && new_value.as_deref().unwrap_or_default().is_empty() {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Required environment variables cannot be empty.",
        ));
    }
    let key = key.trim().to_owned();
    let comment = body
        .get("comment")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let errors = validate_fields(&key, comment.as_deref());
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let mut values: Vec<(&'static str, C)> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    let mut changes = Map::new();
    if key != stored_key {
        values.push(("key", C::Text(Some(key.clone()))));
        changed.push("key");
        changes.insert("key".into(), json!({"old": stored_key, "new": key}));
    }
    if !locked && new_value != current {
        values.push((
            "value",
            C::Text(new_value.as_deref().map(|v| enc.encrypt_serialized(v))),
        ));
        changed.push("value");
        if flag(r, "preserve_compose_null") && new_value.is_some() {
            values.push(("preserve_compose_null", C::Bool(false)));
            changed.push("preserve_compose_null");
            changes.insert(
                "preserve_compose_null".into(),
                json!({"old": true, "new": false}),
            );
        }
    }
    if text(r, "comment") != comment.as_deref() {
        values.push(("comment", C::Text(comment.clone())));
        changed.push("comment");
        changes.insert(
            "comment".into(),
            json!({"old": r.get("comment"), "new": comment}),
        );
    }
    for k in ["is_multiline", "is_literal", "is_runtime", "is_buildtime"] {
        let new = bool_of(body, k, flag(r, k));
        if new != flag(r, k) {
            values.push((k, C::Bool(new)));
            changed.push(k);
            changes.insert(k.to_owned(), json!({"old": flag(r, k), "new": new}));
        }
    }
    let shared = is_shared_reference(new_value.as_deref());
    if shared != flag(r, "is_shared") {
        values.push(("is_shared", C::Bool(shared)));
        changed.push("is_shared");
        changes.insert(
            "is_shared".into(),
            json!({"old": flag(r, "is_shared"), "new": shared}),
        );
    }
    parts::update_row(
        &state.db,
        "environment_variables",
        row_id(r),
        &values,
        state.now(),
    )
    .await?;
    if !changed.is_empty() {
        column_order(&mut changed);
        audit_env(
            state,
            ctx,
            meta,
            res,
            "updated",
            text(r, "uuid").unwrap_or_default(),
            &key,
            changed,
            Value::Object(changes),
        )
        .await;
    }
    Ok(ok(json!({"message": "Environment variable updated."})).into_response())
}

/// `Show::delete`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid, id)): PI,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(r) = re::find(&state.db, &res.morph, res.id, id).await? else {
        return Ok(toast_error(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    let key = text(&r, "key").unwrap_or_default();
    let compose = res.str("docker_compose").filter(|c| !c.is_empty());
    if (res.is_service() || (res.is_application() && compose.is_some()))
        && used_in_compose(compose, key)
    {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Cannot delete environment variable '{key}' <br><br>Please remove it from the Docker Compose file first."
            ),
        ));
    }
    re::delete_ids(&state.db, &[id]).await?;
    audit_deleted(&state, &ctx, &meta, &res, &r).await;
    Ok(ok(json!({"message": "Environment variable deleted successfully."})).into_response())
}
