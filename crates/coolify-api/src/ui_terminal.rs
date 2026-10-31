//! `App\Livewire\Terminal\Index`: the target list of /terminal with its `can.access.terminal` gate.
//! The shell itself (`Project\Shared\Terminal`, upstream's node-pty service behind `/terminal/ws`)
//! is not ported.

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::servers;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::toast_error;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/terminal", get(servers_list))
        .route("/ui/terminal/containers", get(containers))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `Server::isReachable()->get()->filter(isTerminalEnabled)`.
async fn targets(
    state: &AppState,
    team: i64,
) -> Result<Vec<(i64, String, String, bool)>, ApiError> {
    sqlx::query_as(
        "SELECT v.id, v.uuid, v.name, (ss.is_reachable AND ss.is_usable AND NOT ss.force_disabled)
         FROM servers v JOIN server_settings ss ON ss.server_id = v.id
         WHERE v.team_id = $1 AND v.deleted_at IS NULL AND ss.is_reachable AND ss.is_terminal_enabled
         ORDER BY v.name",
    )
    .bind(team)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)
}

/// `mount`, behind `CanAccessTerminal`.
async fn servers_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(toast_error(
            StatusCode::FORBIDDEN,
            "Access to terminal functionality is restricted to team administrators",
        ));
    }
    let list = targets(&state, ctx.team_id).await?;
    Ok(ok(json!({
        "servers": list.iter().map(|(_, uuid, name, _)| json!({"uuid": uuid, "name": name})).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `loadContainers`: the running containers of each functional server, sorted by name.
async fn containers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(toast_error(
            StatusCode::FORBIDDEN,
            "Access to terminal functionality is restricted to team administrators",
        ));
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut out: Vec<Value> = Vec::new();
    for (id, uuid, _, functional) in targets(&state, ctx.team_id).await? {
        if !functional {
            continue;
        }
        let Some(row) = servers::find_by_id(&state.db, id).await? else {
            continue;
        };
        let text = jobs
            .instant_remote(
                &row,
                &["docker ps -a --format '{{json .}}'".to_owned()],
                true,
                "terminal.containers",
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .unwrap_or_default();
        for line in text.lines() {
            let Ok(c) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let state_text = c
                .get("State")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            if !state_text.contains("running") {
                continue;
            }
            let name = c.get("Names").and_then(Value::as_str).unwrap_or_default();
            out.push(json!({
                "name": name,
                "connection_name": name,
                "uuid": format!("{uuid}:{name}"),
                "status": state_text,
                "server_uuid": uuid,
            }));
        }
    }
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    Ok(ok(json!({"containers": out})).into_response())
}
