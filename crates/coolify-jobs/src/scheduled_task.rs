//! `ScheduledTaskJob` and the scheduled tasks part of `ScheduledJobManager`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use coolify_db::applications as apps;
use coolify_db::scheduled_tasks::{self as tasks, TaskContext};
use coolify_db::servers;
use coolify_domain::new_public_id;
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::{Value, json};

use crate::Jobs;
use crate::applications::status::aggregate;
use crate::notifications::{Ctx, FailureEvent, Notification, Task, lookup, notify};
use crate::schedule_sources::notify_missed_occurrence;
use crate::scheduler::{
    BoxFuture, Due, Occurrence, Overlap, ScheduleSource, allowed, server_timezone,
};
use crate::server::ServerInfo;

/// `ScheduledTaskJob::MAX_OUTPUT_SIZE_BYTES`.
const MAX_OUTPUT_SIZE_BYTES: u64 = 5 * 1024 * 1024;
/// `ScheduledTaskJob::WORKER_TIMEOUT_MARGIN_SECONDS`.
const WORKER_TIMEOUT_MARGIN_SECONDS: u64 = 120;
/// What the job fails with when it is handed out after its worker stopped; here at startup.
const WORKER_STOPPED: &str = "Job permanently failed after 2 attempts: The worker stopped while this task was running. The task is not run again late.";

/// `boundedTaskCommand()`.
fn bounded_task_command(command: &str) -> String {
    let max = MAX_OUTPUT_SIZE_BYTES;
    let read_limit = max + 1;
    format!(
        "output_file=$(mktemp); trap 'rm -f \"$output_file\"' EXIT; set +e; set -o pipefail; {command} 2>&1 | {{ head -c {read_limit} > \"$output_file\"; cat > /dev/null; }}; exit_code=${{PIPESTATUS[0]}}; if [ \"$(wc -c < \"$output_file\")\" -gt {max} ]; then truncate -s {max} \"$output_file\"; printf '\n\n[... Output truncated at 5MB limit ...]' >> \"$output_file\"; fi; if [ \"$exit_code\" -eq 0 ]; then cat \"$output_file\"; else cat \"$output_file\" >&2; fi; exit $exit_code"
    )
}

/// `ScheduledTask::splitCommandArguments()`: the words of a command as a POSIX shell splits them,
/// but nothing is expanded.
fn split_command_arguments(command: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = command.chars().collect();
    let mut arguments = Vec::new();
    let mut word: Option<String> = None;
    let mut quote_char: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        i += 1;
        match quote_char {
            Some('\'') => {
                if c == '\'' {
                    quote_char = None;
                } else {
                    word.get_or_insert_with(String::new).push(c);
                }
                continue;
            }
            Some(_) => {
                if c == '"' {
                    quote_char = None;
                } else if c == '\\' && matches!(next, Some('"' | '\\' | '$' | '`')) {
                    word.get_or_insert_with(String::new)
                        .push(next.unwrap_or_default());
                    i += 1;
                } else {
                    word.get_or_insert_with(String::new).push(c);
                }
                continue;
            }
            None => {}
        }
        // A backslash before a new line continues the line.
        if c == '\\' && next == Some('\n') {
            i += 1;
            continue;
        }
        // `ctype_space()`.
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c') {
            if let Some(done) = word.take() {
                arguments.push(done);
            }
            continue;
        }
        let current = word.get_or_insert_with(String::new);
        if c == '\'' || c == '"' {
            quote_char = Some(c);
        } else if c == '\\' && next.is_some() {
            current.push(next.unwrap_or_default());
            i += 1;
        } else {
            current.push(c);
        }
    }
    if quote_char.is_some() {
        return Err("The command has a quote that is not closed.".to_owned());
    }
    if let Some(done) = word {
        arguments.push(done);
    }
    Ok(arguments)
}

