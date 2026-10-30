//! `ServicesController`, `ServiceApplicationsController`, `ServiceDatabasesController` and the
//! service half of `ScheduledTasksController` (app/Http/Controllers/Api).
//!
//! Variable, storage and tag handling live here, not shared with applications and databases yet.
//! Not ported: volume backups, database imports, migrating a service to another destination and
//! cloning volume data (these answer 501, see docs/KNOWN_LIMITATIONS.md).

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine as _;
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue, ServerRow};
use coolify_db::services::{
    self as svc, NewFileStorage, NewService, SERVICE, SERVICE_APPLICATION, SERVICE_DATABASE,
    ServiceRow, SubKind, SubRow,
};
use coolify_domain::naming::strip_tags;
use coolify_jobs::applications::domains as app_domains;
use coolify_jobs::services::{self as jobs_services, status as job_status, storage, templates};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::databases::{
    errors_with, input_bool, nested_server, nested_server_with, query_bool, server_functional,
};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Service not found.");
const APP_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Service application not found.");
const DB_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Service database not found.");
const CONFLICT_WARNING: &str =
    "Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior.";
const NOT_ALLOWED: &str = "This field is not allowed.";

fn field(name: &'static str, rules: Vec<Rule>) -> Field {
    Field { name, rules }
}

fn message(status: StatusCode, v: Value) -> ApiResult {
    Ok(PhpJson(status, v).into_response())
}

fn require_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// `$request->user()->tokenCan('deploy') || tokenCan('root')`.
fn can_deploy(ctx: &AuthCtx) -> bool {
    ctx.abilities.can(coolify_domain::Ability::Deploy)
        || ctx.abilities.can(coolify_domain::Ability::Root)
}

