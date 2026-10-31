//! Preview deployments: `ApplicationPreviewsController` (`index`, `show`, `store`) and
//! `update_preview_by_pull_request_id` and `delete_preview_by_pull_request_id` of
//! `ApplicationsController`.

use std::sync::LazyLock;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::IntoResponse;
use coolify_db::application_ops::{self as ops, NewPreview};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_jobs::applications::domains::{self, UrlParts};
use coolify_jobs::applications::ops::{
    PreviewDomains, compose_preview_volumes, compose_service_names, preview_fqdn,
    preview_fqdn_compose,
};
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::compose_parser::{php, service, validate as compose_validate};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json};
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::routes::application_fields as fields;
use crate::routes::application_ops::NOT_FOUND;
use crate::routes::applications::{field, php_trim};
use crate::routes::databases::input_bool;
use crate::state::AppState;
use crate::validation::{Rule, validate};

const PREVIEW_NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Preview not found.");
const GIT_TYPES: &[&str] = &["github", "gitlab", "gitea", "bitbucket"];

static COMMIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{7,40}\n?\z").unwrap_or_else(|_| unreachable!()));
static DOCKER_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}\n?\z").unwrap_or_else(|_| unreachable!())
});

/// The check every route with a `pull_request_id` makes.
pub(crate) fn parse_pull_request_id(raw: &str) -> Result<i64, ApiError> {
    let invalid = ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, "Invalid pull_request_id.");
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid);
    }
    match raw.parse::<i64>() {
        Ok(id) if id > 0 && id <= 2_147_483_647 => Ok(id),
        _ => Err(invalid),
    }
}

/// `$preview->...` timestamps (`Y-m-d\TH:i:s.u\Z`).
fn iso(r: &Record, key: &str) -> Value {
    match r.get(key).and_then(Value::as_str) {
        Some(t) => Value::String(format!("{t}.000000Z")),
        None => Value::Null,
    }
}

/// `formatPreview($preview)`.
fn format_preview(r: &Record) -> Value {
    let text = |k: &str| r.get(k).and_then(Value::as_str);
    let compose = text("docker_compose_domains")
        .filter(|c| !c.is_empty() && *c != "0")
        .and_then(|c| serde_json::from_str::<Value>(c).ok());
    let compose = match compose {
        Some(Value::Object(services)) => Value::Array(
            services
                .iter()
                .map(|(name, entry)| {
                    let domain = entry
                        .get("domain")
                        .and_then(Value::as_str)
                        .filter(|d| !d.is_empty() && *d != "0");
                    json!({
                        "name": name,
                        "domain": domain,
                        "redirect": entry.get("redirect").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect(),
        ),
        Some(Value::Array(a)) if a.is_empty() => Value::Array(Vec::new()),
        _ => Value::Null,
    };
    json!({
        "uuid": text("uuid"),
        "pull_request_id": r.get("pull_request_id"),
        "pull_request_html_url": text("pull_request_html_url").filter(|u| !u.is_empty() && *u != "0"),
        "git_type": text("git_type"),
        "status": text("status"),
        "domains": text("fqdn"),
        "docker_compose_domains": compose,
        "domain_port_overrides": ops::json_column(r, "domain_port_overrides"),
        "docker_registry_image_tag": text("docker_registry_image_tag"),
        "last_online_at": text("last_online_at").map(|t| t.replace('T', " ")),
        "created_at": iso(r, "created_at"),
        "updated_at": iso(r, "updated_at"),
    })
}

/// `'errors' => [$field => [$message]]`.
fn listed_error(field: &str, message: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), json!([message]));
    ApiError::Validation(errors)
}

fn id_of(r: &Record) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or_default()
}

/// `GET /applications/{uuid}/previews` (`index`).
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let previews: Vec<Value> = ops::previews(&state.db, a.id)
        .await?
        .iter()
        .map(format_preview)
        .collect();
    Ok(ok(Value::Array(previews)).into_response())
}

/// `GET /applications/{uuid}/previews/{pull_request_id}` (`show`).
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, pull_request_id)): Path<(String, String)>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let id = parse_pull_request_id(&pull_request_id)?;
    let preview = ops::preview(&state.db, a.id, id)
        .await?
        .ok_or(PREVIEW_NOT_FOUND)?;
    Ok(ok(format_preview(&preview)).into_response())
}

