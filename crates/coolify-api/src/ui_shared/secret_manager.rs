//! `Project\Shared\SecretManagerLinks`: what can be shown without contacting a secret manager.
//! Linking a source and browsing its keys are not ported.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::resource_envs as re;
use serde_json::json;

use super::find;
use crate::auth::AuthCtx;
use crate::response::{ApiResult, ok};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/ui/resources/{kind}/{uuid}/secret-manager", get(show))
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let (linked, tokens) =
        re::secret_manager_state(&state.db, &res.morph, res.id, res.team_id).await?;
    Ok(ok(json!({"linked": linked, "available_tokens": tokens})).into_response())
}
