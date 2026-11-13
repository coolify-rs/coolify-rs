//! Session endpoints behind `Livewire\Security\PrivateKey\Show` (upstream v4.7.5): the editor of
//! the keys list without the stored key, the password-confirmed reveal, and the "Used by" list.
//! The save goes through `PATCH /ui/api/v1/security/keys/{uuid}` with the new key only when one
//! was pasted (`newPrivateKey`).

use std::collections::HashMap;
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::private_keys::{self, KeyRow};
use coolify_domain::ROOT_TEAM_ID;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;

/// `Show::REVEAL_ATTEMPTS_PER_MINUTE`: reveal attempts per user and minute, so a session cannot
/// guess the password without limit.
const REVEAL_ATTEMPTS_PER_MINUTE: usize = 5;
/// `Show::USAGE_LIMIT`: rows shown in the "Used by" list. The counts always cover everything.
const USAGE_LIMIT: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/keys/{uuid}", get(show))
        .route("/ui/keys/{uuid}/reveal", post(reveal))
}

/// `mount()`: `PrivateKey::ownedByCurrentTeam()->whereUuid()->firstOrFail()` and
/// `authorize('view', ...)`.
async fn find_key(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<KeyRow, ApiError> {
    let key = private_keys::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Private Key not found.",
        ))?;
    // `PrivateKeyPolicy::view`: the keys of the root team need an admin of the root team.
    if key.team_id == ROOT_TEAM_ID && !ctx.is_team_admin() {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You do not have permission to view this private key.",
        ));
    }
    Ok(key)
}

/// `'%'.addcslashes(mb_strtolower($search), '%_\\').'%'`.
fn like_pattern(search: &str) -> String {
    let mut out = String::from("%");
    for c in search.to_lowercase().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// One type of the "Used by" list: the label, the tables with the key's team condition, the name
/// column and the link of a row.
struct UsageType {
    label: &'static str,
    from: &'static str,
    name: &'static str,
    url: &'static str,
}

/// The types of `Show::usages()`, in the order of the list.
const USAGE_TYPES: &[UsageType] = &[
    UsageType {
        label: "Server",
        from: "servers s WHERE s.team_id = $1 AND s.private_key_id = $2 AND s.deleted_at IS NULL",
        name: "s.name",
        // route('server.show')
        url: "'/server/' || s.uuid",
    },
    UsageType {
        label: "Application",
        from: "applications a JOIN environments e ON e.id = a.environment_id \
               JOIN projects p ON p.id = e.project_id \
               WHERE p.team_id = $1 AND a.private_key_id = $2 AND a.deleted_at IS NULL",
        name: "a.name",
        // `Application::link()`
        url: "'/project/' || p.uuid || '/environment/' || e.uuid || '/application/' || a.uuid",
    },
    UsageType {
        label: "GitHub App",
        from: "github_apps g WHERE g.team_id = $1 AND g.private_key_id = $2",
        name: "g.name",
        url: "'/source/github/' || g.uuid",
    },
    UsageType {
        label: "GitLab App",
        from: "gitlab_apps g WHERE g.team_id = $1 AND g.private_key_id = $2",
        name: "g.name",
        url: "'/source/gitlab/' || g.uuid",
    },
    UsageType {
        label: "Gitea App",
        from: "gitea_apps g WHERE g.team_id = $1 AND g.private_key_id = $2",
        name: "g.name",
        url: "'/source/gitea/' || g.uuid",
    },
];

/// `Show::usages()`: what of the key's team uses this key: a count per type (types without a
/// use left out), and the first rows that match the search, with a link.
async fn usages(state: &AppState, key: &KeyRow, search: &str) -> Result<Value, ApiError> {
    let pool = state.db.pool();
    let search = search.trim();
    let pattern = like_pattern(search);
    let mut counts = Vec::new();
    let mut matches: i64 = 0;
    let mut rows: Vec<Value> = Vec::new();
    for t in USAGE_TYPES {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", t.from))
            .bind(key.team_id)
            .bind(key.id)
            .fetch_one(pool)
            .await
            .map_err(coolify_db::DbError::from)?;
        let filter = if search.is_empty() {
            String::new()
        } else {
            format!(" AND LOWER({}) LIKE $3 ESCAPE '\\'", t.name)
        };
        let type_matches: i64 = if search.is_empty() {
            count
        } else {
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}{filter}", t.from))
                .bind(key.team_id)
                .bind(key.id)
                .bind(&pattern)
                .fetch_one(pool)
                .await
                .map_err(coolify_db::DbError::from)?
        };
        matches += type_matches;
        let missing = USAGE_LIMIT - rows.len() as i64;
        if type_matches > 0 && missing > 0 {
            let sql = format!(
                "SELECT COALESCE({name}, '')::text, ({url})::text FROM {from}{filter} ORDER BY {name} LIMIT {missing}",
                name = t.name,
                url = t.url,
                from = t.from,
            );
            let mut query = sqlx::query_as::<_, (String, Option<String>)>(&sql)
                .bind(key.team_id)
                .bind(key.id);
            if !search.is_empty() {
                query = query.bind(&pattern);
            }
            let found = query
                .fetch_all(pool)
                .await
                .map_err(coolify_db::DbError::from)?;
            rows.extend(
                found
                    .into_iter()
                    .map(|(name, url)| json!({"type": t.label, "name": name, "url": url})),
            );
        }
        if count > 0 {
            counts.push(json!({"type": t.label, "count": count}));
        }
    }
    Ok(json!({"counts": counts, "matches": matches, "rows": rows}))
}

