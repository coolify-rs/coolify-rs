//! `ApplicationsController` and `DeployController` for Docker image applications:
//! list, show, create (`dockerimage`, and `public` for the git build packs), start,
//! restart, stop, delete, and the deployment endpoints. The other create types,
//! PATCH, envs, storages, previews and the rest answer 501.
//! compat/notes/applications-api.md and deployment-job-git.md describe upstream.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_compat::Encrypter;
use coolify_db::applications::{self as apps, ApplicationRow, Deployment, NewApplication};
use coolify_db::records::{APPLICATION, APPLICATION_DEPLOYMENT_QUEUE, APPLICATION_SETTING, Record};
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::naming::strip_tags;
use coolify_jobs::applications::domains::{self, UrlParts};
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::git_urls;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::databases::{
    input_bool, nested_server, nested_server_with, query_bool, server_functional,
};
use crate::serialize::{php_truthy, serialize_record};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");
const NOT_FOUND_NO_PERIOD: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Application not found");

/// `$hidden` of the model; the sensitive ones become visible with `read:sensitive`.
const HIDDEN: &[&str] = &[
    "http_basic_auth_password",
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
    "dockerfile",
    "docker_compose",
    "docker_compose_raw",
    "custom_labels",
    "domain_dns_statuses",
    "domain_port_overrides",
];
/// Encrypted (`encrypted` cast) among the visible-with-sensitive attributes.
const ENCRYPTED: &[&str] = &[
    "http_basic_auth_password",
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
];

/// The `status` getter of a single-server application.
fn status_getter(v: Option<&str>) -> String {
    coolify_jobs::applications::status::normalize_status(v.unwrap_or_default())
}

/// `removeSensitiveData($application)` + `serializeApiResponse`.
pub(crate) async fn present(
    state: &AppState,
    ctx: &AuthCtx,
    a: &ApplicationRow,
    show: bool,
) -> Result<Value, ApiError> {
    let server = match a.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let mut r = APPLICATION.present(a.raw.clone());
    for h in HIDDEN {
        let visible = ctx.can_read_sensitive
            && !matches!(*h, "domain_dns_statuses" | "domain_port_overrides");
        if !visible {
            r.shift_remove(*h);
        } else if ENCRYPTED.contains(h)
            && let Some(Value::String(c)) = r.get(*h)
        {
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let plain = enc.decrypt_string(c).map_err(|_| ApiError::Internal)?;
            r.insert((*h).into(), Value::String(plain));
        }
    }
    if !ctx.can_read_sensitive {
        r.shift_remove("private_key_id");
    }
    r.shift_remove("id");
    // Accessors and casts.
    r.insert(
        "status".into(),
        Value::String(status_getter(a.str("status"))),
    );
    if let Some(Value::String(v)) = r.get("custom_nginx_configuration") {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(v)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        r.insert("custom_nginx_configuration".into(), Value::String(decoded));
    }
    if let Some(Value::String(v)) = r.get("custom_network_aliases") {
        let list = serde_json::from_str::<Vec<String>>(v)
            .map(|l| l.join(","))
            .unwrap_or_else(|_| v.clone());
        r.insert("custom_network_aliases".into(), Value::String(list));
    }
    r.insert("additional_servers_count".into(), json!(0));
    r.insert("additional_networks_count".into(), json!(0));
    r.insert(
        "server_status".into(),
        Value::Bool(server.as_ref().is_some_and(server_functional)),
    );
    r.insert("additional_servers".into(), json!([]));
    if let Some(dest) = &a.destination {
        let mut dest = dest.clone();
        if let Some(s) = &server {
            dest.insert("server".into(), nested_server(state, ctx, s)?);
        }
        r.insert("destination".into(), Value::Object(dest));
    }
    if show {
        let mut project = a.project.clone();
        project.insert("team".into(), Value::Object(a.team.clone()));
        let mut env = a.environment.clone();
        env.insert("project".into(), Value::Object(project));
        r.insert("environment".into(), Value::Object(env));
        let mut settings = APPLICATION_SETTING.present(a.settings.clone());
        for k in ["id", "application_id", "created_at", "updated_at"] {
            settings.shift_remove(k);
        }
        r.insert("settings".into(), Value::Object(settings));
    }
    Ok(Value::Object(serialize_record(r)))
}

/// `GET /applications`.
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    uri: Uri,
) -> ApiResult {
    if query_param(&uri, "tag").is_some() {
        return Err(ApiError::NotPorted);
    }
    let rows = apps::list_for_team(&state.db, ctx.team_id).await?;
    let mut items = Vec::with_capacity(rows.len());
    for a in &rows {
        items.push(present(&state, &ctx, a, false).await?);
    }
    Ok(ok(Value::Array(items)).into_response())
}

/// `GET /applications/{uuid}`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(ok(present(&state, &ctx, &a, true).await?).into_response())
}

/// `$allowedFields` of `create_application` plus `APPLICATION_SETTING_FIELDS`.
const ALLOWED: &[&str] = &[
    "project_uuid",
    "environment_name",
    "environment_uuid",
    "server_uuid",
    "destination_uuid",
    "type",
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
    "private_key_uuid",
    "docker_registry_image_name",
    "docker_registry_image_tag",
    "build_pack",
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
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
    "redirect",
    "github_app_uuid",
    "instant_deploy",
    "dockerfile",
    "dockerfile_location",
    "dockerfile_target_build",
    "docker_compose_location",
    "docker_compose_raw",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "docker_compose_domains",
    "watch_paths",
    "use_build_server",
    "use_build_secrets",
    "static_image",
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
    "autogenerate_domain",
    "is_container_label_escape_enabled",
    "tags",
    "is_preserve_repository_enabled",
    "preview_url_template",
    "max_restart_count",
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

/// The fields coolify-rs applies on create, for every type; any other allowed field
/// answers 501.
const SUPPORTED: &[&str] = &[
    "project_uuid",
    "environment_name",
    "environment_uuid",
    "server_uuid",
    "destination_uuid",
    "name",
    "description",
    "domains",
    "ports_exposes",
    "ports_mappings",
    "instant_deploy",
    "autogenerate_domain",
    "force_domain_override",
    "is_force_https_enabled",
    "is_auto_deploy_enabled",
    "redirect",
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
    "is_container_label_escape_enabled",
    "is_gzip_enabled",
    "is_stripprefix_enabled",
    "stop_grace_period",
    "rolling_update_drain_seconds",
    "include_coolify_default_variables",
    "redact_all_env_values_in_logs",
    "tags",
    // Removed from the request (`removeUnnecessaryFieldsFromRequest`) except in their own
    // branch, where they pick the GitHub App or deploy key.
    "github_app_uuid",
    "private_key_uuid",
];

/// The fields only `dockerimage` applies.
const SUPPORTED_DOCKERIMAGE: &[&str] = &["docker_registry_image_name", "docker_registry_image_tag"];

/// The fields only the git types (`public`) apply.
const SUPPORTED_GIT: &[&str] = &[
    "git_repository",
    "git_branch",
    "git_commit_sha",
    "build_pack",
    "install_command",
    "build_command",
    "start_command",
    "base_directory",
    "publish_directory",
    "static_image",
    "dockerfile_location",
    "dockerfile_target_build",
    "docker_compose_location",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "is_static",
    "is_spa",
    "is_preview_deployments_enabled",
    "use_build_secrets",
    "is_preserve_repository_enabled",
    "is_git_submodules_enabled",
    "is_git_lfs_enabled",
    "is_git_shallow_clone_enabled",
    "disable_build_cache",
    "inject_build_args_to_dockerfile",
    "include_source_commit_in_build",
    "is_raw_compose_deployment_enabled",
];

/// `GithubApp::class` (the morph type of a git source).
const GITHUB_APP_MORPH: &str = r"App\Models\GithubApp";

/// `BuildPackTypes` (the git build packs; `dockerimage` is not one of them).
const BUILD_PACKS: &[&str] = &[
    "nixpacks",
    "static",
    "dockerfile",
    "dockercompose",
    "railpack",
];

static DOCKER_IMAGE_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z")
        .unwrap_or_else(|_| unreachable!())
});
static DOCKER_IMAGE_TAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}\z").unwrap_or_else(|_| unreachable!())
});
static PORTS_EXPOSES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(\d+)(,\d+)*$").unwrap_or_else(|_| unreachable!())
});
static PORTS_MAPPINGS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(\d+:\d+)(,\d+:\d+)*$").unwrap_or_else(|_| unreachable!())
});
static HEALTH_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9/\-_.~%,;]+$").unwrap_or_else(|_| unreachable!())
});
static HEALTH_COMMAND: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9 \-_./:=@,+]+$").unwrap_or_else(|_| unreachable!())
});

