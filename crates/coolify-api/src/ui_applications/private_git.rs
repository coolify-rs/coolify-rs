//! The private repository forms: `Project\New\{GithubPrivateRepository, GitlabPrivateRepository,
//! GiteaPrivateRepository, GithubPrivateRepositoryDeployKey}`, and `Project\New\SimpleDockerfile`
//! and `Project\New\DockerCompose`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, NewApplication};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::detect::CheckoutCredentials;
use coolify_jobs::applications::domains;
use serde_json::{Map, Value, json};

use super::public_git::{
    FILE_PATH, base_directory_value, branch_rule, compose_location_value, destination,
    detection_json, failure, import_variables, invalid, publish_directory_value, repository_rule,
};
use super::{body, random_str, s};
use crate::auth::AuthCtx;
use crate::git_urls;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Github,
    Gitlab,
    Gitea,
}

impl Provider {
    fn parse(v: &str) -> Result<Self, ApiError> {
        match v {
            "github" => Ok(Self::Github),
            "gitlab" => Ok(Self::Gitlab),
            "gitea" => Ok(Self::Gitea),
            _ => Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found")),
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Github => "github_apps",
            Self::Gitlab => "gitlab_apps",
            Self::Gitea => "gitea_apps",
        }
    }
    fn morph(self) -> &'static str {
        match self {
            Self::Github => r"App\Models\GithubApp",
            Self::Gitlab => r"App\Models\GitlabApp",
            Self::Gitea => r"App\Models\GiteaApp",
        }
    }
    /// `GithubApp::whereNotNull('app_id')` / `GitlabApp::private()` / `GiteaApp::private()`.
    fn usable(self) -> &'static str {
        match self {
            Self::Github => "app_id IS NOT NULL",
            _ => "access_token IS NOT NULL",
        }
    }
}

fn db_err(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "private repository query failed");
    ApiError::Internal
}

fn int_of(m: &Map<String, Value>, k: &str) -> Option<i64> {
    match m.get(k) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

async fn source_row(
    state: &AppState,
    ctx: &AuthCtx,
    p: Provider,
    id: i64,
) -> Result<Record, ApiError> {
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {} s WHERE (team_id = $1 OR is_system_wide = true) AND is_public = false AND {} AND id = $2",
        p.table(),
        p.usable()
    ))
    .bind(ctx.team_id)
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(db_err)?;
    let text = row.ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    coolify_db::records::parse_row(&text).map_err(|_| ApiError::Internal)
}

/// `mount()`: the Apps the form offers.
pub async fn sources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
) -> ApiResult {
    let p = Provider::parse(&provider)?;
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {} s WHERE (team_id = $1 OR is_system_wide = true) AND is_public = false AND {}",
        p.table(),
        p.usable()
    ))
    .bind(ctx.team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(db_err)?;
    let mut list = Vec::with_capacity(rows.len());
    for t in rows {
        let r = coolify_db::records::parse_row(&t).map_err(|_| ApiError::Internal)?;
        list.push(json!({"id": r.get("id"), "name": r.get("name"), "html_url": r.get("html_url")}));
    }
    let system = crate::routes::git_apps::can_access_system_resources(&state, ctx.user_id).await?;
    Ok(ok(json!({
        "sources": list,
        "can_create": ctx.is_team_admin(),
        "can_make_system_wide": system,
        "random_name": crate::routes::private_keys::random_name().chars().take(30).collect::<String>(),
    }))
    .into_response())
}