/// `Show::render()` with `syncData(false)`: the form without the stored key (`newPrivateKey`
/// starts empty), the fingerprint, the public key and the "Used by" list for `usage_search`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let key = find_key(&state, &ctx, &uuid).await?;
    // `toArray()` hides `private_key` and appends `public_key`.
    let record = crate::routes::private_keys::model_array(&state, &key)?;
    let search = query.get("usage_search").map(String::as_str).unwrap_or("");
    let used_by = usages(&state, &key, search).await?;
    Ok(ok(json!({
        "id": key.id,
        "uuid": key.uuid,
        "name": key.name,
        "description": key.description,
        "fingerprint": record.get("fingerprint").cloned().unwrap_or(Value::Null),
        "public_key": record.get("public_key").cloned().unwrap_or(Value::Null),
        "is_git_related": crate::serialize::php_truthy(record.get("is_git_related").unwrap_or(&Value::Null)),
        "in_use": private_keys::in_use(&state.db, key.id).await?,
        "can_update": ctx.is_team_admin(),
        "can_delete": ctx.is_team_admin(),
        "counts": used_by["counts"],
        "matches": used_by["matches"],
        "rows": used_by["rows"],
    }))
    .into_response())
}

/// `Show::revealPrivateKey($password)`: the stored private key once, after a password
/// confirmation. The value goes back only as the answer of this request.
async fn reveal(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let key = find_key(&state, &ctx, &uuid).await?;
    // `authorize('update', $this->private_key)`, through `handleError()`.
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    let rate_limit_key = format!("private-key-reveal:{}", ctx.user_id);
    if let Some(wait) = crate::rules::throttle(
        &rate_limit_key,
        REVEAL_ATTEMPTS_PER_MINUTE,
        Duration::from_secs(60),
    ) {
        return toast_error(format!("Too many attempts. Try again in {wait} seconds."));
    }
    let context = json!({
        "team_id": key.team_id,
        "private_key_uuid": key.uuid,
        "private_key_name": key.name,
    });
    let password = body.get("password").and_then(Value::as_str);
    if !crate::ui::password_confirmed(&state, ctx.user_id, password).await? {
        crate::audit::log(
            &state,
            &ctx,
            &meta,
            "ui.private_key.reveal_failed",
            context,
            "warning",
        )
        .await;
        return toast_error("The provided password is incorrect.");
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.private_key.revealed",
        context,
        "info",
    )
    .await;
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Message(
        StatusCode::SERVICE_UNAVAILABLE,
        "APP_KEY is not configured; private keys cannot be read or written.",
    ))?;
    let plain = encrypter
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| ApiError::Internal)?;
    Ok(ok(json!({"private_key": plain})).into_response())
}

#[cfg(test)]
mod tests {
    use super::like_pattern;

    // `addcslashes(mb_strtolower($search), '%_\\')`: the search matches literally.
    #[test]
    fn the_usage_search_is_a_literal_lowercase_pattern() {
        assert_eq!(like_pattern("Web"), "%web%");
        assert_eq!(like_pattern("50%_a\\b"), "%50\\%\\_a\\\\b%");
        assert_eq!(like_pattern(""), "%%");
    }
}