static GIT_COMMIT_SHA: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._\-/]*$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::SHELL_SAFE_COMMAND_PATTERN`.
static SHELL_SAFE_COMMAND: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"^(?:[ \t]+|&&|\|\||"[^"$`\\\n\r]*"|'[^'\n\r]*'|[a-zA-Z0-9._\-/=:@,+\[\]{}#%^~*?!]+)+$"#,
    )
    .unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::DIRECTORY_PATH_PATTERN`.
static DIRECTORY_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/([a-zA-Z0-9._\-/~@+]*)?$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::FILE_PATH_PATTERN`.
static FILE_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/[a-zA-Z0-9._\-/~@+]+$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::DOCKER_TARGET_PATTERN`.
static DOCKER_TARGET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::CONTAINER_NAME_PATTERN`.
static CONTAINER_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z").unwrap_or_else(|_| unreachable!())
});

/// The `DockerImageFormat` rule.
fn docker_image_format(v: &Value) -> Option<String> {
    let bad =
        "The :attribute format is invalid. Use image:tag or image@sha256:hash format.".to_owned();
    let Some(s) = v.as_str() else {
        return Some(bad);
    };
    if regex::Regex::new(r"(?i):sha256?:")
        .map(|r| r.is_match(s))
        .unwrap_or(false)
    {
        return Some("The :attribute must use @ before sha256 digest (e.g., image@sha256:hash, not image:sha256:hash).".into());
    }
    let (name, tag) = match regex::Regex::new(r"(?i)\A(.+)@sha256:([a-f0-9]{64})\z")
        .ok()
        .and_then(|r| r.captures(s).map(|c| c[1].to_owned()))
    {
        Some(n) => (n, None),
        None => split_tag(s),
    };
    let name_ok = name.is_empty() || DOCKER_IMAGE_NAME.is_match(&name) && name.len() <= 255;
    let tag_ok = tag
        .as_deref()
        .is_none_or(|t| t.is_empty() || DOCKER_IMAGE_TAG.is_match(t));
    (!(name_ok && tag_ok)).then_some(bad)
}

/// Splits `name:tag` at the last `:` after the last `/`.
fn split_tag(s: &str) -> (String, Option<String>) {
    let slash = s.rfind('/').map_or(0, |i| i + 1);
    match s[slash..].rfind(':') {
        Some(i) => (
            s[..slash + i].to_owned(),
            Some(s[slash + i + 1..].to_owned()),
        ),
        None => (s.to_owned(), None),
    }
}

/// `DockerImageParser::parse("$name[:$tag]")`: the stored name and tag.
fn parse_image(name: &str, tag: Option<&str>) -> (String, String) {
    let full = match tag.filter(|t| !t.is_empty()) {
        Some(t) => format!("{name}:{t}"),
        None => name.to_owned(),
    };
    if let Some(c) = regex::Regex::new(r"(?i)^(.+)@sha256:([a-f0-9]{64})$")
        .ok()
        .and_then(|r| r.captures(&full))
    {
        let main = c[1].to_owned();
        return (format!("{main}@sha256"), c[2].to_owned());
    }
    let (main, tag) = split_tag(&full);
    match tag {
        Some(t) => {
            let is_hash = t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit());
            let main = if is_hash && !main.ends_with("@sha256") {
                format!("{main}@sha256")
            } else {
                main
            };
            (main, t)
        }
        None => (main, "latest".into()),
    }
}

fn domain_rule(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    domains::validate_domains(s).into_iter().next()
}

fn health_host_rule(v: &Value) -> Option<String> {
    let ok = v.as_str().is_some_and(|h| {
        h.parse::<std::net::IpAddr>().is_ok()
            || (!h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)))
    });
    (!ok).then(|| "The health check host must be a hostname or an IP address.".to_owned())
}

fn field(name: &'static str, rules: Vec<Rule>) -> Field {
    Field { name, rules }
}

/// `ValidationPatterns::shellSafeCommandRules($max)`.
fn shell_safe(max: usize) -> Vec<Rule> {
    vec![
        Rule::Nullable,
        Rule::String,
        Rule::Max(max),
        Rule::Regex(&SHELL_SAFE_COMMAND),
    ]
}

/// `ValidationPatterns::directoryPathRules()` / `filePathRules()` / `dockerTargetRules()`.
fn path_rules(max: usize, re: &'static regex::Regex) -> Vec<Rule> {
    vec![
        Rule::Nullable,
        Rule::String,
        Rule::Max(max),
        Rule::Regex(re),
    ]
}

/// `ValidationPatterns::containerNameRules()`.
fn container_name_rules() -> Vec<Rule> {
    vec![Rule::String, Rule::Max(255), Rule::Regex(&CONTAINER_NAME)]
}

/// `sharedDataApplications()` for the rules coolify-rs ports, in upstream order. A type's
/// overrides keep the shared position (`array_merge`); new keys are appended.
fn shared_rules(overrides: Vec<Field>) -> Vec<Field> {
    let boolean = |name| field(name, vec![Rule::Boolean]);
    let mut fields = vec![
        field("git_repository", vec![Rule::String]),
        field(
            "git_branch",
            vec![Rule::String, Rule::CustomOwned(git_urls::valid_git_branch)],
        ),
        field("build_pack", vec![Rule::In(BUILD_PACKS)]),
    ];
    for name in [
        "is_static",
        "is_spa",
        "is_auto_deploy_enabled",
        "is_force_https_enabled",
        "is_preview_deployments_enabled",
        "use_build_secrets",
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
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "is_permanent_redirect_enabled",
        "is_container_label_readonly_enabled",
        "is_raw_compose_deployment_enabled",
        "is_log_drain_enabled",
        "is_gpu_enabled",
        "is_consistent_container_name_enabled",
    ] {
        fields.push(boolean(name));
    }
    fields.extend([
        field("preview_url_template", vec![Rule::String]),
        field("max_restart_count", vec![Rule::Integer, Rule::MinNum(0)]),
        field(
            "stop_grace_period",
            vec![
                Rule::Nullable,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(3600),
            ],
        ),
        field(
            "rolling_update_drain_seconds",
            vec![Rule::Integer, Rule::MinNum(0), Rule::MaxNum(300)],
        ),
        field(
            "docker_images_to_keep",
            vec![Rule::Integer, Rule::MinNum(0), Rule::MaxNum(100)],
        ),
        field("static_image", vec![Rule::In(&["nginx:alpine"])]),
        field(
            "domains",
            vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(2048),
                Rule::CustomOwned(domain_rule),
            ],
        ),
        field("noindex_domains", vec![Rule::Array, Rule::Nullable]),
        field("redirect", vec![Rule::In(&["www", "non-www", "both"])]),
        field(
            "git_commit_sha",
            vec![Rule::String, Rule::Regex(&GIT_COMMIT_SHA)],
        ),
        field(
            "docker_registry_image_name",
            path_rules(255, &DOCKER_IMAGE_NAME),
        ),
        field(
            "docker_registry_image_tag",
            path_rules(128, &DOCKER_IMAGE_TAG),
        ),
        field("install_command", shell_safe(1000)),
        field("build_command", shell_safe(1000)),
        field("start_command", shell_safe(1000)),
        field(
            "ports_exposes",
            vec![Rule::String, Rule::Regex(&PORTS_EXPOSES)],
        ),
        field(
            "ports_mappings",
            vec![Rule::String, Rule::Regex(&PORTS_MAPPINGS), Rule::Nullable],
        ),
        field("custom_network_aliases", vec![Rule::String, Rule::Nullable]),
        field("base_directory", path_rules(255, &DIRECTORY_PATH)),
        field("publish_directory", path_rules(255, &DIRECTORY_PATH)),
        boolean("health_check_enabled"),
        field(
            "health_check_type",
            vec![Rule::String, Rule::In(&["http", "cmd"])],
        ),
        field(
            "health_check_command",
            vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(1000),
                Rule::Regex(&HEALTH_COMMAND),
            ],
        ),
        field(
            "health_check_path",
            vec![Rule::String, Rule::Regex(&HEALTH_PATH)],
        ),
        field(
            "health_check_port",
            vec![
                Rule::Integer,
                Rule::Nullable,
                Rule::MinNum(1),
                Rule::MaxNum(65535),
            ],
        ),
        field(
            "health_check_host",
            vec![Rule::String, Rule::CustomOwned(health_host_rule)],
        ),
        field(
            "health_check_method",
            vec![Rule::String, Rule::In(&["GET", "HEAD", "POST", "OPTIONS"])],
        ),
        field("health_check_return_code", vec![Rule::Numeric]),
        field(
            "health_check_scheme",
            vec![Rule::String, Rule::In(&["http", "https"])],
        ),
        field(
            "health_check_response_text",
            vec![Rule::String, Rule::Nullable],
        ),
        field("health_check_interval", vec![Rule::Numeric]),
        field("health_check_timeout", vec![Rule::Numeric]),
        field("health_check_retries", vec![Rule::Numeric]),
        field("health_check_start_period", vec![Rule::Numeric]),
        field("limits_memory", vec![Rule::String]),
        field("limits_memory_swap", vec![Rule::String]),
        field("limits_memory_swappiness", vec![Rule::Numeric]),
        field("limits_memory_reservation", vec![Rule::String]),
        field("limits_cpus", vec![Rule::String]),
        field("limits_cpuset", vec![Rule::String, Rule::Nullable]),
        field("limits_cpu_shares", vec![Rule::Numeric]),
        field("custom_labels", vec![Rule::String, Rule::Nullable]),
        field("custom_docker_run_options", shell_safe(2000)),
        field(
            "post_deployment_command",
            vec![Rule::String, Rule::Nullable],
        ),
        field("post_deployment_command_container", container_name_rules()),
        field("pre_deployment_command", vec![Rule::String, Rule::Nullable]),
        field("pre_deployment_command_container", container_name_rules()),
        field("dockerfile_location", path_rules(255, &FILE_PATH)),
        field("dockerfile_target_build", path_rules(128, &DOCKER_TARGET)),
        field("docker_compose_location", path_rules(255, &FILE_PATH)),
        field("docker_compose", vec![Rule::String, Rule::Nullable]),
        field("docker_compose_domains", vec![Rule::Array, Rule::Nullable]),
        field("docker_compose_custom_start_command", shell_safe(1000)),
        field("docker_compose_custom_build_command", shell_safe(1000)),
        boolean("is_container_label_escape_enabled"),
        boolean("is_preserve_repository_enabled"),
        field(
            "http_basic_auth_hash_algorithm",
            vec![Rule::In(&["bcrypt", "argon2id"])],
        ),
        field(
            "http_basic_auth_bcrypt_cost",
            vec![Rule::Integer, Rule::MinNum(4), Rule::MaxNum(14)],
        ),
        field(
            "http_basic_auth_argon2id_memory_cost",
            vec![Rule::Integer, Rule::MinNum(8192), Rule::MaxNum(262144)],
        ),
        field(
            "http_basic_auth_argon2id_time_cost",
            vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(12)],
        ),
    ]);
    for o in overrides {
        match fields.iter_mut().find(|f| f.name == o.name) {
            Some(slot) => *slot = o,
            None => fields.push(o),
        }
    }
    fields
}

