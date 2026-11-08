//! The "Public Git Repository" resource form: `Project\New\PublicGitRepository` with the
//! `HasRepositoryDetection` trait. The page keeps the Livewire component's state and sends it
//! with each request; `check` is `loadBranch()` (URL normalisation, the GitHub branch lookup and
//! the Smart Scan it starts), `scan` is `detectRepository()`, `create` is `submit()`.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, NewApplication};
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::detect::{self, Detection};
use coolify_jobs::applications::domains::{self, UrlParts};
use serde_json::{Map, Value, json};

use super::{body, random_str, s, summary};
use crate::auth::AuthCtx;
use crate::git_urls;
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::state::AppState;

const GITHUB_APP_MORPH: &str = r"App\Models\GithubApp";

/// `githubApi()` needs the source's API URL; `getBranch()` only runs for GitHub sources.
struct PublicGithub {
    id: i64,
    api_url: String,
    html_url: String,
}

/// `GithubApp::where('name', 'Public GitHub')->first()`.
async fn public_github(state: &AppState) -> Result<Option<PublicGithub>, ApiError> {
    let row: Option<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, api_url, html_url FROM github_apps WHERE name = 'Public GitHub' LIMIT 1",
    )
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "github source lookup failed");
        ApiError::Internal
    })?;
    Ok(row.map(|(id, api, html)| PublicGithub {
        id,
        api_url: api.unwrap_or_default(),
        html_url: html.unwrap_or_default(),
    }))
}

/// What `getGitSource()` derives from the repository URL.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSource {
    host: String,
    /// `owner/repo` for GitHub, the whole URL otherwise.
    repository: String,
    /// The `tree/<branch…>` part of a GitHub-style URL, else `main`.
    branch: String,
    github: bool,
}

/// `getGitSource()` (without the validation it repeats): `Url::fromString()` segments.
fn git_source(url: &str) -> GitSource {
    let parts = UrlParts::parse(url);
    let host = parts.as_ref().map(|p| p.host.clone()).unwrap_or_default();
    let path = parts.and_then(|p| p.path).unwrap_or_default();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let segment = |n: usize| segments.get(n - 1).copied().unwrap_or_default();
    let mut branch = "main".to_owned();
    if segment(3) == "tree" {
        let trimmed = path.trim_matches('/');
        branch = trimmed
            .split_once("tree/")
            .map_or(trimmed, |(_, after)| after)
            .to_owned();
    }
    let github = host == "github.com";
    GitSource {
        repository: if github {
            format!("{}/{}", segment(1), segment(2))
        } else {
            url.to_owned()
        },
        host,
        branch,
        github,
    }
}

/// `loadBranch()`'s URL rewriting: SSH to HTTPS, `.git` for non-GitHub HTTP(S) URLs.
fn normalize_repository_url(url: &str) -> String {
    let mut url = git_urls::ssh_git_url_to_https(url).unwrap_or_else(|| url.to_owned());
    if (url.starts_with("https://") || url.starts_with("http://"))
        && !url.ends_with(".git")
        && (!url.contains("github.com") || !url.contains("git.sr.ht"))
        && !url.contains("tangled")
    {
        url.push_str(".git");
    }
    if url.contains("github.com") && url.ends_with(".git") {
        url.truncate(url.len() - 4);
    }
    url
}

pub(super) fn repository_rule(url: &str, attribute: &str) -> Option<String> {
    git_urls::valid_git_repository_url(&Value::String(url.to_owned()))
        .map(|m| m.replace(":attribute", attribute))
}

pub(super) fn branch_rule(branch: &str, attribute: &str) -> Option<String> {
    git_urls::valid_git_branch(&Value::String(branch.to_owned()))
        .map(|m| m.replace(":attribute", attribute))
}

/// `str($original)->after($tried)->trim('/')` as the base directory.
fn base_directory_after(original: &str, tried: &str) -> String {
    let rest = if tried.is_empty() {
        original
    } else {
        original.split_once(tried).map_or(original, |(_, r)| r)
    };
    let rest = rest.trim_matches('/');
    if rest.is_empty() {
        "/".into()
    } else {
        format!("/{rest}")
    }
}