/// `ScheduledTask::containerCommand()`: the part of `docker exec <container> ...` that runs the
/// command, quoted for the host shell. Without a shell, the command is split into words, so it
/// also runs in containers that have no `sh` (distroless or scratch images).
fn container_command(command: &str, use_shell: bool) -> Result<String, String> {
    if use_shell {
        return Ok(format!("sh -c {}", quote(command)));
    }
    let arguments = split_command_arguments(command)?;
    if arguments.is_empty() {
        return Err("The command is empty.".to_owned());
    }
    Ok(arguments
        .iter()
        .map(|a| quote(a))
        .collect::<Vec<_>>()
        .join(" "))
}

/// The command `handle()` runs for one container.
fn exec_command(
    command: &str,
    use_shell: bool,
    container: &str,
    non_root: bool,
) -> Result<String, String> {
    let cmd = container_command(command, use_shell)?;
    let docker = if non_root { "sudo docker" } else { "docker" };
    Ok(bounded_task_command(&format!(
        "{docker} exec {} {cmd}",
        quote(container)
    )))
}

/// The container of `handle()`'s loop: the only one, the named one, or the one the service
/// naming `{container}-{uuid}` points at.
fn select_container<'a>(
    containers: &'a [String],
    wanted: Option<&str>,
    resource_uuid: &str,
) -> Option<&'a String> {
    let wanted = wanted.unwrap_or_default();
    let prefix = format!("{wanted}-{resource_uuid}");
    containers
        .iter()
        .find(|c| containers.len() == 1 || c.as_str() == wanted || c.starts_with(&prefix))
}

/// `new TaskSuccess($task, $output)`, `new TaskFailed($task, $message)`: the resource of the task
/// decides the URL, `Team::find($task->team_id)` the receiver.
async fn task_notification(jobs: &Jobs, task: &TaskContext, output: &str) -> Task {
    let db = &jobs.0.db;
    let (uuid, environment, is_application) = match (&task.application_uuid, &task.service_uuid) {
        (Some(uuid), _) => (Some(uuid.as_str()), task.application_environment_id, true),
        (None, Some(uuid)) => (Some(uuid.as_str()), task.service_environment_id, false),
        (None, None) => (None, None, false),
    };
    let mut t = lookup::scheduled_task(
        db,
        &task.name,
        &task.uuid,
        output,
        uuid,
        environment,
        is_application,
    )
    .await;
    // The failure streak of the row, for `TaskFailed`.
    t.id = task.id;
    t.consecutive_failures = i64::from(task.consecutive_failures);
    t.failing_since = task
        .failing_since
        .map(|at| at.format("%Y-%m-%d %H:%M:%S").to_string());
    t.disabled_reason.clone_from(&task.disabled_reason);
    t
}

/// `TaskSuccess`, then `ScheduledJobRecovered` once when `recordSuccess()` says that the team
/// was notified about the failure.
async fn notify_task_success(jobs: &Jobs, task: &TaskContext, output: &str) {
    let db = &jobs.0.db;
    let t = task_notification(jobs, task, output).await;
    let url = t.url(&Ctx::load(db).await);
    notify(
        db,
        jobs.0.encrypter.as_ref(),
        task.team_id,
        Notification::TaskSuccess(t),
    )
    .await;
    let recovered = match tasks::record_success(db, task.id, Jobs::now()).await {
        Ok(recovered) => recovered,
        Err(e) => {
            tracing::warn!(target: "scheduled-errors", task_id = %task.uuid, error = %e, "Could not record the scheduled task success");
            false
        }
    };
    if recovered {
        notify(
            db,
            jobs.0.encrypter.as_ref(),
            task.team_id,
            Notification::ScheduledJobRecovered {
                failure_event: FailureEvent::ScheduledTaskFailure,
                title: format!("Scheduled task ({}) works again.", task.name),
                webhook_event: "task_recovered".to_owned(),
                url,
                webhook_data: json!({"task_name": task.name, "task_uuid": task.uuid}),
            },
        )
        .await;
    }
}