fn deploy_forbidden(ctx: &AuthCtx, reason: &str) -> Option<axum::response::Response> {
    if can_deploy(ctx) {
        return None;
    }
    Some(
        PhpJson(
            StatusCode::FORBIDDEN,
            json!({"message": format!("Missing required permissions: deploy. {reason}")}),
        )
        .into_response(),
    )
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<ServiceRow, ApiError> {
    svc::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

fn jobs_of(state: &AppState) -> Result<&coolify_jobs::Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

fn jobs_error(e: String) -> ApiError {
    tracing::error!(error = %e, "service job failed");
    ApiError::Internal
}

async fn server_of(state: &AppState, s: &ServiceRow) -> Result<Option<ServerRow>, ApiError> {
    Ok(match s.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    })
}

/// `$service->status`.
async fn service_status(state: &AppState, s: &ServiceRow) -> Result<Option<String>, ApiError> {
    if svc::has_running_start(&state.db, &s.uuid).await? {
        return Ok(Some("starting:unhealthy".into()));
    }
    let mut subs = svc::subs(&state.db, SubKind::Application, s.id).await?;
    subs.extend(svc::subs(&state.db, SubKind::Database, s.id).await?);
    Ok(job_status::container_status_of(&subs))
}

/// `ServiceApplication::toArray()`.
fn application_array(sub: &SubRow) -> Record {
    let mut r = SERVICE_APPLICATION.present(sub.raw.clone());
    r.insert("url".into(), url_attribute(sub));
    r
}

/// `ServiceApplication::url()`.
fn url_attribute(sub: &SubRow) -> Value {
    let Some(fqdn) = sub.str("fqdn").filter(|f| !f.trim().is_empty()) else {
        return Value::Null;
    };
    let overrides = sub.raw.get("domain_port_overrides").cloned().unwrap_or(Value::Null);
    let urls: Vec<String> = fqdn
        .split(',')
        .map(|url| {
            let url = url.trim();
            let canonical = app_domains::without_port(url);
            match overrides.get(&canonical).and_then(Value::as_i64) {
                None => url.to_owned(),
                Some(port) => match app_domains::UrlParts::parse(&canonical) {
                    Some(parts) => format!(
                        "{}://{}:{port}{}",
                        parts.scheme,
                        parts.host,
                        parts.path.as_deref().filter(|p| *p != "/").unwrap_or_default()
                    ),
                    None => url.to_owned(),
                },
            }
        })
        .collect();
    Value::String(urls.join(","))
}

/// `removeSensitiveData($service)`.
async fn present_service(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    with_environment: bool,
) -> Result<Value, ApiError> {
    let server = server_of(state, s).await?;
    let mut r = SERVICE.present(s.raw.clone());
    r.shift_remove("id");
    if !ctx.can_read_sensitive {
        r.shift_remove("docker_compose");
        r.shift_remove("docker_compose_raw");
    }
    r.insert(
        "server_status".into(),
        Value::Bool(server.as_ref().is_some_and(server_functional)),
    );
    r.insert(
        "status".into(),
        service_status(state, s).await?.map_or(Value::Null, Value::String),
    );
    let apps = svc::subs(&state.db, SubKind::Application, s.id).await?;
    let databases = svc::subs(&state.db, SubKind::Database, s.id).await?;
    if let Some(server) = &server {
        // `exposeNestedServerSecrets()` only touches `destination.server`: this is another instance.
        r.insert(
            "server".into(),
            nested_server_with(state, server, false, ctx.can_read_sensitive)?,
        );
    }
    r.insert(
        "applications".into(),
        Value::Array(apps.iter().map(|a| Value::Object(application_array(a))).collect()),
    );
    r.insert(
        "databases".into(),
        Value::Array(
            databases
                .iter()
                .map(|d| Value::Object(SERVICE_DATABASE.present(d.raw.clone())))
                .collect(),
        ),
    );
    // `$service->team()` (the policy, or redacting the compose file) loads these relations.
    if with_environment {
        let mut project = s.project.clone();
        project.insert("team".into(), Value::Object(s.team.clone()));
        let mut env = s.environment.clone();
        env.insert("project".into(), Value::Object(project));
        r.insert("environment".into(), Value::Object(env));
    }
    if ctx.can_read_sensitive
        && let Some(mut dest) = svc::destination_record(&state.db, s).await?
    {
        if let Some(server) = &server {
            dest.insert("server".into(), nested_server(state, ctx, server)?);
        }
        r.insert("destination".into(), Value::Object(dest));
    }
    Ok(Value::Object(serialize_record(r)))
}

/// `GET /services`.
pub async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = svc::list_for_team(&state.db, ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for s in &rows {
        out.push(present_service(&state, &ctx, s, ctx.can_read_sensitive).await?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `GET /services/{uuid}`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    Ok(ok(present_service(&state, &ctx, &s, true).await?).into_response())
}

// --- validation -------------------------------------------------------------------------------

fn add_error(errors: &mut Map<String, Value>, key: &str, msg: String) {
    if let Some(list) = errors
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
    {
        list.push(Value::String(msg));
    }
}

/// Runs the flat rules and collects their errors without the extra-field check.
fn field_errors(input: &Input, fields: &[Field]) -> Result<Map<String, Value>, ApiError> {
    let keys: Vec<String> = input.keys().cloned().collect();
    let allowed: Vec<&str> = keys.iter().map(String::as_str).collect();
    match validate(input, fields, &[], &allowed) {
        Ok(()) => Ok(Map::new()),
        Err(ApiError::Validation(m)) => Ok(m),
        Err(e) => Err(e),
    }
}

/// `urls`, `urls.*`, `urls.*.name`, `urls.*.url`.
fn url_errors(input: &Input, errors: &mut Map<String, Value>) {
    let Some(Value::Array(urls)) = input.get("urls") else {
        return;
    };
    for (i, item) in urls.iter().enumerate() {
        let Some(map) = item.as_object() else {
            add_error(
                errors,
                &format!("urls.{i}"),
                "An item in the urls array has invalid fields. Only name and url fields are supported.".into(),
            );
            continue;
        };
        if map.keys().any(|k| k != "name" && k != "url") {
            add_error(
                errors,
                &format!("urls.{i}"),
                "An item in the urls array has invalid fields. Only name and url fields are supported.".into(),
            );
        }
        match map.get("name") {
            None | Some(Value::Null) => add_error(
                errors,
                &format!("urls.{i}.name"),
                format!("The urls.{i}.name field is required."),
            ),
            Some(Value::String(_)) => {}
            Some(_) => add_error(
                errors,
                &format!("urls.{i}.name"),
                format!("The urls.{i}.name field must be a string."),
            ),
        }
        match map.get("url") {
            None | Some(Value::Null) => {}
            Some(Value::String(u)) => {
                if u.chars().count() > 2048 {
                    add_error(
                        errors,
                        &format!("urls.{i}.url"),
                        format!("The urls.{i}.url field must not be greater than 2048 characters."),
                    );
                }
                for e in app_domains::validate_domains(u) {
                    add_error(errors, &format!("urls.{i}.url"), e);
                }
            }
            Some(_) => add_error(
                errors,
                &format!("urls.{i}.url"),
                format!("The urls.{i}.url field must be a string."),
            ),
        }
    }
}

/// `tags`, `tags.*`.
fn tag_errors(input: &Input, errors: &mut Map<String, Value>) {
    let Some(Value::Array(tags)) = input.get("tags") else {
        return;
    };
    for (i, tag) in tags.iter().enumerate() {
        match tag {
            Value::String(s) => {
                if s.chars().count() < 2 {
                    add_error(
                        errors,
                        &format!("tags.{i}"),
                        format!("The tags.{i} field must be at least 2 characters."),
                    );
                }
            }
            _ => add_error(
                errors,
                &format!("tags.{i}"),
                format!("The tags.{i} field must be a string."),
            ),
        }
    }
}

fn add_extra_fields(input: &Input, allowed: &[&str], errors: &mut Map<String, Value>) {
    for k in input.keys() {
        if !allowed.contains(&k.as_str()) {
            add_error(errors, k, NOT_ALLOWED.into());
        }
    }
}

fn fail_if_any(errors: Map<String, Value>) -> Result<(), ApiError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

fn normalize_tag_names(tags: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let text = match t {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let name = strip_tags(Some(&text))
            .trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
            .to_lowercase();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

const CREATE_ALLOWED: &[&str] = &[
    "type",
    "name",
    "description",
    "project_uuid",
    "environment_name",
    "environment_uuid",
    "server_uuid",
    "destination_uuid",
    "instant_deploy",
    "docker_compose_raw",
    "connect_to_docker_network",
    "urls",
    "force_domain_override",
    "is_container_label_escape_enabled",
    "redact_all_env_values_in_logs",
    "tags",
];

fn common_fields() -> Vec<Field> {
    vec![
        field("name", vec![Rule::String, Rule::Max(255)]),
        field("description", vec![Rule::String, Rule::Nullable]),
        field("instant_deploy", vec![Rule::Boolean]),
        field("connect_to_docker_network", vec![Rule::Boolean]),
        field("urls", vec![Rule::Array, Rule::Nullable]),
        field("force_domain_override", vec![Rule::Boolean]),
        field("is_container_label_escape_enabled", vec![Rule::Boolean]),
        field("redact_all_env_values_in_logs", vec![Rule::Boolean]),
    ]
}

/// `isBase64Encoded($value)`, then the UTF-8 check of the controller.
fn decode_compose(value: Option<&str>) -> Result<String, ApiError> {
    let invalid = || errors_with("docker_compose_raw", "The docker_compose_raw should be base64 encoded.");
    let value = value.unwrap_or_default();
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(value).map_err(|_| invalid())?;
    if engine.encode(&bytes) != value {
        return Err(invalid());
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}

fn injection_check(compose: &str) -> Result<(), ApiError> {
    jobs_services::validate_compose(compose)
        .map_err(|e| errors_with_owned("docker_compose_raw", &e))
}

fn errors_with_owned(key: &str, msg: &str) -> ApiError {
    let mut m = Map::new();
    m.insert(key.to_owned(), Value::String(msg.to_owned()));
    ApiError::Validation(m)
}

/// The domains of the response: `$service->applications()->pluck('fqdn')->filter()->sort()->values()`.
async fn domains_of(state: &AppState, id: i64) -> Result<Value, ApiError> {
    let mut list: Vec<String> = svc::subs(&state.db, SubKind::Application, id)
        .await?
        .iter()
        .filter_map(|a| a.str("fqdn").map(str::to_owned))
        .filter(|f| !f.is_empty() && f != "0")
        .collect();
    list.sort();
    Ok(json!(list))
}

/// `domainConflictKey($domain)`.
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

/// `checkIfDomainIsAlreadyUsedViaAPI($domains, $teamId, $uuid)`.
async fn domain_conflicts(
    state: &AppState,
    team_id: i64,
    domains: &[String],
    exclude: &str,
) -> Result<Vec<Value>, ApiError> {
    let wanted: Vec<String> = domains.iter().map(|d| conflict_key(d)).collect();
    let mut conflicts = Vec::new();
    for (uuid, name, fqdn) in coolify_db::applications::fqdns_of_team(&state.db, team_id).await? {
        if uuid == exclude {
            continue;
        }
        for d in fqdn.split(',').filter(|d| !d.is_empty()) {
            let naked = d.trim().strip_suffix('/').unwrap_or(d.trim()).to_owned();
            if wanted.contains(&conflict_key(&naked)) {
                conflicts.push(json!({
                    "domain": naked,
                    "resource_name": name,
                    "resource_uuid": uuid,
                    "resource_type": "application",
                    "message": format!("Domain {naked} is already in use by application '{name}'"),
                }));
            }
        }
    }
    for (uuid, name, fqdn) in svc::application_fqdns_of_team(&state.db, team_id).await? {
        if uuid == exclude {
            continue;
        }
        let Some(fqdn) = fqdn.filter(|f| !f.is_empty()) else {
            continue;
        };
        for d in fqdn.split(',').filter(|d| !d.is_empty()) {
            let naked = d.strip_suffix('/').unwrap_or(d).to_owned();
            if wanted.contains(&conflict_key(&naked)) {
                conflicts.push(json!({
                    "domain": naked,
                    "resource_name": name,
                    "resource_uuid": uuid,
                    "resource_type": "service",
                    "message": format!("Domain {naked} is already in use by service '{name}'"),
                }));
            }
        }
    }
    let (fqdn, _, _) = servers::instance_urls(&state.db).await?;
    if let Some(d) = fqdn.filter(|f| !f.is_empty() && f != "0") {
        let naked = d.strip_suffix('/').unwrap_or(&d).to_owned();
        if wanted.contains(&conflict_key(&naked)) {
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

enum UrlFailure {
    Errors(Vec<String>),
    Conflicts(Vec<Value>),
}

impl UrlFailure {
    fn response(self) -> ApiResult {
        match self {
            UrlFailure::Errors(e) => message(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": "Validation failed.", "errors": e}),
            ),
            UrlFailure::Conflicts(c) => message(
                StatusCode::CONFLICT,
                json!({
                    "message": "Domain conflicts detected. Use force_domain_override=true to proceed.",
                    "conflicts": c,
                    "warning": CONFLICT_WARNING,
                }),
            ),
        }
    }
}

/// `$application->fqdn = $urls; $application->save(); updateCompose($application)`.
async fn save_application_urls(
    state: &AppState,
    s: &ServiceRow,
    app: &SubRow,
    urls: Option<String>,
) -> Result<(), ApiError> {
    let now = state.now();
    let existing = app
        .raw
        .get("domain_port_overrides")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_i64().map(|p| (k.clone(), p)))
                .collect()
        })
        .unwrap_or_default();
    let (fqdn, overrides) = app_domains::normalize_port_overrides(urls.as_deref(), &existing);
    svc::sub_update(
        &state.db,
        SubKind::Application,
        app.id,
        &[("fqdn", ColumnValue::Text(fqdn.clone()))],
        now,
    )
    .await?;
    let json_text = overrides
        .as_ref()
        .map(|o| app_domains::overrides_json(fqdn.as_deref(), o));
    if json_text.is_some() || !existing.is_empty() {
        svc::sub_set_json(
            &state.db,
            SubKind::Application,
            app.id,
            "domain_port_overrides",
            json_text.as_deref(),
            now,
        )
        .await?;
    }
    jobs_of(&state)?
        .update_service_compose(s.id, SubKind::Application, &app.name)
        .await
        .map_err(jobs_error)
}

/// `applyServiceUrls($service, $urlsArray, $teamId, $forceDomainOverride)`.
async fn apply_service_urls(
    state: &AppState,
    s: &ServiceRow,
    urls: &[Value],
    team_id: i64,
    force: bool,
) -> Result<Option<UrlFailure>, ApiError> {
    let mut all: Vec<String> = Vec::new();
    for item in urls {
        if let Some(u) = item.get("url").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) {
            all.extend(
                app_domains::domain_list(Some(u))
                    .into_iter()
                    .map(|d| d.trim().to_owned())
                    .filter(|d| !d.is_empty()),
            );
        }
    }
    let joined = all.join(",");
    let mut errors = app_domains::validate_domains(&joined);
    let normalized = app_domains::domain_list(app_domains::normalize_domains(Some(&joined)).as_deref());
    let mut duplicates: Vec<String> = Vec::new();
    for (i, d) in normalized.iter().enumerate() {
        if normalized[..i].contains(d) && !duplicates.contains(d) {
            duplicates.push(d.clone());
        }
    }
    if !duplicates.is_empty() && !force {
        errors.push(format!(
            "The current request contains conflicting URLs across containers: {}. Use force_domain_override=true to proceed.",
            duplicates.join(", ")
        ));
    }
    if !errors.is_empty() {
        return Ok(Some(UrlFailure::Errors(errors)));
    }
    let mut conflicts = Vec::new();
    for item in urls {
        let name = item.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty());
        let Some(name) = name else {
            errors.push("Service container name is required to apply URLs.".into());
            continue;
        };
        let Some(app) = svc::sub_by_name(&state.db, SubKind::Application, s.id, name).await? else {
            errors.push(format!("Service container with '{name}' not found."));
            continue;
        };
        let raw = item.get("url").and_then(Value::as_str).filter(|u| !u.trim().is_empty());
        let urls = match raw {
            Some(u) => {
                let normalized = app_domains::normalize_domains(Some(u));
                let list = app_domains::domain_list(normalized.as_deref());
                let found = domain_conflicts(state, team_id, &list, &app.uuid).await?;
                if !found.is_empty() && !force {
                    conflicts.extend(found);
                    continue;
                }
                normalized
            }
            None => None,
        };
        save_application_urls(state, s, &app, urls).await?;
    }
    if !errors.is_empty() {
        return Ok(Some(UrlFailure::Errors(errors)));
    }
    if !conflicts.is_empty() {
        return Ok(Some(UrlFailure::Conflicts(conflicts)));
    }
    Ok(None)
}

/// `generateEnvValue` for the `envs` of a template: `SERVICE_x_y` values.
fn template_env_value(value: &str) -> Option<String> {
    if !value.contains("SERVICE_") {
        return Some(value.to_owned());
    }
    let after = value.split_once("SERVICE_").map_or("", |x| x.1);
    let command = after.rsplit_once('_').map_or(after, |x| x.0);
    coolify_jobs::compose_parser::env::generate_env_value(
        command,
        None,
        chrono::Utc::now().timestamp(),
    )
}

/// `POST /services` (`create_service`).
#[allow(clippy::too_many_lines)]
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    require_admin(&ctx)?;
    let probe = lenient_json(&headers, &uri, &body);
    if input_bool(&probe, "instant_deploy") && !can_deploy(&ctx) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let input = validated_json(&headers, &uri, &body)?;
    let mut fields = vec![
        field("type", vec![Rule::String]),
        field("docker_compose_raw", vec![Rule::String]),
        field("project_uuid", vec![Rule::String, Rule::Required]),
        field("environment_name", vec![Rule::String, Rule::Nullable]),
        field("environment_uuid", vec![Rule::String, Rule::Nullable]),
        field("server_uuid", vec![Rule::String, Rule::Required]),
        field("destination_uuid", vec![Rule::String, Rule::Nullable]),
    ];
    fields.extend(common_fields());
    let mut errors = Map::new();
    let missing = |k: &str| !input.has(k) || input.get(k) == Some(&Value::Null);
    if missing("type") && missing("docker_compose_raw") {
        add_error(
            &mut errors,
            "type",
            "The type field is required when docker compose raw is not present.".into(),
        );
        add_error(
            &mut errors,
            "docker_compose_raw",
            "The docker compose raw field is required when type is not present.".into(),
        );
    }
    for (k, v) in field_errors(&input, &fields)? {
        errors.insert(k, v);
    }
    url_errors(&input, &mut errors);
    tag_errors(&input, &mut errors);
    add_extra_fields(&input, CREATE_ALLOWED, &mut errors);
    fail_if_any(errors)?;

    let tag_names = match input.get("tags") {
        Some(Value::Array(tags)) => {
            let names = normalize_tag_names(tags);
            if names.iter().any(|n| n.chars().count() < 2) {
                let mut m = Map::new();
                m.insert(
                    "tags".into(),
                    json!(["Each tag name must be at least 2 characters after sanitization."]),
                );
                return Err(ApiError::Validation(m));
            }
            Some(names)
        }
        _ => None,
    };
    if input.filled("type") && input.filled("docker_compose_raw") {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "You cannot provide both service type and docker_compose_raw. Use one or the other.",
        ));
    }
    let env_uuid = input.str("environment_uuid");
    let env_name = input.str("environment_name");
    if !input.filled("environment_uuid") && !input.filled("environment_name") {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "You need to provide at least one of environment_name or environment_uuid.",
        ));
    }
    let project = coolify_db::projects::find(
        &state.db,
        ctx.team_id,
        input.str("project_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Project not found."))?;
    let environment_id = svc::project_environment(&state.db, project.id, env_name, env_uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment not found.",
        ))?;
    let server = servers::find(
        &state.db,
        ctx.team_id,
        input.str("server_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Server not found."))?;
    let is_build = server
        .settings
        .as_ref()
        .and_then(|s| s.get("is_build_server"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_build {
        let mut m = Map::new();
        m.insert(
            "server_uuid".into(),
            json!(["The specified server is configured as a build server and cannot host resources."]),
        );
        return Err(ApiError::Validation(m));
    }
    let destinations = coolify_db::databases::destinations(&state.db, server.id).await?;
    if destinations.is_empty() {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server has no destinations.",
        ));
    }
    if destinations.len() > 1 && !input.has("destination_uuid") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server has multiple destinations and you do not set destination_uuid.",
        ));
    }
    let destination = if destinations.len() > 1 {
        let wanted = input.str("destination_uuid").unwrap_or_default();
        match destinations.iter().find(|(_, u)| u == wanted) {
            Some(d) => d.0,
            None => {
                return Err(errors_with(
                    "destination_uuid",
                    "Provided destination_uuid does not belong to the specified server.",
                ));
            }
        }
    } else {
        destinations[0].0
    };
    let now = state.now();
    let destination_type = coolify_db::databases::STANDALONE_DOCKER_MORPH.to_owned();
    let flag = |k: &str| input.has(k).then(|| input_bool(&input, k));
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let keys = templates::service_keys();

    let kind = input.str("type").map(templates::resolve_service_template_key);
    let template = kind.as_deref().filter(|k| keys.iter().any(|x| x == k));
    let (service_id, service_uuid, service_type_for_log) = if let Some(kind) = template {
        let Some(compose) = templates::template_compose(kind) else {
            return message(
                StatusCode::NOT_FOUND,
                json!({"message": "Service not found.", "valid_service_types": keys}),
            );
        };
        injection_check(&compose)?;
        let connect = templates::NEEDS_TO_CONNECT_TO_PREDEFINED_NETWORK.contains(&kind)
            || input_bool(&input, "connect_to_docker_network");
        let (id, uuid) = svc::create(
            &state.db,
            &NewService {
                uuid: None,
                name: format!("{kind}-{}", random_str(10)),
                description: None,
                docker_compose_raw: compose,
                environment_id,
                service_type: Some(kind.to_owned()),
                server_id: server.id,
                destination_id: destination,
                destination_type,
                connect_to_docker_network: connect.then_some(true),
                is_container_label_escape_enabled: None,
                redact_all_env_values_in_logs: None,
            },
            "5",
            now,
        )
        .await?;
        let name = match input.str("name") {
            Some(n) => strip_tags(Some(n)),
            None => format!("{kind}-{uuid}"),
        };
        let mut values = vec![
            ("name", ColumnValue::Text(Some(name))),
            (
                "description",
                ColumnValue::Text(Some(strip_tags(input.str("description")))),
            ),
        ];
        if let Some(v) = flag("is_container_label_escape_enabled") {
            values.push(("is_container_label_escape_enabled", ColumnValue::Bool(v)));
        }
        if let Some(v) = flag("redact_all_env_values_in_logs") {
            values.push(("redact_all_env_values_in_logs", ColumnValue::Bool(v)));
        }
        svc::update(&state.db, id, &values, now).await?;
        for line in templates::template_envs(kind) {
            let (key, value) = line.split_once('=').unwrap_or((line.as_str(), line.as_str()));
            let generated = template_env_value(value);
            let new = coolify_db::compose::NewEnv {
                key: key.to_owned(),
                value_encrypted: generated
                    .as_deref()
                    .map(|v| enc.encrypt_serialized(v.trim())),
                is_preview: false,
                is_required: false,
                is_multiline: false,
                is_literal: false,
                is_shown_once: false,
                is_shared: false,
                comment: None,
            };
            coolify_db::compose::env_insert(&state.db, svc::MORPH, id, &new, &state.version, now)
                .await?;
        }
        jobs_of(&state)?
            .parse_new_service(id, flag("is_container_label_escape_enabled").unwrap_or(false))
            .await
            .map_err(parse_error)?;
        apply_prerequisites(&state, id, kind).await?;
        (id, uuid, kind.to_owned())
    } else if input.filled("docker_compose_raw") {
        let mut errors = Map::new();
        add_extra_fields(
            &input,
            &CREATE_ALLOWED[1..],
            &mut errors,
        );
        fail_if_any(errors)?;
        let compose = decode_compose(input.str("docker_compose_raw"))?;
        jobs_services::compose_parses(&compose).map_err(|_| ApiError::Internal)?;
        injection_check(&compose)?;
        let name = match input.str("name") {
            Some(n) => strip_tags(Some(n)),
            None => format!("service-{}", random_str(10)),
        };
        let (id, uuid) = svc::create(
            &state.db,
            &NewService {
                uuid: None,
                name,
                description: Some(strip_tags(input.str("description"))),
                docker_compose_raw: compose,
                environment_id,
                service_type: None,
                server_id: server.id,
                destination_id: destination,
                destination_type,
                connect_to_docker_network: Some(input_bool(&input, "connect_to_docker_network")),
                is_container_label_escape_enabled: flag("is_container_label_escape_enabled"),
                redact_all_env_values_in_logs: flag("redact_all_env_values_in_logs"),
            },
            "5",
            now,
        )
        .await?;
        jobs_of(&state)?
            .parse_new_service(id, flag("is_container_label_escape_enabled").unwrap_or(false))
            .await
            .map_err(parse_error)?;
        (id, uuid, "docker_compose".to_owned())
    } else {
        return message(
            StatusCode::NOT_FOUND,
            json!({"message": "Invalid service type.", "valid_service_types": keys}),
        );
    };
    let fresh = svc::find_by_id(&state.db, service_id)
        .await?
        .ok_or(ApiError::Internal)?;
    if let Some(Value::Array(urls)) = input.get("urls")
        && let Some(failure) = apply_service_urls(
            &state,
            &fresh,
            urls,
            ctx.team_id,
            input_bool(&input, "force_domain_override"),
        )
        .await?
    {
        svc::soft_delete(&state.db, service_id, state.now()).await?;
        return failure.response();
    }
    if let Some(names) = tag_names {
        for name in names {
            svc::attach_tag(&state.db, ctx.team_id, service_id, &name, state.now()).await?;
        }
    }
    if input_bool(&input, "instant_deploy") {
        jobs_of(&state)?.spawn_service_start(service_id, false, false);
    }
    tracing::info!(target: "audit", event = "api.service.created", team_id = ctx.team_id, service_uuid = %service_uuid, service_type = %service_type_for_log, "service created");
    Ok(created(json!({"uuid": service_uuid, "domains": domains_of(&state, service_id).await?})).into_response())
}

