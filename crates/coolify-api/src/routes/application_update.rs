//! `PATCH /applications/{uuid}` (`ApplicationsController::update_by_uuid`).
//!
//! Upstream saves settings and the model in several steps. This computes what those steps leave
//! behind on a copy of the row, in upstream's order, then writes it. A request that upstream
//! fails halfway (a database error on save) therefore leaves nothing behind here.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_compat::yaml::PhpValue;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::naming::strip_tags;
use coolify_jobs::applications::domains;
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::application_fields::{
    self as fields, Columns, SERVER_ERROR, WEBHOOK_SECRETS, decrypted, field_error, put,
};
use crate::routes::applications::{
    NOT_FOUND_APPLICATION, deployable, extra_field_errors, field, gpu_options_throws, shared_rules,
    validate_data_applications,
};
use crate::routes::databases::input_bool;
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::validation::{Rule, validate};
use coolify_domain::grants::AccessLevel;

/// `$allowedFields` of `update_by_uuid` plus `APPLICATION_SETTING_FIELDS`.
const ALLOWED: &[&str] = &[
    "name",
    "description",
    "is_static",
    "is_spa",
    "is_auto_deploy_enabled",
    "is_force_https_enabled",
    "is_preview_deployments_enabled",
    "domains",
    "noindex_domains",
    "git_repository",
    "git_branch",
    "git_commit_sha",
    "docker_registry_image_name",
    "docker_registry_image_tag",
    "build_pack",
    "static_image",
    "install_command",
    "build_command",
    "start_command",
    "ports_exposes",
    "ports_mappings",
    "custom_network_aliases",
    "base_directory",
    "publish_directory",
    "health_check_enabled",
    "health_check_type",
    "health_check_command",
    "health_check_path",
    "health_check_port",
    "health_check_host",
    "health_check_method",
    "health_check_return_code",
    "health_check_scheme",
    "health_check_response_text",
    "health_check_interval",
    "health_check_timeout",
    "health_check_retries",
    "health_check_start_period",
    "limits_memory",
    "limits_memory_swap",
    "limits_memory_swappiness",
    "limits_memory_reservation",
    "limits_cpus",
    "limits_cpuset",
    "limits_cpu_shares",
    "custom_labels",
    "custom_docker_run_options",
    "post_deployment_command",
    "post_deployment_command_container",
    "pre_deployment_command",
    "pre_deployment_command_container",
    "watch_paths",
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
    "dockerfile_location",
    "dockerfile_target_build",
    "docker_compose_location",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "docker_compose_domains",
    "redirect",
    "instant_deploy",
    "use_build_server",
    "use_build_secrets",
    "custom_nginx_configuration",
    "is_http_basic_auth_enabled",
    "http_basic_auth_username",
    "http_basic_auth_password",
    "http_basic_auth_hash_algorithm",
    "http_basic_auth_bcrypt_cost",
    "http_basic_auth_argon2id_memory_cost",
    "http_basic_auth_argon2id_time_cost",
    "connect_to_docker_network",
    "force_domain_override",
    "is_container_label_escape_enabled",
    "is_preserve_repository_enabled",
    "preview_url_template",
    "max_restart_count",
    "github_app_uuid",
    "dockerfile",
    // APPLICATION_SETTING_FIELDS
    "is_git_submodules_enabled",
    "is_git_lfs_enabled",
    "is_git_shallow_clone_enabled",
    "disable_build_cache",
    "inject_build_args_to_dockerfile",
    "include_source_commit_in_build",
    "is_env_sorting_enabled",
    "redact_all_env_values_in_logs",
    "include_coolify_default_variables",
    "is_pr_deployments_public_enabled",
    "stop_grace_period",
    "rolling_update_drain_seconds",
    "docker_images_to_keep",
    "is_gzip_enabled",
    "is_stripprefix_enabled",
    "is_permanent_redirect_enabled",
    "traefik_certificate_resolver",
    "is_container_label_readonly_enabled",
    "is_raw_compose_deployment_enabled",
    "is_log_drain_enabled",
    "is_gpu_enabled",
    "gpu_driver",
    "gpu_count",
    "gpu_device_ids",
    "gpu_options",
    "is_consistent_container_name_enabled",
    "custom_internal_name",
    "custom_container_name_prefix",
];