/// `failed()`: `recordFailure()`, then `TaskFailed` with `disabled` when this failure disabled
/// the task.
async fn notify_task_failed(jobs: &Jobs, task: &TaskContext, message: &str) {
    let db = &jobs.0.db;
    let mut t = task_notification(jobs, task, message).await;
    match tasks::record_failure(db, task.id, Jobs::now()).await {
        Ok(Some(streak)) => {
            t.disabled = streak.disabled;
            t.consecutive_failures = i64::from(streak.consecutive_failures);
            t.failing_since = streak
                .failing_since
                .map(|at| at.format("%Y-%m-%d %H:%M:%S").to_string());
            t.disabled_reason = streak.disabled_reason;
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(target: "scheduled-errors", task_id = %task.uuid, error = %e, "Could not record the scheduled task failure");
        }
    }
    notify(
        db,
        jobs.0.encrypter.as_ref(),
        task.team_id,
        Notification::TaskFailed(t),
    )
    .await;
}

/// `ScheduledTaskDone::dispatch($team_id)`: the UI polls instead of listening.
fn scheduled_task_done(_team_id: i64) {}

impl Jobs {
    /// `ScheduledTaskJob::dispatch($task)`: runs now on a tokio task, without an occurrence. A run
    /// that finds the task's overlap lock held does nothing, like the job's middleware.
    pub fn dispatch_scheduled_task(&self, task_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let task = match tasks::context(&jobs.0.db, task_id).await {
                Ok(Some(task)) => task,
                Ok(None) => return,
                Err(e) => {
                    tracing::error!(target: "scheduled-errors", task_id, error = %e, "Could not load the scheduled task");
                    return;
                }
            };
            let Some(_guard) = jobs.try_overlap(&overlap_of(&task)) else {
                return;
            };
            let _ = jobs.run_scheduled_task(&task).await;
        });
    }

    /// Fails the runs a stopped process left behind (see [`tasks::fail_interrupted_executions`]).
    pub async fn fail_interrupted_scheduled_tasks(&self) {
        let now = Self::now();
        match tasks::fail_interrupted_executions(&self.0.db, WORKER_STOPPED, now).await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(count = n, "failed interrupted scheduled task executions"),
            Err(e) => tracing::warn!(error = %e, "could not fail interrupted scheduled tasks"),
        }
    }

    /// `handle()` and `failed()`. The worker timeout that kills a queued job is an outer timeout.
    pub(crate) async fn run_scheduled_task(&self, task: &TaskContext) -> Result<(), String> {
        let execution_uuid = new_public_id();
        let worker_timeout =
            u64::try_from(task.timeout).unwrap_or(300) + WORKER_TIMEOUT_MARGIN_SECONDS;
        let handled = tokio::time::timeout(
            Duration::from_secs(worker_timeout),
            self.handle_scheduled_task(task, &execution_uuid),
        )
        .await;
        let error = match handled {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => e,
            Err(_) => {
                // `failed()` after a worker timeout: only the queued uuid finds this run.
                let error = "App\\Jobs\\ScheduledTaskJob has timed out.".to_owned();
                let message = format!("Job permanently failed after 1 attempts: {error}");
                if let Err(e) = tasks::fail_unfinished_execution(
                    &self.0.db,
                    &execution_uuid,
                    &message,
                    Self::now(),
                )
                .await
                {
                    tracing::error!(target: "scheduled-errors", error = %e, "Could not update the execution");
                }
                error
            }
        };
        tracing::error!(target: "scheduled-errors", task_id = %task.uuid, task_name = %task.name, error = %error, "ScheduledTask permanently failed");
        notify_task_failed(self, task, &error).await;
        Err(error)
    }

    /// `handle()`. An `Err` carries the redacted message.
    async fn handle_scheduled_task(
        &self,
        task: &TaskContext,
        execution_uuid: &str,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let started = Instant::now();
        let started_at = Self::now();
        let fail_early = |message: String| {
            tracing::error!(target: "scheduled-errors", task_id = %task.uuid, task_name = %task.name, error = %message, "ScheduledTask execution failed");
            message
        };

        // initializeExecutionContext()
        let Some(resource_uuid) = task
            .service_uuid
            .clone()
            .or_else(|| task.application_uuid.clone())
        else {
            return Err(fail_early(
                "ScheduledTaskJob failed: No resource found.".to_owned(),
            ));
        };
        let team_exists = tasks::team_exists(db, task.team_id)
            .await
            .map_err(|e| e.to_string())?;
        if !team_exists {
            return Err(fail_early(format!(
                "No query results for model [App\\Models\\Team] {}",
                task.team_id
            )));
        }
        let Some(server_id) = task.server_id else {
            return Err(fail_early(
                "Attempt to read property \"server\" on null".to_owned(),
            ));
        };
        let Some(row) = servers::find_by_id(db, server_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Err(fail_early(
                "Attempt to read property \"settings\" on null".to_owned(),
            ));
        };
        let server = ServerInfo::from_row(&row);

        let execution_id = tasks::start_execution(db, execution_uuid, task.id, 0, started_at)
            .await
            .map_err(|e| fail_early(e.to_string()))?;

        let (known, locked) = self.task_redaction_secrets(task, server_id).await;
        let redact = |text: &str| {
            let known: Vec<&str> = known.iter().map(String::as_str).collect();
            let locked: Vec<&str> = locked.iter().map(String::as_str).collect();
            coolify_compat::redact::redact_text(text, &known, &locked)
        };

        let result = match self
            .task_command_output(task, &server, &resource_uuid)
            .await
        {
            // A stopped resource is not a task failure: it sends no notification and does not
            // count toward auto-disable.
            Ok(None) => tasks::set_execution_result(
                db,
                execution_id,
                "skipped",
                "Skipped: the resource has no running container.",
                Self::now(),
            )
            .await
            .map_err(|e| (None, e.to_string())),
            Ok(Some(output)) => {
                let output = redact(&output);
                match tasks::set_execution_result(db, execution_id, "success", &output, Self::now())
                    .await
                {
                    Ok(()) => {
                        notify_task_success(self, task, &output).await;
                        Ok(())
                    }
                    Err(e) => Err((Some(output), e.to_string())),
                }
            }
            Err(e) => Err((None, e)),
        };
        let result = match result {
            Ok(()) => Ok(()),
            Err((output, error)) => {
                let safe = redact(&error);
                let message = output.as_deref().unwrap_or(&safe);
                if let Err(e) =
                    tasks::set_execution_result(db, execution_id, "failed", message, Self::now())
                        .await
                {
                    tracing::error!(target: "scheduled-errors", error = %e, "Could not update the execution");
                }
                tracing::error!(target: "scheduled-errors", task_id = %task.uuid, task_name = %task.name, server = %server.uuid, attempt = 1, error = %safe, "ScheduledTask execution failed");
                Err(safe)
            }
        };

        // finally: round($startTime->floatDiffInSeconds($finishedAt), 2)
        scheduled_task_done(task.team_id);
        let duration = started.elapsed().as_secs_f64();
        let duration = (duration * 100.0).round() / 100.0;
        if let Err(e) = tasks::finish_execution(db, execution_id, Self::now(), duration).await {
            tracing::error!(target: "scheduled-errors", error = %e, "Could not finish the execution");
        }
        result
    }

    /// The `try` of `handle()` up to the command's output. `None` when the resource has no
    /// running container: the run is skipped.
    async fn task_command_output(
        &self,
        task: &TaskContext,
        server: &ServerInfo,
        resource_uuid: &str,
    ) -> Result<Option<String>, String> {
        let target = server.target(self).await?;
        let containers = if task.service_id.is_some() {
            self.running_service_containers(task, resource_uuid).await?
        } else {
            // getCurrentApplicationContainerStatus($server, $application, 0), running ones only.
            self.containers_for_pull_request(&target, server, resource_uuid, Some(0))
                .await?
                .iter()
                .filter(|c| c.get("State").and_then(Value::as_str) == Some("running"))
                .filter_map(|c| c.get("Names").and_then(Value::as_str))
                .map(|name| name.replace('/', ""))
                .collect()
        };
        if containers.is_empty() {
            return Ok(None);
        }
        let wanted = task.container.as_deref().filter(|c| !is_php_empty(c));
        if containers.len() > 1 && wanted.is_none() {
            return Err(
                "ScheduledTaskJob failed: More than one container exists but no container name was provided."
                    .to_owned(),
            );
        }
        let Some(container) =
            select_container(&containers, task.container.as_deref(), resource_uuid)
        else {
            return Err(
                "ScheduledTaskJob failed: No valid container was found. Is the container name correct?"
                    .to_owned(),
            );
        };
        let exec = exec_command(
            &task.command,
            task.use_shell,
            container,
            server.is_non_root(),
        )?;
        let timeout = Duration::from_secs(u64::try_from(task.timeout).unwrap_or(300));
        // upstream disables SSH multiplexing here; coolify-remote does not multiplex at all.
        let output = self
            .remote()
            .run(
                &target,
                &[&exec],
                RunOptions::op("scheduled_task.exec")
                    .no_sudo()
                    .timeout(timeout),
            )
            .await
            .map_err(|e| e.message)?;
        Ok(Some(output.unwrap_or_default()))
    }

    /// The service branch of `handle()`: `{name}-{uuid}` of applications and databases that run.
    async fn running_service_containers(
        &self,
        task: &TaskContext,
        resource_uuid: &str,
    ) -> Result<Vec<String>, String> {
        let resources = tasks::service_resources(&self.0.db, task.service_id.unwrap_or_default())
            .await
            .map_err(|e| e.to_string())?;
        Ok(resources
            .into_iter()
            .filter(|(_, status, _)| status.as_deref().is_some_and(|s| s.contains("running")))
            .map(|(name, _, _)| format!("{name}-{resource_uuid}"))
            .collect())
    }

    /// `EnvironmentVariable::logRedactionSecretsFor($this->resource)`.
    async fn task_redaction_secrets(
        &self,
        task: &TaskContext,
        server_id: i64,
    ) -> (Vec<String>, Vec<String>) {
        if let Some(service_id) = task.service_id {
            return self
                .service_log_redaction_secrets(service_id, task.team_id, server_id)
                .await;
        }
        let Some(id) = task.application_id else {
            return (Vec::new(), Vec::new());
        };
        match apps::find_by_id(&self.0.db, id).await {
            Ok(Some(app)) => self.log_redaction_secrets(&app, server_id, false).await,
            _ => (Vec::new(), Vec::new()),
        }
    }
}