fn parse_error(e: String) -> ApiError {
    tracing::error!(error = %e, "service parse failed");
    ApiError::Internal
}

/// `applyServiceApplicationPrerequisites($service)`.
async fn apply_prerequisites(state: &AppState, id: i64, kind: &str) -> Result<(), ApiError> {
    let (column, names): (&'static str, &[&str]) = match kind {
        "beszel" => ("is_gzip_enabled", &["beszel"]),
        "appwrite" => (
            "is_stripprefix_enabled",
            &["appwrite", "appwrite-console", "appwrite-realtime"],
        ),
        _ => return Ok(()),
    };
    for name in names {
        if let Some(app) = svc::sub_by_name(&state.db, SubKind::Application, id, name).await? {
            svc::sub_update(
                &state.db,
                SubKind::Application,
                app.id,
                &[(column, ColumnValue::Bool(false))],
                state.now(),
            )
            .await?;
        }
    }
    Ok(())
}

/// `str()->random($n)`.
pub(crate) fn random_str(n: usize) -> String {
    coolify_jobs::compose_parser::php::str_random(n)
}

/// `PATCH /services/{uuid}` (`update_by_uuid`).
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if input_bool(&input, "instant_deploy") && !can_deploy(&ctx) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let allowed = [
        "name",
        "description",
        "instant_deploy",
        "docker_compose_raw",
        "connect_to_docker_network",
        "urls",
        "force_domain_override",
        "is_container_label_escape_enabled",
        "redact_all_env_values_in_logs",
        "include_coolify_default_variables",
    ];
    let mut fields = common_fields();
    fields.push(field("docker_compose_raw", vec![Rule::String, Rule::Nullable]));
    fields.push(field("include_coolify_default_variables", vec![Rule::Boolean]));
    let mut errors = field_errors(&input, &fields)?;
    url_errors(&input, &mut errors);
    add_extra_fields(&input, &allowed, &mut errors);
    fail_if_any(errors)?;

    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    if input.has("docker_compose_raw") {
        let compose = decode_compose(input.str("docker_compose_raw"))?;
        jobs_services::compose_parses(&compose).map_err(|_| ApiError::Internal)?;
        injection_check(&compose)?;
        values.push(("docker_compose_raw", ColumnValue::Text(Some(compose))));
    }
    if input.has("name") {
        values.push(("name", ColumnValue::Text(Some(strip_tags(input.str("name"))))));
    }
    if input.has("description") {
        values.push((
            "description",
            ColumnValue::Text(Some(strip_tags(input.str("description")))),
        ));
    }
    for key in [
        "connect_to_docker_network",
        "is_container_label_escape_enabled",
        "redact_all_env_values_in_logs",
        "include_coolify_default_variables",
    ] {
        if input.has(key) {
            values.push((key, ColumnValue::Bool(input_bool(&input, key))));
        }
    }
    svc::update(&state.db, s.id, &values, state.now()).await?;
    jobs_of(&state)?.parse_service(s.id).await.map_err(parse_error)?;
    let fresh = svc::find_by_id(&state.db, s.id).await?.ok_or(ApiError::Internal)?;
    if let Some(Value::Array(urls)) = input.get("urls")
        && let Some(failure) = apply_service_urls(
            &state,
            &fresh,
            urls,
            ctx.team_id,
            input_bool(&input, "force_domain_override"),
        )
        .await?
    {
        return failure.response();
    }
    if input_bool(&input, "instant_deploy") {
        jobs_of(&state)?.spawn_service_start(s.id, false, false);
    }
    tracing::info!(target: "audit", event = "api.service.updated", team_id = ctx.team_id, service_uuid = %s.uuid, "service updated");
    Ok(ok(json!({"uuid": s.uuid, "domains": domains_of(&state, s.id).await?})).into_response())
}

/// `DELETE /services/{uuid}` (`delete_by_uuid`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if query_bool(&uri, &input, "delete_from_coolify_only", false) {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The delete_from_coolify_only parameter is no longer supported. Nothing was deleted. Send the request without it: Coolify removes the service also when the server is not reachable.",
        ));
    }
    svc::soft_delete(&state.db, s.id, state.now()).await?;
    jobs_of(&state)?.spawn_service_delete(
        s.clone(),
        query_bool(&uri, &input, "delete_volumes", true),
        query_bool(&uri, &input, "delete_connected_networks", true),
        query_bool(&uri, &input, "delete_configurations", true),
    );
    tracing::info!(target: "audit", event = "api.service.deleted", team_id = ctx.team_id, service_uuid = %s.uuid, "service deleted");
    Ok(ok(json!({"message": "Service deletion request queued."})).into_response())
}

/// `POST /services/{uuid}/start` (`action_deploy`).
pub async fn start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if service_status(&state, &s)
        .await?
        .unwrap_or_default()
        .contains("running")
    {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Service is already running.",
        ));
    }
    jobs_of(&state)?.spawn_service_start(s.id, false, false);
    tracing::info!(target: "audit", event = "api.service.deployed", team_id = ctx.team_id, service_uuid = %s.uuid, "service start queued");
    Ok(ok(json!({"message": "Service starting request queued."})).into_response())
}

/// `POST /services/{uuid}/stop` (`action_stop`).
pub async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let status = service_status(&state, &s).await?.unwrap_or_default();
    if status.contains("stopped") || status.contains("exited") {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Service is already stopped.",
        ));
    }
    jobs_of(&state)?.spawn_service_stop(s.clone());
    tracing::info!(target: "audit", event = "api.service.stopped", team_id = ctx.team_id, service_uuid = %s.uuid, "service stop queued");
    Ok(ok(json!({"message": "Service stopping request queued."})).into_response())
}

/// `POST /services/{uuid}/restart` (`action_restart`).
pub async fn restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let latest = query_bool(&uri, &input, "latest", false);
    jobs_of(&state)?.spawn_service_start(s.id, latest, true);
    tracing::info!(target: "audit", event = "api.service.restarted", team_id = ctx.team_id, service_uuid = %s.uuid, "service restart queued");
    Ok(ok(json!({"message": "Service restarting request queued."})).into_response())
}

// --- logs -------------------------------------------------------------------------------------

/// `normalizeLogLines($lines)`.
fn normalize_log_lines(v: Option<String>) -> String {
    let Some(v) = v else { return "100".into() };
    if v == "all" {
        return v;
    }
    match v.trim().parse::<i64>() {
        Ok(-1) => "all".into(),
        Ok(n) if n < -1 => "100".into(),
        Ok(n) => n.min(10000).to_string(),
        Err(_) => "100".into(),
    }
}

/// `removeAnsiColors($text)`.
fn remove_ansi(text: &str) -> String {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b[\[][A-Za-z0-9];?[0-9]*m?").unwrap_or_else(|_| unreachable!())
    });
    ANSI.replace_all(text, "").into_owned()
}