/// A thrown exception rendered by Laravel's handler (`APP_DEBUG=false`).
const SERVER_ERROR: ApiError = ApiError::Abort(StatusCode::INTERNAL_SERVER_ERROR, "Server Error");

/// What the shared prologue of `create_application()` resolved.
struct CreateContext {
    input: Input,
    instant: bool,
    /// `$fqdn`: the request `domains`, normalized when it is a string.
    fqdn: Option<String>,
    autogenerate: bool,
    server: servers::ServerRow,
    server_settings: Map<String, Value>,
    environment_id: i64,
    destination_id: i64,
}

/// `create_application($request, $type)` up to the type branch (applications-api.md 2.1
/// steps 1-23). `supported` lists the fields coolify-rs applies for this type.
async fn create_prologue(
    state: &AppState,
    ctx: &AuthCtx,
    request: (&HeaderMap, &Uri, &Bytes),
    kind: &str,
    supported: &[&[&str]],
) -> Result<CreateContext, ApiError> {
    use coolify_domain::Ability;
    let (headers, uri, body) = request;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let raw_input = lenient_json(headers, uri, body);
    let instant = raw_input.get("instant_deploy").is_some_and(php_truthy);
    if instant && !(ctx.abilities.can(Ability::Deploy) || ctx.abilities.can(Ability::Root)) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy",
        ));
    }
    let input = validated_json(headers, uri, body)?;
    let present_keys: Vec<&str> = input.keys().map(String::as_str).collect();
    // Base validation, with the extra-field messages appended.
    let base = [
        field("name", vec![Rule::String, Rule::Max(255)]),
        field("description", vec![Rule::String, Rule::Nullable]),
        field("project_uuid", vec![Rule::String, Rule::Required]),
        field("environment_name", vec![Rule::String, Rule::Nullable]),
        field("environment_uuid", vec![Rule::String, Rule::Nullable]),
        field("server_uuid", vec![Rule::String, Rule::Required]),
        field("destination_uuid", vec![Rule::String]),
        field("is_http_basic_auth_enabled", vec![Rule::Boolean]),
        field(
            "http_basic_auth_username",
            vec![Rule::String, Rule::Nullable],
        ),
        field(
            "http_basic_auth_password",
            vec![Rule::String, Rule::Nullable, Rule::Max(72)],
        ),
        field("autogenerate_domain", vec![Rule::Boolean]),
        field("tags", vec![Rule::Array, Rule::Nullable]),
    ];
    if let Err(ApiError::Validation(mut errors)) =
        validate(&input, &base, CUSTOM_API_MESSAGES, &present_keys)
    {
        extra_field_errors(&input, &mut errors);
        return Err(ApiError::Validation(errors));
    }
    let mut extra = Map::new();
    extra_field_errors(&input, &mut extra);
    if !extra.is_empty() {
        return Err(ApiError::Validation(extra));
    }
    // Fields coolify-rs does not apply yet.
    if input.keys().any(|k| {
        !supported.iter().any(|s| s.contains(&k.as_str()))
            && input.get(k).is_some_and(|v| !v.is_null())
    }) {
        return Err(ApiError::NotPorted);
    }
    if input
        .get("tags")
        .is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(ApiError::NotPorted);
    }
    let env_uuid = input
        .get("environment_uuid")
        .filter(|v| php_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let env_name = input
        .get("environment_name")
        .filter(|v| php_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if env_uuid.is_none() && env_name.is_none() {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "You need to provide at least one of environment_name or environment_uuid.",
        ));
    }
    let mut fqdn: Option<String> = input.str("domains").map(str::to_owned);
    if let Some(d) = input.str("domains") {
        fqdn = domains::normalize_domains(Some(d));
    }
    // A Docker Compose application uses per-service domains; a top-level domain is never routed.
    let autogenerate = (!input.has("autogenerate_domain")
        || input_bool(&input, "autogenerate_domain"))
        && input.str("build_pack") != Some("dockercompose");
    let requested_build_pack = if matches!(kind, "public" | "private-gh-app" | "private-deploy-key")
    {
        input.str("build_pack")
    } else {
        Some(kind)
    };
    if input.has("is_raw_compose_deployment_enabled")
        && input_bool(&input, "is_raw_compose_deployment_enabled")
        && requested_build_pack != Some("dockercompose")
    {
        let mut m = Map::new();
        m.insert(
            "is_raw_compose_deployment_enabled".into(),
            json!("Raw compose deployment can only be enabled for Docker Compose applications."),
        );
        return Err(ApiError::Validation(m));
    }
    let project = coolify_db::projects::find(
        &state.db,
        ctx.team_id,
        input.str("project_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Project not found.",
    ))?;
    let mut env = None;
    for wanted in [env_name.as_deref(), env_uuid.as_deref()]
        .into_iter()
        .flatten()
    {
        if env.is_none() {
            env = coolify_db::projects::find_environment(&state.db, project.id, wanted).await?;
        }
    }
    let env = env.ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Environment not found.",
    ))?;
    let server = servers::find(
        &state.db,
        ctx.team_id,
        input.str("server_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Server not found.",
    ))?;
    let server_settings = server.settings.clone().unwrap_or_default();
    if crate::routes::servers::effective_role_is_build(&server_settings) {
        let mut m = Map::new();
        m.insert(
            "server_uuid".into(),
            json!([
                "The specified server is configured as a build server and cannot host resources."
            ]),
        );
        return Err(ApiError::Validation(m));
    }
    if coolify_db::databases::has_swarm_destinations(&state.db, server.id).await? {
        return Err(ApiError::NotPorted);
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
    let mut destination_id = destinations[0].0;
    if destinations.len() > 1 {
        let wanted = input.str("destination_uuid").unwrap_or_default();
        destination_id = destinations
            .iter()
            .find(|(_, u)| u == wanted)
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                let mut m = Map::new();
                m.insert(
                    "destination_uuid".into(),
                    json!("Provided destination_uuid does not belong to the specified server."),
                );
                ApiError::Validation(m)
            })?;
    }
    Ok(CreateContext {
        input,
        instant,
        fqdn,
        autogenerate,
        server,
        server_settings,
        environment_id: env.id,
        destination_id,
    })
}