/// PHP `empty()` of a string.
fn is_php_empty(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// `middleware()`: the lock outlives the worker timeout.
fn overlap_of(task: &TaskContext) -> Overlap {
    Overlap {
        key: format!("scheduled-task-{}", task.id),
        ttl: Duration::from_secs(u64::try_from(task.timeout).unwrap_or(300) + 300),
    }
}

/// `getTaskCriticalSkipReason()` and `getTaskRuntimeSkipReason()` without the two cases that delete
/// the task. `subscription_unpaid` applies to Coolify Cloud only.
fn skip_reason(
    task: &TaskContext,
    server: &ServerInfo,
    service_status: Option<&str>,
) -> Option<String> {
    if !server.is_functional() {
        return Some("server_not_functional".into());
    }
    if task.application_id.is_some()
        && !task
            .application_status
            .as_deref()
            .is_some_and(|s| s.contains("running"))
    {
        return Some("application_not_running".into());
    }
    if task.service_id.is_some() && !service_status.is_some_and(|s| s.contains("running")) {
        return Some("service_not_running".into());
    }
    None
}

/// `Service::containerStatus()`. `isStarting()` needs the start activities of services, which are
/// not ported: a starting service whose containers already run counts as running here.
fn service_status(resources: &[(String, Option<String>, bool)]) -> String {
    let collect = |excluded: bool| -> Vec<String> {
        resources
            .iter()
            .filter(|(_, status, exclude)| {
                let status = status.as_deref().unwrap_or_default();
                (*exclude || status.contains(":excluded")) == excluded
            })
            .map(|(_, status, _)| {
                let status = status.as_deref().unwrap_or_default();
                status
                    .split(":excluded")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    };
    let included = collect(false);
    if !included.is_empty() {
        return aggregate(&included);
    }
    let excluded = collect(true);
    if excluded.is_empty() {
        return "unknown:unknown:excluded".to_owned();
    }
    format!("{}:excluded", aggregate(&excluded))
}

/// The `tasks` type of `ScheduledJobManager::TYPES`.
#[derive(Debug, Clone, Copy)]
pub struct ScheduledTaskSource;

impl ScheduleSource for ScheduledTaskSource {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn job_types(&self) -> &'static [&'static str] {
        &["scheduled-task"]
    }

    /// `scheduledTaskQuery()`, `dueScheduledTasks()`: a task without a server or a resource is
    /// deleted (`server_deleted`, `resource_deleted`).
    fn due<'a>(
        &'a self,
        jobs: &'a Jobs,
        _now: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<Vec<Due>, String>> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let enabled = tasks::enabled(db).await.map_err(|e| e.to_string())?;
            let mut servers_seen: HashMap<i64, Option<ServerInfo>> = HashMap::new();
            let mut due = Vec::with_capacity(enabled.len());
            for task in enabled {
                if !allowed(&format!("scheduled-task:{}", task.id)) {
                    continue;
                }
                let server = match task.server_id {
                    None => None,
                    Some(id) => {
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            servers_seen.entry(id)
                        {
                            let info = servers::find_by_id(db, id)
                                .await
                                .map_err(|e| e.to_string())?
                                .map(|row| ServerInfo::from_row(&row));
                            slot.insert(info);
                        }
                        servers_seen.get(&id).and_then(Option::as_ref)
                    }
                };
                let has_resource = task.application_id.is_some() || task.service_id.is_some();
                let Some(server) = server.filter(|_| has_resource) else {
                    let reason = if server.is_none() {
                        "server_deleted"
                    } else {
                        "resource_deleted"
                    };
                    if let Err(e) = tasks::delete(db, task.id).await {
                        tracing::error!(target: "scheduled-errors", task_id = task.id, error = %e, "Error processing task");
                    } else {
                        tracing::info!(target: "scheduled", task_id = task.id, skip_reason = reason, "Task skipped");
                    }
                    continue;
                };
                let status = match task.service_id {
                    Some(id) => Some(service_status(
                        &tasks::service_resources(db, id)
                            .await
                            .map_err(|e| e.to_string())?,
                    )),
                    None => None,
                };
                due.push(Due {
                    schedule_key: format!("scheduled-task:{}", task.id),
                    frequency: task.frequency.clone(),
                    timezone: server_timezone(&server.settings),
                    job_type: "scheduled-task",
                    resource_id: task.id,
                    payload: json!([]),
                    skip: skip_reason(&task, server, status.as_deref()),
                });
            }
            Ok(due)
        })
    }

    fn overlap<'a>(&'a self, jobs: &'a Jobs, resource_id: i64) -> BoxFuture<'a, Option<Overlap>> {
        Box::pin(async move {
            let task = tasks::context(&jobs.0.db, resource_id)
                .await
                .ok()
                .flatten()?;
            Some(overlap_of(&task))
        })
    }

    fn run<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let task = tasks::context(&jobs.0.db, occurrence.resource_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "The scheduled task no longer exists.".to_owned())?;
            jobs.run_scheduled_task(&task).await
        })
    }

    /// `reportMissedOccurrence()` for `scheduled-task`.
    fn report_missed<'a>(
        &'a self,
        jobs: &'a Jobs,
        occurrence: &'a Occurrence,
        message: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let db = &jobs.0.db;
            let Ok(Some(task)) = tasks::context(db, occurrence.resource_id).await else {
                return;
            };
            if let Err(e) = tasks::record_missed_execution(db, task.id, message, Jobs::now()).await
            {
                tracing::error!(target: "scheduled-errors", occurrence_uuid = %occurrence.uuid, error = %e, "Failed to report missed scheduled occurrence");
            }
            scheduled_task_done(task.team_id);
            let t = task_notification(jobs, &task, message).await;
            notify_missed_occurrence(
                jobs,
                "App\\Models\\ScheduledTask",
                task.id,
                task.team_id,
                Notification::TaskFailed(t),
            )
            .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_matches_upstream() {
        let exec = exec_command("echo 'it works'", true, "app-1", false).unwrap();
        assert_eq!(
            exec,
            "output_file=$(mktemp); trap 'rm -f \"$output_file\"' EXIT; set +e; set -o pipefail; docker exec 'app-1' sh -c 'echo '\\''it works'\\''' 2>&1 | { head -c 5242881 > \"$output_file\"; cat > /dev/null; }; exit_code=${PIPESTATUS[0]}; if [ \"$(wc -c < \"$output_file\")\" -gt 5242880 ]; then truncate -s 5242880 \"$output_file\"; printf '\n\n[... Output truncated at 5MB limit ...]' >> \"$output_file\"; fi; if [ \"$exit_code\" -eq 0 ]; then cat \"$output_file\"; else cat \"$output_file\" >&2; fi; exit $exit_code"
        );
        assert!(
            exec_command("x", true, "a", true)
                .unwrap()
                .contains("sudo docker exec 'a'")
        );
    }

    #[test]
    fn a_command_without_a_shell_is_split_into_arguments() {
        assert_eq!(
            container_command("/app/bin/task --name 'my task' \"a b\"", false).as_deref(),
            Ok("'/app/bin/task' '--name' 'my task' 'a b'")
        );
        // Nothing is expanded, and shell operators are plain arguments.
        assert_eq!(
            container_command("echo $HOME | cat > /tmp/x", false).as_deref(),
            Ok("'echo' '$HOME' '|' 'cat' '>' '/tmp/x'")
        );
        assert!(
            exec_command("/bin/tool run", false, "app-1", false)
                .unwrap()
                .contains("docker exec 'app-1' '/bin/tool' 'run' 2>&1")
        );
    }

    #[test]
    fn arguments_follow_posix_quoting() {
        let split = |c: &str| split_command_arguments(c).unwrap();
        assert_eq!(split("a  b\tc\nd"), ["a", "b", "c", "d"]);
        assert_eq!(split("say 'it''s' \"\""), ["say", "its", ""]);
        // In double quotes a backslash only escapes `"`, `\\`, `$` and a backtick.
        assert_eq!(
            split(r#"x "a\"b" "c\\d" "e\$f" "g\nh""#),
            ["x", "a\"b", "c\\d", "e$f", "g\\nh"]
        );
        // Outside quotes a backslash keeps the next character, and joins lines.
        assert_eq!(split("a\\ b c\\\nd"), ["a b", "cd"]);
        assert_eq!(split("  "), Vec::<String>::new());
        assert_eq!(split("it'\"'s"), ["it\"s"]);
    }

    #[test]
    fn a_command_that_cannot_be_split_fails() {
        assert_eq!(
            container_command("echo 'open", false).unwrap_err(),
            "The command has a quote that is not closed."
        );
        assert_eq!(
            container_command("  ", false).unwrap_err(),
            "The command is empty."
        );
        // With a shell the command is passed as it is.
        assert_eq!(
            container_command("echo 'open", true).as_deref(),
            Ok("sh -c 'echo '\\''open'")
        );
    }

    #[test]
    fn container_selection() {
        let one = vec!["only".to_owned()];
        assert_eq!(select_container(&one, None, "u"), Some(&one[0]));
        assert_eq!(select_container(&one, Some("other"), "u"), Some(&one[0]));
        let many = vec!["web-u".to_owned(), "db-u-1".to_owned()];
        assert_eq!(select_container(&many, Some("web-u"), "u"), Some(&many[0]));
        assert_eq!(select_container(&many, Some("db"), "u"), Some(&many[1]));
        assert_eq!(select_container(&many, Some("cache"), "u"), None);
    }

    #[test]
    fn service_status_ignores_excluded_containers() {
        let r = |status: &str, exclude: bool| ("n".to_owned(), Some(status.to_owned()), exclude);
        assert!(
            service_status(&[r("running:healthy", false), r("exited", true)]).contains("running")
        );
        assert!(
            !service_status(&[r("exited", false), r("running:healthy", true)]).contains("running")
        );
        assert!(service_status(&[r("running:healthy", true)]).ends_with(":excluded"));
        assert_eq!(service_status(&[]), "unknown:unknown:excluded");
    }
}
