//! `GiteaController` (`GET /gitea-apps`) and the Gitea parts of `ApplicationsController`:
//! `teamSourceByUuid()`, `giteaAppRepository()` and `giteaRepositoryPath()`, which
//! `POST /applications/private-gitea-app` and `gitea_app_uuid` of `PATCH /applications/{uuid}`
//! use to check the repository against the Gitea or Forgejo instance of the source.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use coolify_db::records::Record;
use coolify_jobs::applications::domains::UrlParts;
use serde_json::{Value, json};

use super::git_apps::{attributes, decrypted_into, list_rows};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::serialize::serialize_record;
use crate::state::AppState;

/// `GiteaApp::$hidden`, which are also the fields `removeSensitiveData()` shows to a token that
/// may read sensitive data.
const GITEA_HIDDEN: &[&str] = &[
    "client_secret",
    "access_token",
    "refresh_token",
    "webhook_secret",
];

fn filled(row: &Record, key: &str) -> bool {
    row.get(key)
        .and_then(Value::as_str)
        .is_some_and(|v| !v.is_empty())
}

/// `GiteaApp::isConnected()`: the OAuth connection is complete.
pub fn is_connected(row: &Record) -> bool {
    filled(row, "access_token") && filled(row, "refresh_token")
}

/// `GiteaController::removeSensitiveData()` with `->put('is_connected', …)`.
fn present(state: &AppState, ctx: &AuthCtx, raw: &Record) -> Result<Value, ApiError> {
    let mut r = attributes(raw, GITEA_HIDDEN);
    if ctx.can_read_sensitive && raw.get("team_id").and_then(Value::as_i64) == Some(ctx.team_id) {
        decrypted_into(state, raw, &mut r, GITEA_HIDDEN)?;
    }
    let mut out = serialize_record(r);
    out.insert("is_connected".into(), Value::Bool(is_connected(raw)));
    Ok(Value::Object(out))
}

/// `GET /gitea-apps` (`GiteaController::list_gitea_apps()`): the Gitea apps of the team and the
/// system-wide ones.
pub async fn gitea_index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let rows = list_rows(&state, "gitea_apps", ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(present(&state, &ctx, r)?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `teamSourceByUuid($sourceClass, $uuid, $teamId)`: a Git source of the team, or a system-wide
/// one, by UUID. `table` is `github_apps` or `gitea_apps`.
pub async fn team_source_by_uuid(
    state: &AppState,
    table: &'static str,
    uuid: &str,
    team_id: i64,
) -> Result<Option<Record>, ApiError> {
    debug_assert!(matches!(table, "github_apps" | "gitea_apps"));
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(s)::text FROM {table} s WHERE uuid = $1 AND (team_id = $2 OR is_system_wide = true) ORDER BY id LIMIT 1"
    ))
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "git source query failed");
        ApiError::Internal
    })?;
    row.map(|t| coolify_db::records::parse_row(&t).map_err(|_| ApiError::Internal))
        .transpose()
}

/// `\w` and `.` `-`: the characters of one part of `owner/name`.
fn is_repository_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `giteaRepositoryPath($giteaApp, $gitRepository)`: owner/name from owner/name, or from an HTTPS
/// or SSH URL on the Gitea instance of the source (`html_url`). `None` for anything else.
pub fn gitea_repository_path(html_url: &str, git_repository: &str) -> Option<String> {
    let mut repository =
        crate::git_urls::ssh_git_url_to_https(git_repository).unwrap_or(git_repository.to_owned());
    if repository.contains("://") {
        let url = UrlParts::parse(&repository);
        let instance = UrlParts::parse(html_url);
        let host = |p: &Option<UrlParts>| p.as_ref().map(|p| p.host.to_lowercase());
        if host(&url).unwrap_or_default() != host(&instance).unwrap_or_default() {
            return None;
        }
        let path = url.and_then(|p| p.path).unwrap_or_default();
        let prefix = instance.and_then(|p| p.path).unwrap_or_default();
        let prefix = prefix.trim_end_matches('/');
        // `Str::chopStart($path, $prefix)`.
        repository = path.strip_prefix(prefix).unwrap_or(&path).to_owned();
    }
    let repository = repository.trim_matches('/');
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    let mut parts = repository.split('/');
    let valid = parts.next().is_some_and(is_repository_part)
        && parts.next().is_some_and(is_repository_part)
        && parts.next().is_none();
    valid.then(|| repository.to_owned())
}

/// What `giteaAppRepository()` answers for a repository the source can read.
#[derive(Debug, Clone, PartialEq)]
pub struct GiteaRepository {
    /// The `gitea_apps` row.
    pub source: Record,
    /// `owner/name`, which becomes `applications.git_repository`.
    pub repository: String,
    /// The repository ID on the instance, which becomes `applications.repository_project_id`.
    pub repository_project_id: Option<i64>,
}

fn message(status: StatusCode, text: impl Into<String>) -> Response {
    PhpJson(status, json!({"message": text.into()})).into_response()
}