const BASIC_AUTH_FIELDS: &[&str] = &[
    "is_http_basic_auth_enabled",
    "http_basic_auth_username",
    "http_basic_auth_password",
    "http_basic_auth_hash_algorithm",
    "http_basic_auth_bcrypt_cost",
    "http_basic_auth_argon2id_memory_cost",
    "http_basic_auth_argon2id_time_cost",
];

static PORTS_EXPOSES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(\d+)(,\d+)*$").unwrap_or_else(|_| unreachable!())
});

/// The rules of `update_by_uuid`, with `ManualWebhookSecret` given the stored values.
fn rules(input: &Input, stored_secrets: &[Option<String>; 4]) -> Vec<crate::validation::Field> {
    let mut overrides = vec![
        field("name", vec![Rule::String, Rule::Max(255)]),
        field("github_app_uuid", vec![Rule::String]),
        field("dockerfile", vec![Rule::String]),
        field("description", vec![Rule::String, Rule::Nullable]),
        field("watch_paths", vec![Rule::String, Rule::Nullable]),
        field("docker_compose_domains", vec![Rule::Array, Rule::Nullable]),
        field(
            "docker_compose_domains.*",
            vec![Rule::Expand(fields::compose_domain_rules)],
        ),
        field(
            "custom_nginx_configuration",
            vec![Rule::String, Rule::Nullable],
        ),
        field(
            "is_http_basic_auth_enabled",
            vec![Rule::Boolean, Rule::Nullable],
        ),
        field(
            "is_preview_deployments_enabled",
            vec![Rule::Boolean, Rule::Nullable],
        ),
        field("http_basic_auth_username", vec![Rule::String]),
        field(
            "http_basic_auth_password",
            vec![Rule::String, Rule::Max(72)],
        ),
        field("include_source_commit_in_build", vec![Rule::Boolean]),
        field(
            "ports_exposes",
            vec![Rule::Nullable, Rule::String, Rule::Regex(&PORTS_EXPOSES)],
        ),
    ];
    for (key, stored) in WEBHOOK_SECRETS.into_iter().zip(stored_secrets) {
        let failure = input
            .get(key)
            .and_then(|v| fields::webhook_secret_failure(v, stored.as_deref()));
        overrides.push(field(
            key,
            vec![Rule::Nullable, Rule::String, Rule::Evaluated(failure)],
        ));
    }
    shared_rules(overrides)
}

/// `Application::deploymentType()`.
async fn is_deploy_key(state: &AppState, a: &ApplicationRow) -> Result<bool, ApiError> {
    let key = a.int("private_key_id");
    if key.is_some_and(|k| k > 0) {
        return Ok(true);
    }
    if let (Some(source_type), Some(source_id)) = (a.str("source_type"), a.int("source_id"))
        && apps::source_exists(&state.db, source_type, source_id).await?
    {
        return Ok(false);
    }
    Ok(key == Some(0))
}

/// `GithubApp::isConnected()`.
fn github_app_connected(app: &Map<String, Value>) -> bool {
    let filled = |k: &str| match app.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    };
    app.get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || (filled("app_id") && filled("installation_id"))
}

/// The `$application` being changed and the columns its saves write.
struct Pending {
    a: ApplicationRow,
    writes: Columns,
}

impl Pending {
    /// `$application->col = $value`.
    fn set(&mut self, col: &'static str, value: ColumnValue) {
        self.a.raw.insert(col.to_owned(), column_json(&value));
        put(&mut self.writes, col, value);
    }
}

fn column_json(value: &ColumnValue) -> Value {
    match value {
        ColumnValue::Text(t) => t.clone().map_or(Value::Null, Value::String),
        ColumnValue::Int(i) => json!(i),
        ColumnValue::Bool(b) => json!(b),
        ColumnValue::Time(t) => json!(t.to_string()),
    }
}