/// `sanitizeLogsForExport($logs, EnvironmentVariable::logRedactionSecretsFor($service))`.
async fn sanitize_logs(state: &AppState, s: &ServiceRow, logs: &str) -> Result<String, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let rows = svc::env_rows(&state.db, s.id).await?;
    let redact_all = s.raw.get("redact_all_env_values_in_logs") != Some(&Value::Bool(false));
    let (mut known, mut locked): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for r in &rows {
        let Some(value) = env_value(enc, r) else { continue };
        if value.is_empty() {
            continue;
        }
        if r.get("is_shown_once").and_then(Value::as_bool).unwrap_or(false) {
            locked.push(value);
        } else if redact_all {
            known.push(value);
        }
    }
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    let locked: Vec<&str> = locked.iter().map(String::as_str).collect();
    Ok(coolify_compat::redact::redact_text(logs, &known, &locked))
}

/// `GET /services/{uuid}/logs` (`logs_by_uuid`).
pub async fn logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let Some(sub) = query_param(&uri, "sub_service_name").filter(|n| !n.is_empty()) else {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Sub service name is required.",
        ));
    };
    let s = find(&state, &ctx, &uuid).await?;
    let name = format!("{sub}-{}", s.uuid);
    let (_, target) = jobs_of(&state)?.service_target(&s).await.map_err(jobs_error)?;
    let status = jobs_of(&state)?.service_container_status(&target, &name).await;
    if status == "exited" {
        // The owner listing finds no container, or the inspect found none running.
        let found = jobs_of(&state)?.service_container_exists(&target, &s.uuid, &name).await;
        if !found {
            return Err(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Container not found.",
            ));
        }
    }
    if status != "running" {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Container is not running.",
        ));
    }
    let lines = normalize_log_lines(query_param(&uri, "lines"));
    let timestamps = query_param(&uri, "show_timestamps")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"));
    let out = jobs_of(&state)?
        .service_container_logs(&target, &name, &lines, timestamps)
        .await
        .map_err(jobs_error)?;
    let logs = sanitize_logs(&state, &s, &remove_ansi(&out)).await?;
    Ok(ok(json!({"logs": logs})).into_response())
}

// --- environment variables --------------------------------------------------------------------

fn env_value(enc: &coolify_compat::Encrypter, r: &Record) -> Option<String> {
    let raw = r.get("value").and_then(Value::as_str)?;
    if raw.is_empty() || raw == "0" {
        return None;
    }
    enc.decrypt_maybe_serialized(raw)
        .ok()
        .map(|v| v.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']).to_owned())
}

const BUILDPACK_ALLOWED: &[&str] = &[
    "NIXPACKS_PHP_ROOT_DIR",
    "NIXPACKS_PHP_FALLBACK_PATH",
    "NIXPACKS_SPA_OUTPUT_DIR",
    "RAILPACK_SKIP_MIGRATIONS",
];

/// `removeSensitiveData($env)`: the attributes, the appends and the visible values.
async fn present_env(state: &AppState, ctx: &AuthCtx, s: &ServiceRow, r: &Record) -> Result<Value, ApiError> {
    present_env_without(state, ctx, s, r, &[]).await
}

/// [`present_env`] of a model created in this request: attributes it was not given are absent.
async fn present_env_without(
    state: &AppState,
    ctx: &AuthCtx,
    s: &ServiceRow,
    r: &Record,
    absent: &[&str],
) -> Result<Value, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let value = env_value(enc, r);
    let key = r.get("key").and_then(Value::as_str).unwrap_or_default();
    let is_required = r.get("is_required").and_then(Value::as_bool).unwrap_or(false);
    let shown_once = r.get("is_shown_once").and_then(Value::as_bool).unwrap_or(false);
    let owner = coolify_jobs::databases::env::Owner {
        morph: svc::MORPH,
        id: s.id,
        team_id: s.team_id,
        project_id: s.project.get("id").and_then(Value::as_i64),
        environment_id: s.environment.get("id").and_then(Value::as_i64),
    };
    let real = match &value {
        Some(v) if !v.is_empty() => Some(
            coolify_jobs::databases::env::resolve_references(
                &state.db,
                enc,
                &owner,
                s.server_id(),
                v,
            )
            .await
            .map_err(jobs_error)?,
        ),
        other => other.clone(),
    };
    let mut out = Record::new();
    for (k, v) in r {
        match k.as_str() {
            "id" | "resourceable_id" | "resourceable_type" | "uses_legacy_escaping" | "value" => {}
            "created_at" | "updated_at" => {
                let v = v
                    .as_str()
                    .map_or(Value::Null, |t| Value::String(coolify_db::records::laravel_datetime(t)));
                out.insert(k.clone(), v);
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    let is_control = (key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_"))
        && !BUILDPACK_ALLOWED.contains(&key);
    out.insert(
        "is_really_required".into(),
        Value::Bool(is_required && real.as_deref().unwrap_or_default().is_empty()),
    );
    out.insert("is_buildpack_control".into(), Value::Bool(is_control));
    out.insert("is_coolify".into(), Value::Bool(key.starts_with("SERVICE_")));
    if ctx.can_read_sensitive && !shown_once {
        out.insert("value".into(), value.map_or(Value::Null, Value::String));
        out.insert("real_value".into(), real.map_or(Value::Null, Value::String));
    }
    for k in absent {
        out.shift_remove(*k);
    }
    Ok(Value::Object(serialize_record(out)))
}

/// `GET /services/{uuid}/envs`.


pub async fn envs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let mut out = Vec::new();
    for r in svc::env_rows(&state.db, s.id).await? {
        out.push(present_env(&state, &ctx, &s, &r).await?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

fn env_fields() -> Vec<Field> {
    vec![
        field(
            "key",
            vec![
                Rule::Required,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&ENV_KEY),
            ],
        ),
        field("value", vec![Rule::String, Rule::Nullable]),
        field("is_literal", vec![Rule::Boolean]),
        field("is_multiline", vec![Rule::Boolean]),
        field("is_shown_once", vec![Rule::Boolean]),
        field("comment", vec![Rule::String, Rule::Nullable, Rule::Max(256)]),
    ]
}

static ENV_KEY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!())
});

fn env_validate(input: &Input) -> Result<(), ApiError> {
    let keys: Vec<String> = input.keys().cloned().collect();
    let allowed: Vec<&str> = keys.iter().map(String::as_str).collect();
    validate(input, &env_fields(), CUSTOM_API_MESSAGES, &allowed)
}

fn shared_reference(value: Option<&str>) -> bool {
    static SHARED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
            .unwrap_or_else(|_| unreachable!())
    });
    value.is_some_and(|v| SHARED.is_match(v.trim()))
}

/// Creates or updates one variable with the attributes the API accepts.
async fn env_upsert(
    state: &AppState,
    s: &ServiceRow,
    key: &str,
    input: &Input,
    create_defaults: bool,
) -> Result<Record, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let plain = input.str("value").map(|v| v.trim().to_owned());
    let flag = |k: &str| input.has(k).then(|| input_bool(input, k));
    if let Some(row) = svc::env_row_by_key(&state.db, s.id, key).await? {
        let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
        if input.has("value") || create_defaults {
            let old = env_value(enc, &row);
            if old != plain {
                values.push((
                    "value",
                    ColumnValue::Text(plain.as_deref().map(|v| enc.encrypt_serialized(v))),
                ));
            }
            values.push(("is_shared", ColumnValue::Bool(shared_reference(plain.as_deref()))));
        }
        for (k, column) in [
            ("is_literal", "is_literal"),
            ("is_multiline", "is_multiline"),
            ("is_shown_once", "is_shown_once"),
        ] {
            if let Some(v) = flag(k) {
                values.push((column, ColumnValue::Bool(v)));
            }
        }
        if input.has("comment") {
            values.push((
                "comment",
                ColumnValue::Text(input.str("comment").map(str::to_owned)),
            ));
        }
        svc::env_update_columns(&state.db, id, &values, now).await?;
    } else {
        let new = coolify_db::compose::NewEnv {
            key: key.to_owned(),
            value_encrypted: plain.as_deref().map(|v| enc.encrypt_serialized(v)),
            is_preview: false,
            is_required: false,
            is_multiline: flag("is_multiline").unwrap_or(false),
            is_literal: flag("is_literal").unwrap_or(false),
            is_shown_once: flag("is_shown_once").unwrap_or(false),
            is_shared: shared_reference(plain.as_deref()),
            comment: input.str("comment").map(str::to_owned),
        };
        coolify_db::compose::env_insert(&state.db, svc::MORPH, s.id, &new, &state.version, now)
            .await?;
    }
    svc::env_row_by_key(&state.db, s.id, key)
        .await?
        .ok_or(ApiError::Internal)
}

fn env_key_of(input: &Input) -> String {
    input.str("key").unwrap_or_default().trim().replace(' ', "_")
}

/// `POST /services/{uuid}/envs` (`create_env`).
pub async fn create_env(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = lenient_json(&headers, &uri, &body);
    env_validate(&input)?;
    let key = env_key_of(&input);
    if svc::env_row_by_key(&state.db, s.id, &key).await?.is_some() {
        return Err(ApiError::Message(
            StatusCode::CONFLICT,
            "Environment variable already exists. Use PATCH request to update it.",
        ));
    }
    let row = env_upsert(&state, &s, &key, &input, true).await?;
    tracing::info!(target: "audit", event = "api.service.env_created", team_id = ctx.team_id, service_uuid = %s.uuid, env_key = %key, "service variable created");
    let absent = ["is_preview", "is_required", "order", "preserve_compose_null"];
    Ok(created(present_env_without(&state, &ctx, &s, &row, &absent).await?).into_response())
}

/// `PATCH /services/{uuid}/envs` (`update_env_by_uuid`).
pub async fn update_env(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = lenient_json(&headers, &uri, &body);
    env_validate(&input)?;
    let key = env_key_of(&input);
    if svc::env_row_by_key(&state.db, s.id, &key).await?.is_none() {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    }
    // `$env->value = $request->value`: a request without value sets null.
    let row = env_upsert(&state, &s, &key, &input, true).await?;
    tracing::info!(target: "audit", event = "api.service.env_updated", team_id = ctx.team_id, service_uuid = %s.uuid, env_key = %key, "service variable updated");
    Ok(created(present_env(&state, &ctx, &s, &row).await?).into_response())
}

/// `PATCH /services/{uuid}/envs/bulk` (`create_bulk_envs`).
pub async fn bulk_envs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = lenient_json(&headers, &uri, &body);
    let Some(Value::Array(items)) = input.get("data").filter(|v| php_truthy(v)) else {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Bulk data is required.",
        ));
    };
    let mut out = Vec::new();
    for item in items {
        let map = item.as_object().cloned().unwrap_or_default();
        let one = Input(map);
        env_validate(&one)?;
        let key = env_key_of(&one);
        let existed = svc::env_row_by_key(&state.db, s.id, &key).await?.is_some();
        let row = env_upsert(&state, &s, &key, &one, false).await?;
        // `updateOrCreate(['key' => $key], $item)`: a new model has only the attributes of the item.
        let mut absent: Vec<&str> = Vec::new();
        if !existed {
            absent.extend(["is_preview", "is_required", "order", "preserve_compose_null"]);
            for k in ["comment", "is_literal", "is_multiline", "is_shown_once", "value"] {
                if !one.has(k) {
                    absent.push(k);
                }
            }
        }
        out.push(present_env_without(&state, &ctx, &s, &row, &absent).await?);
    }
    tracing::info!(target: "audit", event = "api.service.env_bulk_upserted", team_id = ctx.team_id, service_uuid = %s.uuid, "service variables upserted");
    Ok(created(Value::Array(out)).into_response())
}

/// `DELETE /services/{uuid}/envs/{env_uuid}` (`delete_env_by_uuid`).
pub async fn delete_env(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, env_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let Some(row) = svc::env_row_by_uuid(&state.db, s.id, &env_uuid).await? else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Environment variable not found.",
        ));
    };
    let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
    svc::env_delete(&state.db, id).await?;
    tracing::info!(target: "audit", event = "api.service.env_deleted", team_id = ctx.team_id, service_uuid = %s.uuid, env_uuid = %env_uuid, "service variable deleted");
    Ok(ok(json!({"message": "Environment variable deleted."})).into_response())
}