/// `$resource->previews()->where('pull_request_id', $pr)->first()`, or
/// `upsertDockerImagePreview($application, $pr, $dockerTag)` for a Docker Image application.
pub(crate) async fn deploy_preview(
    state: &AppState,
    a: &ApplicationRow,
    pull_request_id: i64,
    docker_tag: Option<&str>,
) -> Result<Option<Record>, ApiError> {
    use coolify_db::application_ops as store;
    let existing = store::preview(&state.db, a.id, pull_request_id).await?;
    if a.str("build_pack") != Some("dockerimage") {
        return Ok(existing);
    }
    let Some(preview) = existing else {
        let Some(tag) = docker_tag else {
            return Ok(None);
        };
        let preview = store::create_preview(
            &state.db,
            &store::NewPreview {
                application_id: a.id,
                pull_request_id,
                pull_request_html_url: String::new(),
                git_type: None,
                docker_compose_domains: None,
                docker_registry_image_tag: Some(tag.to_owned()),
            },
            state.now(),
        )
        .await?;
        store_preview_domains(state, a, &preview).await?;
        return Ok(store::preview(&state.db, a.id, pull_request_id).await?);
    };
    let current = preview
        .get("docker_registry_image_tag")
        .and_then(Value::as_str);
    if let Some(tag) = docker_tag.filter(|t| current != Some(*t)) {
        store::update_preview(
            &state.db,
            id_of(&preview),
            &[(
                "docker_registry_image_tag",
                coolify_db::servers::ColumnValue::Text(Some(tag.to_owned())),
            )],
            state.now(),
        )
        .await?;
        return Ok(store::preview(&state.db, a.id, pull_request_id).await?);
    }
    Ok(Some(preview))
}

/// `generate_preview_fqdn_compose()` or `generate_preview_fqdn()` for a preview of `a`, saved.
pub(crate) async fn store_preview_domains(
    state: &AppState,
    a: &ApplicationRow,
    preview: &Record,
) -> Result<(), ApiError> {
    let pull_request_id = preview
        .get("pull_request_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let template = a.str("preview_url_template").unwrap_or_default();
    let overrides = fields::stored_overrides(a);
    let generated: Option<PreviewDomains> = if a.str("build_pack") == Some("dockercompose") {
        let raw = a.str("docker_compose_raw").unwrap_or_default();
        // The parse in `composeServiceNamesForPreview()` stores the preview's volumes.
        for (name, target) in compose_preview_volumes(raw, &a.uuid, pull_request_id) {
            coolify_db::compose::persistent_volume_upsert(
                &state.db,
                apps::MORPH,
                a.id,
                &name,
                &target,
                state.now(),
            )
            .await?;
        }
        let names = compose_service_names(raw);
        Some(preview_fqdn_compose(
            a.str("docker_compose_domains"),
            preview
                .get("docker_compose_domains")
                .and_then(Value::as_str),
            &names,
            template,
            pull_request_id,
            &overrides,
        ))
    } else {
        preview_fqdn(a.str("fqdn"), template, pull_request_id, &overrides)
    };
    let Some(generated) = generated else {
        return Ok(());
    };
    let mut values = vec![
        ("fqdn", ColumnValue::Text(generated.fqdn)),
        (
            "domain_port_overrides",
            ColumnValue::Text(generated.overrides),
        ),
    ];
    if let Some(compose) = generated.compose {
        values.push(("docker_compose_domains", ColumnValue::Text(Some(compose))));
    }
    ops::update_preview(&state.db, id_of(preview), &values, state.now())
        .await
        .map_err(fields::save_error)?;
    Ok(())
}

/// `sourceGitType($application)`.
async fn source_git_type(
    state: &AppState,
    a: &ApplicationRow,
) -> Result<Option<&'static str>, ApiError> {
    let kind = match a.str("source_type") {
        Some("App\\Models\\GithubApp") => "github",
        Some("App\\Models\\GitlabApp") => "gitlab",
        Some("App\\Models\\GiteaApp") => "gitea",
        _ => return Ok(None),
    };
    let found = match (a.str("source_type"), a.int("source_id")) {
        (Some(t), Some(id)) => apps::source_exists(&state.db, t, id).await?,
        _ => false,
    };
    Ok(found.then_some(kind))
}

