//! Session endpoints behind `Livewire\Security\PrivateKey\Rotate` (upstream v4.7.5): moves the
//! servers of a private key to a new key.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::private_keys::{self, KeyRow};
use coolify_jobs::server_ops::key_rotation::{self, StartError};
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/keys/{uuid}/rotation", get(show))
        .route("/ui/keys/{uuid}/rotate", post(rotate))
}

/// `mount()`: `PrivateKey::ownedByCurrentTeam()->findOrFail()` and `authorize('update', ...)`.
async fn find_key(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<KeyRow, ApiError> {
    let key = private_keys::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(ApiError::Message(
            axum::http::StatusCode::NOT_FOUND,
            "Private Key not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(key)
}

/// The rows of `Rotate::render()`: the servers of the last rotation, then the servers that
/// still use this key.
fn rows(
    state: Option<&Map<String, Value>>,
    servers: &[(String, String, bool, bool)],
) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    if let Some(Value::Object(results)) = state.and_then(|s| s.get("servers")) {
        for (uuid, result) in results {
            let mut row = result.as_object().cloned().unwrap_or_default();
            row.insert("uuid".into(), json!(uuid));
            row.insert("reachable".into(), json!(true));
            rows.push(Value::Object(row));
            seen.push(uuid);
        }
    }
    for (uuid, name, reachable, transferred) in servers {
        if seen.contains(&uuid.as_str()) {
            continue;
        }
        rows.push(json!({
            "uuid": uuid,
            "name": name,
            "status": null,
            "reachable": reachable,
            "transferred": transferred,
        }));
    }
    rows
}

/// What `livewire/security/private-key/rotate.blade.php` shows.
async fn page(state: &AppState, key: &KeyRow) -> Result<Value, ApiError> {
    let rotation = key_rotation::state(&key.uuid);
    let new_key = match rotation
        .as_ref()
        .and_then(|s| s.get("new_private_key_id"))
        .and_then(Value::as_i64)
    {
        Some(id) => private_keys::find_by_id(&state.db, key.team_id, id).await?,
        None => None,
    };
    let servers: Vec<(String, String, bool, bool)> = sqlx::query_as(
        "SELECT s.uuid::text, s.name::text, \
         COALESCE((SELECT ss.is_reachable FROM server_settings ss WHERE ss.server_id = s.id LIMIT 1), false), \
         COALESCE(s.server_metadata::jsonb #>> '{transfer,status}' = 'transferred', false) \
         FROM servers s WHERE s.team_id = $1 AND s.private_key_id = $2 AND s.deleted_at IS NULL ORDER BY s.id",
    )
    .bind(key.team_id)
    .bind(key.id)
    .fetch_all(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    // `applications()`, `githubApps()`, `gitlabApps()`, `giteaApps()`.
    let used_by_others: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM applications WHERE private_key_id = $1 AND deleted_at IS NULL) \
             OR EXISTS (SELECT 1 FROM github_apps WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM gitlab_apps WHERE private_key_id = $1) \
             OR EXISTS (SELECT 1 FROM gitea_apps WHERE private_key_id = $1)",
    )
    .bind(key.id)
    .fetch_one(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    let is_running = rotation
        .as_ref()
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        == Some("running");
    Ok(json!({
        "rows": rows(rotation.as_ref(), &servers),
        "has_servers": !servers.is_empty(),
        "used_by_others": used_by_others,
        "state": rotation.as_ref().map(|s| json!({
            "status": s.get("status"),
            "error": s.get("error"),
        })),
        "new_key": new_key.map(|k| json!({"uuid": k.uuid, "name": k.name})),
        "is_running": is_running,
        "private_key_id": key.id,
    }))
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let key = find_key(&state, &ctx, &uuid).await?;
    Ok(ok(page(&state, &key).await?).into_response())
}

/// `Rotate::rotate()`.
async fn rotate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let key = find_key(&state, &ctx, &uuid).await?;
    // `'keyType' => 'required|in:ed25519,rsa'`.
    let rsa = match body.get("key_type").and_then(Value::as_str) {
        Some("ed25519") => false,
        Some("rsa") => true,
        None | Some("") => return toast_error("The key type field is required."),
        Some(_) => return toast_error("The selected key type is invalid."),
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let new_key_uuid = match key_rotation::start(jobs, &key, rsa, Some(ctx.user_id)).await {
        Ok(uuid) => uuid,
        Err(StartError::Message(message)) => return toast_error(message),
        Err(StartError::Other(message)) => return toast_error(message),
    };
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.private_key.rotation_started",
        json!({
            "team_id": key.team_id,
            "private_key_uuid": key.uuid,
            "private_key_name": key.name,
            "new_private_key_uuid": new_key_uuid,
        }),
        "info",
    )
    .await;
    let mut v = page(&state, &key).await?;
    v["message"] = json!("Key rotation started.");
    Ok(ok(v).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `Rotate::render()`: one row per server.
    #[test]
    fn rows_show_the_last_rotation_and_the_servers_still_on_the_key() {
        let state = json!({
            "status": "finished",
            "servers": {
                "moved-uuid": {"name": "web-1", "status": "moved_old_pending", "message": "Moved to the new key."},
                "failed-uuid": {"name": "web-2", "status": "failed", "message": "The new key could not log in."},
            },
        });
        let servers = vec![
            ("failed-uuid".to_owned(), "web-2".to_owned(), true, false),
            ("new-uuid".to_owned(), "web-3".to_owned(), false, true),
        ];
        let rows = rows(state.as_object(), &servers);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["uuid"], "moved-uuid");
        assert_eq!(rows[0]["status"], "moved_old_pending");
        assert_eq!(rows[0]["reachable"], true);
        // A server of the last rotation keeps its result.
        assert_eq!(rows[1]["uuid"], "failed-uuid");
        assert_eq!(rows[1]["status"], "failed");
        assert_eq!(
            rows[2],
            json!({"uuid": "new-uuid", "name": "web-3", "status": null, "reachable": false, "transferred": true})
        );
        assert_eq!(super::rows(None, &servers).len(), 2);
    }
}