// --- move, migrate, clone ---------------------------------------------------------------------

/// `POST /services/{uuid}/move` (`moveResourceToEnvironment`).
pub async fn move_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = lenient_json(&headers, &uri, &body);
    let fields = [field("environment_uuid", vec![Rule::Required, Rule::String])];
    let keys: Vec<String> = input.keys().cloned().collect();
    let allowed: Vec<&str> = keys.iter().map(String::as_str).collect();
    validate(&input, &fields, &[], &allowed)?;
    let extras: Map<String, Value> = input
        .keys()
        .filter(|k| *k != "environment_uuid")
        .map(|k| (k.clone(), Value::String(NOT_ALLOWED.into())))
        .collect();
    if !extras.is_empty() {
        return Err(ApiError::Validation(extras));
    }
    let wanted = input.str("environment_uuid").unwrap_or_default();
    let Some((env_id, project_uuid)) =
        svc::environment_of_team(&state.db, ctx.team_id, wanted).await?
    else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Target environment not found or not owned by your team.",
        ));
    };
    if s.int("environment_id") == Some(env_id) {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Service is already in this environment.",
        ));
    }
    svc::update(
        &state.db,
        s.id,
        &[("environment_id", ColumnValue::Int(env_id))],
        state.now(),
    )
    .await?;
    Ok(ok(json!({
        "message": "Service moved successfully.",
        "uuid": s.uuid,
        "project_uuid": project_uuid,
        "environment_uuid": wanted,
    }))
    .into_response())
}

/// `POST /services/{uuid}/migrate`: `migrateResourceToDestination` is `abort(404)` outside dev.
pub async fn migrate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    Err(ApiError::Abort(StatusCode::NOT_FOUND, ""))
}

/// `POST /services/{uuid}/clone` (`clone_by_uuid`, `clone_service()`).
pub async fn clone_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let fields = [
        field("destination_uuid", vec![Rule::Required, Rule::String]),
        field("name", vec![Rule::String, Rule::Max(255), Rule::Nullable]),
        field("clone_volumes", vec![Rule::Boolean]),
    ];
    validate(
        &input,
        &fields,
        CUSTOM_API_MESSAGES,
        &["destination_uuid", "name", "clone_volumes"],
    )?;
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let clone_volumes = input_bool(&input, "clone_volumes");
    if clone_volumes
        && let Some(r) = deploy_forbidden(
            &ctx,
            "Cloning volume data stops and starts the source resource, so it needs a token with the deploy permission.",
        )
    {
        return Ok(r);
    }
    let destination = svc::team_destination_by_uuid(
        &state.db,
        ctx.team_id,
        input.str("destination_uuid").unwrap_or_default(),
    )
    .await?;
    let not_found = ApiError::Message(StatusCode::NOT_FOUND, "Destination not found.");
    let Some((dest_id, morph, server_id)) = destination else {
        return Err(not_found);
    };
    let server = servers::find_by_id(&state.db, server_id).await?;
    let hosts = server.as_ref().is_some_and(|srv| {
        !srv.settings
            .as_ref()
            .and_then(|x| x.get("is_build_server"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    if !hosts {
        return Err(not_found);
    }
    if clone_volumes {
        // VolumeCloneJob is not ported.
        return Err(ApiError::NotPorted);
    }
    let new_uuid = coolify_domain::new_public_id();
    let name = match input.str("name").filter(|n| !n.is_empty()) {
        Some(n) => strip_tags(Some(n)),
        None => format!("{}-clone-{new_uuid}", s.name()),
    };
    let now = state.now();
    let new_id = svc::replicate(&state.db, s.id, &new_uuid, &name, dest_id, morph, server_id, now).await?;
    svc::replicate_children(&state.db, s.id, new_id, ctx.team_id, now).await?;
    jobs_of(&state)?.parse_service(new_id).await.map_err(parse_error)?;
    for kind in [SubKind::Application, SubKind::Database] {
        for sub in svc::subs(&state.db, kind, new_id).await? {
            svc::sub_set_status(&state.db, kind, sub.id, "exited", state.now()).await?;
        }
    }
    tracing::info!(target: "audit", event = "api.service.cloned", team_id = ctx.team_id, source_uuid = %s.uuid, service_uuid = %new_uuid, "service cloned");
    Ok(created(json!({"uuid": new_uuid, "message": "Service cloned."})).into_response())
}

// --- tags -------------------------------------------------------------------------------------

fn present_tag(r: &Record) -> Value {
    let date = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .map_or(Value::Null, |t| Value::String(coolify_db::records::laravel_datetime(t)))
    };
    json!({
        "uuid": r.get("uuid"),
        "name": r.get("name"),
        "created_at": date("created_at"),
        "updated_at": date("updated_at"),
    })
}

async fn tags_json(state: &AppState, id: i64) -> Result<Value, ApiError> {
    Ok(Value::Array(
        svc::tags_of(&state.db, id).await?.iter().map(present_tag).collect(),
    ))
}

/// `GET /services/{uuid}/tags`.
pub async fn tags(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    Ok(ok(tags_json(&state, s.id).await?).into_response())
}

/// `POST /services/{uuid}/tags` (`createTag`).
pub async fn create_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if input.has("tag_name") && input.has("tag_names") {
        let mut m = Map::new();
        m.insert(
            "tag_name".into(),
            json!(["Provide either tag_name or tag_names, not both."]),
        );
        return Err(ApiError::Validation(m));
    }
    let mut errors = Map::new();
    let missing = |k: &str| !input.has(k) || input.get(k) == Some(&Value::Null);
    if missing("tag_name") && missing("tag_names") {
        add_error(
            &mut errors,
            "tag_name",
            "The tag name field is required when tag names is not present.".into(),
        );
        add_error(
            &mut errors,
            "tag_names",
            "The tag names field is required when tag name is not present.".into(),
        );
    }
    if input.get("tag_name").is_some_and(|v| !v.is_string() && !v.is_null()) {
        add_error(&mut errors, "tag_name", "The tag name field must be a string.".into());
    }
    match input.get("tag_names") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            if items.is_empty() {
                add_error(
                    &mut errors,
                    "tag_names",
                    "The tag names field must have at least 1 items.".into(),
                );
            }
            for (i, v) in items.iter().enumerate() {
                if !v.is_string() {
                    add_error(
                        &mut errors,
                        &format!("tag_names.{i}"),
                        format!("The tag_names.{i} field must be a string."),
                    );
                }
            }
        }
        Some(_) => add_error(&mut errors, "tag_names", "The tag names field must be an array.".into()),
    }
    add_extra_fields(&input, &["tag_name", "tag_names"], &mut errors);
    fail_if_any(errors)?;
    let raw: Vec<Value> = match input.get("tag_names") {
        Some(Value::Array(items)) if input.has("tag_names") => items.clone(),
        _ => vec![input.get("tag_name").cloned().unwrap_or(Value::Null)],
    };
    let names = normalize_tag_names(&raw);
    if names.iter().any(|n| n.chars().count() < 2) {
        let mut m = Map::new();
        m.insert(
            "tag_name".into(),
            json!(["Each tag name must be at least 2 characters after sanitization."]),
        );
        return Err(ApiError::Validation(m));
    }
    for name in names {
        svc::attach_tag(&state.db, ctx.team_id, s.id, &name, state.now()).await?;
    }
    Ok(created(tags_json(&state, s.id).await?).into_response())
}

/// `DELETE /services/{uuid}/tags/{tag_uuid}` (`deleteTag`).
pub async fn delete_tag(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, tag_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let Some(tag) = svc::tag_id(&state.db, ctx.team_id, &tag_uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Tag not found."));
    };
    if !svc::has_tag(&state.db, s.id, tag).await? {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Tag not found on resource.",
        ));
    }
    svc::detach_tag(&state.db, s.id, tag).await?;
    Ok(ok(json!({"message": "Tag removed."})).into_response())
}

// --- storages ---------------------------------------------------------------------------------

fn storage_times(r: &mut Record) {
    for k in ["created_at", "updated_at"] {
        if let Some(Value::String(t)) = r.get(k) {
            let v = coolify_db::records::laravel_datetime(t);
            r.insert(k.into(), Value::String(v));
        }
    }
}

/// `LocalPersistentVolume::toArray()`.
fn persistent_array(r: &Record) -> Record {
    let mut out = r.clone();
    storage_times(&mut out);
    out
}

/// `LocalFileVolume::toArray()` with `content` visible when the token may read it.
fn file_array(state: &AppState, ctx: &AuthCtx, r: &Record) -> Record {
    let mut out = r.clone();
    storage_times(&mut out);
    let plain = out
        .get("content")
        .and_then(Value::as_str)
        .and_then(|c| state.encrypter.as_ref().and_then(|e| e.decrypt_string(c).ok()));
    if ctx.can_read_sensitive {
        out.insert(
            "content".into(),
            plain.clone().map_or(Value::Null, Value::String),
        );
    } else {
        out.shift_remove("content");
    }
    out.insert(
        "is_binary".into(),
        Value::Bool(plain.as_deref() == Some("[binary file]")),
    );
    out.insert(
        "is_too_large".into(),
        Value::Bool(plain.as_deref() == Some("[file too large to display]")),
    );
    out
}

/// `GET /services/{uuid}/storages`.
pub async fn storages(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    let mut persistent: Vec<Record> = Vec::new();
    let mut files: Vec<Record> = Vec::new();
    for kind in [SubKind::Application, SubKind::Database] {
        for sub in svc::subs(&state.db, kind, s.id).await? {
            let tag = |mut r: Record| {
                r.insert("resource_uuid".into(), Value::String(sub.uuid.clone()));
                r.insert("resource_type".into(), Value::String(kind.sub_type().into()));
                r
            };
            for r in svc::persistent_storages(&state.db, kind, sub.id).await? {
                persistent.push(tag(persistent_array(&r)));
            }
            for r in svc::file_storages(&state.db, kind, sub.id).await? {
                files.push(tag(file_array(&state, &ctx, &r)));
            }
        }
    }
    let id = |r: &Record| r.get("id").and_then(Value::as_i64).unwrap_or_default();
    persistent.sort_by_key(id);
    files.sort_by_key(id);
    Ok(ok(json!({"persistent_storages": persistent, "file_storages": files})).into_response())
}

async fn sub_by_uuid_any(
    state: &AppState,
    service_id: i64,
    uuid: &str,
) -> Result<Option<SubRow>, ApiError> {
    if let Some(a) = svc::sub_by_uuid(&state.db, SubKind::Application, service_id, uuid).await? {
        return Ok(Some(a));
    }
    Ok(svc::sub_by_uuid(&state.db, SubKind::Database, service_id, uuid).await?)
}

fn invalid_for(fields: &[&str], kind: &str) -> ApiError {
    let mut m = Map::new();
    for f in fields {
        m.insert(
            (*f).to_owned(),
            Value::String(format!("Field '{f}' is not valid for type '{kind}'.")),
        );
    }
    ApiError::Validation(m)
}

static VOLUME_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap_or_else(|_| unreachable!())
});
const STORAGE_TYPES: &[&str] = &["persistent", "file"];

/// `validateFileMountPath($path, $context)`.
fn validate_mount_path(path: &str, context: &str) -> Result<String, String> {
    let p = path.trim();
    if p.is_empty() {
        return Err(format!("Invalid {context}: path is empty."));
    }
    let with_slash = if p.starts_with('/') { p.to_owned() } else { format!("/{p}") };
    storage::normalize_unix_path(&with_slash).map_err(|e| e.replace("path", context))
}