/// `validateDataApplications($request, $server)`: `ports_mappings`, then the domains (when
/// the proxy runs). `Some` is the early answer.
async fn validate_data_applications(
    state: &AppState,
    ctx: &AuthCtx,
    cx: &CreateContext,
) -> Result<Option<Response>, ApiError> {
    let input = &cx.input;
    if let Some(p) = input.str("ports_mappings") {
        let mut seen: Vec<&str> = Vec::new();
        for m in p.split(',') {
            let first = m.split(':').next().unwrap_or_default();
            if seen.contains(&first) {
                let mut e = Map::new();
                e.insert(
                    "ports_mappings".into(),
                    json!("The first number before : should be unique between mappings."),
                );
                return Err(ApiError::Validation(e));
            }
            seen.push(first);
        }
    }
    let proxy_type = cx
        .server
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("NONE");
    let proxy_should_run = proxy_type != "NONE"
        && !crate::routes::servers::effective_role_is_build(&cx.server_settings);
    if input.has("domains") && proxy_should_run {
        let errors = domains::validate_domains(input.str("domains").unwrap_or_default());
        if !errors.is_empty() {
            return Ok(Some(
                PhpJson(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": "Validation failed.", "errors": errors}),
                )
                .into_response(),
            ));
        }
        if let Some(conflicts) =
            domain_conflicts(state, ctx.team_id, cx.fqdn.as_deref(), None).await?
            && !input_bool(input, "force_domain_override")
        {
            return Ok(Some(PhpJson(
                StatusCode::CONFLICT,
                json!({
                    "message": "Domain conflicts detected. Use force_domain_override=true to proceed.",
                    "conflicts": conflicts,
                    "warning": "Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior.",
                }),
            )
            .into_response()));
        }
    }
    Ok(None)
}