/// The entries of `$dockerComposeDomainsJson`: only services of the compose file, keeping a stored
/// `redirect` the request does not mention.
fn compose_entries(
    input: &Input,
    a: &ApplicationRow,
    yaml: &PhpValue,
) -> Result<Map<String, Value>, ApiError> {
    let services = match yaml {
        PhpValue::Array(items) => items
            .iter()
            .find(|(k, _)| k.as_string() == "services")
            .map(|(_, v)| v),
        _ => None,
    };
    let existing: Map<String, Value> = a
        .str("docker_compose_domains")
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    let mut entries = Map::new();
    for item in fields::compose_items(input) {
        let Some(name) = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && *n != "0")
        else {
            continue;
        };
        // `is_array($services) && isset($services[$name])`.
        let known = matches!(services, Some(PhpValue::Array(list)) if list
            .iter()
            .any(|(k, v)| k.as_string() == name && !matches!(v, PhpValue::Null)));
        if !known {
            continue;
        }
        let mut entry = Map::new();
        entry.insert(
            "domain".into(),
            item.get("domain").cloned().unwrap_or(Value::Null),
        );
        let redirect = match item.get("redirect") {
            Some(v) => fields::redirect_of(Some(v)),
            None => fields::redirect_of(existing.get(name).and_then(|e| e.get("redirect"))),
        };
        if let Some(r) = redirect {
            entry.insert("redirect".into(), json!(r));
        }
        entries.insert(name.to_owned(), Value::Object(entry));
    }
    Ok(entries)
}

/// `(int) $port` of a numeric string.
fn php_int(text: &str) -> i64 {
    text.trim().parse::<f64>().map_or(0, |f| f as i64)
}

/// `PATCH /applications/{uuid}`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    use coolify_domain::Ability;
    let mut input = validated_json(&headers, &uri, &body)?;
    let found = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND_APPLICATION)?;
    // `update`, `manageEnvironment` and `deploy`: write includes deploy.
    if !ctx.can_app(AccessLevel::Write, &found) {
        return Err(ApiError::Unauthorized);
    }
    let instant = input.get("instant_deploy").is_some_and(php_truthy);
    if instant && !(ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let server_id = found.server_id().ok_or(ApiError::NotPorted)?;
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let stored_secrets = [
        decrypted(enc, &found, WEBHOOK_SECRETS[0])?,
        decrypted(enc, &found, WEBHOOK_SECRETS[1])?,
        decrypted(enc, &found, WEBHOOK_SECRETS[2])?,
        decrypted(enc, &found, WEBHOOK_SECRETS[3])?,
    ];
    // The validator holds the data as it is now, and fails only further down.
    let keys: Vec<&str> = input.keys().map(String::as_str).collect();
    let validated = if gpu_options_throws(&input) {
        Err(SERVER_ERROR)
    } else {
        validate(&input, &rules(&input, &stored_secrets), &[], &keys)
    };

    if input.filled("ports_exposes") {
        let value = input.get("ports_exposes").unwrap_or(&Value::Null);
        let text = fields::php_text(value)?.unwrap_or_default();
        for port in text.split(',') {
            let bad = !coolify_compat::yaml::php_is_numeric(port)
                || !(1..=65535).contains(&php_int(port));
            if bad {
                return Err(field_error(
                    "ports_exposes",
                    "The ports_exposes should be a comma separated list of numbers.",
                ));
            }
        }
    }
    fields::check_custom_nginx(&input)?;
    if input.has("dockerfile") {
        if found.str("dockerfile").is_none_or(|d| d.trim().is_empty()) {
            return Err(field_error(
                "dockerfile",
                "The dockerfile can only be updated for applications created from a Dockerfile.",
            ));
        }
        let decoded = input
            .str("dockerfile")
            .and_then(fields::base64_bytes)
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|d| !d.trim().is_empty())
            .ok_or_else(|| field_error("dockerfile", "The dockerfile should be base64 encoded."))?;
        input.0.insert("dockerfile".into(), Value::String(decoded));
    }
    let request_uuid = input.str("uuid").unwrap_or(&uuid).to_owned();
    if let Some(early) = validate_data_applications(
        &state,
        ctx.team_id,
        &mut input,
        &server,
        Some(&request_uuid),
    )
    .await?
    {
        return Ok(early);
    }
    let mut errors = match validated {
        Ok(()) => Map::new(),
        Err(ApiError::Validation(errors)) => errors,
        Err(other) => return Err(other),
    };
    extra_field_errors(&input, &mut errors, ALLOWED);
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let mut github_app = None;
    if input.has("github_app_uuid") {
        if found.str("build_pack") == Some("dockerimage")
            || found
                .str("dockerfile")
                .is_some_and(|d| !d.trim().is_empty())
            || is_deploy_key(&state, &found).await?
        {
            return Err(field_error(
                "github_app_uuid",
                "The github_app_uuid can only be changed for applications deployed from a GitHub App or public repository.",
            ));
        }
        let app = apps::github_app_for_team(
            &state.db,
            ctx.team_id,
            input.str("github_app_uuid").unwrap_or_default(),
        )
        .await?
        .filter(github_app_connected)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Github App not found.",
        ))?;
        github_app = Some(app);
    }

    let app_settings = fields::settings_from_request(&input);
    fields::check_container_name_prefix(&state, &app_settings, server_id, Some(found.id)).await?;
    fields::check_custom_internal_name(&app_settings)?;
    let requested_pack = match input.get("build_pack") {
        Some(v) => v.as_str().map(str::to_owned),
        None => found.str("build_pack").map(str::to_owned),
    };
    if fields::setting(&app_settings, "is_raw_compose_deployment_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && requested_pack.as_deref() != Some("dockercompose")
    {
        return Err(field_error(
            "is_raw_compose_deployment_enabled",
            "Raw compose deployment can only be enabled for Docker Compose applications.",
        ));
    }

    let has_basic_auth = BASIC_AUTH_FIELDS.iter().any(|k| input.has(k));
    if input.get("is_http_basic_auth_enabled") == Some(&Value::Bool(true)) {
        let blank = |s: Option<&str>| s.is_none_or(|s| s.trim().is_empty());
        let password = decrypted(enc, &found, "http_basic_auth_password")?;
        if blank(found.str("http_basic_auth_username")) || blank(password.as_deref()) {
            let mut errors = Map::new();
            if blank(input.str("http_basic_auth_username")) {
                errors.insert(
                    "http_basic_auth_username".into(),
                    json!("The http_basic_auth_username is required."),
                );
            }
            if blank(input.str("http_basic_auth_password")) {
                errors.insert(
                    "http_basic_auth_password".into(),
                    json!("The http_basic_auth_password is required."),
                );
            }
            if !errors.is_empty() {
                return Err(ApiError::Validation(errors));
            }
        }
    }

    if found.str("build_pack") == Some("dockercompose") && input.has("domains") {
        return Err(field_error(
            "domains",
            "The domains field cannot be used for dockercompose applications. Use docker_compose_domains instead to set domains for individual services.",
        ));
    }

    let mut compose_entries_json = Map::new();
    if input.has("docker_compose_domains") {
        let raw = found.str("docker_compose_raw").unwrap_or_default();
        if raw.is_empty() || raw == "0" {
            return Err(field_error(
                "docker_compose_domains",
                "Cannot set docker_compose_domains without docker_compose_raw. Reload the compose file from the git repository first.",
            ));
        }
        if let Some(early) =
            fields::check_compose_domain_urls(&state, ctx.team_id, &input, "", Some(&request_uuid))
                .await?
        {
            return Ok(early);
        }
        let yaml = coolify_jobs::compose_parser::validate::parse_docker_compose_yaml(raw)
            .map_err(|_| SERVER_ERROR)?;
        compose_entries_json = compose_entries(&input, &found, &yaml)?;
    }

    apply(
        &state,
        &ctx,
        found,
        server,
        input,
        Applied {
            app_settings,
            github_app,
            compose_entries: compose_entries_json,
            has_basic_auth,
            instant,
        },
    )
    .await
}

