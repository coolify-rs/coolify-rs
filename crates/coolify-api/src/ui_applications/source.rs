//! The Git Source page of an application: `Project\Application\Source` (repository, branch,
//! commit, the deploy key and the Git provider App).

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::ColumnValue;
use serde_json::{Value, json};

use super::{body, find};
use crate::auth::AuthCtx;
use crate::git_urls;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;

const GITHUB: &str = r"App\Models\GithubApp";
const GITLAB: &str = r"App\Models\GitlabApp";
const GITEA: &str = r"App\Models\GiteaApp";

static COMMIT_SHA: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._\-/]*$").unwrap_or_else(|_| unreachable!())
});

fn db_err(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "source query failed");
    ApiError::Internal
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A provider App row (`github_apps`, `gitlab_apps`, `gitea_apps`) as JSON.
async fn source_row(state: &AppState, morph: &str, id: i64) -> Result<Option<Value>, ApiError> {
    let table = match morph {
        GITHUB => "github_apps",
        GITLAB => "gitlab_apps",
        GITEA => "gitea_apps",
        _ => return Ok(None),
    };
    let row: Option<Value> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s) FROM {table} s WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(db_err)?;
    Ok(row)
}

/// The card subtitle of `source.blade.php` (`$currentSourceSubtitle` / `$sourceSubtitle`).
fn subtitle(morph: &str, row: &Value, current: bool) -> String {
    let public = row
        .get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match morph {
        GITHUB => match str_of(row, "organization") {
            Some(o) => format!("GitHub · {o}"),
            None if public => "Public GitHub".into(),
            None => "GitHub · Personal account".into(),
        },
        GITLAB => str_of(row, "group_name").map_or_else(
            || "GitLab · Personal account".into(),
            |g| format!("GitLab · {g}"),
        ),
        GITEA => str_of(row, "organization").map_or_else(
            || "Gitea · Personal account".into(),
            |o| format!("Gitea · {o}"),
        ),
        _ if current => "Connected source".into(),
        _ => "Git source".into(),
    }
}

fn card(morph: &str, row: &Value, current: bool) -> Value {
    json!({
        "id": row.get("id"),
        "type": morph,
        "name": row.get("name"),
        "subtitle": subtitle(morph, row, current),
    })
}

/// `Team::sources()` filtered like `Source::getSources()`: GitHub Apps that finished their
/// setup, connected GitLab and Gitea Apps, without the current source, sorted by name.
async fn other_sources(
    state: &AppState,
    ctx: &AuthCtx,
    a: &ApplicationRow,
) -> Result<Vec<Value>, ApiError> {
    let current = (
        a.int("source_id"),
        a.str("source_type").unwrap_or_default().to_owned(),
    );
    let mut out: Vec<(String, Value)> = Vec::new();
    for (morph, table, usable) in [
        (GITHUB, "github_apps", "app_id IS NOT NULL"),
        (
            GITLAB,
            "gitlab_apps",
            "COALESCE(access_token, '') <> '' AND COALESCE(refresh_token, '') <> ''",
        ),
        (
            GITEA,
            "gitea_apps",
            "COALESCE(access_token, '') <> '' AND COALESCE(refresh_token, '') <> ''",
        ),
    ] {
        let rows: Vec<Value> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(s) FROM {table} s WHERE (team_id = $1 OR is_system_wide = true) AND is_public = false AND {usable} ORDER BY id"
        ))
        .bind(ctx.team_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(db_err)?;
        for row in rows {
            let id = row.get("id").and_then(Value::as_i64);
            if id == current.0 && morph == current.1 {
                continue;
            }
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            out.push((name, card(morph, &row, false)));
        }
    }
    // Collection::sortBy('name') (stable).
    out.sort_by(|x, y| x.0.cmp(&y.0));
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