/// `$application->fill($request->only($allowedFields))` for the columns every type shares.
fn fill_columns(input: &Input, name: String) -> Vec<(&'static str, ColumnValue)> {
    let mut columns: Vec<(&'static str, ColumnValue)> =
        vec![("name", ColumnValue::Text(Some(name)))];
    if input.has("description") {
        columns.push((
            "description",
            ColumnValue::Text(Some(strip_tags(input.str("description")))),
        ));
    }
    for key in [
        "ports_exposes",
        "ports_mappings",
        "redirect",
        "static_image",
        "dockerfile_target_build",
        "docker_compose_custom_start_command",
        "docker_compose_custom_build_command",
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
    ] {
        if input.has(key) {
            let v = match input.get(key) {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            // The `ports_mappings` setter stores '' as null.
            let v = v.filter(|s| !(key == "ports_mappings" && s.is_empty()));
            columns.push((key, ColumnValue::Text(v)));
        }
    }
    for key in [
        "health_check_return_code",
        "health_check_interval",
        "health_check_timeout",
        "health_check_retries",
        "health_check_start_period",
        "limits_memory_swappiness",
        "limits_cpu_shares",
    ] {
        if let Some(v) = input.get(key) {
            let n = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .or_else(|| v.as_f64().map(|f| f as i64));
            columns.push((key, n.map_or(ColumnValue::Text(None), ColumnValue::Int)));
        }
    }
    if input.has("health_check_enabled") {
        columns.push((
            "health_check_enabled",
            ColumnValue::Bool(input_bool(input, "health_check_enabled")),
        ));
    }
    columns
}

/// The `creating` hook (random manual webhook secrets) and the `saving` hook's domain
/// normalization (ports move to `domain_port_overrides`).
fn creating_columns(
    enc: &Encrypter,
    fqdn: Option<&str>,
    columns: &mut Vec<(&'static str, ColumnValue)>,
) {
    for k in [
        "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab",
        "manual_webhook_secret_bitbucket",
        "manual_webhook_secret_gitea",
    ] {
        columns.push((
            k,
            ColumnValue::Text(Some(enc.encrypt_string(&random_str(40)))),
        ));
    }
    let (stored_fqdn, overrides) = domains::normalize_port_overrides(fqdn, &Default::default());
    columns.push(("fqdn", ColumnValue::Text(stored_fqdn.clone())));
    if let Some(o) = &overrides {
        columns.push((
            "domain_port_overrides",
            ColumnValue::Text(Some(domains::overrides_json(stored_fqdn.as_deref(), o))),
        ));
    }
}

/// `applicationSettingsFromRequest()` + `applyApplicationSettings()`, then the explicit
/// settings writes of the type branch: each `explicit` key only when sent (`isset`),
/// `is_container_label_escape_enabled` always, `is_preserve_repository_enabled` always
/// when `preserve` (the git branches).
fn request_settings(
    input: &Input,
    explicit: &[&'static str],
    preserve: bool,
) -> Vec<(&'static str, ColumnValue)> {
    let mut settings: Vec<(&'static str, ColumnValue)> = Vec::new();
    for &key in [
        "is_git_submodules_enabled",
        "is_git_lfs_enabled",
        "is_git_shallow_clone_enabled",
        "disable_build_cache",
        "inject_build_args_to_dockerfile",
        "include_source_commit_in_build",
        "redact_all_env_values_in_logs",
        "include_coolify_default_variables",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "is_raw_compose_deployment_enabled",
    ]
    .iter()
    .chain(explicit)
    {
        if input.get(key).is_some_and(|v| !v.is_null()) {
            settings.push((key, ColumnValue::Bool(input_bool(input, key))));
        }
    }
    for key in ["stop_grace_period", "rolling_update_drain_seconds"] {
        if let Some(v) = input.get(key) {
            let n = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
            settings.push((key, n.map_or(ColumnValue::Text(None), ColumnValue::Int)));
        }
    }
    settings.push((
        "is_container_label_escape_enabled",
        ColumnValue::Bool(
            !input.has("is_container_label_escape_enabled")
                || input_bool(input, "is_container_label_escape_enabled"),
        ),
    ));
    if preserve {
        settings.push((
            "is_preserve_repository_enabled",
            ColumnValue::Bool(input_bool(input, "is_preserve_repository_enabled")),
        ));
    }
    settings
}

/// PHP `trim()` (the model's `saving` hook trims git and command columns).
fn php_trim(s: &str) -> String {
    s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
        .to_owned()
}

/// `$application->save()` and the rest of the type branch: the `created` hook, the
/// generated domain, read-only labels, `isConfigurationChanged(true)`, instant deploy, 201.
async fn finish_create(
    state: &AppState,
    ctx: &AuthCtx,
    cx: &CreateContext,
    columns: Vec<(&'static str, ColumnValue)>,
    settings: Vec<(&'static str, ColumnValue)>,
    application_type: &'static str,
) -> ApiResult {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let build_pack = columns
        .iter()
        .rev()
        .find(|(k, _)| *k == "build_pack")
        .and_then(|(_, v)| match v {
            ColumnValue::Text(t) => t.clone(),
            _ => None,
        })
        .unwrap_or_default();
    let uuid = coolify_domain::new_public_id();
    let now = state.now();
    let id = apps::create(
        &state.db,
        NewApplication {
            uuid: uuid.clone(),
            environment_id: cx.environment_id,
            destination_id: cx.destination_id,
            columns,
            settings,
        },
        now,
    )
    .await?;
    // `created`: Nixpacks applications get NIXPACKS_NODE_VERSION=22 (and its preview copy).
    if build_pack == "nixpacks" {
        coolify_db::environment_variables::create_for_application(
            &state.db,
            id,
            "NIXPACKS_NODE_VERSION",
            &enc.encrypt_serialized("22"),
            &enc.encrypt_serialized("22"),
            &state.version,
            now,
        )
        .await?;
    }
    // generateUrl() when no domain is given.
    let mut app = apps::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    let blank_fqdn = cx.fqdn.as_deref().is_none_or(|f| f.trim().is_empty());
    if cx.autogenerate && blank_fqdn {
        let ip = cx
            .server
            .record
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let wildcard = cx
            .server_settings
            .get("wildcard_domain")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let instance = coolify_db::instance::deployment_settings(&state.db).await?;
        let public = instance.public_ipv4.or(instance.public_ipv6);
        let url = domains::generate_url(wildcard.as_deref(), &ip, public.as_deref(), &uuid).await;
        let (f, o) = domains::normalize_port_overrides(Some(&url), &Default::default());
        let mut values = vec![("fqdn", ColumnValue::Text(f.clone()))];
        if let Some(o) = o {
            values.push((
                "domain_port_overrides",
                ColumnValue::Text(Some(domains::overrides_json(f.as_deref(), &o))),
            ));
        }
        apps::update_if_changed(&state.db, id, &values, now).await?;
        app = apps::find_by_id(&state.db, id)
            .await?
            .ok_or(ApiError::Internal)?;
    }
    // Read-only labels: generated and stored as plain text.
    if app.setting_bool("is_container_label_readonly_enabled") {
        let labels = coolify_jobs::applications::labels::application(
            &coolify_jobs::applications::labels::LabelInput::new(&app, &cx.server),
        );
        apps::update_if_changed(
            &state.db,
            id,
            &[("custom_labels", ColumnValue::Text(Some(labels.join("\n"))))],
            now,
        )
        .await?;
        app = apps::find_by_id(&state.db, id)
            .await?
            .ok_or(ApiError::Internal)?;
    }
    // isConfigurationChanged(true)
    let hash = coolify_jobs::applications::snapshot::config_hash(&state.db, enc, &app)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "configuration hash failed");
            ApiError::Internal
        })?;
    apps::update_if_changed(
        &state.db,
        id,
        &[("config_hash", ColumnValue::Text(Some(hash)))],
        now,
    )
    .await?;
    if cx.instant {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let _ = jobs
            .queue_application_deployment(
                &app,
                &coolify_domain::new_public_id(),
                QueueOptions {
                    is_api: true,
                    no_questions_asked: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "deployment could not be queued");
                ApiError::Internal
            })?;
    }
    // Without instant deploy, upstream dispatches `LoadComposeFile` for dockercompose
    // (clone, read and parse the compose file); not ported (docs/KNOWN_LIMITATIONS.md).
    tracing::info!(target: "audit", event = "api.application.created", team_id = ctx.team_id, application_uuid = %uuid, application_type, build_pack = %build_pack, instant_deploy = cx.instant, "application created");
    let fresh = apps::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    let mut payload = Map::new();
    payload.insert("uuid".into(), Value::String(uuid));
    payload.insert(
        "domains".into(),
        fresh
            .str("fqdn")
            .map_or(Value::Null, |f| Value::String(f.to_owned())),
    );
    Ok(created(Value::Object(serialize_record(payload))).into_response())
}

/// `POST /applications/dockerimage` (`create_application($request, 'dockerimage')`).
pub async fn create_dockerimage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let cx = create_prologue(
        &state,
        &ctx,
        (&headers, &uri, &body),
        "dockerimage",
        &[SUPPORTED, SUPPORTED_DOCKERIMAGE],
    )
    .await?;
    let input = &cx.input;
    let present_keys: Vec<&str> = input.keys().map(String::as_str).collect();
    let rules = shared_rules(vec![
        field(
            "docker_registry_image_name",
            vec![
                Rule::Required,
                Rule::String,
                Rule::Max(255),
                Rule::CustomOwned(docker_image_format),
            ],
        ),
        field(
            "docker_registry_image_tag",
            path_rules(128, &DOCKER_IMAGE_TAG),
        ),
        field(
            "ports_exposes",
            vec![Rule::String, Rule::Regex(&PORTS_EXPOSES), Rule::Nullable],
        ),
    ]);
    validate(input, &rules, CUSTOM_API_MESSAGES, &present_keys)?;
    let name = match input.get("name") {
        Some(v) => strip_tags(v.as_str()),
        None => format!("docker-image-{}", coolify_domain::new_public_id()),
    };
    if let Some(early) = validate_data_applications(&state, &ctx, &cx).await? {
        return Ok(early);
    }
    let (image_name, image_tag) = parse_image(
        input.str("docker_registry_image_name").unwrap_or_default(),
        input.str("docker_registry_image_tag"),
    );
    // fill(): the fillable request values, then the explicit ones.
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut columns = fill_columns(input, name);
    columns.push((
        "docker_registry_image_name",
        ColumnValue::Text(Some(image_name)),
    ));
    columns.push((
        "docker_registry_image_tag",
        ColumnValue::Text(Some(image_tag)),
    ));
    columns.push(("build_pack", ColumnValue::Text(Some("dockerimage".into()))));
    columns.push((
        "git_repository",
        ColumnValue::Text(Some("coollabsio/coolify".into())),
    ));
    columns.push(("git_branch", ColumnValue::Text(Some("main".into()))));
    creating_columns(enc, cx.fqdn.as_deref(), &mut columns);
    let settings = request_settings(
        input,
        &["is_auto_deploy_enabled", "is_force_https_enabled"],
        false,
    );
    finish_create(&state, &ctx, &cx, columns, settings, "dockerimage").await
}

/// `POST /applications/public` (`create_application($request, 'public')`): an application
/// built from a public git repository (applications-api.md 2.1, deployment-job-git.md 1.2).
pub async fn create_public(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    create_git(&state, &ctx, (&headers, &uri, &body), GitKind::Public).await
}

/// `POST /applications/private-github-app` (`create_application($request, 'private-gh-app')`).
pub async fn create_private_github_app(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    create_git(&state, &ctx, (&headers, &uri, &body), GitKind::GithubApp).await
}

/// `POST /applications/private-deploy-key` (`create_application($request, 'private-deploy-key')`).
pub async fn create_private_deploy_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    create_git(&state, &ctx, (&headers, &uri, &body), GitKind::DeployKey).await
}

/// The three git branches of `create_application()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitKind {
    Public,
    GithubApp,
    DeployKey,
}

impl GitKind {
    /// `$type`.
    fn name(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::GithubApp => "private-gh-app",
            Self::DeployKey => "private-deploy-key",
        }
    }
}

/// `gitRepositorySlug()`: `owner/repo` of a GitHub URL, SSH URL or slug.
fn git_repository_slug(repository: &str) -> String {
    if repository.is_empty() {
        return String::new();
    }
    let slug = match git_urls::ssh_git_url_to_https(repository) {
        Some(https) => UrlParts::parse(&https)
            .and_then(|p| p.path)
            .unwrap_or_default(),
        None if repository.starts_with("http") || repository.contains("github.com") => repository
            .replace("https://", "")
            .replace("http://", "")
            .replace("github.com/", ""),
        None => repository.to_owned(),
    };
    let slug = slug.trim_matches('/');
    slug.strip_suffix(".git").unwrap_or(slug).to_owned()
}