/// What the checks of `update()` resolved.
struct Applied {
    app_settings: Vec<(&'static str, Value)>,
    github_app: Option<Map<String, Value>>,
    compose_entries: Map<String, Value>,
    has_basic_auth: bool,
    instant: bool,
}

/// `update_by_uuid` after its checks: the settings, `fill()`, the labels, `save()`, the deployment.
async fn apply(
    state: &AppState,
    ctx: &AuthCtx,
    found: ApplicationRow,
    server: servers::ServerRow,
    input: Input,
    resolved: Applied,
) -> Result<Response, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let Applied {
        app_settings,
        github_app,
        compose_entries,
        has_basic_auth,
        instant,
    } = resolved;
    let mut settings: Columns = Vec::new();
    for (key, column) in [
        ("use_build_server", "is_build_server_enabled"),
        ("use_build_secrets", "use_build_secrets"),
        ("is_static", "is_static"),
        ("is_spa", "is_spa"),
        ("is_auto_deploy_enabled", "is_auto_deploy_enabled"),
        ("is_force_https_enabled", "is_force_https_enabled"),
        (
            "is_preview_deployments_enabled",
            "is_preview_deployments_enabled",
        ),
        ("connect_to_docker_network", "connect_to_docker_network"),
    ] {
        if let Some(v) = input.get(key).filter(|v| !v.is_null())
            && let Some(b) = fields::unvalidated_flag(v, found.setting_bool(column))?
        {
            put(&mut settings, column, ColumnValue::Bool(b));
        }
    }
    for key in [
        "is_container_label_escape_enabled",
        "is_preserve_repository_enabled",
        "include_source_commit_in_build",
    ] {
        if input.has(key) {
            put(
                &mut settings,
                key,
                ColumnValue::Bool(input_bool(&input, key)),
            );
        }
    }
    let was_readonly = found.setting_bool("is_container_label_readonly_enabled");
    for (col, value) in fields::settings_columns(&app_settings)? {
        put(&mut settings, col, value);
    }