/// `giteaAppRepository($giteaApp, $gitRepository)`: checks that a connected Gitea App can read
/// the repository. `Err` is the response upstream returns instead (404, 422 or 400); a connection
/// failure is upstream's uncaught exception, a 500.
pub async fn gitea_app_repository(
    state: &AppState,
    source: Option<Record>,
    git_repository: &str,
) -> Result<GiteaRepository, Response> {
    let Some(source) = source.filter(is_connected) else {
        return Err(message(StatusCode::NOT_FOUND, "Gitea App not found."));
    };
    let html_url = source
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(repository) = gitea_repository_path(html_url, git_repository) else {
        return Err(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Validation failed.",
                "errors": {"git_repository": "The git_repository must be owner/name or a repository URL on the Gitea instance of the source."},
            }),
        )
        .into_response());
    };
    let server_error = || ApiError::Internal.into_response();
    let enc = state.encrypter.as_ref().ok_or_else(server_error)?;
    // refreshGiteaToken() throws a RuntimeException, which is answered with its message.
    let token = coolify_jobs::gitea::access_token(&state.db, enc, &source)
        .await
        .map_err(|e| message(StatusCode::BAD_REQUEST, e))?;
    let (status, found) =
        coolify_jobs::gitea::repository_access(Some(&state.db), &source, &token, &repository)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "gitea repository check failed");
                server_error()
            })?;
    let successful = (200..300).contains(&status);
    let owner = coolify_jobs::git_http::path_str(&found, "owner.login");
    if status == 404
        || status == 403
        || (successful && !coolify_jobs::gitea::allows_repository_owner(&source, owner))
    {
        return Err(message(
            StatusCode::NOT_FOUND,
            "Repository not found or not accessible by the Gitea App.",
        ));
    }
    if !successful {
        let error = found
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown error");
        return Err(message(
            StatusCode::BAD_REQUEST,
            format!("Failed to verify repository access: {error}"),
        ));
    }
    Ok(GiteaRepository {
        repository_project_id: found.get("id").and_then(Value::as_i64),
        repository,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_paths_are_owner_and_name() {
        let instance = "https://gitea.example.com";
        assert_eq!(
            gitea_repository_path(instance, "acme/web").as_deref(),
            Some("acme/web")
        );
        assert_eq!(
            gitea_repository_path(instance, "/acme/web.git/").as_deref(),
            Some("acme/web")
        );
        assert_eq!(
            gitea_repository_path(instance, "https://gitea.example.com/acme/web.git").as_deref(),
            Some("acme/web")
        );
        // The host is compared without case.
        assert_eq!(
            gitea_repository_path(instance, "https://GITEA.example.com/acme/web").as_deref(),
            Some("acme/web")
        );
        // An SSH URL of the instance.
        assert_eq!(
            gitea_repository_path(instance, "git@gitea.example.com:acme/web.git").as_deref(),
            Some("acme/web")
        );
        assert_eq!(
            gitea_repository_path(instance, "ssh://git@gitea.example.com:2222/acme/web.git")
                .as_deref(),
            Some("acme/web")
        );
    }

    #[test]
    fn repository_paths_of_other_hosts_or_shapes_are_refused() {
        let instance = "https://gitea.example.com";
        assert_eq!(
            gitea_repository_path(instance, "https://github.com/acme/web"),
            None
        );
        assert_eq!(
            gitea_repository_path(instance, "git@codeberg.org:acme/web.git"),
            None
        );
        assert_eq!(gitea_repository_path(instance, "web"), None);
        assert_eq!(gitea_repository_path(instance, "acme/web/extra"), None);
        assert_eq!(gitea_repository_path(instance, "acme/.."), None);
        assert_eq!(gitea_repository_path(instance, "../web"), None);
        assert_eq!(gitea_repository_path(instance, "acme/we b"), None);
        assert_eq!(gitea_repository_path(instance, ""), None);
    }

    #[test]
    fn an_instance_under_a_path_keeps_owner_and_name_only() {
        let instance = "https://example.com/gitea/";
        assert_eq!(
            gitea_repository_path(instance, "https://example.com/gitea/acme/web.git").as_deref(),
            Some("acme/web")
        );
        // Not under the path of the instance: three parts are left.
        assert_eq!(
            gitea_repository_path(instance, "https://example.com/other/acme/web"),
            None
        );
    }

    #[test]
    fn connected_needs_both_tokens() {
        let row = |v: Value| -> Record { serde_json::from_value(v).unwrap() };
        assert!(is_connected(&row(
            json!({"access_token": "a", "refresh_token": "r"})
        )));
        assert!(!is_connected(&row(
            json!({"access_token": "a", "refresh_token": null})
        )));
        assert!(!is_connected(&row(
            json!({"access_token": "", "refresh_token": "r"})
        )));
        assert!(!is_connected(&row(json!({}))));
    }
}