/// The git branches of `create_application()` (AC:1542-2395): validation, the type's
/// source (public GitHub, a GitHub App, or a deploy key), `fill()` with the git setters.
async fn create_git(
    state: &AppState,
    ctx: &AuthCtx,
    request: (&HeaderMap, &Uri, &Bytes),
    kind: GitKind,
) -> ApiResult {
    let mut cx = create_prologue(
        state,
        ctx,
        request,
        kind.name(),
        &[SUPPORTED, SUPPORTED_GIT],
    )
    .await?;
    let compose = cx.input.str("build_pack") == Some("dockercompose");
    // Public: ports_exposes is not required for dockercompose, the request value becomes '80'
    // before validation. The private types set it after validation.
    let ports_rule = if compose && kind == GitKind::Public {
        cx.input.0.insert("ports_exposes".into(), json!("80"));
        vec![Rule::String]
    } else {
        vec![Rule::String, Rule::Regex(&PORTS_EXPOSES), Rule::Nullable]
    };
    let repository_rule = if kind == GitKind::GithubApp {
        vec![Rule::String, Rule::Required]
    } else {
        vec![
            Rule::String,
            Rule::Required,
            Rule::CustomOwned(git_urls::valid_git_repository_url),
        ]
    };
    // The rule objects throw a TypeError on a non-empty array.
    if (kind != GitKind::GithubApp && git_urls::rule_throws(cx.input.get("git_repository")))
        || git_urls::rule_throws(cx.input.get("git_branch"))
    {
        return Err(SERVER_ERROR);
    }
    // Plain `Validator::make`: Laravel's default `required` message.
    let mut overrides = vec![
        field("git_repository", repository_rule),
        field(
            "git_branch",
            vec![
                Rule::String,
                Rule::Required,
                Rule::CustomOwned(git_urls::valid_git_branch),
            ],
        ),
        field("build_pack", vec![Rule::Required, Rule::In(BUILD_PACKS)]),
        field("ports_exposes", ports_rule),
    ];
    match kind {
        GitKind::Public => {}
        GitKind::GithubApp => {
            overrides.push(field("github_app_uuid", vec![Rule::String, Rule::Required]))
        }
        GitKind::DeployKey => overrides.push(field(
            "private_key_uuid",
            vec![Rule::String, Rule::Required],
        )),
    }
    if kind != GitKind::Public {
        overrides.push(field("watch_paths", vec![Rule::String, Rule::Nullable]));
    }
    overrides.push(field(
        "docker_compose_domains",
        vec![Rule::Array, Rule::Nullable],
    ));
    let rules = shared_rules(overrides);
    let present_keys: Vec<&str> = cx.input.keys().map(String::as_str).collect();
    validate(&cx.input, &rules, &[], &present_keys)?;
    if compose && cx.input.has("domains") {
        let mut m = Map::new();
        m.insert(
            "domains".into(),
            json!("The domains field cannot be used for dockercompose applications. Use docker_compose_domains instead to set domains for individual services."),
        );
        return Err(ApiError::Validation(m));
    }
    let repository = cx
        .input
        .str("git_repository")
        .unwrap_or_default()
        .to_owned();
    let branch = cx.input.str("git_branch").unwrap_or_default().to_owned();
    let name = match cx.input.get("name") {
        Some(v) => strip_tags(v.as_str()),
        None => git_urls::generate_application_name(&repository, &branch),
    };
    if compose {
        cx.input.0.insert("ports_exposes".into(), json!("80"));
    }
    if let Some(early) = validate_data_applications(state, ctx, &cx).await? {
        return Ok(early);
    }
    // The type's source.
    let mut source_columns: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut git_repository = repository.clone();
    match kind {
        GitKind::Public => {
            // SSH URLs become https://, github.com gets the public GitHub source.
            if let Some(https) = git_urls::ssh_git_url_to_https(&git_repository) {
                git_repository = https;
            }
            let parts = UrlParts::parse(&git_repository);
            // Spatie `Url::fromString()` throws on any other scheme (`git://`).
            if parts.as_ref().is_some_and(|p| {
                !["http", "https", "mailto", "tel"].contains(&p.scheme.to_lowercase().as_str())
            }) {
                return Err(SERVER_ERROR);
            }
            if let Some(p) = parts.filter(|p| p.host == "github.com") {
                let path = p.path_or_root();
                let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
                git_repository = php_trim(&format!(
                    "{}/{}",
                    segments.first().copied().unwrap_or_default(),
                    segments.get(1).copied().unwrap_or_default()
                ));
                source_columns.push((
                    "source_type",
                    ColumnValue::Text(Some(GITHUB_APP_MORPH.into())),
                ));
                source_columns.push(("source_id", ColumnValue::Int(0)));
            }
        }
        GitKind::GithubApp => {
            let github_app = apps::github_app_for_team(
                &state.db,
                ctx.team_id,
                cx.input.str("github_app_uuid").unwrap_or_default(),
            )
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Github App not found.",
            ))?;
            // generateGithubInstallationToken() throws on failure.
            let token = coolify_jobs::github::installation_token(
                &state.db,
                state.encrypter.as_ref(),
                Some(&github_app),
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "github app installation token failed");
                SERVER_ERROR
            })?;
            git_repository = git_repository_slug(&repository);
            let (status, found) = coolify_jobs::github::repository_access(
                Some(&state.db),
                &github_app,
                &token,
                &git_repository,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "github repository check failed");
                SERVER_ERROR
            })?;
            if status == 404 || status == 403 {
                return Err(ApiError::Message(
                    StatusCode::NOT_FOUND,
                    "Repository not found or not accessible by the GitHub App.",
                ));
            }
            if !(200..300).contains(&status) {
                let message = found
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown error");
                return Ok(PhpJson(
                    StatusCode::BAD_REQUEST,
                    json!({"message": format!("Failed to verify repository access: {message}")}),
                )
                .into_response());
            }
            source_columns.push((
                "source_type",
                ColumnValue::Text(Some(GITHUB_APP_MORPH.into())),
            ));
            source_columns.push((
                "source_id",
                ColumnValue::Int(
                    github_app
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
            ));
            source_columns.push((
                "repository_project_id",
                found
                    .get("id")
                    .and_then(Value::as_i64)
                    .map_or(ColumnValue::Text(None), ColumnValue::Int),
            ));
        }
        GitKind::DeployKey => {
            let key = coolify_db::private_keys::find(
                &state.db,
                ctx.team_id,
                cx.input.str("private_key_uuid").unwrap_or_default(),
            )
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Private Key not found.",
            ))?;
            source_columns.push(("private_key_id", ColumnValue::Int(key.id)));
        }
    }
    let input = &cx.input;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let build_pack = input.str("build_pack").unwrap_or_default().to_owned();
    // fill(): `build_pack` precedes the location setters in `$allowedFields`.
    let mut columns = fill_columns(input, name);
    columns.push(("build_pack", ColumnValue::Text(Some(build_pack.clone()))));
    columns.push(("git_branch", ColumnValue::Text(Some(php_trim(&branch)))));
    if let Some(sha) = input.str("git_commit_sha") {
        columns.push(("git_commit_sha", ColumnValue::Text(Some(php_trim(sha)))));
    }
    // `saving` stores `str($value)->trim()`, so a null command becomes ''.
    for key in ["install_command", "build_command", "start_command"] {
        if input.has(key) {
            let v = php_trim(input.str(key).unwrap_or_default());
            columns.push((key, ColumnValue::Text(Some(v))));
        }
    }
    // `baseDirectory` setter: '/'.ltrim($value, '/').
    if input.has("base_directory") {
        let v = input.str("base_directory").unwrap_or_default();
        let v = php_trim(&format!("/{}", v.trim_start_matches('/')));
        columns.push(("base_directory", ColumnValue::Text(Some(v))));
    }
    // `publishDirectory` setter: '/'.ltrim or null; the `saving` hook then sets the trimmed
    // Stringable, which is truthy even when empty, so null ends up as '/'.
    if input.has("publish_directory") {
        let v = input
            .get("publish_directory")
            .filter(|v| php_truthy(v))
            .and_then(Value::as_str)
            .map(|v| format!("/{}", php_trim(v).trim_start_matches('/')))
            .unwrap_or_else(|| "/".into());
        columns.push(("publish_directory", ColumnValue::Text(Some(v))));
    }
    // `dockerfileLocation` / `dockerComposeLocation` setters.
    let location = |v: &str| {
        if v == "/" {
            "/".to_owned()
        } else {
            format!(
                "/{}",
                v.strip_suffix('/').unwrap_or(v).trim_start_matches('/')
            )
        }
    };
    if input.has("dockerfile_location") {
        let v = match input.str("dockerfile_location") {
            Some(v) => Some(location(v)),
            None => (build_pack == "dockerfile").then(|| "/Dockerfile".to_owned()),
        };
        columns.push(("dockerfile_location", ColumnValue::Text(v)));
    }
    if input.has("docker_compose_location") {
        let v = input
            .str("docker_compose_location")
            .map_or_else(|| "/docker-compose.yaml".to_owned(), location);
        columns.push(("docker_compose_location", ColumnValue::Text(Some(v))));
    }
    columns.push((
        "git_repository",
        ColumnValue::Text(Some(php_trim(&git_repository))),
    ));
    columns.extend(source_columns);
    // The `is_static` setter saves `ports_exposes = 80` on the application when truthy.
    if input.get("is_static").is_some_and(php_truthy) {
        columns.push(("ports_exposes", ColumnValue::Text(Some("80".into()))));
    }
    creating_columns(enc, cx.fqdn.as_deref(), &mut columns);
    let settings = request_settings(
        input,
        &[
            "is_static",
            "is_spa",
            "is_auto_deploy_enabled",
            "is_force_https_enabled",
            "is_preview_deployments_enabled",
            "use_build_secrets",
        ],
        true,
    );
    finish_create(state, ctx, &cx, columns, settings, kind.name()).await
}