/// `POST /services/{uuid}/storages` (`create_storage`).
#[allow(clippy::too_many_lines)]
pub async fn create_storage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let fields = [
        field("type", vec![Rule::Required, Rule::String, Rule::In(STORAGE_TYPES)]),
        field("resource_uuid", vec![Rule::Required, Rule::String]),
        field("name", vec![Rule::String, Rule::Regex(&VOLUME_NAME)]),
        field("mount_path", vec![Rule::Required, Rule::String]),
        field("content", vec![Rule::String, Rule::Nullable]),
        field("is_directory", vec![Rule::Boolean]),
        field("is_host_file", vec![Rule::Boolean]),
        field("fs_path", vec![Rule::String]),
    ];
    validate(
        &input,
        &fields,
        CUSTOM_API_MESSAGES,
        &[
            "type",
            "resource_uuid",
            "name",
            "mount_path",
            "content",
            "is_directory",
            "is_host_file",
            "fs_path",
        ],
    )?;
    let Some(sub) = sub_by_uuid_any(
        &state,
        s.id,
        input.str("resource_uuid").unwrap_or_default(),
    )
    .await?
    else {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Service resource not found.",
        ));
    };
    let now = state.now();
    let mount = input.str("mount_path").unwrap_or_default();
    if input.str("type") == Some("persistent") {
        let Some(name) = input.str("name").filter(|n| !n.is_empty()) else {
            return Err(errors_with(
                "name",
                "The name field is required for persistent storages.",
            ));
        };
        let invalid: Vec<&str> = ["content", "is_directory", "is_host_file", "fs_path"]
            .into_iter()
            .filter(|f| input.has(f))
            .collect();
        if !invalid.is_empty() {
            return Err(invalid_for(&invalid, "persistent"));
        }
        let row = svc::persistent_storage_create(
            &state.db,
            sub.kind.morph(),
            sub.id,
            &format!("{}-{name}", sub.uuid),
            mount,
            now,
        )
        .await?;
        return Ok(created(Value::Object(persistent_array(&row))).into_response());
    }
    let invalid: Vec<&str> = ["name", "host_path"]
        .into_iter()
        .filter(|f| input.has(f))
        .collect();
    if !invalid.is_empty() {
        return Err(invalid_for(&invalid, "file"));
    }
    let is_directory = input_bool(&input, "is_directory");
    let is_host_file = input_bool(&input, "is_host_file");
    if is_directory && is_host_file {
        return Err(errors_with(
            "is_host_file",
            "Host file mounts cannot also be directory mounts.",
        ));
    }
    let dir = jobs_services::workdir(&s.uuid);
    let (_, target) = jobs_of(&state)?.service_target(&s).await.map_err(jobs_error)?;
    let fail = |k: &str, e: String| errors_with_owned(k, &e);
    let outside_forbidden = |fs: &str| -> Option<axum::response::Response> {
        if storage::is_outside_resource_directory(fs, &dir) {
            deploy_forbidden(
                &ctx,
                "A mount outside the resource directory needs a token with the deploy permission.",
            )
        } else {
            None
        }
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let new = if is_directory {
        let Some(fs) = input.str("fs_path").filter(|f| !f.is_empty()) else {
            return Err(errors_with(
                "fs_path",
                "The fs_path field is required for directory mounts.",
            ));
        };
        let fs_path = storage::resolve_host_path(&dir, fs, "storage source path")
            .map_err(|e| fail("fs_path", e))?;
        let mount_path = validate_mount_path(mount, "storage destination path")
            .map_err(|e| fail("fs_path", e))?;
        if let Some(r) = outside_forbidden(&fs_path) {
            return Ok(r);
        }
        storage::assert_host_path_on_server(jobs_of(&state)?, &target, &dir, &fs_path, true)
            .await
            .map_err(|e| fail("fs_path", e))?;
        NewFileStorage {
            fs_path,
            mount_path,
            content_encrypted: None,
            is_directory: true,
            is_host_file: false,
        }
    } else if is_host_file {
        let Some(fs) = input.str("fs_path").filter(|f| !f.is_empty()) else {
            return Err(errors_with(
                "fs_path",
                "The fs_path field is required for host file mounts.",
            ));
        };
        if input.filled("content") {
            return Err(errors_with(
                "content",
                "Content is not valid for host file mounts.",
            ));
        }
        let fs_path = validate_mount_path(fs, "host file source path")
            .map_err(|e| fail("mount_path", e))?;
        let mount_path = validate_mount_path(mount, "host file destination path")
            .map_err(|e| fail("mount_path", e))?;
        NewFileStorage {
            fs_path,
            mount_path,
            content_encrypted: None,
            is_directory: false,
            is_host_file: true,
        }
    } else {
        let mount_path = validate_mount_path(mount, "file storage path")
            .map_err(|e| fail("mount_path", e))?;
        let fs_path = if let Some(fs) = input.str("fs_path").filter(|f| !f.is_empty()) {
            let resolved = storage::resolve_host_path(&dir, fs, "file storage source path")
                .map_err(|e| fail("fs_path", e))?;
            if let Some(r) = outside_forbidden(&resolved) {
                return Ok(r);
            }
            storage::assert_host_path_on_server(jobs_of(&state)?, &target, &dir, &resolved, false)
                .await
                .map_err(|e| fail("fs_path", e))?;
            resolved
        } else {
            // confineFileMountPath($dir, $mountPath, ...)
            format!("{dir}{mount_path}")
        };
        if input.filled("content")
            && let Some(r) = deploy_forbidden(
                &ctx,
                "Coolify writes the file content on the server immediately, so it needs a token with the deploy permission.",
            )
        {
            return Ok(r);
        }
        NewFileStorage {
            fs_path,
            mount_path,
            content_encrypted: input.str("content").map(|c| enc.encrypt_string(c)),
            is_directory: false,
            is_host_file: false,
        }
    };
    let row = svc::file_storage_create(&state.db, sub.kind.morph(), sub.id, &new, now).await?;
    let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
    // The `created` hook: ServerStorageSaveJob, unless it is a host file or outside the directory.
    if !new.is_host_file && !storage::is_outside_resource_directory(&new.fs_path, &dir) {
        let jobs = jobs_of(&state)?.clone();
        let service = s.clone();
        tokio::spawn(async move {
            let _ = storage::save_storage_on_server(&jobs, &service, &target, id).await;
        });
    }
    tracing::info!(target: "audit", event = "api.service.storage_created", team_id = ctx.team_id, service_uuid = %s.uuid, storage_id = id, "service storage created");
    Ok(created(Value::Object(file_array(&state, &ctx, &row))).into_response())
}

/// The storage with this column value among the parts of a service.
async fn find_storage(
    state: &AppState,
    service_id: i64,
    persistent: bool,
    column: &str,
    value: &Value,
) -> Result<Option<Record>, ApiError> {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    for kind in [SubKind::Application, SubKind::Database] {
        for sub in svc::subs(&state.db, kind, service_id).await? {
            let rows = if persistent {
                svc::persistent_storages(&state.db, kind, sub.id).await?
            } else {
                svc::file_storages(&state.db, kind, sub.id).await?
            };
            for r in rows {
                let current = match r.get(column) {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                if current == text {
                    return Ok(Some(r));
                }
            }
        }
    }
    Ok(None)
}

/// `PATCH /services/{uuid}/storages` (`update_storage`). Every storage of a service part is
/// read-only (`shouldBeReadOnlyInUI()`), so only `is_preview_suffix_enabled` changes.
pub async fn update_storage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let fields = [
        field("uuid", vec![Rule::String]),
        field("id", vec![Rule::Integer]),
        field("type", vec![Rule::Required, Rule::String, Rule::In(STORAGE_TYPES)]),
        field("is_preview_suffix_enabled", vec![Rule::Boolean]),
        field("name", vec![Rule::String, Rule::Regex(&VOLUME_NAME)]),
        field("mount_path", vec![Rule::String]),
        field("content", vec![Rule::String, Rule::Nullable]),
    ];
    validate(
        &input,
        &fields,
        CUSTOM_API_MESSAGES,
        &[
            "uuid",
            "id",
            "type",
            "is_preview_suffix_enabled",
            "name",
            "mount_path",
            "content",
        ],
    )?;
    let (column, value) = match (input.get("uuid").filter(|v| php_truthy(v)), input.get("id").filter(|v| php_truthy(v))) {
        (Some(v), _) => ("uuid", v.clone()),
        (None, Some(v)) => ("id", v.clone()),
        (None, None) => return Err(errors_with("uuid", "Either uuid or id is required.")),
    };
    let persistent = input.str("type") == Some("persistent");
    let Some(row) = find_storage(&state, s.id, persistent, column, &value).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Storage not found."));
    };
    let editable: Vec<&str> = ["name", "mount_path", "content"]
        .into_iter()
        .filter(|f| input.has(f))
        .collect();
    if !editable.is_empty() {
        return message(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "This storage is read-only (managed by docker-compose or service definition). Only is_preview_suffix_enabled can be updated.",
                "read_only_fields": editable,
            }),
        );
    }
    let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
    let table = if persistent {
        "local_persistent_volumes"
    } else {
        "local_file_volumes"
    };
    let values: Vec<(&'static str, ColumnValue)> = if input.has("is_preview_suffix_enabled") {
        vec![(
            "is_preview_suffix_enabled",
            ColumnValue::Bool(input_bool(&input, "is_preview_suffix_enabled")),
        )]
    } else {
        Vec::new()
    };
    let fresh = svc::storage_update(&state.db, table, id, &values, state.now())
        .await?
        .ok_or(ApiError::Internal)?;
    tracing::info!(target: "audit", event = "api.service.storage_updated", team_id = ctx.team_id, service_uuid = %s.uuid, storage_id = id, "service storage updated");
    let out = if persistent {
        persistent_array(&fresh)
    } else {
        file_array(&state, &ctx, &fresh)
    };
    Ok(ok(Value::Object(out)).into_response())
}

/// `DELETE /services/{uuid}/storages/{storage_uuid}` (`delete_storage`): read-only for service parts.
pub async fn delete_storage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, storage_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let needle = Value::String(storage_uuid);
    let found = match find_storage(&state, s.id, true, "uuid", &needle).await? {
        Some(r) => Some(r),
        None => find_storage(&state, s.id, false, "uuid", &needle).await?,
    };
    if found.is_none() {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Storage not found."));
    }
    Err(ApiError::Message(
        StatusCode::UNPROCESSABLE_ENTITY,
        "This storage is read-only (managed by docker-compose or service definition) and cannot be deleted.",
    ))
}

// --- service applications ---------------------------------------------------------------------

fn present_application(sub: &SubRow) -> Value {
    let mut r = application_array(sub);
    for k in ["id"] {
        r.shift_remove(k);
    }
    Value::Object(serialize_record(r))
}

/// `GET /services/{uuid}/applications`.
pub async fn applications(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    let items: Vec<Value> = svc::subs(&state.db, SubKind::Application, s.id)
        .await?
        .iter()
        .map(present_application)
        .collect();
    Ok(ok(Value::Array(items)).into_response())
}

async fn find_app(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    app_uuid: &str,
) -> Result<(ServiceRow, SubRow), ApiError> {
    let s = find(state, ctx, uuid).await?;
    let app = svc::sub_by_uuid(&state.db, SubKind::Application, s.id, app_uuid)
        .await?
        .ok_or(APP_NOT_FOUND)?;
    Ok((s, app))
}

/// `GET /services/{uuid}/applications/{app_uuid}`.
pub async fn application(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (_, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    Ok(ok(present_application(&app)).into_response())
}

const REDIRECTS: &[&str] = &["www", "non-www", "both"];
static RESOLVER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9_-]+$").unwrap_or_else(|_| unreachable!())
});

/// `filter_var($v, FILTER_VALIDATE_BOOLEAN)`.
fn filter_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        _ => false,
    }
}

