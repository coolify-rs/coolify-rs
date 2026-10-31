//! What the deployment job reads and writes for git-based applications: the
//! source (`github_apps`), the deploy key, build-time variable order and the
//! commit columns of `application_deployment_queues`.

use chrono::NaiveDateTime;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

/// A `github_apps` row (raw attributes).
pub async fn github_app(db: &Db, id: i64) -> DbResult<Option<Record>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(g)::text FROM github_apps g WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    row.map(|t| parse_row(&t).map_err(|e| DbError::Schema(format!("decoding github app: {e}"))))
        .transpose()
}

/// A row of `github_apps`, `gitlab_apps` or `gitea_apps` (raw attributes).
pub async fn source_row(db: &Db, table: &str, id: i64) -> DbResult<Option<Record>> {
    debug_assert!(matches!(
        table,
        "github_apps" | "gitlab_apps" | "gitea_apps"
    ));
    let row: Option<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(g)::text FROM {table} g WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    row.map(|t| parse_row(&t).map_err(|e| DbError::Schema(format!("decoding git source: {e}"))))
        .transpose()
}

/// `$source->update([...])` after `refreshGitlabToken()` / `refreshGiteaToken()` or the OAuth
/// callback: the encrypted tokens and the expiry.
pub async fn store_oauth_tokens(
    db: &Db,
    table: &str,
    id: i64,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {table} SET access_token = $1, refresh_token = $2, expires_at = $3, updated_at = NOW() AT TIME ZONE 'UTC' WHERE id = $4"
    ))
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at as i32)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `private_keys.private_key` (still encrypted) of a key id, any team.
pub async fn private_key_ciphertext(db: &Db, id: i64) -> DbResult<Option<String>> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT private_key FROM private_keys WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.flatten())
}

/// Ids of a resource's non-preview variables ordered by `key` (the database collation, as
/// `orderBy('key')` sorts them) or by `id`.
pub async fn variable_ids_ordered(
    db: &Db,
    resource_type: &str,
    resource_id: i64,
    by_key: bool,
) -> DbResult<Vec<i64>> {
    let sql = format!(
        "SELECT id FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND is_preview = false ORDER BY {}",
        if by_key { "key" } else { "id" }
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(resource_type)
        .bind(resource_id)
        .fetch_all(db.pool())
        .await?)
}

/// `reuse_commit_message()`: the latest non-empty message of a deployment of this commit.
pub async fn latest_commit_message(
    db: &Db,
    application_id: i64,
    commit: &str,
) -> DbResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT commit_message FROM application_deployment_queues WHERE application_id = $1 AND commit = $2 AND commit_message IS NOT NULL AND commit_message != '' ORDER BY id DESC LIMIT 1",
    )
    .bind(application_id.to_string())
    .bind(commit)
    .fetch_optional(db.pool())
    .await?)
}

/// `ApplicationDeploymentQueue::whereCommit($commit)->whereApplicationId($id)->update(['commit_message' => …])`.
pub async fn set_commit_message_for_commit(
    db: &Db,
    application_id: i64,
    commit: &str,
    message: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE application_deployment_queues SET commit_message = $1, updated_at = $2 WHERE commit = $3 AND application_id = $4",
    )
    .bind(message)
    .bind(now)
    .bind(commit)
    .bind(application_id.to_string())
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Which of `names` are deployment uuids (build helper containers).
pub async fn existing_deployment_uuids(db: &Db, names: &[String]) -> DbResult<Vec<String>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar(
        "SELECT deployment_uuid FROM application_deployment_queues WHERE deployment_uuid = ANY($1)",
    )
    .bind(names)
    .fetch_all(db.pool())
    .await?)
}

/// `(team_id, private_key ciphertext)` of a private key.
pub async fn private_key_row(db: &Db, id: i64) -> DbResult<Option<(Option<i64>, String)>> {
    let row: Option<(Option<i64>, String)> =
        sqlx::query_as("SELECT team_id, private_key FROM private_keys WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row)
}

/// `in_progress` deployments run by `worker` (`horizon_job_worker`) → `failed`.
pub async fn fail_in_progress_for_worker(
    db: &Db,
    worker: &str,
    now: NaiveDateTime,
) -> DbResult<usize> {
    if worker.is_empty() || db.ensure_writable().is_err() {
        return Ok(0);
    }
    let done = sqlx::query(
        "UPDATE application_deployment_queues SET status = 'failed', updated_at = $1 WHERE status = 'in_progress' AND horizon_job_worker = $2",
    )
    .bind(now)
    .bind(worker)
    .execute(db.pool())
    .await?;
    Ok(usize::try_from(done.rows_affected()).unwrap_or(0))
}