/// `missing`: any value fails, `Expand` only runs when the key is there.
fn missing_rule(
    attribute: &'static str,
    message: fn(&Value) -> Vec<(String, String)>,
) -> crate::validation::Field {
    field(attribute, vec![Rule::Expand(message)])
}

/// `POST /applications/{uuid}/previews` (`store`).
#[allow(clippy::too_many_lines)]
pub async fn store(
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
    // git_based() is false for an inline Dockerfile.
    let is_docker_image = a.str("build_pack") == Some("dockerimage");
    let inline_dockerfile = a
        .str("dockerfile")
        .is_some_and(|d| !d.is_empty() && d != "0");
    if !is_docker_image && inline_dockerfile {
        return Ok(PhpJson(
            StatusCode::BAD_REQUEST,
            json!({"message": "Preview deployments need a Git based or Docker Image application."}),
        )
        .into_response());
    }
    let input = lenient_json(&headers, &uri, &body);
    let mut rules = vec![
        field(
            "pull_request_id",
            vec![
                Rule::Required,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(2_147_483_647),
            ],
        ),
        field(
            "pull_request_html_url",
            vec![Rule::Nullable, Rule::Url, Rule::Max(2048)],
        ),
        field("force", vec![Rule::Boolean]),
        field("instant_deploy", vec![Rule::Boolean]),
    ];
    if is_docker_image {
        rules.push(missing_rule("git_type", |_| {
            vec![(
                "git_type".into(),
                "The git type field must be missing.".into(),
            )]
        }));
        rules.push(missing_rule("commit", |_| {
            vec![("commit".into(), "The commit field must be missing.".into())]
        }));
        rules.push(field(
            "docker_tag",
            vec![Rule::Nullable, Rule::String, Rule::Regex(&DOCKER_TAG)],
        ));
    } else {
        rules.push(field(
            "git_type",
            vec![Rule::Nullable, Rule::String, Rule::In(GIT_TYPES)],
        ));
        let mut commit = vec![Rule::Nullable, Rule::String, Rule::Regex(&COMMIT)];
        // required_if:git_type,bitbucket
        if input.str("git_type") == Some("bitbucket") {
            commit.push(Rule::Required);
        }
        rules.push(field("commit", commit));
        rules.push(missing_rule("docker_tag", |_| {
            vec![(
                "docker_tag".into(),
                "The docker tag field must be missing.".into(),
            )]
        }));
    }
    validate(
        &input,
        &rules,
        &[(
            "commit.required",
            "The commit field is required when git type is bitbucket.",
        )],
        // `Validator::make` without an extra field check.
        &input.keys().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let pull_request_id = match input.get("pull_request_id") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_default(),
        Some(Value::String(s)) => s.trim().parse().unwrap_or_default(),
        _ => 0,
    };
    // `$request->string('docker_tag')->trim()->value() ?: null`.
    let docker_tag = input
        .str("docker_tag")
        .map(php_trim)
        .filter(|t| !t.is_empty() && t != "0");
    let existing = ops::preview(&state.db, a.id, pull_request_id).await?;
    let stored = |k: &str| {
        existing
            .as_ref()
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let git_type: Option<String> = match input.str("git_type") {
        Some(t) => Some(t.to_owned()),
        None => match stored("git_type") {
            Some(t) => Some(t),
            None => source_git_type(&state, &a).await?.map(str::to_owned),
        },
    };
    if !is_docker_image && git_type.is_none() {
        return Err(listed_error(
            "git_type",
            "The git type field is required for applications without a GitHub, GitLab, or Gitea App source.",
        ));
    }
    if is_docker_image
        && docker_tag.is_none()
        && stored("docker_registry_image_tag").is_none_or(|t| t.trim().is_empty())
    {
        return Err(listed_error(
            "docker_tag",
            "The docker tag field is required for a new Docker Image preview.",
        ));
    }
    let now = state.now();
    let html_url = input.str("pull_request_html_url");
    let is_created = existing.is_none();
    let preview_id = if let Some(preview) = &existing {
        // fill(array_filter([...]))
        let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
        if let Some(u) = html_url {
            values.push((
                "pull_request_html_url",
                ColumnValue::Text(Some(u.to_owned())),
            ));
        }
        if !is_docker_image && let Some(t) = &git_type {
            values.push(("git_type", ColumnValue::Text(Some(t.clone()))));
        }
        if let Some(t) = &docker_tag {
            values.push((
                "docker_registry_image_tag",
                ColumnValue::Text(Some(t.clone())),
            ));
        }
        ops::update_preview(&state.db, id_of(preview), &values, now)
            .await
            .map_err(fields::save_error)?;
        id_of(preview)
    } else {
        let is_compose = a.str("build_pack") == Some("dockercompose");
        let preview = ops::create_preview(
            &state.db,
            &NewPreview {
                application_id: a.id,
                pull_request_id,
                pull_request_html_url: html_url.unwrap_or_default().to_owned(),
                git_type: if is_docker_image {
                    None
                } else {
                    git_type.clone()
                },
                docker_compose_domains: if is_compose {
                    a.str("docker_compose_domains").map(str::to_owned)
                } else {
                    None
                },
                docker_registry_image_tag: docker_tag.clone(),
            },
            now,
        )
        .await
        .map_err(fields::save_error)?;
        store_preview_domains(&state, &a, &preview).await?;
        tracing::info!(target: "audit", event = "api.application.preview_created", team_id = ctx.team_id, application_uuid = %a.uuid, pull_request_id, "preview created");
        id_of(&preview)
    };
    let status = if is_created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let fresh = ops::preview_by_id(&state.db, preview_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let instant = !input.has("instant_deploy") || input_bool(&input, "instant_deploy");
    if !instant {
        return Ok(PhpJson(
            status,
            json!({
                "message": if is_created { "Preview created." } else { "Preview updated." },
                "deployment_uuid": null,
                "preview": format_preview(&fresh),
            }),
        )
        .into_response());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let text = |k: &str| fresh.get(k).and_then(Value::as_str).map(str::to_owned);
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                commit: input.str("commit").map(str::to_owned),
                force_rebuild: input_bool(&input, "force"),
                is_api: true,
                pull_request_id,
                git_type: text("git_type"),
                docker_registry_image_tag: text("docker_registry_image_tag"),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "preview deployment could not be queued");
            ApiError::Internal
        })?;
    let (message, deployment) = match &result {
        Queued::QueueFull => {
            return Ok((
                [(header::RETRY_AFTER, "60")],
                PhpJson(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"message": result.message()}),
                ),
            )
                .into_response());
        }
        Queued::Skipped { .. } => (result.message(), None),
        Queued::Queued { .. } => ("Preview deployment queued.", Some(deployment_uuid.clone())),
    };
    if deployment.is_some() {
        tracing::info!(target: "audit", event = "api.deployment.triggered", application_uuid = %a.uuid, deployment_uuid = %deployment_uuid, pull_request_id, "preview deployment queued");
    }
    let fresh = ops::preview_by_id(&state.db, preview_id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(PhpJson(
        status,
        json!({
            "message": message,
            "deployment_uuid": deployment,
            "preview": format_preview(&fresh),
        }),
    )
    .into_response())
}