/// `PATCH /services/{uuid}/applications/{app_uuid}` (`UpdateServiceApplicationFromApi`).
#[allow(clippy::too_many_lines)]
pub async fn update_application(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let (s, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    require_admin(&ctx)?;
    let fields = [
        field("url", vec![Rule::Nullable, Rule::String, Rule::Max(2048), Rule::CustomOwned(domain_rule)]),
        field("noindex_domains", vec![Rule::Array, Rule::Nullable]),
        field("human_name", vec![Rule::Nullable, Rule::String, Rule::Max(255)]),
        field("description", vec![Rule::Nullable, Rule::String]),
        field("image", vec![Rule::Nullable, Rule::String]),
        field("exclude_from_status", vec![Rule::Boolean]),
        field("is_log_drain_enabled", vec![Rule::Boolean]),
        field("is_gzip_enabled", vec![Rule::Boolean]),
        field("traefik_certificate_resolver", vec![Rule::Nullable, Rule::String, Rule::Max(255), Rule::Regex(&RESOLVER)]),
        field("is_stripprefix_enabled", vec![Rule::Boolean]),
        field("is_force_https_enabled", vec![Rule::Boolean]),
        field("is_permanent_redirect_enabled", vec![Rule::Boolean]),
        field("redirect", vec![Rule::In(REDIRECTS)]),
        field("max_restart_count", vec![Rule::Integer, Rule::MinNum(0)]),
    ];
    validate(
        &input,
        &fields,
        &[],
        &[
            "url",
            "noindex_domains",
            "human_name",
            "description",
            "image",
            "exclude_from_status",
            "is_log_drain_enabled",
            "is_gzip_enabled",
            "traefik_certificate_resolver",
            "is_stripprefix_enabled",
            "is_force_https_enabled",
            "is_permanent_redirect_enabled",
            "redirect",
            "max_restart_count",
        ],
    )?;
    let force = input_bool(&input, "force_domain_override");
    let mut fqdn = app.str("fqdn").map(str::to_owned);
    let mut new_urls: Option<Option<String>> = None;
    if input.has("url") {
        let raw = input.str("url").filter(|u| !u.is_empty());
        let normalized = match raw {
            None => None,
            Some(u) => {
                let list = app_domains::domain_list(Some(u));
                let mut errors = app_domains::validate_domains(&list.join(","));
                let mut duplicates: Vec<String> = Vec::new();
                for (i, d) in list.iter().enumerate() {
                    if list[..i].contains(d) && !duplicates.contains(d) {
                        duplicates.push(d.clone());
                    }
                }
                if !duplicates.is_empty() && !force {
                    errors.push(format!(
                        "The current request contains duplicate URLs: {}. Use force_domain_override=true to proceed.",
                        duplicates.join(", ")
                    ));
                }
                if !errors.is_empty() {
                    return UrlFailure::Errors(errors).response();
                }
                let mut unique: Vec<String> = Vec::new();
                for d in list {
                    let lower = d.to_lowercase();
                    if !unique.contains(&lower) {
                        unique.push(lower);
                    }
                }
                let joined = unique.join(",");
                (!joined.is_empty()).then_some(joined)
            }
        };
        if let Some(n) = &normalized {
            let list: Vec<String> = n.split(',').map(str::to_owned).collect();
            let found = domain_conflicts(&state, ctx.team_id, &list, &app.uuid).await?;
            if !found.is_empty() && !force {
                return UrlFailure::Conflicts(found).response();
            }
        }
        fqdn.clone_from(&normalized);
        new_urls = Some(normalized);
    }
    if input.has("redirect")
        && matches!(input.str("redirect"), Some("www" | "non-www"))
        && !has_www_and_non_www(fqdn.as_deref())
    {
        let mut m = Map::new();
        m.insert(
            "redirect".into(),
            json!(["The www and non-www redirects need both a www and a non-www domain in url."]),
        );
        return Err(ApiError::Validation(m));
    }
    let server = server_of(&state, &s).await?;
    if input.has("is_log_drain_enabled")
        && filter_bool(input.get("is_log_drain_enabled"))
        && !server
            .as_ref()
            .and_then(|x| x.settings.clone())
            .is_some_and(|x| {
                coolify_jobs::compose_parser::service::is_log_drain_enabled(&Value::Object(x))
            })
    {
        let mut m = Map::new();
        m.insert(
            "is_log_drain_enabled".into(),
            json!(["Log drain is not enabled on the server for this service."]),
        );
        return Err(ApiError::Validation(m));
    }
    let now = state.now();
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for (k, column) in [("human_name", "human_name"), ("description", "description"), ("image", "image"), ("redirect", "redirect")] {
        if input.has(k) {
            values.push((column, ColumnValue::Text(input.str(k).map(str::to_owned))));
        }
    }
    for k in [
        "exclude_from_status",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "is_force_https_enabled",
        "is_permanent_redirect_enabled",
        "is_log_drain_enabled",
    ] {
        if input.has(k) {
            values.push((k, ColumnValue::Bool(filter_bool(input.get(k)))));
        }
    }
    if input.has("traefik_certificate_resolver") {
        values.push((
            "traefik_certificate_resolver",
            ColumnValue::Text(
                input
                    .str("traefik_certificate_resolver")
                    .filter(|v| !v.trim().is_empty())
                    .map(str::to_owned),
            ),
        ));
    }
    if let Some(n) = input.get("max_restart_count").and_then(Value::as_i64) {
        values.push(("max_restart_count", ColumnValue::Int(n)));
        values.push(("restart_limit_reached", ColumnValue::Bool(false)));
    }
    svc::sub_update(&state.db, SubKind::Application, app.id, &values, now).await?;
    if input.has("noindex_domains") {
        let list = app_domains::domain_list(fqdn.as_deref());
        let normalize = |d: &str| app_domains::without_port(&app_domains::normalize_url(d));
        let current: Vec<String> = list.iter().map(|d| normalize(d)).collect();
        let mut out: Vec<String> = Vec::new();
        if let Some(Value::Array(items)) = input.get("noindex_domains") {
            for d in items.iter().filter_map(Value::as_str).filter(|d| !d.trim().is_empty()) {
                let n = normalize(d);
                if current.contains(&n) && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        svc::sub_set_json(
            &state.db,
            SubKind::Application,
            app.id,
            "noindex_domains",
            Some(&json!(out).to_string()),
            now,
        )
        .await?;
    }
    if let Some(urls) = new_urls {
        save_application_urls(&state, &s, &app, urls).await?;
    } else {
        jobs_of(&state)?
            .update_service_compose(s.id, SubKind::Application, &app.name)
            .await
            .map_err(jobs_error)?;
    }
    let fresh = svc::sub_by_uuid(&state.db, SubKind::Application, s.id, &app.uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    tracing::info!(target: "audit", event = "api.service_application.updated", team_id = ctx.team_id, service_uuid = %s.uuid, service_application_uuid = %app.uuid, "service application updated");
    Ok(ok(present_application(&fresh)).into_response())
}

fn domain_rule(v: &Value) -> Option<String> {
    let errors = app_domains::validate_domains(v.as_str()?);
    errors.into_iter().next()
}

/// `hasWwwAndNonWwwDomains($fqdn)`.
fn has_www_and_non_www(fqdn: Option<&str>) -> bool {
    let hosts: Vec<String> = fqdn
        .unwrap_or_default()
        .split(',')
        .filter_map(|u| app_domains::UrlParts::parse(u.trim()))
        .map(|p| p.host.to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    hosts.iter().any(|h| h.starts_with("www.")) && hosts.iter().any(|h| !h.starts_with("www."))
}

/// The checks the part actions share: admin, swarm, functional server.
async fn part_guard(state: &AppState, s: &ServiceRow) -> Result<(), ApiError> {
    let server = server_of(state, s).await?;
    let settings = server.as_ref().and_then(|x| x.settings.clone()).unwrap_or_default();
    let swarm = settings.get("is_swarm_manager").and_then(Value::as_bool).unwrap_or(false)
        || settings.get("is_swarm_worker").and_then(Value::as_bool).unwrap_or(false);
    if swarm {
        return Err(ApiError::Message(
            StatusCode::NOT_IMPLEMENTED,
            "This operation is not supported for Swarm servers yet.",
        ));
    }
    if !server.as_ref().is_some_and(server_functional) {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Server is not functional.",
        ));
    }
    Ok(())
}

async fn part_logs(
    state: &AppState,
    s: &ServiceRow,
    sub: &SubRow,
    uri: &Uri,
    not_running: &'static str,
) -> ApiResult {
    part_guard(state, s).await?;
    let name = format!("{}-{}", sub.name, s.uuid);
    let (_, target) = jobs_of(&state)?.service_target(s).await.map_err(jobs_error)?;
    if jobs_of(&state)?.service_container_status(&target, &name).await != "running" {
        return Err(ApiError::Message(StatusCode::BAD_REQUEST, not_running));
    }
    let lines = normalize_log_lines(query_param(uri, "lines"));
    let out = jobs_of(&state)?
        .service_container_logs(&target, &name, &lines, false)
        .await
        .map_err(jobs_error)?;
    let logs = sanitize_logs(state, s, &remove_ansi(&out)).await?;
    Ok(ok(json!({"logs": logs})).into_response())
}

/// `GET|POST /services/{uuid}/applications/{app_uuid}/logs`.
pub async fn application_logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult {
    let (s, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    part_logs(&state, &s, &app, &uri, "Service application container is not running.").await
}

/// `POST /services/{uuid}/applications/{app_uuid}/start`.
pub async fn application_start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let (s, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    let input = lenient_json(&headers, &uri, &body);
    jobs_of(&state)?.spawn_sub_deploy(
        s.id,
        SubKind::Application,
        app.name.clone(),
        query_bool(&uri, &input, "latest", false),
        query_bool(&uri, &input, "force", false),
    );
    Ok(ok(json!({"message": "Service application deploy request queued."})).into_response())
}

/// `POST /services/{uuid}/applications/{app_uuid}/restart`.
pub async fn application_restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (s, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    jobs_of(&state)?.spawn_sub_restart(s, app);
    Ok(ok(json!({"message": "Service application restart request queued."})).into_response())
}

/// `POST /services/{uuid}/applications/{app_uuid}/stop`.
pub async fn application_stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, app_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (s, app) = find_app(&state, &ctx, &uuid, &app_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    jobs_of(&state)?.spawn_sub_stop(s, app);
    Ok(ok(json!({"message": "Service application stop request queued."})).into_response())
}

// --- service databases ------------------------------------------------------------------------

fn present_database(sub: &SubRow) -> Value {
    let mut r = SERVICE_DATABASE.present(sub.raw.clone());
    r.shift_remove("id");
    r.shift_remove("service_id");
    Value::Object(serialize_record(r))
}

/// `GET /services/{uuid}/databases`.
pub async fn databases(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    let items: Vec<Value> = svc::subs(&state.db, SubKind::Database, s.id)
        .await?
        .iter()
        .map(present_database)
        .collect();
    Ok(ok(Value::Array(items)).into_response())
}

async fn find_db(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    database_uuid: &str,
) -> Result<(ServiceRow, SubRow), ApiError> {
    let s = find(state, ctx, uuid).await?;
    let d = svc::sub_by_uuid(&state.db, SubKind::Database, s.id, database_uuid)
        .await?
        .ok_or(DB_NOT_FOUND)?;
    Ok((s, d))
}

/// `GET /services/{uuid}/databases/{database_uuid}`.
pub async fn database(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (_, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    Ok(ok(present_database(&d)).into_response())
}

/// `PATCH /services/{uuid}/databases/{database_uuid}`. Starting or stopping the database proxy is
/// not ported: such a change answers 501.
pub async fn update_database(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let (s, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    require_admin(&ctx)?;
    let fields = [
        field("human_name", vec![Rule::Nullable, Rule::String, Rule::Max(255)]),
        field("description", vec![Rule::Nullable, Rule::String]),
        field("image", vec![Rule::String]),
        field("exclude_from_status", vec![Rule::Boolean]),
        field("is_log_drain_enabled", vec![Rule::Boolean]),
        field("is_public", vec![Rule::Boolean]),
        field("public_port", vec![Rule::Nullable, Rule::Integer, Rule::MinNum(1), Rule::MaxNum(65535)]),
        field("public_port_timeout", vec![Rule::Nullable, Rule::Integer, Rule::MinNum(1)]),
    ];
    validate(
        &input,
        &fields,
        &[],
        &[
            "human_name",
            "description",
            "image",
            "exclude_from_status",
            "is_log_drain_enabled",
            "is_public",
            "public_port",
            "public_port_timeout",
        ],
    )?;
    let current_public = d.bool("is_public");
    if input.has("is_public")
        && filter_bool(input.get("is_public")) != current_public
        && let Some(r) = deploy_forbidden(
            &ctx,
            "Making a database public or private starts or stops its proxy on the server, so it needs a token with the deploy permission.",
        )
    {
        return Ok(r);
    }
    let server = server_of(&state, &s).await?;
    if filter_bool(input.get("is_log_drain_enabled"))
        && !server
            .as_ref()
            .and_then(|x| x.settings.clone())
            .is_some_and(|x| {
                coolify_jobs::compose_parser::service::is_log_drain_enabled(&Value::Object(x))
            })
    {
        let mut m = Map::new();
        m.insert(
            "is_log_drain_enabled".into(),
            json!(["Log drain is not enabled on the server for this service."]),
        );
        return Err(ApiError::Validation(m));
    }
    let is_public = if input.has("is_public") {
        filter_bool(input.get("is_public"))
    } else {
        current_public
    };
    let public_port = input
        .get("public_port")
        .and_then(Value::as_i64)
        .or_else(|| d.int("public_port"));
    if is_public && public_port.is_none() {
        let mut m = Map::new();
        m.insert(
            "public_port".into(),
            json!(["A public port is required when the database is public."]),
        );
        return Err(ApiError::Validation(m));
    }
    if is_public
        && let (Some(port), Some(srv)) = (public_port, &server)
    {
        let standalone = coolify_db::databases::public_port_used(&state.db, srv.id, port).await?;
        if standalone || svc::public_port_used(&state.db, srv.id, port, d.id).await? {
            return Err(ApiError::Message(
                StatusCode::BAD_REQUEST,
                "Public port already used by another database.",
            ));
        }
    }
    if is_public != current_public {
        return Err(ApiError::NotPorted);
    }
    let now = state.now();
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in ["human_name", "description", "image"] {
        if input.has(k) {
            values.push((k, ColumnValue::Text(input.str(k).map(str::to_owned))));
        }
    }
    for k in ["exclude_from_status", "is_log_drain_enabled"] {
        if input.has(k) {
            values.push((k, ColumnValue::Bool(filter_bool(input.get(k)))));
        }
    }
    for k in ["public_port", "public_port_timeout"] {
        if input.has(k) {
            values.push((
                k,
                input
                    .get(k)
                    .and_then(Value::as_i64)
                    .map_or(ColumnValue::Text(None), ColumnValue::Int),
            ));
        }
    }
    svc::sub_update(&state.db, SubKind::Database, d.id, &values, now).await?;
    jobs_of(&state)?
        .update_service_compose(s.id, SubKind::Database, &d.name)
        .await
        .map_err(jobs_error)?;
    let fresh = svc::sub_by_uuid(&state.db, SubKind::Database, s.id, &d.uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    tracing::info!(target: "audit", event = "api.service_database.updated", team_id = ctx.team_id, service_uuid = %s.uuid, service_database_uuid = %d.uuid, "service database updated");
    Ok(ok(present_database(&fresh)).into_response())
}

/// `GET /services/{uuid}/databases/{database_uuid}/logs`.
pub async fn database_logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult {
    let (s, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    part_logs(&state, &s, &d, &uri, "Service database container is not running.").await
}

/// `POST /services/{uuid}/databases/{database_uuid}/start`.
pub async fn database_start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let (s, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    let input = lenient_json(&headers, &uri, &body);
    jobs_of(&state)?.spawn_sub_deploy(
        s.id,
        SubKind::Database,
        d.name.clone(),
        query_bool(&uri, &input, "latest", false),
        query_bool(&uri, &input, "force", false),
    );
    Ok(ok(json!({"message": "Service database deploy request queued."})).into_response())
}

/// `POST /services/{uuid}/databases/{database_uuid}/restart`.
pub async fn database_restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (s, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    jobs_of(&state)?.spawn_sub_restart(s, d);
    Ok(ok(json!({"message": "Service database restart request queued."})).into_response())
}

/// `POST /services/{uuid}/databases/{database_uuid}/stop`.
pub async fn database_stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
) -> ApiResult {
    let (s, d) = find_db(&state, &ctx, &uuid, &database_uuid).await?;
    require_admin(&ctx)?;
    part_guard(&state, &s).await?;
    jobs_of(&state)?.spawn_sub_stop(s, d);
    Ok(ok(json!({"message": "Service database stop request queued."})).into_response())
}

// --- scheduled tasks --------------------------------------------------------------------------

const ENABLED_TASK: &str =
    "An enabled scheduled task runs commands in the resource containers, so it needs a token with the deploy permission.";

fn present_task(ctx: &AuthCtx, r: &Record) -> Value {
    let mut out = r.clone();
    storage_times(&mut out);
    for k in ["id", "team_id", "application_id", "service_id"] {
        out.shift_remove(k);
    }
    if !ctx.can_read_sensitive {
        out.shift_remove("command");
    }
    Value::Object(serialize_record(out))
}

const TASK_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Scheduled task not found.");
const TASK_ALLOWED: &[&str] = &["name", "command", "frequency", "container", "timeout", "enabled"];

/// `GET /services/{uuid}/scheduled-tasks`.
pub async fn scheduled_tasks(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    let rows = svc::scheduled_tasks(&state.db, s.id).await?;
    let out: Vec<Value> = rows.iter().map(|r| present_task(&ctx, r)).collect();
    Ok(ok(Value::Array(out)).into_response())
}

fn invalid_frequency() -> ApiError {
    let mut m = Map::new();
    m.insert(
        "frequency".into(),
        json!(["Invalid cron expression or frequency format."]),
    );
    ApiError::Validation(m)
}

/// `POST /services/{uuid}/scheduled-tasks`.
pub async fn create_scheduled_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    let fields = [
        field("name", vec![Rule::Required, Rule::String, Rule::Max(255)]),
        field("command", vec![Rule::Required, Rule::String]),
        field("frequency", vec![Rule::Required, Rule::String]),
        field("container", vec![Rule::String, Rule::Nullable]),
        field("timeout", vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(36000)]),
        field("enabled", vec![Rule::Boolean]),
    ];
    validate(&input, &fields, CUSTOM_API_MESSAGES, TASK_ALLOWED)?;
    if !coolify_compat::cron::validate_cron_expression(input.str("frequency").unwrap_or_default()) {
        return Err(invalid_frequency());
    }
    let enabled = if input.has("enabled") {
        input_bool(&input, "enabled")
    } else {
        true
    };
    if enabled
        && let Some(r) = deploy_forbidden(&ctx, ENABLED_TASK)
    {
        return Ok(r);
    }
    let row = svc::scheduled_task_create(
        &state.db,
        s.id,
        ctx.team_id,
        &strip_tags(input.str("name")),
        input.str("command").unwrap_or_default(),
        input.str("frequency").unwrap_or_default(),
        input.str("container"),
        input.get("timeout").and_then(Value::as_i64).unwrap_or(300),
        enabled,
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.scheduled_task.created", team_id = ctx.team_id, resource_uuid = %s.uuid, "scheduled task created");
    Ok(created(present_task(&ctx, &row)).into_response())
}

/// `PATCH /services/{uuid}/scheduled-tasks/{task_uuid}`.
pub async fn update_scheduled_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    let fields = [
        field("name", vec![Rule::String, Rule::Max(255)]),
        field("command", vec![Rule::String]),
        field("frequency", vec![Rule::String]),
        field("container", vec![Rule::String, Rule::Nullable]),
        field("timeout", vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(36000)]),
        field("enabled", vec![Rule::Boolean]),
    ];
    validate(&input, &fields, CUSTOM_API_MESSAGES, TASK_ALLOWED)?;
    if input.has("frequency")
        && !coolify_compat::cron::validate_cron_expression(input.str("frequency").unwrap_or_default())
    {
        return Err(invalid_frequency());
    }
    let Some(task) = svc::scheduled_task(&state.db, s.id, &task_uuid).await? else {
        return Err(TASK_NOT_FOUND);
    };
    let enabled = if input.has("enabled") {
        input_bool(&input, "enabled")
    } else {
        task.get("enabled").and_then(Value::as_bool).unwrap_or(false)
    };
    if enabled
        && let Some(r) = deploy_forbidden(&ctx, ENABLED_TASK)
    {
        return Ok(r);
    }
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    if input.has("name") {
        values.push(("name", ColumnValue::Text(Some(strip_tags(input.str("name"))))));
    }
    for k in ["command", "frequency", "container"] {
        if input.has(k) {
            values.push((k, ColumnValue::Text(input.str(k).map(str::to_owned))));
        }
    }
    if let Some(t) = input.get("timeout").and_then(Value::as_i64) {
        values.push(("timeout", ColumnValue::Int(t)));
    }
    if input.has("enabled") {
        values.push(("enabled", ColumnValue::Bool(enabled)));
    }
    let id = task.get("id").and_then(Value::as_i64).unwrap_or_default();
    let fresh = svc::scheduled_task_update(&state.db, id, &values, state.now())
        .await?
        .ok_or(ApiError::Internal)?;
    tracing::info!(target: "audit", event = "api.scheduled_task.updated", team_id = ctx.team_id, task_uuid = %task_uuid, "scheduled task updated");
    Ok(ok(present_task(&ctx, &fresh)).into_response())
}

/// `DELETE /services/{uuid}/scheduled-tasks/{task_uuid}`.
pub async fn delete_scheduled_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    let Some(task) = svc::scheduled_task(&state.db, s.id, &task_uuid).await? else {
        return Err(TASK_NOT_FOUND);
    };
    let id = task.get("id").and_then(Value::as_i64).unwrap_or_default();
    svc::scheduled_task_delete(&state.db, id).await?;
    tracing::info!(target: "audit", event = "api.scheduled_task.deleted", team_id = ctx.team_id, task_uuid = %task_uuid, "scheduled task deleted");
    Ok(ok(json!({"message": "Scheduled task deleted."})).into_response())
}

/// `GET /services/{uuid}/scheduled-tasks/{task_uuid}/executions`.
pub async fn scheduled_task_executions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    let Some(task) = svc::scheduled_task(&state.db, s.id, &task_uuid).await? else {
        return Err(TASK_NOT_FOUND);
    };
    let id = task.get("id").and_then(Value::as_i64).unwrap_or_default();
    let mut out = Vec::new();
    for r in svc::scheduled_task_executions(&state.db, id).await? {
        let mut x = r.clone();
        storage_times(&mut x);
        x.shift_remove("id");
        x.shift_remove("scheduled_task_id");
        if !ctx.can_read_sensitive {
            x.shift_remove("message");
        }
        out.push(Value::Object(serialize_record(x)));
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `POST /services/{uuid}/scheduled-tasks/{task_uuid}/execute`: `ScheduledTaskJob` is not ported.
pub async fn execute_scheduled_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, task_uuid)): Path<(String, String)>,
) -> ApiResult {
    let s = find(&state, &ctx, &uuid).await?;
    require_admin(&ctx)?;
    if svc::scheduled_task(&state.db, s.id, &task_uuid).await?.is_none() {
        return Err(TASK_NOT_FOUND);
    }
    Err(ApiError::NotPorted)
}
