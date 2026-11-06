//! `ValidateNewCloudServerJob`: waits until a server that was just created at a cloud provider
//! is running and accepts SSH, then validates it and installs Docker
//! (`ValidateAndInstallServerJob`).

use std::time::Duration;

use coolify_db::servers::{self, ColumnValue, Row};
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;
use crate::server_ops::Srv;

/// `ValidateNewCloudServerJob::MAX_ATTEMPTS`.
pub const MAX_ATTEMPTS: u32 = 40;

/// `ValidateNewCloudServerJob::RETRY_DELAY_SECONDS`.
pub const RETRY_DELAY_SECONDS: u64 = 30;

/// `ValidateNewCloudServerJob::$timeout`.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What one attempt decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do: the server is gone, transferred, validating or already functional.
    Skip,
    /// The server is ready; `ValidateAndInstallServerJob` was dispatched.
    Install,
    /// Not ready yet; the job runs again after `RETRY_DELAY_SECONDS`.
    Retry,
    /// Not ready after `MAX_ATTEMPTS`; `validation_logs` says so.
    GaveUp,
}

/// The message stored when the server never became ready.
pub fn gave_up_message() -> String {
    format!(
        "The server did not start or accept SSH connections within {} minutes after it was created. Validate it manually.",
        u64::from(MAX_ATTEMPTS) * RETRY_DELAY_SECONDS / 60
    )
}

impl Jobs {
    /// `ValidateNewCloudServerJob::dispatch($server)`.
    pub fn spawn_validate_new_cloud_server(&self, server_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let mut attempt = 1;
            loop {
                let step = match tokio::time::timeout(TIMEOUT, handle(&jobs, server_id, attempt))
                    .await
                {
                    Ok(Ok(step)) => step,
                    Ok(Err(e)) => {
                        tracing::warn!(server_id, attempt, error = %e, "validating the new cloud server failed");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(
                            server_id,
                            attempt,
                            "validating the new cloud server timed out"
                        );
                        return;
                    }
                };
                if step != Step::Retry {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECONDS)).await;
                attempt += 1;
            }
        });
    }
}

/// `ValidateNewCloudServerJob::handle` for one attempt.
pub async fn handle(jobs: &Jobs, server_id: i64, attempt: u32) -> Result<Step, String> {
    // `$deleteWhenMissingModels`.
    let Some(row) = servers::find_by_id(&jobs.0.db, server_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(Step::Skip);
    };
    let info = ServerInfo::from_row(&row);
    let validating = row.record.get("is_validating") == Some(&Value::Bool(true));
    if info.transferred || validating || info.is_functional() {
        return Ok(Step::Skip);
    }
    if is_ready(jobs, &row).await {
        jobs.spawn_validate_and_install(server_id);
        return Ok(Step::Install);
    }
    if attempt >= MAX_ATTEMPTS {
        servers::update_if_changed(
            &jobs.0.db,
            Row::Server(server_id),
            &[(
                "validation_logs",
                ColumnValue::Text(Some(gave_up_message())),
            )],
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
        return Ok(Step::GaveUp);
    }
    Ok(Step::Retry)
}

/// `isReady()`: running at the provider, a real IP, and SSH answers.
async fn is_ready(jobs: &Jobs, row: &servers::ServerRow) -> bool {
    let status = match crate::cloud::refresh_cloud_provider_state(jobs, row).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    if !matches!(status.as_deref(), Some("running" | "active")) {
        return false;
    }
    // The refresh can have replaced the placeholder IP.
    let Ok(srv) = Srv::load(jobs, row.id).await else {
        return false;
    };
    if srv.info.has_placeholder_ip() {
        return false;
    }
    crate::server_ops::install::validate_connection(&srv)
        .await
        .map(|(uptime, _)| uptime)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_give_up_message_names_twenty_minutes() {
        assert_eq!(
            gave_up_message(),
            "The server did not start or accept SSH connections within 20 minutes after it was created. Validate it manually."
        );
    }
}