/// The destination of `?destination=` in the current team and its server.
pub(super) async fn destination(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
) -> Result<Option<(i64, i64)>, ApiError> {
    let Some((destination_id, server_id)) =
        coolify_db::databases::team_destination(&state.db, ctx.team_id, uuid).await?
    else {
        return Ok(None);
    };
    // `$destination->server?->canHostResources()`: a build server cannot deploy.
    let server = servers::find_by_id(&state.db, server_id).await?;
    if server.is_none_or(|s| {
        crate::routes::servers::effective_role_is_build(&s.settings.unwrap_or_default())
    }) {
        return Ok(None);
    }
    Ok(Some((destination_id, server_id)))
}

/// `applyDetectionResult()`'s input, as the page applies it.
pub(super) fn detection_json(d: &Detection, failed: bool) -> Value {
    let ports: Map<String, Value> = d
        .dockerfile_ports
        .iter()
        .map(|(f, p)| (f.clone(), p.map_or(Value::Null, Value::from)))
        .collect();
    // `parseEnvFormatToArray()` flattened per file, in file order.
    let env_files: Vec<Value> = d
        .env_files
        .iter()
        .map(|(file, content)| {
            let vars = content
                .as_deref()
                .map(detect::parse_env_format)
                .unwrap_or_default();
            json!({"file": file, "variables": vars.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()})
        })
        .collect();
    json!({
        "failed": failed,
        "suggested_build_pack": d.suggested_build_pack(),
        "dockerfiles": d.dockerfiles,
        "docker_compose_files": d.docker_compose_files,
        "dockerfile_ports": ports,
        "env_files": env_files,
    })
}

