//! The import routes of `ServiceDatabasesController`: `upload_import`, `create_import` and
//! `show_import`, which run the same `HandlesDatabaseImportsApi` code as standalone databases.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use coolify_db::databases::{self as dbs, DatabaseRow};
use coolify_db::services as svc;

use super::database_more;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult};
use crate::state::AppState;

/// `withImportDatabase`.
async fn database(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    database_uuid: &str,
) -> Result<DatabaseRow, ApiError> {
    const NOT_FOUND: ApiError =
        ApiError::Message(StatusCode::NOT_FOUND, "Service database not found.");
    let service = svc::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let row = dbs::service_database_in_team(&state.db, ctx.team_id, database_uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if row.str("__service_uuid") != Some(service.uuid.as_str()) {
        return Err(NOT_FOUND);
    }
    Ok(row)
}

/// `POST /services/{uuid}/databases/{database_uuid}/imports/uploads`.
pub async fn upload_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
    multipart: Multipart,
) -> ApiResult {
    let d = database(&state, &ctx, &uuid, &database_uuid).await?;
    database_more::upload_database_import(state, ctx, d, multipart).await
}

/// `POST /services/{uuid}/databases/{database_uuid}/imports`.
pub async fn create_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let d = database(&state, &ctx, &uuid, &database_uuid).await?;
    let status_base = format!("/api/v1/services/{uuid}/databases/{database_uuid}/imports");
    database_more::start_database_import(state, ctx, d, status_base, headers, uri, body).await
}

/// `GET /services/{uuid}/databases/{database_uuid}/imports/{activity_id}`.
pub async fn show_import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, database_uuid, activity_id)): Path<(String, String, String)>,
) -> ApiResult {
    // `whereNumber('activity_id')`
    let Ok(activity_id) = activity_id.parse::<i64>() else {
        return Err(ApiError::RouteNotFound);
    };
    let d = database(&state, &ctx, &uuid, &database_uuid).await?;
    database_more::show_database_import(state, ctx, d, activity_id).await
}
