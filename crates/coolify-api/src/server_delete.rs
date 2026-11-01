//! The step `ServersController::delete_server` and `Server\\Delete::delete` share after their checks.

use coolify_db::servers::{self, ServerRow};
use coolify_jobs::cloud::DeleteFrom;

use crate::response::ApiError;
use crate::state::AppState;

/// `$server->delete()` and `DeleteServer::dispatch`. Without a job runner, which only tests lack, the
/// rows go at once.
pub async fn delete(
    state: &AppState,
    row: ServerRow,
    force: bool,
    from: DeleteFrom,
) -> Result<(), ApiError> {
    match &state.jobs {
        Some(jobs) => jobs.delete_server(row, force, from).await?,
        None => {
            servers::soft_delete(&state.db, row.id, state.now()).await?;
            servers::force_delete(&state.db, row.id).await?;
        }
    }
    Ok(())
}