/// `detectRepository()`: `Ok(None)` when it stops before scanning (validation), the toast
/// message in `Err`.
async fn run_detection(
    state: &AppState,
    ctx: &AuthCtx,
    repository_url: &str,
    branch: &str,
    base_directory: &str,
    destination_uuid: &str,
) -> Result<Option<Value>, String> {
    // Validator::make(['branch', 'base_directory'])->validate() throws outside the try:
    // Livewire keeps the errors in a bag no field of this form shows.
    let base_re = regex::Regex::new(r"^/?(?:[a-zA-Z0-9_.-]+/)*[a-zA-Z0-9_.-]*$")
        .unwrap_or_else(|_| unreachable!());
    let dots = regex::Regex::new(r"(^|/)\.\.?(/|$)").unwrap_or_else(|_| unreachable!());
    if branch.is_empty()
        || branch_rule(branch, "branch").is_some()
        || base_directory.is_empty()
        || !base_re.is_match(base_directory)
        || dots.is_match(base_directory)
    {
        return Ok(None);
    }
    let Ok(Some((_, server_id))) = destination(state, ctx, destination_uuid).await else {
        return Err("Destination not found.".into());
    };
    let source = git_source(repository_url);
    let clone_url = if source.github {
        let html = public_github(state)
            .await
            .ok()
            .flatten()
            .map(|g| g.html_url)
            .unwrap_or_else(|| "https://github.com".into());
        format!("{html}/{}", source.repository)
    } else {
        source.repository.clone()
    };
    let Some(jobs) = state.jobs.as_ref() else {
        return Err(
            "Smart Scan could not read the repository. Check the repository access and the branch."
                .into(),
        );
    };
    match jobs
        .detect_repository(server_id, &clone_url, branch, base_directory)
        .await
    {
        Ok(d) => Ok(Some(detection_json(&d, false))),
        Err(e) => {
            // The error can contain the clone command, so only its kind is logged.
            tracing::debug!(len = e.len(), "repository detection failed");
            Ok(Some(json!({
                "failed": true,
                "toast": "Smart Scan could not read the repository. Check the repository access and the branch.",
                "suggested_build_pack": null,
                "dockerfiles": [],
                "docker_compose_files": [],
                "dockerfile_ports": {},
                "env_files": [],
            })))
        }
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// `PublicGitRepository::loadBranch()`, then `detectRepository()` when the branch was found.
/// The body carries the component state (`rate_limit_remaining`, `git_branch`, …); the answer
/// is the new state, `error` for the toast `handleError()` shows, and `detection` when the
/// Smart Scan ran.
pub async fn check(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let url = m
        .get("repository_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let required = url.trim().is_empty();
    if let Some(msg) = if required {
        Some("The repository url field is required.".to_owned())
    } else {
        repository_rule(&url, "repository url")
    } {
        return Ok(ok(json!({"error": format!("Invalid repository URL: {msg}")})).into_response());
    }
    let url = normalize_repository_url(&url);
    let mut rate_remaining = int_of(m.get("rate_limit_remaining"));
    let mut rate_reset = m.get("rate_limit_reset").cloned().unwrap_or(json!(0));

    // getGitSource()
    let mut error: Option<String> = None;
    let source = git_source(&url);
    let mut git_branch = source.branch.clone();
    let mut base_directory = "/".to_owned();
    let mut branch_found = false;
    let github = if source.github {
        public_github(&state).await?
    } else {
        None
    };
    // getBranch(): `Err` is the exception of the last lookup.
    let lookup = |branch: String| {
        let repository = source.repository.clone();
        let api = github
            .as_ref()
            .map(|g| g.api_url.clone())
            .unwrap_or_default();
        async move {
            let mut tried = branch.clone();
            loop {
                match detect::github_branch(&api, &repository, &tried).await {
                    Ok(limit) => return Ok((tried, limit)),
                    Err(e) => {
                        if let Some((before, _)) = tried.rsplit_once('/') {
                            tried = before.to_owned();
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
        }
    };
    let apply = |original: &str,
                 tried: String,
                 limit: detect::RateLimit,
                 remaining: &mut i64,
                 reset: &mut Value|
     -> Result<(String, String), String> {
        // `['rate_limit_remaining' => $this->rate_limit_remaining] = …` into an int property.
        let r = limit
            .remaining
            .as_deref()
            .and_then(|v| v.trim().parse::<i64>().ok());
        let Some(r) = r else {
            return Err("Cannot assign null to property $rate_limit_remaining of type int".into());
        };
        *remaining = r;
        *reset = Value::String(detect::format_timestamp(
            limit.reset.as_deref(),
            "%Y-%b-%d %H:%M:%S",
        ));
        Ok((tried.clone(), base_directory_after(original, &tried)))
    };
    let first = if source.github {
        match lookup(git_branch.clone()).await {
            Ok((tried, limit)) => apply(
                &git_branch,
                tried,
                limit,
                &mut rate_remaining,
                &mut rate_reset,
            ),
            Err(e) => Err(e),
        }
    } else {
        Ok((git_branch.clone(), base_directory.clone()))
    };
    match first {
        Ok((b, base)) => {
            git_branch = b;
            base_directory = base;
            branch_found = true;
            if url.contains("tangled") {
                git_branch = "master".into();
            }
        }
        Err(e) => {
            if rate_remaining == 0 {
                branch_found = true;
            } else if git_branch == "main" {
                git_branch = "master".into();
                match lookup(git_branch.clone()).await {
                    Ok((tried, limit)) => {
                        match apply("master", tried, limit, &mut rate_remaining, &mut rate_reset) {
                            Ok((b, base)) => {
                                git_branch = b;
                                base_directory = base;
                                branch_found = true;
                            }
                            Err(e) => error = Some(e),
                        }
                    }
                    Err(e) => error = Some(e),
                }
            } else {
                error = Some(e);
            }
        }
    }
    let mut detection = Value::Null;
    if branch_found {
        let destination_uuid = s(&m, "destination_uuid");
        match run_detection(
            &state,
            &ctx,
            &url,
            &git_branch,
            &base_directory,
            &destination_uuid,
        )
        .await
        {
            Ok(Some(d)) => detection = d,
            Ok(None) => {}
            Err(toast) => error = Some(toast),
        }
    }
    Ok(ok(json!({
        "repository_url": url,
        "branch_found": branch_found,
        "git_branch": git_branch,
        "base_directory": base_directory,
        "git_repository": source.repository,
        "git_source": if source.github { "github" } else { "other" },
        "rate_limit_remaining": rate_remaining,
        "rate_limit_reset": rate_reset,
        "error": error,
        "detection": detection,
    }))
    .into_response())
}

/// `detectRepository()` from the Smart Scan button, with the current branch and base directory.
pub async fn scan(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let base = m
        .get("base_directory")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_owned();
    let answer = run_detection(
        &state,
        &ctx,
        &s(&m, "repository_url"),
        &s(&m, "git_branch"),
        &base,
        &s(&m, "destination_uuid"),
    )
    .await;
    Ok(match answer {
        Ok(d) => ok(json!({"detection": d})),
        Err(toast) => ok(json!({"detection": null, "error": toast})),
    }
    .into_response())
}

/// `ValidationPatterns::FILE_PATH_PATTERN`.
pub(super) static FILE_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/[a-zA-Z0-9._\-/~@+]+$").unwrap_or_else(|_| unreachable!())
});
/// The shared-variable reference `EnvironmentVariable::isSharedReference()` looks for.
static SHARED_REFERENCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
        .unwrap_or_else(|_| unreachable!())
});
static ENV_KEY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^[A-Z_][A-Z0-9_]*$").unwrap_or_else(|_| unreachable!())
});

/// `ValidationPatterns::DIRECTORY_PATH_PATTERN`.
static DIRECTORY_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/([a-zA-Z0-9._\-/~@+]*)?$").unwrap_or_else(|_| unreachable!())
});