/// The extra-field messages of `create_application` (`fqdn` has its own).
fn extra_field_errors(input: &Input, errors: &mut Map<String, Value>) {
    for k in input.keys() {
        if !ALLOWED.contains(&k.as_str()) {
            let msg = if k == "fqdn" {
                "Use the domains field to set application URLs."
            } else {
                "This field is not allowed."
            };
            let entry = errors
                .entry(k.clone())
                .or_insert_with(|| Value::Array(vec![]));
            if let Some(list) = entry.as_array_mut() {
                list.push(Value::String(msg.into()));
            }
        }
    }
}

/// `Str::random($n)`.
fn random_str(n: usize) -> String {
    use rand::Rng as _;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// `checkIfDomainIsAlreadyUsedViaAPI`: conflicts with the team's other applications.
async fn domain_conflicts(
    state: &AppState,
    team_id: i64,
    fqdn: Option<&str>,
    exclude_uuid: Option<&str>,
) -> Result<Option<Vec<Value>>, ApiError> {
    let key = |d: &str| {
        let d = d.trim().trim_end_matches('/');
        let lower = d.to_lowercase();
        lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
            .unwrap_or(&lower)
            .to_owned()
    };
    let wanted: Vec<String> = domains::domain_list(fqdn);
    if wanted.is_empty() {
        return Ok(None);
    }
    let mut conflicts = Vec::new();
    for (uuid, name, other) in apps::fqdns_of_team(&state.db, team_id).await? {
        if Some(uuid.as_str()) == exclude_uuid {
            continue;
        }
        for d in &wanted {
            if other.split(',').any(|o| key(o) == key(d)) {
                conflicts.push(json!({
                    "domain": key(d),
                    "resource_name": name,
                    "resource_uuid": uuid,
                    "resource_type": "application",
                    "message": format!("Domain {} is already in use by application '{name}'", key(d)),
                }));
            }
        }
    }
    Ok((!conflicts.is_empty()).then_some(conflicts))
}

/// What a start, restart, stop or delete of this application needs that coolify-rs lacks.
async fn deployable(state: &AppState, a: &ApplicationRow) -> Result<(), ApiError> {
    // The deploy job builds Docker images and the dockerfile, static, nixpacks and railpack
    // git build packs.
    let unsupported = !matches!(
        a.str("build_pack"),
        Some("dockerimage" | "dockerfile" | "static" | "nixpacks" | "railpack" | "dockercompose")
    ) || a.destination.is_none()
        || a.bool("is_http_basic_auth_enabled")
        || a.str("custom_docker_run_options")
            .is_some_and(|o| !o.trim().is_empty())
        || a.str("pre_deployment_command")
            .is_some_and(|o| !o.trim().is_empty())
        || a.str("post_deployment_command")
            .is_some_and(|o| !o.trim().is_empty())
        || !apps::unported_parts(&state.db, a.id).await?.is_empty();
    if unsupported {
        Err(ApiError::NotPorted)
    } else {
        Ok(())
    }
}

/// `POST /applications/{uuid}/start` (`action_deploy`).
pub async fn start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    deployable(&state, &a).await?;
    let input = lenient_json(&headers, &uri, &body);
    let force = query_bool(&uri, &input, "force", false);
    let instant = query_bool(&uri, &input, "instant_deploy", false);
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                force_rebuild: force,
                is_api: true,
                no_questions_asked: instant,
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
    tracing::info!(target: "audit", event = "api.application.deployed", team_id = ctx.team_id, application_uuid = %a.uuid, deployment_uuid = %deployment_uuid, "deployment queued");
    Ok(
        ok(json!({"message": "Deployment request queued.", "deployment_uuid": deployment_uuid}))
            .into_response(),
    )
}

/// `POST /applications/{uuid}/restart` (`action_restart`): a `restart_only` deployment.
pub async fn restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    deployable(&state, &a).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                restart_only: true,
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
    tracing::info!(target: "audit", event = "api.application.restarted", team_id = ctx.team_id, application_uuid = %a.uuid, deployment_uuid = %deployment_uuid, "restart queued");
    Ok(
        ok(json!({"message": "Restart request queued.", "deployment_uuid": deployment_uuid}))
            .into_response(),
    )
}

/// `POST /applications/{uuid}/stop` (`action_stop`).
pub async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if a.destination.is_none() || !apps::unported_parts(&state.db, a.id).await?.is_empty() {
        return Err(ApiError::NotPorted);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_application_stop(a.clone());
    tracing::info!(target: "audit", event = "api.application.stopped", team_id = ctx.team_id, application_uuid = %a.uuid, "stop queued");
    Ok(ok(json!({"message": "Application stopping request queued."})).into_response())
}

/// `DELETE /applications/{uuid}` (`delete_by_uuid`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND_NO_PERIOD)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if a.destination.is_none() || !apps::unported_parts(&state.db, a.id).await?.is_empty() {
        return Err(ApiError::NotPorted);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let input = lenient_json(&headers, &uri, &body);
    let volumes = query_bool(&uri, &input, "delete_volumes", true);
    let networks = query_bool(&uri, &input, "delete_connected_networks", true);
    let configurations = query_bool(&uri, &input, "delete_configurations", true);
    apps::soft_delete(&state.db, a.id, state.now()).await?;
    jobs.spawn_application_delete(a.clone(), volumes, configurations, networks);
    tracing::info!(target: "audit", event = "api.application.deleted", team_id = ctx.team_id, application_uuid = %a.uuid, "deletion queued");
    Ok(ok(json!({"message": "Application deletion request queued."})).into_response())
}

// ---------------------------------------------------------------------------
// Deployments

/// `$deployment->application->toArray()` as `deployment_by_uuid` loads it: the model with its
/// `$hidden`, accessors and the relations the team check and the appends loaded.
async fn deployment_application(
    state: &AppState,
    ctx: &AuthCtx,
    a: &ApplicationRow,
) -> Result<Value, ApiError> {
    let server = match a.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let mut r = APPLICATION.present(a.raw.clone());
    for h in HIDDEN {
        r.shift_remove(*h);
    }
    r.insert(
        "status".into(),
        Value::String(status_getter(a.str("status"))),
    );
    if let Some(Value::String(v)) = r.get("custom_network_aliases") {
        let list = serde_json::from_str::<Vec<String>>(v)
            .map(|l| l.join(","))
            .unwrap_or_else(|_| v.clone());
        r.insert("custom_network_aliases".into(), Value::String(list));
    }
    r.insert("additional_servers_count".into(), json!(0));
    r.insert("additional_networks_count".into(), json!(0));
    r.insert(
        "server_status".into(),
        Value::Bool(server.as_ref().is_some_and(server_functional)),
    );
    let mut project = a.project.clone();
    project.insert("team".into(), Value::Object(a.team.clone()));
    let mut env = a.environment.clone();
    env.insert("project".into(), Value::Object(project));
    r.insert("environment".into(), Value::Object(env));
    r.insert("additional_servers".into(), json!([]));
    if let Some(dest) = &a.destination {
        let mut dest = dest.clone();
        if let Some(s) = &server {
            dest.insert(
                "server".into(),
                nested_server_with(state, s, false, ctx.can_read_sensitive)?,
            );
        }
        r.insert("destination".into(), Value::Object(dest));
    }
    Ok(Value::Object(r))
}

/// `removeSensitiveData($deployment)` + `serializeApiResponse`.
fn present_deployment(ctx: &AuthCtx, d: &Deployment, serialize: bool) -> Value {
    let mut r: Record = APPLICATION_DEPLOYMENT_QUEUE.present(d.raw.clone());
    for k in ["configuration_snapshot", "configuration_diff"] {
        r.shift_remove(k);
    }
    if !ctx.can_read_sensitive {
        r.shift_remove("logs");
    }
    if serialize {
        Value::Object(serialize_record(r))
    } else {
        Value::Object(r)
    }
}

/// `GET /deployments`: queued and in-progress deployments of the team, by id.
pub async fn deployments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let server_ids: Vec<i64> = servers::list_by_name(&state.db, ctx.team_id)
        .await?
        .iter()
        .map(|s| s.id)
        .collect();
    let team_apps: Vec<String> = apps::list_for_team(&state.db, ctx.team_id)
        .await?
        .iter()
        .map(|a| a.id.to_string())
        .collect();
    let rows = apps::running_for_servers(&state.db, &server_ids).await?;
    let items: Vec<Value> = rows
        .iter()
        .filter(|d| {
            d.str("application_id")
                .is_some_and(|id| team_apps.iter().any(|t| t == id))
        })
        .map(|d| present_deployment(&ctx, d, true))
        .collect();
    Ok(ok(Value::Array(items)).into_response())
}