/// `gitBranchLocation` / `gitCommits` (`httpsUrlFromSshGitRepository()` without a source).
fn links(a: &ApplicationRow, source: Option<(&str, &Value)>) -> (Option<String>, Option<String>) {
    let repo = a.str("git_repository");
    let branch = a.str("git_branch");
    let base = a.str("base_directory").unwrap_or("/");
    if let (Some((morph, row)), Some(repo), Some(branch)) = (source, repo, branch)
        && let Some(html) = row.get("html_url").and_then(Value::as_str)
    {
        let location = if morph == GITEA {
            format!("{html}/{repo}/src/branch/{branch}{base}")
        } else if repo.contains("bitbucket") {
            format!("{html}/{repo}/src/{branch}{base}")
        } else {
            format!("{html}/{repo}/tree/{branch}{base}")
        };
        let commits = if morph == GITEA {
            format!("{html}/{repo}/commits/branch/{branch}")
        } else {
            format!("{html}/{repo}/commits/{branch}")
        };
        return (Some(location), Some(commits));
    }
    let https = repo
        .and_then(git_urls::ssh_git_url_to_https)
        .map(|u| u.strip_suffix(".git").map(str::to_owned).unwrap_or(u));
    match https {
        Some(h) => {
            let branch = branch.unwrap_or_default();
            let location = if repo.unwrap_or_default().contains("bitbucket") {
                format!("{h}/src/{branch}{base}")
            } else {
                format!("{h}/tree/{branch}{base}")
            };
            (Some(location), Some(format!("{h}/commits/{branch}")))
        }
        None => (repo.map(str::to_owned), repo.map(str::to_owned)),
    }
}

/// `getInstallationPath($source)`.
fn installation_path(row: &Value) -> String {
    match row {
        Value::Object(map) => crate::ui_sources::installation_path(map),
        _ => String::new(),
    }
}

/// `Source::mount()`: the form values, the deploy key, the current source and the others.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let morph = a.str("source_type").unwrap_or_default().to_owned();
    let source = match a.int("source_id") {
        Some(id) => source_row(&state, &morph, id).await?,
        None => None,
    };
    let (location, commits) = links(&a, source.as_ref().map(|r| (morph.as_str(), r)));
    let key_id = a.int("private_key_id");
    let keys: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM private_keys WHERE team_id = $1 ORDER BY id")
            .bind(ctx.team_id)
            .fetch_all(state.db.pool())
            .await
            .map_err(db_err)?;
    let key_name = keys
        .iter()
        .find(|(id, _)| Some(*id) == key_id)
        .map(|(_, n)| n.clone());
    let can_update = ctx.is_team_admin();
    let sources = if can_update && key_id.is_none() {
        other_sources(&state, &ctx, &a).await?
    } else {
        Vec::new()
    };
    let private_github = source
        .as_ref()
        .filter(|r| morph == GITHUB && r.get("is_public").and_then(Value::as_bool) == Some(false));
    Ok(ok(json!({
        "git_repository": a.str("git_repository"),
        "git_branch": a.str("git_branch"),
        "git_commit_sha": a.str("git_commit_sha"),
        "private_key_id": key_id,
        "private_key_name": key_name,
        "private_keys": keys.iter().filter(|(id, _)| Some(*id) != key_id).map(|(id, n)| json!({"id": id, "name": n})).collect::<Vec<_>>(),
        "branch_location": location,
        "commits_url": commits,
        "installation_path": private_github.map(installation_path),
        "current_source": source.as_ref().map(|r| card(&morph, r, true)),
        "sources": sources,
        "can_update": can_update,
    }))
    .into_response())
}

/// `handleError()`: the message as an error toast.
fn failure(message: String) -> ApiResult {
    Ok(PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message}),
    )
    .into_response())
}

/// `Source::submit()`: trims the fields (`updated*` hooks), an empty commit becomes `HEAD`,
/// then the `#[Validate]` rules and the update.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let field = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let repository = field("git_repository");
    let branch = field("git_branch");
    let mut sha = field("git_commit_sha");
    if sha.is_empty() {
        sha = "HEAD".into();
    }
    let mut errors: Vec<String> = Vec::new();
    if repository.is_empty() {
        errors.push("The git repository field is required.".into());
    }
    if branch.is_empty() {
        errors.push("The git branch field is required.".into());
    } else if let Some(msg) = git_urls::valid_git_branch(&Value::String(branch.clone())) {
        errors.push(msg.replace(":attribute", "git branch"));
    }
    if !COMMIT_SHA.is_match(&sha) {
        errors.push("The git commit sha field format is invalid.".into());
    }
    if let Some(first) = errors.first() {
        let message = match errors.len() {
            1 => first.clone(),
            2 => format!("{first} (and 1 more error)"),
            n => format!("{first} (and {} more errors)", n - 1),
        };
        return failure(message);
    }
    apps::update_if_changed(
        &state.db,
        a.id,
        &[
            ("git_repository", ColumnValue::Text(Some(repository))),
            ("git_branch", ColumnValue::Text(Some(branch))),
            ("git_commit_sha", ColumnValue::Text(Some(sha))),
        ],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"message": "Application source updated!"})).into_response())
}