/// `Collection::sortBy('name')`.
fn sort_by_name(list: &mut [Value]) {
    list.sort_by(|a, b| {
        let name = |v: &Value| {
            v.get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        name(a).cmp(&name(b))
    });
}

/// `sortBranchesByPriority()`.
fn sort_branches(list: &mut [Value]) {
    list.sort_by_key(|b| {
        let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
        match name {
            "main" => "0_main".to_owned(),
            "master" => "1_master".to_owned(),
            other => format!("2_{other}"),
        }
    });
}

/// `loadRepositories($id)`.
pub async fn repositories(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let p = Provider::parse(&provider)?;
    let m = body(&bytes);
    let row = source_row(&state, &ctx, p, int_of(&m, "source_id").unwrap_or(-1)).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let answer: Result<Vec<Value>, String> = async {
        let mut all = Vec::new();
        match p {
            Provider::Github => {
                let token =
                    coolify_jobs::github::installation_token(&state.db, Some(enc), Some(&row))
                        .await?;
                let mut page = 1;
                loop {
                    let r = coolify_jobs::github::repository_page(&state.db, &row, &token, page)
                        .await?;
                    let total = r.get("total_count").and_then(Value::as_i64).unwrap_or(0);
                    let items = r
                        .get("repositories")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let empty = items.is_empty();
                    all.extend(items);
                    if empty || all.len() as i64 >= total {
                        break;
                    }
                    page += 1;
                }
            }
            Provider::Gitlab | Provider::Gitea => {
                let mut page = 1;
                loop {
                    let r = if p == Provider::Gitlab {
                        coolify_jobs::gitlab::load_repositories(&state.db, enc, &row, page).await?
                    } else {
                        coolify_jobs::gitea::load_repositories(&state.db, enc, &row, page).await?
                    };
                    all.extend(
                        r.get("repositories")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    if r.get("has_more").and_then(Value::as_bool) != Some(true) || page >= 50 {
                        break;
                    }
                    page += 1;
                }
            }
        }
        Ok(all)
    }
    .await;
    let mut list = match answer {
        Ok(l) => l,
        Err(e) => return failure(e),
    };
    sort_by_name(&mut list);
    let installation_path =
        (p == Provider::Github).then(|| crate::ui_sources::installation_path(&row));
    Ok(ok(json!({"repositories": list, "installation_path": installation_path})).into_response())
}

/// `loadBranches()`: the sorted branches of the repository the form selected.
pub async fn branches(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let p = Provider::parse(&provider)?;
    let m = body(&bytes);
    let row = source_row(&state, &ctx, p, int_of(&m, "source_id").unwrap_or(-1)).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let repository = s(&m, "repository");
    let answer: Result<Vec<Value>, String> = async {
        let mut all: Vec<Value> = Vec::new();
        let mut page: i64 = 1;
        loop {
            let items: Vec<Value> = match p {
                Provider::Github => {
                    let token =
                        coolify_jobs::github::installation_token(&state.db, Some(enc), Some(&row))
                            .await?;
                    let page_text = page.to_string();
                    let (status, body) = coolify_jobs::github::get_with_token(
                        &state.db,
                        &row,
                        &token,
                        &format!("/repos/{repository}/branches"),
                        &[("per_page", "100"), ("page", page_text.as_str())],
                    )
                    .await?;
                    if status != 200 {
                        let error = body
                            .get("message")
                            .and_then(Value::as_str)
                            .map_or_else(|| format!("HTTP {status}"), str::to_owned);
                        return Err(format!(
                            "Failed to load branches of {repository} with error: {error}"
                        ));
                    }
                    body.as_array().cloned().unwrap_or_default()
                }
                Provider::Gitlab => coolify_jobs::gitlab::load_branches(
                    &state.db,
                    enc,
                    &row,
                    int_of(&m, "project_id").unwrap_or(-1),
                    page,
                )
                .await?
                .as_array()
                .cloned()
                .unwrap_or_default(),
                Provider::Gitea => {
                    coolify_jobs::gitea::load_branches(&state.db, enc, &row, &repository, page)
                        .await?
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                }
            };
            let limit = if p == Provider::Gitea { 50 } else { 100 };
            let more = items.len() == limit && (p == Provider::Github || page < 50);
            all.extend(items);
            if !more {
                break;
            }
            page += 1;
        }
        Ok(all)
    }
    .await;
    let mut list = match answer {
        Ok(l) => l,
        Err(e) => return failure(e),
    };
    sort_branches(&mut list);
    Ok(ok(json!({"branches": list})).into_response())
}

/// `detectRepository()` for a private repository.
pub async fn scan(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let branch = s(&m, "git_branch");
    let base = m
        .get("base_directory")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_owned();
    let base_re = regex::Regex::new(r"^/?(?:[a-zA-Z0-9_.-]+/)*[a-zA-Z0-9_.-]*$")
        .unwrap_or_else(|_| unreachable!());
    let dots = regex::Regex::new(r"(^|/)\.\.?(/|$)").unwrap_or_else(|_| unreachable!());
    if branch.is_empty()
        || branch_rule(&branch, "branch").is_some()
        || base.is_empty()
        || !base_re.is_match(&base)
        || dots.is_match(&base)
    {
        return Ok(ok(json!({"detection": null})).into_response());
    }
    let Ok(Some((_, server_id))) = destination(&state, &ctx, &s(&m, "destination_uuid")).await
    else {
        return Ok(
            ok(json!({"detection": null, "error": "Destination not found."})).into_response(),
        );
    };
    let Some(jobs) = state.jobs.as_ref() else {
        return Err(ApiError::NotPorted);
    };
    let mut repository = s(&m, "repository");
    let pem;
    let port;
    let row;
    let credentials = if provider == "deploy-key" {
        // applicationForDetection(): get_git_source() first.
        match deploy_key_source(&state, &repository).await? {
            Ok((converted, _)) => repository = converted,
            Err(e) => return failure(e),
        }
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let id = int_of(&m, "private_key_id").unwrap_or(-1);
        if !coolify_db::private_keys::exists_in_team(&state.db, ctx.team_id, id).await? {
            return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
        }
        let cipher = coolify_db::git_sources::private_key_ciphertext(&state.db, id)
            .await?
            .ok_or(ApiError::Internal)?;
        pem = enc
            .decrypt_maybe_serialized(&cipher)
            .map_err(|_| ApiError::Internal)?;
        port = "22".to_owned();
        CheckoutCredentials::DeployKey {
            pem: &pem,
            port: &port,
        }
    } else {
        let p = Provider::parse(&provider)?;
        row = source_row(&state, &ctx, p, int_of(&m, "source_id").unwrap_or(-1)).await?;
        match p {
            Provider::Github => CheckoutCredentials::Github(&row),
            Provider::Gitlab => CheckoutCredentials::Oauth(&row, false),
            Provider::Gitea => CheckoutCredentials::Oauth(&row, true),
        }
    };
    let answer = jobs
        .detect_private_repository(server_id, credentials, &repository, &branch, &base)
        .await;
    Ok(match answer {
        Ok(d) => ok(json!({"detection": detection_json(&d, false)})),
        Err(e) => {
            tracing::debug!(len = e.len(), "repository detection failed");
            ok(json!({"detection": {
                "failed": true,
                "toast": "Smart Scan could not read the repository. Check the repository access and the branch.",
                "suggested_build_pack": null,
                "dockerfiles": [],
                "docker_compose_files": [],
                "dockerfile_ports": {},
                "env_files": [],
            }}))
        }
    }
    .into_response())
}

/// What the forms set beyond the build configuration.
struct Target<'a> {
    name_repository: String,
    git_repository: String,
    git_branch: String,
    project_id: Option<i64>,
    private_key_id: Option<i64>,
    source: Option<(i64, &'a str)>,
    /// `true` for the deploy key form, `generate_random_name($uuid)`.
    random_name: bool,
}

/// The common part of `submit()`.
async fn create_application(
    state: &AppState,
    ctx: &AuthCtx,
    m: &Map<String, Value>,
    t: Target<'_>,
) -> ApiResult {
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    let build_pack = text("build_pack").unwrap_or_default();
    let compose_location = text("docker_compose_location");
    let base = text("base_directory");
    let is_static = m.get("is_static").and_then(Value::as_bool).unwrap_or(false);
    let port = match m.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_owned(),
        _ => String::new(),
    };
    let Some((destination_id, server_id)) =
        destination(state, ctx, &s(m, "destination_uuid")).await?
    else {
        return failure("Destination not found.".into());
    };
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &s(m, "project_uuid"))
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let env =
        coolify_db::projects::find_environment(&state.db, project.id, &s(m, "environment_uuid"))
            .await?
            .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let mut dockerfile_location: Option<String> = None;
    if build_pack == "dockerfile"
        && let Some(path) = text("selected_dockerfile")
            .map(|p| p.trim().trim_start_matches('/').to_owned())
            .filter(|p| !p.is_empty())
    {
        let location = format!("/{path}");
        if location.chars().count() > 255 || !FILE_PATH.is_match(&location) {
            let msg = if location.chars().count() > 255 {
                "The selected dockerfile field must not be greater than 255 characters."
            } else {
                "The Dockerfile location must be a valid path starting with / and containing only alphanumeric characters, dots, hyphens, underscores, slashes, @, ~, and +."
            };
            return invalid(vec![("selectedDockerfile", msg.into())]);
        }
        dockerfile_location = Some(location);
    }
    let env_imported = m
        .get("env_imported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let variables: Vec<(String, Option<String>)> = match m.get("env_vars") {
        Some(Value::Array(list)) if env_imported => list
            .iter()
            .map(|pair| {
                (
                    pair.get(0)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    pair.get(1).and_then(Value::as_str).map(str::to_owned),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    let trimmed = |v: &str| ColumnValue::Text(Some(v.trim().to_owned()));
    let uuid = coolify_domain::new_public_id();
    let name = if t.random_name {
        crate::routes::private_keys::random_name()
    } else {
        git_urls::application_name_with(&t.name_repository, &t.git_branch, &uuid)
    };
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(Some(name))),
        ("git_repository", trimmed(&t.git_repository)),
        ("git_branch", trimmed(&t.git_branch)),
        ("build_pack", ColumnValue::Text(Some(build_pack.clone()))),
        ("ports_exposes", ColumnValue::Text(Some(port))),
        (
            "publish_directory",
            ColumnValue::Text(Some(publish_directory_value(
                text("publish_directory").as_deref(),
            ))),
        ),
    ];
    if let Some(id) = t.project_id {
        columns.push(("repository_project_id", ColumnValue::Int(id)));
    }
    if let Some(id) = t.private_key_id {
        columns.push(("private_key_id", ColumnValue::Int(id)));
    }
    if let Some((id, morph)) = t.source {
        columns.push(("source_id", ColumnValue::Int(id)));
        columns.push(("source_type", ColumnValue::Text(Some(morph.to_owned()))));
    }
    columns.push((
        "base_directory",
        ColumnValue::Text(Some(base_directory_value(base.as_deref()))),
    ));
    if matches!(build_pack.as_str(), "dockerfile" | "dockerimage") {
        columns.push(("health_check_enabled", ColumnValue::Bool(false)));
    }
    if let Some(l) = dockerfile_location {
        columns.push(("dockerfile_location", ColumnValue::Text(Some(l))));
    }
    if build_pack == "dockercompose" {
        columns.push((
            "docker_compose_location",
            ColumnValue::Text(Some(compose_location_value(compose_location.as_deref()))),
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
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
    let now = state.now();
    let id = apps::create(
        &state.db,
        NewApplication {
            destination_type: coolify_db::databases::destination_morph(
                &state.db,
                &s(m, "destination_uuid"),
            )
            .await?,
            uuid: uuid.clone(),
            environment_id: env.id,
            destination_id,
            columns,
            settings: Vec::new(),
        },
        now,
    )
    .await?;
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
    apps::update_settings(
        &state.db,
        id,
        &[("is_static", ColumnValue::Bool(is_static))],
        now,
    )
    .await?;
    if is_static {
        apps::update_if_changed(
            &state.db,
            id,
            &[("ports_exposes", ColumnValue::Text(Some("80".into())))],
            now,
        )
        .await?;
    }
    set_generated_url(state, id, server_id, &uuid).await?;
    import_variables(state, id, &variables).await?;
    tracing::info!(target: "audit", event = "application.created", team_id = ctx.team_id, application_uuid = %uuid, build_pack = %build_pack, "application created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `generateUrl(server: $destination->server, random: $application->uuid)` and its save.
async fn set_generated_url(
    state: &AppState,
    id: i64,
    server_id: i64,
    uuid: &str,
) -> Result<(), ApiError> {
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let wildcard = server
        .settings
        .as_ref()
        .and_then(|s| s.get("wildcard_domain"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let instance = coolify_db::instance::deployment_settings(&state.db).await?;
    let public = instance.public_ipv4.or(instance.public_ipv6);
    let url = domains::generate_url(wildcard.as_deref(), &ip, public.as_deref(), uuid).await;
    let (fqdn, overrides) = domains::normalize_port_overrides(Some(&url), &Default::default());
    let mut values = vec![("fqdn", ColumnValue::Text(fqdn.clone()))];
    if let Some(o) = overrides {
        values.push((
            "domain_port_overrides",
            ColumnValue::Text(Some(domains::overrides_json(fqdn.as_deref(), &o))),
        ));
    }
    apps::update_if_changed(&state.db, id, &values, state.now()).await?;
    Ok(())
}

static OWNER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9\-_]+$").unwrap_or_else(|_| unreachable!())
});
static REPO: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9\-_\.]+$").unwrap_or_else(|_| unreachable!())
});

/// `validator(...)->fails()`: `Invalid repository data: …` with the first message.
fn invalid_repository(m: &Map<String, Value>) -> Option<String> {
    let branch = s(m, "git_branch");
    if branch.is_empty() {
        return Some("The selected branch name field is required.".into());
    }
    if let Some(msg) = branch_rule(&branch, "selected branch name") {
        return Some(msg);
    }
    if let Some(c) = m
        .get("docker_compose_location")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        if c.chars().count() > 255 {
            return Some(
                "The docker compose location field must not be greater than 255 characters.".into(),
            );
        }
        if !FILE_PATH.is_match(c) {
            return Some("The docker compose location field format is invalid.".into());
        }
    }
    super::public_git::base_directory_rule(m, "base directory")
}

/// `GithubPrivateRepository::submit()`, `GitlabPrivateRepository::submit()` and
/// `GiteaPrivateRepository::submit()`.
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut m = body(&bytes);
    super::public_git::normalize_base_directory(&mut m);
    if provider == "deploy-key" {
        return create_deploy_key(&state, &ctx, &m).await;
    }
    let p = Provider::parse(&provider)?;
    let repository = s(&m, "repository");
    let bad = if p == Provider::Github {
        let (owner, repo) = repository.split_once('/').unwrap_or((&repository, ""));
        if owner.is_empty() {
            Some("The selected repository owner field is required.".to_owned())
        } else if !OWNER.is_match(owner) {
            Some("The selected repository owner field format is invalid.".to_owned())
        } else if repo.is_empty() {
            Some("The selected repository repo field is required.".to_owned())
        } else if !REPO.is_match(repo) {
            Some("The selected repository repo field format is invalid.".to_owned())
        } else {
            None
        }
    } else if repository.is_empty() {
        Some(format!(
            "The selected repository {} field is required.",
            if p == Provider::Gitlab {
                "path"
            } else {
                "full name"
            }
        ))
    } else {
        None
    };
    if let Some(msg) = bad.or_else(|| invalid_repository(&m)) {
        return failure(format!("Invalid repository data: {msg}"));
    }
    let row = source_row(&state, &ctx, p, int_of(&m, "source_id").unwrap_or(-1)).await?;
    let source_id = row.get("id").and_then(Value::as_i64).unwrap_or(-1);
    create_application(
        &state,
        &ctx,
        &m,
        Target {
            name_repository: repository.clone(),
            git_repository: repository,
            git_branch: s(&m, "git_branch"),
            project_id: int_of(&m, "repository_project_id"),
            private_key_id: None,
            source: Some((source_id, p.morph())),
            random_name: false,
        },
    )
    .await
}

/// `get_git_source()`: `(repository, Public GitHub source)`.
async fn deploy_key_source(
    state: &AppState,
    url: &str,
) -> Result<Result<(String, Option<i64>), String>, ApiError> {
    if let Some(msg) = repository_rule(url, "repository url") {
        return Ok(Err(format!("Invalid repository URL: {msg}")));
    }
    if git_urls::parse_scp_style_git_url(url).is_some() || url.starts_with("ssh://") {
        return Ok(Ok((url.to_owned(), None)));
    }
    let parts = domains::UrlParts::parse(url);
    let host = parts.as_ref().map(|p| p.host.clone()).unwrap_or_default();
    let path = parts.and_then(|p| p.path).unwrap_or_default();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let repository = format!(
        "{}/{}",
        segments.first().copied().unwrap_or_default(),
        segments.get(1).copied().unwrap_or_default()
    );
    if host == "github.com" {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM github_apps WHERE name = 'Public GitHub' LIMIT 1")
                .fetch_optional(state.db.pool())
                .await
                .map_err(db_err)?;
        return Ok(Ok((repository, id)));
    }
    if url.starts_with("http") {
        let git = format!("git@{host}:{repository}");
        let finished = if git.ends_with(".git") {
            git
        } else {
            format!("{git}.git")
        };
        return Ok(Ok((finished, None)));
    }
    Ok(Ok((url.to_owned(), None)))
}

/// `GithubPrivateRepositoryDeployKey::submit()`.
async fn create_deploy_key(state: &AppState, ctx: &AuthCtx, m: &Map<String, Value>) -> ApiResult {
    let key = int_of(m, "private_key_id").unwrap_or(-1);
    if !coolify_db::private_keys::exists_in_team(&state.db, ctx.team_id, key).await? {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let url = s(m, "repository_url");
    let branch = s(m, "git_branch");
    let mut errors: Vec<(&str, String)> = Vec::new();
    if url.is_empty() {
        errors.push(("repository_url", "The Repository field is required.".into()));
    } else if let Some(msg) = repository_rule(&url, "Repository") {
        errors.push(("repository_url", msg));
    }
    if branch.is_empty() {
        errors.push(("branch", "The Branch field is required.".into()));
    } else if let Some(msg) = branch_rule(&branch, "Branch") {
        errors.push(("branch", msg));
    }
    let port = match m.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_owned(),
        _ => String::new(),
    };
    if port.is_empty() {
        errors.push(("port", "The Port field is required.".into()));
    } else if !coolify_compat::yaml::php_is_numeric(&port) {
        errors.push(("port", "The Port field must be a number.".into()));
    }
    if s(m, "build_pack").is_empty() {
        errors.push(("build_pack", "The Build pack field is required.".into()));
    }
    if let Some(msg) = super::public_git::base_directory_rule(m, "Base directory") {
        errors.push(("base_directory", msg));
    }
    if let Some(c) = m
        .get("docker_compose_location")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        && !FILE_PATH.is_match(c)
    {
        errors.push((
            "docker_compose_location",
            "The docker compose location field format is invalid.".into(),
        ));
    }
    if !errors.is_empty() {
        return invalid(errors);
    }
    let (repository, source) = match deploy_key_source(state, &url).await? {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    create_application(
        state,
        ctx,
        m,
        Target {
            name_repository: repository.clone(),
            git_repository: repository,
            git_branch: branch,
            project_id: None,
            private_key_id: Some(key),
            source: source.map(|id| (id, r"App\Models\GithubApp")),
            random_name: true,
        },
    )
    .await
}

/// `GithubPrivateRepositoryDeployKey::mount()`: the keys of the team, without the root key.
pub async fn keys(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, name, description FROM private_keys WHERE team_id = $1 AND id != 0",
    )
    .bind(ctx.team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(ok(json!({"keys": rows.iter().map(|(id, name, d)| json!({"id": id, "name": name, "description": d})).collect::<Vec<_>>()})).into_response())
}

/// `get_port_from_dockerfile()`.
fn port_from_dockerfile(dockerfile: &str) -> Option<i64> {
    let line = dockerfile
        .split('\n')
        .map(str::trim)
        .find(|l| l.starts_with("EXPOSE"))?;
    let value = line.replacen("EXPOSE", "", 1);
    // `(int) "8080/tcp"`.
    let digits: String = value
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some(digits.parse().unwrap_or(0)).filter(|p| *p != 0)
}

/// `SimpleDockerfile::submit()`.
pub async fn create_dockerfile(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let dockerfile = m
        .get("dockerfile")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if dockerfile.trim().is_empty() {
        return invalid(vec![(
            "dockerfile",
            "The dockerfile field is required.".into(),
        )]);
    }
    let Some((destination_id, server_id)) =
        destination(&state, &ctx, &s(&m, "destination_uuid")).await?
    else {
        return failure("Destination not found.".into());
    };
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &s(&m, "project_uuid"))
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let env =
        coolify_db::projects::find_environment(&state.db, project.id, &s(&m, "environment_uuid"))
            .await?
            .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let port = port_from_dockerfile(&dockerfile).unwrap_or(80);
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        (
            "name",
            ColumnValue::Text(Some(format!("dockerfile-{uuid}"))),
        ),
        ("repository_project_id", ColumnValue::Int(0)),
        (
            "git_repository",
            ColumnValue::Text(Some("coollabsio/coolify".into())),
        ),
        ("git_branch", ColumnValue::Text(Some("main".into()))),
        ("build_pack", ColumnValue::Text(Some("dockerfile".into()))),
        ("dockerfile", ColumnValue::Text(Some(dockerfile.clone()))),
        ("ports_exposes", ColumnValue::Text(Some(port.to_string()))),
        ("health_check_enabled", ColumnValue::Bool(false)),
        ("source_id", ColumnValue::Int(0)),
        (
            "source_type",
            ColumnValue::Text(Some(r"App\Models\GithubApp".into())),
        ),
        ("base_directory", ColumnValue::Text(Some("/".into()))),
        ("publish_directory", ColumnValue::Text(None)),
    ];
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
    let id = apps::create(
        &state.db,
        NewApplication {
            destination_type: coolify_db::databases::destination_morph(
                &state.db,
                &s(&m, "destination_uuid"),
            )
            .await?,
            uuid: uuid.clone(),
            environment_id: env.id,
            destination_id,
            columns,
            settings: Vec::new(),
        },
        state.now(),
    )
    .await?;
    set_generated_url(&state, id, server_id, &uuid).await?;
    // parseHealthcheckFromDockerfile(isInit: true) only runs with health checks enabled.
    tracing::info!(target: "audit", event = "application.created", team_id = ctx.team_id, application_uuid = %uuid, build_pack = "dockerfile", "application created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `DockerCompose::submit()`: through the same code as `POST /services` with a raw Compose file.
pub async fn create_compose(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    bytes: Bytes,
) -> ApiResult {
    use base64::Engine as _;
    let m = body(&bytes);
    let raw = s(&m, "docker_compose_raw");
    if raw.trim().is_empty() {
        return invalid(vec![(
            "dockerComposeRaw",
            "The docker compose raw field is required.".into(),
        )]);
    }
    let Some((_, server_id)) = destination(&state, &ctx, &s(&m, "destination_uuid")).await? else {
        return failure("Destination not found.".into());
    };
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let server_uuid = server
        .record
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let request = json!({
        "project_uuid": s(&m, "project_uuid"),
        "environment_uuid": s(&m, "environment_uuid"),
        "server_uuid": server_uuid,
        "destination_uuid": s(&m, "destination_uuid"),
        "docker_compose_raw": base64::engine::general_purpose::STANDARD.encode(raw.as_bytes()),
    });
    let mut json_headers = headers;
    json_headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    crate::routes::services::create(
        State(state),
        Extension(ctx),
        json_headers,
        uri,
        Bytes::from(request.to_string()),
    )
    .await
}