/// `ValidationPatterns::directoryPathRules()` (`nullable|string|max:255|regex`) for `base_directory`.
pub(super) fn base_directory_rule(m: &Map<String, Value>, attribute: &str) -> Option<String> {
    let v = m
        .get("base_directory")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())?;
    if v.chars().count() > 255 {
        Some(format!(
            "The {attribute} field must not be greater than 255 characters."
        ))
    } else if !DIRECTORY_PATH.is_match(v) {
        Some(format!("The {attribute} field format is invalid."))
    } else {
        None
    }
}

/// `ValidationPatterns::normalizeDirectoryPath()` (v4.6.0): "apps/web/" becomes "/apps/web", and an
/// empty value "/".
pub(super) fn normalize_directory_path(path: Option<&str>) -> String {
    let php_trim = |s: &str| {
        s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B'))
            .to_owned()
    };
    format!("/{}", php_trim(path.unwrap_or_default()).trim_matches('/'))
}

/// `$this->base_directory = ValidationPatterns::normalizeDirectoryPath($this->base_directory)` at
/// the start of `submit()` of the new-resource forms.
pub(super) fn normalize_base_directory(m: &mut Map<String, Value>) {
    let value = match m.get("base_directory") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    m.insert(
        "base_directory".into(),
        Value::String(normalize_directory_path(value.as_deref())),
    );
}

/// The `baseDirectory` mutator, then the `saving` hook's trim (which runs the mutator again).
pub(super) fn base_directory_value(v: Option<&str>) -> String {
    let set = |v: &str| format!("/{}", v.trim_start_matches('/'));
    set(set(v.unwrap_or_default()).trim())
}

/// The `publishDirectory` mutator (`null` for a falsy value), then the `saving` hook's trim:
/// `str(null)->trim()` is a Stringable, which the mutator turns into `/`.
pub(super) fn publish_directory_value(v: Option<&str>) -> String {
    let set = |v: &str| format!("/{}", v.trim_start_matches('/'));
    let first = v
        .filter(|v| !v.is_empty() && *v != "0")
        .map(set)
        .unwrap_or_default();
    set(first.trim())
}

/// The `dockerComposeLocation` mutator.
pub(super) fn compose_location_value(v: Option<&str>) -> String {
    match v {
        None | Some("") => "/docker-compose.yaml".into(),
        Some("/") => "/".into(),
        Some(v) => {
            let v = v.strip_suffix('/').unwrap_or(v);
            if v.starts_with('/') {
                v.to_owned()
            } else {
                format!("/{v}")
            }
        }
    }
}

/// A Laravel `ValidationException` that `handleError()` shows as a toast.
pub(super) fn invalid(errors: Vec<(&str, String)>) -> ApiResult {
    let mut e = Map::new();
    for (k, msg) in errors {
        if let Value::Array(list) = e.entry(k.to_owned()).or_insert_with(|| json!([])) {
            list.push(Value::String(msg));
        }
    }
    Ok(PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": summary(&e), "errors": e}),
    )
    .into_response())
}

/// `handleError()` of a plain exception: the message as an error toast.
pub(super) fn failure(message: String) -> ApiResult {
    Ok(PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message}),
    )
    .into_response())
}