/// `GET /deployments/{uuid}`.
pub async fn deployment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    const MISSING: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Deployment not found.");
    let d = apps::deployment_by_uuid(&state.db, &uuid)
        .await?
        .ok_or(MISSING)?;
    let a = apps::find_by_id(&state.db, d.application_id)
        .await?
        .ok_or(MISSING)?;
    if a.team_id != ctx.team_id {
        return Err(MISSING);
    }
    let Value::Object(mut m) = present_deployment(&ctx, &d, false) else {
        return Err(ApiError::Internal);
    };
    m.insert(
        "application".into(),
        deployment_application(&state, &ctx, &a).await?,
    );
    Ok(ok(Value::Object(serialize_record(m))).into_response())
}

/// `GET /deployments/applications/{uuid}`: `{count, deployments}`, newest first.
pub async fn application_deployments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let num = |k: &str, d: i64| {
        query_param(&uri, k)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(d)
    };
    let (skip, take) = (num("skip", 0).max(0), num("take", 10).max(1));
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND_NO_PERIOD)?;
    let (rows, count) = apps::deployments_for_application(&state.db, a.id, skip, take).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|d| present_deployment(&ctx, d, false))
        .collect();
    Ok(ok(json!({"count": count, "deployments": items})).into_response())
}

/// `POST /deployments/{uuid}/cancel`.
pub async fn cancel_deployment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let d = apps::deployment_by_uuid(&state.db, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Deployment not found.",
        ))?;
    let a = apps::find_by_id(&state.db, d.application_id).await?;
    if a.as_ref().is_none_or(|a| a.team_id != ctx.team_id) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You do not have permission to cancel this deployment.",
        ));
    }
    let message = |status: &str| {
        PhpJson(
            StatusCode::BAD_REQUEST,
            json!({"message": format!("Deployment cannot be cancelled. Current status: {status}")}),
        )
        .into_response()
    };
    if !matches!(d.status.as_str(), "queued" | "in_progress") {
        return Ok(message(&d.status));
    }
    let prev = apps::transition_status(&state.db, d.id, "cancelled-by-user", state.now()).await?;
    if !matches!(prev.as_deref(), Some("queued" | "in_progress")) {
        return Ok(message(prev.as_deref().unwrap_or_default()));
    }
    let entry = |out: &str| apps::LogEntry {
        command: None,
        output: out.to_owned(),
        kind: "stderr",
        timestamp: chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string(),
        hidden: false,
        batch: 1,
    };
    apps::append_log(
        &state.db,
        d.id,
        &[entry("Deployment cancelled by user via API.")],
        state.now(),
    )
    .await?;
    if let Some(jobs) = &state.jobs {
        jobs.cleanup_cancelled_deployment(&d).await;
        if let Some(a) = &a {
            jobs.queue_next_deployment(a.id, d.int("server_id").unwrap_or_default())
                .await;
        }
    }
    tracing::info!(target: "audit", event = "api.deployment.cancelled", team_id = ctx.team_id, deployment_uuid = %uuid, "deployment cancelled");
    Ok(ok(json!({"message": "Deployment cancelled successfully.", "deployment_uuid": uuid, "status": "cancelled-by-user"})).into_response())
}

/// `POST /deploy` by `uuid` (tags and pull requests are not ported).
pub async fn deploy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let s = |k: &str| {
        input.get(k).and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| (!v.is_null()).then(|| v.to_string()))
        })
    };
    let (uuid, tag) = (
        s("uuid").filter(|v| !v.is_empty()),
        s("tag").filter(|v| !v.is_empty()),
    );
    let pr = s("pull_request_id")
        .or_else(|| s("pr"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let docker_tag = s("docker_tag")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let bad = |m: &'static str| Err(ApiError::Message(StatusCode::BAD_REQUEST, m));
    if pr > 2_147_483_647 {
        return bad("pull_request_id must be between 1 and 2147483647.");
    }
    if uuid.is_some() && tag.is_some() {
        return bad("You can only use uuid or tag, not both.");
    }
    if tag.is_some() && pr != 0 {
        return bad("You can only use tag or pr, not both.");
    }
    if docker_tag.is_some() && pr == 0 {
        return bad("docker_tag requires pull_request_id.");
    }
    if docker_tag.is_some() && tag.is_some() {
        return bad("You can only use tag or docker_tag, not both.");
    }
    if tag.is_some() || pr != 0 {
        return Err(ApiError::NotPorted);
    }
    let Some(uuids) = uuid else {
        return bad("You must provide uuid or tag.");
    };
    let list: Vec<&str> = uuids.split(',').filter(|u| !u.is_empty()).collect();
    if list.is_empty() {
        return bad("No UUIDs provided.");
    }
    let force = input_bool(&input, "force");
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut items = Vec::new();
    for u in list {
        let Some(a) = apps::find_in_team(&state.db, ctx.team_id, u).await? else {
            // Services and databases are deployed by their own endpoints in coolify-rs.
            if coolify_db::databases::find_in_team(&state.db, ctx.team_id, u)
                .await?
                .is_some()
            {
                return Err(ApiError::NotPorted);
            }
            continue;
        };
        if !ctx.is_team_admin() {
            items.push(
                json!({"message": "Unauthorized to deploy this application.", "resource_uuid": u}),
            );
            continue;
        }
        deployable(&state, &a).await?;
        let deployment_uuid = coolify_domain::new_public_id();
        let result = jobs
            .queue_application_deployment(
                &a,
                &deployment_uuid,
                QueueOptions {
                    force_rebuild: force,
                    is_api: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "deployment could not be queued");
                ApiError::Internal
            })?;
        match result {
            Queued::QueueFull => {
                let mut resp = PhpJson(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"message": "Deployment queue is full. Please wait for existing deployments to complete."}),
                )
                .into_response();
                resp.headers_mut().insert("Retry-After", axum::http::HeaderValue::from_static("60"));
                return Ok(resp);
            }
            Queued::Skipped { .. } => items.push(json!({
                "message": "Deployment already queued for this commit.",
                "resource_uuid": u,
                "deployment_uuid": deployment_uuid,
            })),
            Queued::Queued { .. } => items.push(json!({
                "message": format!("Application {} deployment queued.", a.str("name").unwrap_or_default()),
                "resource_uuid": u,
                "deployment_uuid": deployment_uuid,
            })),
        }
    }
    if items.is_empty() {
        return Err(ApiError::Message(
            StatusCode::NOT_FOUND,
            "No resources found.",
        ));
    }
    let mut payload = Map::new();
    payload.insert("deployments".into(), Value::Array(items));
    Ok(ok(Value::Object(serialize_record(payload))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_slugs() {
        for (input, slug) in [
            (
                "https://github.com/coollabsio/coolify-examples",
                "coollabsio/coolify-examples",
            ),
            ("https://github.com/o/r.git", "o/r"),
            ("git@github.com:o/r.git", "o/r"),
            ("ssh://git@github.com:22/o/r.git", "o/r"),
            ("o/r", "o/r"),
            ("/o/r/", "o/r"),
            ("https://gitlab.com/o/r", "gitlab.com/o/r"),
            ("", ""),
        ] {
            assert_eq!(git_repository_slug(input), slug, "{input}");
        }
    }

    #[test]
    fn image_parsing() {
        assert_eq!(
            parse_image("nginx", None),
            ("nginx".into(), "latest".into())
        );
        assert_eq!(
            parse_image("nginx:1.25", None),
            ("nginx".into(), "1.25".into())
        );
        assert_eq!(
            parse_image("nginx", Some("alpine")),
            ("nginx".into(), "alpine".into())
        );
        assert_eq!(
            parse_image("registry:5000/app", None),
            ("registry:5000/app".into(), "latest".into())
        );
        let hex = "a".repeat(64);
        assert_eq!(
            parse_image(&format!("nginx@sha256:{hex}"), None),
            ("nginx@sha256".into(), hex.clone())
        );
        assert_eq!(
            parse_image(&format!("nginx:{hex}"), None),
            ("nginx@sha256".into(), hex)
        );
        assert!(docker_image_format(&json!("Nginx")).is_some());
        assert!(
            docker_image_format(&json!("nginx:sha256:abc"))
                .unwrap()
                .contains("must use @")
        );
        assert!(docker_image_format(&json!("ghcr.io/o/a:v1")).is_none());
    }
}