    let original = found.clone();
    let mut p = Pending {
        a: found,
        writes: Vec::new(),
    };
    for (col, value) in &settings {
        p.a.settings.insert((*col).to_owned(), column_json(value));
    }
    // The `is_static` setter saves `ports_exposes = 80` through another instance of the model:
    // `$application` keeps the old value and only overwrites it when the request changes it.
    if input.get("is_static").is_some_and(php_truthy) {
        put(
            &mut p.writes,
            "ports_exposes",
            ColumnValue::Text(Some("80".into())),
        );
    }
    // applyApplicationSettings().
    let readonly = p.a.setting_bool("is_container_label_readonly_enabled");
    if !app_settings.is_empty()
        && readonly
        && (!was_readonly
            || [
                "is_gzip_enabled",
                "is_stripprefix_enabled",
                "is_permanent_redirect_enabled",
                "traefik_certificate_resolver",
            ]
            .iter()
            .any(|k| fields::setting(&app_settings, k).is_some()))
    {
        let labels = fields::generated_labels(&p.a, &server, enc)?;
        p.set("custom_labels", ColumnValue::Text(Some(labels)));
    }

    // $application->fill($data).
    let setter_pack = match input.get("build_pack") {
        Some(v) => v.as_str().map(str::to_owned),
        None => original.str("build_pack").map(str::to_owned),
    };
    let name = input.get("name").map(|v| strip_tags(v.as_str()));
    let filled = fields::fill_columns(&input, enc, name, setter_pack.as_deref(), Some(&original))?;
    let stale_ports = original.str("ports_exposes").map(str::to_owned);
    let mut ports_dirty = false;
    for (col, value) in filled {
        if col == "ports_exposes" {
            let new = match &value {
                ColumnValue::Text(t) => t.clone(),
                _ => None,
            };
            if new == stale_ports {
                continue;
            }
            ports_dirty = true;
        }
        p.set(col, value);
    }
    let proxy_runs = fields::proxy_should_run(&server);
    let has_domains = input.has("domains");
    let mut fqdn_dirty = false;
    if has_domains && proxy_runs {
        let new = input.str("domains").map(str::to_owned);
        fqdn_dirty = new.as_deref() != original.str("fqdn");
        p.a.raw
            .insert("fqdn".into(), new.map_or(Value::Null, Value::String));
    }
    if !compose_entries.is_empty() {
        let (json, overrides) = fields::normalize_compose_domains(
            compose_entries,
            &fields::stored_overrides(&original),
        );
        p.set("docker_compose_domains", ColumnValue::Text(Some(json)));
        p.set("domain_port_overrides", ColumnValue::Text(overrides));
    }
    if let Some(app) = &github_app {
        let id = app.get("id").and_then(Value::as_i64).unwrap_or_default();
        const MORPH: &str = r"App\Models\GithubApp";
        if original.int("source_id") != Some(id) || original.str("source_type") != Some(MORPH) {
            p.set("source_id", ColumnValue::Int(id));
            p.set("source_type", ColumnValue::Text(Some(MORPH.into())));
            p.set("repository_project_id", ColumnValue::Text(None));
        }
    }
    let has_noindex = input.has("noindex_domains");
    if has_noindex {
        let json = fields::noindex_json(
            &fields::noindex_values(&input),
            p.a.str("fqdn"),
            p.a.str("docker_compose_domains"),
        );
        p.set("noindex_domains", ColumnValue::Text(Some(json)));
    }
    // The labels read the row before the `saving` hook normalizes it.
    if p.a.setting_bool("is_container_label_readonly_enabled")
        && (has_domains || has_noindex || has_basic_auth || ports_dirty)
        && proxy_runs
    {
        let labels = fields::generated_labels(&p.a, &server, enc)?;
        p.set("custom_labels", ColumnValue::Text(Some(labels)));
    }