/// `PublicGitRepository::submit()`: validates the form, creates the application with its
/// settings and generated domain, imports the confirmed env file variables and answers with
/// the application uuid.
pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut m = body(&bytes);
    normalize_base_directory(&mut m);
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    let repository_url = text("repository_url").unwrap_or_default();
    let git_branch = text("git_branch").unwrap_or_default();
    let build_pack = text("build_pack").unwrap_or_default();
    let base_directory = text("base_directory");
    let publish_directory = text("publish_directory");
    let compose_location = text("docker_compose_location");
    let port = match m.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_owned(),
        _ => String::new(),
    };
    let is_static = m.get("is_static").and_then(Value::as_bool).unwrap_or(false);

    // $this->validate() with the component's $validationAttributes.
    let mut errors: Vec<(&str, String)> = Vec::new();
    if repository_url.is_empty() {
        errors.push(("repository_url", "The repository field is required.".into()));
    } else if let Some(msg) = repository_rule(&repository_url, "repository") {
        errors.push(("repository_url", msg));
    }
    if port.is_empty() {
        errors.push(("port", "The port field is required.".into()));
    } else if !coolify_compat::yaml::php_is_numeric(&port) {
        errors.push(("port", "The port field must be a number.".into()));
    }
    if build_pack.is_empty() {
        errors.push(("build_pack", "The build pack field is required.".into()));
    }
    if let Some(msg) = base_directory_rule(&m, "base directory") {
        errors.push(("base_directory", msg));
    }
    if let Some(c) = compose_location.as_deref().filter(|c| !c.is_empty()) {
        if c.chars().count() > 255 {
            errors.push((
                "docker_compose_location",
                "The docker compose location field must not be greater than 255 characters.".into(),
            ));
        }
        if !FILE_PATH.is_match(c) {
            errors.push((
                "docker_compose_location",
                "The docker compose location field format is invalid.".into(),
            ));
        }
    }
    if git_branch.is_empty() {
        errors.push(("git_branch", "The git branch field is required.".into()));
    } else if let Some(msg) = branch_rule(&git_branch, "git branch") {
        errors.push(("git_branch", msg));
    }
    if !errors.is_empty() {
        return invalid(errors);
    }
    let source = git_source(&repository_url);
    let github = if source.github {
        public_github(&state).await?
    } else {
        None
    };
    if !source.github
        && let Some(msg) = repository_rule(&source.repository, "git repository")
    {
        return failure(format!("Invalid repository URL: {msg}"));
    }
    if let Some(msg) = branch_rule(&git_branch, "git branch") {
        return failure(format!("Invalid branch: {msg}"));
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
    // selectedDockerfileLocation()
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
    // importDetectedEnvironmentVariables(): `variables.*` must be strings or null.
    let env_imported = m
        .get("env_imported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let variables: Vec<(String, Option<String>)> = match m.get("env_vars") {
        Some(Value::Array(list)) if env_imported => {
            let mut out = Vec::new();
            for pair in list {
                let key = pair
                    .get(0)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                match pair.get(1) {
                    Some(Value::String(v)) => out.push((key, Some(v.clone()))),
                    Some(Value::Null) | None => out.push((key, None)),
                    Some(_) => {
                        return invalid(vec![(
                            "variables",
                            format!("The variables.{key} field must be a string."),
                        )]);
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    };

    // Trimmed like the model's `saving` hook (`str(null)->trim()` stores an empty string).
    let trimmed =
        |v: Option<&str>| ColumnValue::Text(Some(v.unwrap_or_default().trim().to_owned()));
    let name = if source.github {
        git_urls::generate_application_name(&source.repository, &git_branch)
    } else {
        crate::routes::private_keys::random_name()
    };
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        ("name", ColumnValue::Text(Some(name))),
        ("git_repository", trimmed(Some(&source.repository))),
        ("git_branch", trimmed(Some(&git_branch))),
        ("ports_exposes", ColumnValue::Text(Some(port.clone()))),
        (
            "publish_directory",
            ColumnValue::Text(Some(publish_directory_value(publish_directory.as_deref()))),
        ),
    ];
    if let Some(g) = &github {
        columns.push(("source_id", ColumnValue::Int(g.id)));
        columns.push((
            "source_type",
            ColumnValue::Text(Some(GITHUB_APP_MORPH.into())),
        ));
    }
    columns.push(("build_pack", ColumnValue::Text(Some(build_pack.clone()))));
    columns.push((
        "base_directory",
        ColumnValue::Text(Some(base_directory_value(base_directory.as_deref()))),
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
    let uuid = coolify_domain::new_public_id();
    let now = state.now();
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
    apps::update_settings(
        &state.db,
        id,
        &[("is_static", ColumnValue::Bool(is_static))],
        now,
    )
    .await?;
    // The `isStatic` mutator: a static site listens on port 80.
    if is_static {
        apps::update_if_changed(
            &state.db,
            id,
            &[("ports_exposes", ColumnValue::Text(Some("80".into())))],
            now,
        )
        .await?;
    }
    // generateUrl(server: $destination->server, random: $application->uuid)
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
    let url = domains::generate_url(wildcard.as_deref(), &ip, public.as_deref(), &uuid).await;
    let (fqdn, overrides) = domains::normalize_port_overrides(Some(&url), &Default::default());
    let mut values = vec![("fqdn", ColumnValue::Text(fqdn.clone()))];
    if let Some(o) = overrides {
        values.push((
            "domain_port_overrides",
            ColumnValue::Text(Some(domains::overrides_json(fqdn.as_deref(), &o))),
        ));
    }
    apps::update_if_changed(&state.db, id, &values, now).await?;
    import_variables(&state, id, &variables).await?;
    tracing::info!(target: "audit", event = "application.created", team_id = ctx.team_id, application_uuid = %uuid, build_pack = %build_pack, "application created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `importDetectedEnvironmentVariables()`: `EnvironmentVariable::create()` per valid key in one
/// transaction, each with the preview copy the model's `created` hook adds (from the in-memory
/// model, so with its default flags; not for a generated magic variable of a compose
/// application) and the version it writes.
pub(super) async fn import_variables(
    state: &AppState,
    application_id: i64,
    variables: &[(String, Option<String>)],
) -> Result<(), ApiError> {
    if variables.is_empty() {
        return Ok(());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let db_err = |e: sqlx::Error| {
        tracing::error!(error = %e, "environment variable import failed");
        ApiError::Internal
    };
    let now = state.now();
    let mut tx = state.db.pool().begin().await.map_err(db_err)?;
    for (key, value) in variables {
        if !ENV_KEY.is_match(key) {
            continue;
        }
        let key = key.trim();
        let value = value.as_deref().map(str::trim);
        let shared = value.is_some_and(|v| !v.is_empty() && SHARED_REFERENCE.is_match(v));
        for preview in [false, true] {
            if preview {
                let found: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM environment_variables WHERE key = $1 AND resourceable_type = $2 AND resourceable_id = $3 AND is_preview = true)",
                )
                .bind(key)
                .bind(apps::MORPH)
                .bind(application_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_err)?;
                if found
                    || !coolify_db::environment_variables::application_copies_to_preview(
                        &mut *tx,
                        application_id,
                        key,
                    )
                    .await?
                {
                    continue;
                }
            }
            sqlx::query(
                "INSERT INTO environment_variables (key, value, is_preview, is_multiline, is_literal, is_runtime, is_buildtime, \
                 is_required, is_shown_once, comment, resourceable_type, resourceable_id, is_shared, uuid, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, false, false, true, true, false, false, NULL, $4, $5, $6, $7, $8, $9, $9)",
            )
            .bind(key)
            .bind(value.map(|v| enc.encrypt_serialized(v)))
            .bind(preview)
            .bind(apps::MORPH)
            .bind(application_id)
            .bind(shared)
            .bind(coolify_domain::new_public_id())
            .bind(&state.version)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
    }
    tx.commit().await.map_err(db_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_paths_are_normalized() {
        assert_eq!(normalize_directory_path(Some("apps/web/")), "/apps/web");
        assert_eq!(normalize_directory_path(Some(" /apps/web ")), "/apps/web");
        assert_eq!(normalize_directory_path(Some("")), "/");
        assert_eq!(normalize_directory_path(None), "/");
        assert_eq!(normalize_directory_path(Some("//")), "/");
    }

    #[test]
    fn sources() {
        let s = git_source("https://github.com/coollabsio/coolify-examples/tree/main/static");
        assert_eq!(s.repository, "coollabsio/coolify-examples");
        assert_eq!(s.branch, "main/static");
        assert!(s.github);
        let o = git_source("https://gitlab.com/a/b.git");
        assert_eq!(o.repository, "https://gitlab.com/a/b.git");
        assert_eq!(o.branch, "main");
        assert_eq!(base_directory_after("main/static", "main"), "/static");
        assert_eq!(base_directory_after("main", "main"), "/");
        assert_eq!(
            normalize_repository_url("https://github.com/coollabsio/coolify-examples.git"),
            "https://github.com/coollabsio/coolify-examples"
        );
        assert_eq!(
            normalize_repository_url("https://gitlab.com/a/b"),
            "https://gitlab.com/a/b.git"
        );
        assert_eq!(
            normalize_repository_url("git@github.com:a/b.git"),
            "https://github.com/a/b"
        );
    }
}
