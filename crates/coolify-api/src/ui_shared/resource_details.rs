//! `Project\Shared\ResourceDetails` for a service. Databases show it from their own record.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::servers;
use coolify_db::services::{self as svc, SubKind, SubRow};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiResult, ok};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/ui/resources/service/{uuid}/details", get(show))
}

/// `$app->human_name ?: $app->name`.
fn items(rows: &[SubRow]) -> Vec<Value> {
    rows.iter()
        .map(|s| {
            json!({
                "name": s.str("human_name").filter(|n| !n.is_empty() && *n != "0").unwrap_or(&s.name),
                "uuid": s.uuid,
            })
        })
        .collect()
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = svc::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let server = match s.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let applications = svc::subs(&state.db, SubKind::Application, s.id).await?;
    let databases = svc::subs(&state.db, SubKind::Database, s.id).await?;
    let pick = |r: &coolify_db::records::Record, k: &str| r.get(k).cloned();
    Ok(ok(json!({
        "name": s.str("name"),
        "uuid": s.uuid,
        "environment_name": pick(&s.environment, "name"),
        "environment_uuid": pick(&s.environment, "uuid"),
        "project_name": pick(&s.project, "name"),
        "project_uuid": pick(&s.project, "uuid"),
        "server_name": server.as_ref().and_then(|r| pick(&r.record, "name")),
        "server_uuid": server.as_ref().and_then(|r| pick(&r.record, "uuid")),
        "stack_applications": items(&applications),
        "stack_databases": items(&databases),
    }))
    .into_response())
}