    // The `saving` hook.
    if fqdn_dirty {
        let compose = p.a.str("build_pack") == Some("dockercompose");
        let fqdn = p.a.str("fqdn").map(str::to_owned);
        let mut stored = fqdn.clone();
        if !compose || fqdn.as_deref().is_some_and(|f| !f.trim().is_empty()) {
            let (normalized, overrides) =
                domains::normalize_port_overrides(fqdn.as_deref(), &fields::stored_overrides(&p.a));
            p.set(
                "domain_port_overrides",
                ColumnValue::Text(
                    overrides.map(|o| domains::overrides_json(normalized.as_deref(), &o)),
                ),
            );
            stored = normalized;
        }
        p.set("fqdn", ColumnValue::Text(stored));
        // syncNoindexDomains().
        let flags = match p.a.raw.get("noindex_domains") {
            Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        if !flags.is_empty() {
            let json =
                fields::noindex_json(&flags, p.a.str("fqdn"), p.a.str("docker_compose_domains"));
            p.set("noindex_domains", ColumnValue::Text(Some(json)));
        }
    }
    // Build pack switching: what the old build pack leaves behind goes.
    let original_pack = original.str("build_pack");
    let mut delete_service_variables = false;
    if p.a.str("build_pack") != original_pack {
        if original_pack == Some("dockercompose") {
            p.set("docker_compose_domains", ColumnValue::Text(None));
            p.set("docker_compose_raw", ColumnValue::Text(None));
            delete_service_variables = true;
        }
        if original_pack == Some("dockerfile") {
            p.set("dockerfile", ColumnValue::Text(None));
            p.set("dockerfile_location", ColumnValue::Text(None));
            p.set("dockerfile_target_build", ColumnValue::Text(None));
            p.set("custom_healthcheck_found", ColumnValue::Bool(false));
        }
    }
    // `if ($application->wasChanged('dockerfile'))`.
    if p.a.str("dockerfile") != original.str("dockerfile") {
        let dockerfile = p.a.str("dockerfile").map(str::to_owned);
        for (col, value) in fields::healthcheck_from_dockerfile(&p.a, dockerfile.as_deref()) {
            p.set(col, value);
        }
    }

    // The deployment job cannot run this application: nothing is saved either.
    if instant {
        deployable(state, &p.a).await?;
    }

    let now = state.now();
    let previous_dns_hostnames =
        crate::ui_shared::dns::previous_hostnames(state, apps::MORPH, original.id).await;
    apps::update_settings(&state.db, original.id, &settings, now)
        .await
        .map_err(fields::save_error)?;
    apps::update_if_changed(&state.db, original.id, &p.writes, now)
        .await
        .map_err(fields::save_error)?;
    crate::ui_shared::dns::queue_release_of_removed_hostnames(
        state,
        apps::MORPH,
        original.id,
        previous_dns_hostnames,
        ctx.team_id,
    );
    if delete_service_variables {
        apps::delete_service_url_variables(&state.db, original.id).await?;
    }
    // `ManagedDnsRecordCleanup::queueReleaseOfRemovedHostnames`: managed DNS is not ported.
    let changed: Vec<&str> = ALLOWED.iter().copied().filter(|k| input.has(k)).collect();
    tracing::info!(target: "audit", event = "api.application.updated", team_id = ctx.team_id, application_uuid = %original.uuid, changed_fields = ?changed, "application updated");

    if instant {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let fresh = apps::find_by_id(&state.db, original.id)
            .await?
            .ok_or(ApiError::Internal)?;
        let result = jobs
            .queue_application_deployment(
                &fresh,
                &coolify_domain::new_public_id(),
                QueueOptions {
                    is_api: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "deployment could not be queued");
                ApiError::Internal
            })?;
        if let Queued::Skipped { .. } = result {
            return Ok(ok(json!({"message": result.message()})).into_response());
        }
    }
    Ok(ok(json!({"uuid": original.uuid})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_int_of_numeric_strings() {
        assert_eq!(php_int("80"), 80);
        assert_eq!(php_int("1e3"), 1000);
        assert_eq!(php_int("5.9"), 5);
    }
}
