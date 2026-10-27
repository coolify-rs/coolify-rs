//! Port of upstream `App\Actions\Server\ValidateServer` and the `Server`
//! methods it calls (`validateConnection`, `validateOS`,
//! `validatePrerequisites`, `validateDockerEngine`, `validateDockerCompose`,
//! `validateDockerEngineVersion`). The Livewire flow differs only in wording
//! and in what happens after success (proxy start, server details), which
//! coolify-rs does not port.

use coolify_db::servers::{self, ColumnValue, Row};
use coolify_remote::{RunOptions, Target};

use crate::checks::{self, Wording};
use crate::server::ServerInfo as Server;
use crate::{Jobs, Progress};

/// `Server::setValidationLogsAttribute`: redacted, then purified.
fn stored_logs(message: &str) -> String {
    coolify_compat::purify::validation_logs(&coolify_compat::redact::redact_text(message, &[], &[]))
}

pub(crate) async fn run(
    jobs: &Jobs,
    server_id: i64,
    team_id: i64,
    uuid: &str,
    wording: Wording,
) -> Result<(), String> {
    let db = &jobs.0.db;
    let row = servers::find(db, team_id, uuid)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Server not found.")?;
    if row.id != server_id {
        return Err("Server not found.".into());
    }
    let s = Server::from_row(&row);
    let transferred = s.transferred;
    let v = Validation { jobs, s, wording };

    if transferred {
        v.server_columns(&[
            (
                "validation_logs",
                ColumnValue::Text(Some(stored_logs(checks::TRANSFERRED))),
            ),
            ("is_validating", ColumnValue::Bool(false)),
        ])
        .await?;
        return v.fail(checks::TRANSFERRED.into());
    }
    v.server_columns(&[("validation_logs", ColumnValue::Text(None))])
        .await?;
    v.steps().await
}

struct Validation<'a> {
    jobs: &'a Jobs,
    s: Server,
    wording: Wording,
}