/// `applicationDomainRules()` with the errors of the closure already evaluated.
fn domain_rules(input: &Input) -> Vec<Rule> {
    let mut rules = vec![Rule::Present, Rule::Nullable, Rule::String, Rule::Max(2048)];
    if let Some(Value::String(s)) = input.get("domains") {
        for e in domains::validate_domains(s) {
            rules.push(Rule::Evaluated(Some(e)));
        }
    }
    rules
}

fn compose_items_rules(v: &Value) -> Vec<(String, String)> {
    fields::compose_domain_rules_distinct(v)
}

/// The `$dockerComposeDomains` of `update_preview_by_pull_request_id`, as an ordered map.
fn compose_entries(input: &Input, names: &[String], existing: Option<&str>) -> Map<String, Value> {
    let existing: Value = existing
        .and_then(|e| serde_json::from_str(e).ok())
        .unwrap_or(Value::Null);
    let old_redirect = |name: &str| {
        fields::redirect_of(existing.get(name).and_then(|e| e.get("redirect"))).map(str::to_owned)
    };
    let mut entries = Map::new();
    for name in names {
        let mut entry = Map::new();
        entry.insert("domain".into(), json!(""));
        if let Some(r) = old_redirect(name) {
            entry.insert("redirect".into(), json!(r));
        }
        entries.insert(name.clone(), Value::Object(entry));
    }
    for item in fields::compose_items(input) {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let domain = item.get("domain").and_then(Value::as_str);
        let mut entry = Map::new();
        entry.insert(
            "domain".into(),
            json!(domains::normalize_domains(domain).unwrap_or_default()),
        );
        let redirect = match item.get("redirect") {
            Some(v) => fields::redirect_of(Some(v)).map(str::to_owned),
            None => old_redirect(name),
        };
        if let Some(r) = redirect {
            entry.insert("redirect".into(), json!(r));
        }
        entries.insert(name.to_owned(), Value::Object(entry));
    }
    entries
}