/// `Source::setPrivateKey()` (`private_key_id`) and `Source::changeSource()` (`source_id` +
/// `source_type`).
pub async fn change(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    let now = state.now();
    if let Some(key) = m.get("private_key_id").and_then(Value::as_i64) {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT id FROM private_keys WHERE team_id = $1 AND id = $2")
                .bind(ctx.team_id)
                .bind(key)
                .fetch_optional(state.db.pool())
                .await
                .map_err(db_err)?;
        let Some(key) = found else {
            return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
        };
        apps::update_if_changed(
            &state.db,
            a.id,
            &[("private_key_id", ColumnValue::Int(key))],
            now,
        )
        .await?;
        return Ok(ok(json!({"message": "Private key updated!"})).into_response());
    }
    let morph = m
        .get("source_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = m.get("source_id").and_then(Value::as_i64).unwrap_or(-1);
    if ![GITHUB, GITLAB, GITEA].contains(&morph) {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let table = match morph {
        GITHUB => "github_apps",
        GITLAB => "gitlab_apps",
        _ => "gitea_apps",
    };
    let row: Option<Value> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s) FROM {table} s WHERE (team_id = $1 OR is_system_wide = true) AND id = $2"
    ))
    .bind(ctx.team_id)
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(db_err)?;
    let Some(row) = row else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    };
    apps::update_if_changed(
        &state.db,
        a.id,
        &[
            ("source_id", ColumnValue::Int(id)),
            ("source_type", ColumnValue::Text(Some(morph.to_owned()))),
        ],
        now,
    )
    .await?;
    let record = match row {
        Value::Object(o) => o,
        _ => serde_json::Map::new(),
    };
    // `gitRepositorySlug($customRepository)`: an SSH repository URL becomes `owner/repo`.
    let repository =
        coolify_jobs::github::git_repository_slug(a.str("git_repository").unwrap_or_default());
    let enc = state.encrypter.as_ref();
    let connected = |r: &serde_json::Map<String, Value>| {
        let filled = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        };
        filled("access_token") && filled("refresh_token")
    };
    let project_id: Result<Option<i64>, String> = async {
        match morph {
            GITHUB => {
                // githubApi($source, "repos/{$customRepository}")['data']['id']
                let token =
                    coolify_jobs::github::installation_token(&state.db, enc, Some(&record)).await?;
                coolify_jobs::github::repository_id(&state.db, &record, &token, &repository).await
            }
            GITLAB if connected(&record) => {
                let enc = enc.ok_or("No application encryption key has been specified.")?;
                let encoded = coolify_jobs::gitea::raw_url_encode(&repository);
                let (project, _) = coolify_jobs::gitlab::api(
                    &state.db,
                    enc,
                    &record,
                    coolify_jobs::git_http::Method::GET,
                    &format!("/projects/{encoded}"),
                    None,
                )
                .await?;
                Ok(project.get("id").and_then(Value::as_i64))
            }
            GITEA if connected(&record) => {
                let enc = enc.ok_or("No application encryption key has been specified.")?;
                let path = repository
                    .splitn(2, '/')
                    .map(coolify_jobs::gitea::raw_url_encode)
                    .collect::<Vec<_>>()
                    .join("/");
                let (project, _) = coolify_jobs::gitea::api(
                    &state.db,
                    enc,
                    &record,
                    coolify_jobs::git_http::Method::GET,
                    &format!("/repos/{path}"),
                    None,
                )
                .await?;
                Ok(project.get("id").and_then(Value::as_i64))
            }
            _ => Ok(None),
        }
    }
    .await;
    match project_id {
        Ok(Some(pid)) if a.int("repository_project_id") != Some(pid) => {
            apps::update_if_changed(
                &state.db,
                a.id,
                &[("repository_project_id", ColumnValue::Int(pid))],
                now,
            )
            .await?;
        }
        Ok(_) => {}
        Err(e) => return failure(e),
    }
    Ok(ok(json!({"message": "Source updated!"})).into_response())
}