impl Validation<'_> {
    fn progress(&self, f: impl FnOnce(&mut Progress)) {
        self.jobs.set_progress(self.s.id, f);
    }

    async fn server_columns(&self, values: &[(&'static str, ColumnValue)]) -> Result<(), String> {
        servers::update_if_changed(&self.jobs.0.db, Row::Server(self.s.id), values, Jobs::now())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn settings(&self, values: &[(&'static str, ColumnValue)]) -> Result<(), String> {
        servers::update_if_changed(
            &self.jobs.0.db,
            Row::Settings(self.s.id),
            values,
            Jobs::now(),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Stores the message in `validation_logs` and fails the job.
    async fn log_failure(&self, message: String) -> Result<(), String> {
        let stored = stored_logs(&message);
        self.server_columns(&[("validation_logs", ColumnValue::Text(Some(stored.clone())))])
            .await?;
        self.fail(stored)
    }

    fn fail(&self, message: String) -> Result<(), String> {
        self.progress(|p| p.error = Some(message.clone()));
        Err(message)
    }

    async fn target(&self) -> Result<Target, String> {
        self.s.target(self.jobs).await
    }

    async fn steps(&self) -> Result<(), String> {
        let remote = self.jobs.remote();

        // validateConnection(): one attempt, so a host that is not the user's
        // does not see repeated failed logins.
        let (uptime, error, target) = if self.s.skipped() {
            (false, "Server skipped.".to_owned(), None)
        } else {
            match self.target().await {
                Err(e) => (false, e, None),
                Ok(t) => match remote
                    .run(
                        &t,
                        &["ls /"],
                        RunOptions::op("validate.connection").attempts(1),
                    )
                    .await
                {
                    Ok(_) => (true, String::new(), Some(t)),
                    Err(e) => (false, e.message, None),
                },
            }
        };
        // A skipped server keeps its flag; any other outcome records it.
        if !self.s.skipped() && self.s.setting_bool("is_reachable") != Some(uptime) {
            // ServerReachabilityChanged only drives notifications, which are not ported.
            self.settings(&[("is_reachable", ColumnValue::Bool(uptime))])
                .await?;
        }
        self.progress(|p| p.uptime = Some(uptime));
        let Some(t) = target.filter(|_| uptime) else {
            return self.log_failure(checks::unreachable_message(&error)).await;
        };

        // validateOS(): this call throws on failure, which fails the job without a log.
        let os_release = remote
            .run(&t, &["cat /etc/os-release"], RunOptions::op("validate.os"))
            .await
            .map_err(|e| e.message)?;
        let supported = checks::supported_os_from_os_release(os_release.as_deref()).is_some();
        self.progress(|p| p.supported_os_type = Some(supported));
        if !supported {
            return self.log_failure(checks::os_message(self.wording)).await;
        }

        // ValidatePrerequisites.
        let mut missing = Vec::new();
        for cmd in ["git", "curl", "jq"] {
            let script = format!("command -v {cmd}");
            let found = remote
                .run_quiet(
                    &t,
                    &[script.as_str()],
                    RunOptions::op("validate.prerequisites"),
                )
                .await;
            if !checks::php_truthy(found.as_deref()) {
                missing.push(cmd);
            }
        }
        self.progress(|p| p.prerequisites_installed = Some(missing.is_empty()));
        if !missing.is_empty() {
            return self
                .log_failure(checks::prerequisites_message(self.wording, &missing))
                .await;
        }

        // validateDockerEngine() and validateDockerCompose(), both always run.
        let docker = self.docker_engine(&t).await?;
        let compose = remote
            .run_quiet(
                &t,
                &["docker compose version"],
                RunOptions::op("validate.compose"),
            )
            .await
            .is_some();
        self.settings(&[("is_usable", ColumnValue::Bool(compose))])
            .await?;
        self.progress(|p| {
            p.docker_installed = Some(docker);
            p.docker_compose_installed = Some(compose);
        });
        if !docker || !compose {
            return self
                .log_failure(checks::docker_missing_message(self.wording))
                .await;
        }

        // validateDockerEngineVersion().
        let raw = remote
            .run_quiet(
                &t,
                &["docker version --format json"],
                RunOptions::op("validate.docker_version"),
            )
            .await;
        let version = checks::docker_engine_version_from_json(raw.as_deref());
        let stored = checks::parse_docker_engine_version(version.as_deref());
        self.settings(&[
            ("docker_version", ColumnValue::Text(stored.clone())),
            ("docker_version_checked_at", ColumnValue::Time(Jobs::now())),
        ])
        .await?;
        let compose_raw = remote
            .run_quiet(
                &t,
                &["docker compose version --short"],
                RunOptions::op("validate.compose_version"),
            )
            .await;
        self.settings(&[
            (
                "compose_version",
                ColumnValue::Text(checks::parse_docker_engine_version(compose_raw.as_deref())),
            ),
            ("compose_version_checked_at", ColumnValue::Time(Jobs::now())),
        ])
        .await?;
        let new_enough = checks::check_minimum_docker_engine_version(version.as_deref()).is_some();
        if new_enough {
            self.settings(&[
                ("is_reachable", ColumnValue::Bool(true)),
                ("is_usable", ColumnValue::Bool(true)),
            ])
            .await?;
        } else {
            self.settings(&[("is_usable", ColumnValue::Bool(false))])
                .await?;
        }
        self.progress(|p| p.docker_version = Some(new_enough));
        if !new_enough {
            return self
                .log_failure(checks::docker_version_message(
                    stored.as_deref(),
                    &self.s.user,
                ))
                .await;
        }
        if self.wording == Wording::Ui {
            // Livewire marks the validation finished.
            self.server_columns(&[("is_validating", ColumnValue::Bool(false))])
                .await?;
        }
        Ok(())
    }

    /// `validateDockerEngine()`.
    async fn docker_engine(&self, t: &Target) -> Result<bool, String> {
        let remote = self.jobs.remote();
        let binary = remote
            .run_quiet(
                t,
                &["command -v docker"],
                RunOptions::op("validate.docker").no_sudo(),
            )
            .await;
        if binary.is_none() {
            self.settings(&[("is_usable", ColumnValue::Bool(false))])
                .await?;
            return Ok(false);
        }
        if remote
            .run(t, &["docker version"], RunOptions::op("validate.docker"))
            .await
            .is_err()
        {
            self.settings(&[("is_usable", ColumnValue::Bool(false))])
                .await?;
            return Ok(false);
        }
        self.settings(&[("is_usable", ColumnValue::Bool(true))])
            .await?;
        if !self.s.is_build_server() {
            remote
                .run_quiet(
                    t,
                    &["docker network create coolify --attachable >/dev/null 2>&1 || true"],
                    RunOptions::op("validate.network"),
                )
                .await;
        }
        Ok(true)
    }
}