/// The non-database services of the Compose file, or the validation error upstream answers with.
fn compose_services_of(a: &ApplicationRow) -> Result<Vec<String>, ApiError> {
    let compose = compose_validate::parse_docker_compose_yaml(
        a.str("docker_compose_raw").unwrap_or_default(),
    )
    .map_err(|_| {
        fields::field_message(
            "docker_compose_domains",
            "The Docker Compose configuration could not be parsed.",
        )
    })?;
    let services = match php::get(&compose, "services") {
        Some(coolify_compat::yaml::PhpValue::Array(items)) if !items.is_empty() => items,
        _ => {
            return Err(fields::field_message(
                "docker_compose_domains",
                "The Docker Compose configuration must define at least one service.",
            ));
        }
    };
    Ok(services
        .iter()
        .filter(|(_, s)| {
            let image = php::get(s, "image").map(php::to_string).unwrap_or_default();
            !service::is_database_image(&image, s)
        })
        .map(|(name, _)| name.as_string())
        .collect())
}

/// `PATCH /applications/{uuid}/previews/{pull_request_id}` (`update_preview_by_pull_request_id`).
#[allow(clippy::too_many_lines)]
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, pull_request_id)): Path<(String, String)>,
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
    let id = parse_pull_request_id(&pull_request_id)?;
    let preview = ops::preview(&state.db, a.id, id)
        .await?
        .ok_or(PREVIEW_NOT_FOUND)?;
    let is_compose = a.str("build_pack") == Some("dockercompose");
    let input = lenient_json(&headers, &uri, &body);
    let mut rules = vec![field("force_domain_override", vec![Rule::Boolean])];
    if is_compose {
        rules.push(missing_rule("domains", |_| {
            vec![(
                "domains".into(),
                "The domains field must be missing.".into(),
            )]
        }));
        rules.push(field(
            "docker_compose_domains",
            vec![Rule::Present, Rule::Array],
        ));
        rules.push(field(
            "docker_compose_domains.*",
            vec![Rule::Expand(compose_items_rules)],
        ));
    } else {
        rules.push(field("domains", domain_rules(&input)));
        rules.push(missing_rule("docker_compose_domains", |_| {
            vec![(
                "docker_compose_domains".into(),
                "The docker compose domains field must be missing.".into(),
            )]
        }));
    }
    validate(
        &input,
        &rules,
        &[],
        &input.keys().map(String::as_str).collect::<Vec<_>>(),
    )?;

    let error_key = if is_compose {
        "docker_compose_domains"
    } else {
        "domains"
    };
    let mut compose_map: Option<Map<String, Value>> = None;
    let submitted: Option<String> = if is_compose {
        let services = compose_services_of(&a)?;
        let requested_invalid = fields::compose_items(&input).iter().any(|item| {
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            !services.iter().any(|s| s == name)
        });
        if requested_invalid {
            return Err(fields::field_message(
                "docker_compose_domains",
                "One or more Docker Compose services are invalid.",
            ));
        }
        let entries = compose_entries(
            &input,
            &services,
            preview
                .get("docker_compose_domains")
                .and_then(Value::as_str),
        );
        let all: Vec<String> = entries
            .values()
            .filter_map(|e| e.get("domain").and_then(Value::as_str))
            .filter(|d| !d.is_empty() && *d != "0")
            .map(str::to_owned)
            .collect();
        compose_map = Some(entries);
        (!all.is_empty()).then(|| all.join(","))
    } else {
        domains::normalize_domains(input.str("domains"))
    };
    let portless_submitted: Vec<String> = domains::domain_list(submitted.as_deref())
        .iter()
        .map(|d| domains::without_port(d))
        .collect();
    let has_duplicates = portless_submitted
        .iter()
        .enumerate()
        .any(|(i, d)| portless_submitted[..i].contains(d));
    if has_duplicates {
        return Err(fields::field_message(
            error_key,
            "The same domain cannot be configured more than once.",
        ));
    }
    let (portless, overrides) =
        domains::normalize_port_overrides(submitted.as_deref(), &std::collections::BTreeMap::new());
    let urls = domains::domain_list(portless.as_deref());
    let force = input_bool(&input, "force_domain_override");
    let conflicts = fields::domain_conflicts(&state, ctx.team_id, &urls, None).await?;
    if !conflicts.is_empty() && !force {
        return Ok(fields::conflict_response(conflicts));
    }
    // A domain of another preview of the team.
    let hosts: Vec<String> = urls
        .iter()
        .filter_map(|u| UrlParts::parse(u))
        .map(|p| p.host)
        .filter(|h| !h.is_empty())
        .collect();
    if !force {
        for (other_uuid, other_pr, other_fqdn) in
            ops::other_preview_domains(&state.db, ctx.team_id, id_of(&preview), &hosts).await?
        {
            let shared = domains::domain_list(Some(&other_fqdn))
                .iter()
                .map(|d| domains::without_port(d))
                .find(|d| urls.contains(d));
            if let Some(domain) = shared {
                return Ok(fields::conflict_response(vec![json!({
                    "domain": domain,
                    "resource_name": format!("Preview deployment #{other_pr}"),
                    "resource_uuid": other_uuid,
                    "resource_type": "application",
                    "message": "Domain is already in use by another preview deployment.",
                })]));
            }
        }
    }
    let overrides_text = overrides
        .as_ref()
        .map(|o| domains::overrides_json(portless.as_deref(), o));
    let mut values = vec![
        (
            "domain_port_overrides",
            ColumnValue::Text(overrides_text.clone()),
        ),
        ("fqdn", ColumnValue::Text(portless.clone())),
    ];
    let mut compose_response = Value::Null;
    if let Some(entries) = compose_map {
        let mut portless_entries = Map::new();
        let mut list = Vec::new();
        for (name, mut entry) in entries {
            let domain = domains::domain_list(entry.get("domain").and_then(Value::as_str))
                .iter()
                .map(|d| domains::without_port(d))
                .collect::<Vec<_>>()
                .join(",");
            if let Some(e) = entry.as_object_mut() {
                e.insert("domain".into(), json!(domain));
            }
            let mut item = Map::new();
            item.insert("name".into(), json!(name));
            if let Some(e) = entry.as_object() {
                item.extend(e.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            list.push(Value::Object(item));
            portless_entries.insert(name, entry);
        }
        compose_response = Value::Array(list);
        values.push((
            "docker_compose_domains",
            ColumnValue::Text(Some(coolify_compat::php_json::encode(&Value::Object(
                portless_entries,
            )))),
        ));
    }
    ops::update_preview(&state.db, id_of(&preview), &values, state.now())
        .await
        .map_err(fields::save_error)?;
    tracing::info!(target: "audit", event = "api.application.preview_updated", team_id = ctx.team_id, application_uuid = %a.uuid, pull_request_id = id, "preview updated");
    let overrides_value = overrides_text
        .as_deref()
        .and_then(|o| serde_json::from_str::<Value>(o).ok())
        .unwrap_or(Value::Null);
    Ok(ok(json!({
        "uuid": preview.get("uuid"),
        "pull_request_id": id,
        "domains": portless,
        "docker_compose_domains": compose_response,
        "domain_port_overrides": overrides_value,
    }))
    .into_response())
}

/// `DELETE /applications/{uuid}/previews/{pull_request_id}` (`delete_preview_by_pull_request_id`).
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, pull_request_id)): Path<(String, String)>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let id = parse_pull_request_id(&pull_request_id)?;
    let preview = ops::preview(&state.db, a.id, id)
        .await?
        .ok_or(PREVIEW_NOT_FOUND)?;
    ops::soft_delete_preview(&state.db, id_of(&preview), state.now()).await?;
    if let Some(jobs) = state.jobs.as_ref() {
        jobs.cleanup_preview_deployment(&a, id, id_of(&preview))
            .await;
    }
    tracing::info!(target: "audit", event = "api.application.preview_deleted", team_id = ctx.team_id, application_uuid = %a.uuid, pull_request_id = id, "preview deleted");
    Ok(ok(json!({"message": "Preview deletion request queued."})).into_response())
}
