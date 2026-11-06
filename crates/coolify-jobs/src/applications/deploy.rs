//! `ApplicationDeploymentJob` on a standalone Docker destination: the same
//! commands in the same order, the same log entries in
//! `application_deployment_queues.logs`, the same rows.
//! compat/notes/deployment-job-dockerimage.md and deployment-job-git.md describe
//! upstream's job; the git build packs are in `build.rs`.

mod build;
mod commands;
pub(super) mod compose_pack;
mod git;
mod preview;
mod secrets;
pub(crate) use secrets::remote_redaction_values;
mod servers_multi;

use servers_multi::multi_server_mark_queued;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::{NaiveDateTime, Utc};
use coolify_db::applications::{self as apps, ApplicationRow, LogEntry};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::shell::quote;
use coolify_remote::{RunOptions, Stream, Target};
use serde_json::Value;

use super::compose::{self, ENV_FILE};
use super::docker_run;
use super::labels::{self, LabelInput};
use super::snapshot;
use crate::Jobs;
use crate::databases::env::{self, Owner, Var};
use crate::integrations::secret_managers;
use crate::server::ServerInfo;
use crate::storages::{FileStorage, StorageOwner};

/// `ExecuteRemoteCommand::$batch_counter`: one per `execute_remote_command` call, never reset.
static BATCH: AtomicU64 = AtomicU64::new(0);

/// `config('constants.coolify.helper_version')`.
const HELPER_VERSION: &str = "1.0.19";
/// `config('constants.ssh.command_timeout')`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3600);
const SEPARATOR: &str = "----------------------------------------";

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Carbon's JSON form with microseconds.
fn timestamp() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// One command of an `execute_remote_command` call.
#[derive(Debug, Clone, Default)]
struct Cmd {
    command: String,
    hidden: bool,
    ignore_errors: bool,
    save: Option<String>,
    append: bool,
    kind: Option<&'static str>,
    skip_command_log: bool,
    /// `command_hidden`: the command goes to a hidden `[CMD]:` line, the output stays visible.
    command_hidden: bool,
    /// Content upstream sends on the command's stdin.
    input: Option<String>,
}

impl Cmd {
    fn new(command: impl Into<String>) -> Self {
        Cmd {
            command: command.into(),
            append: true,
            ..Default::default()
        }
    }
    fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
    fn ignore_errors(mut self) -> Self {
        self.ignore_errors = true;
        self
    }
    fn save(mut self, key: impl Into<String>, append: bool) -> Self {
        self.save = Some(key.into());
        self.append = append;
        self
    }
    fn kind(mut self, kind: &'static str) -> Self {
        self.kind = Some(kind);
        self
    }
    fn skip_command_log(mut self) -> Self {
        self.skip_command_log = true;
        self
    }
    fn command_hidden(mut self) -> Self {
        self.command_hidden = true;
        self
    }
    fn input(mut self, input: String) -> Self {
        self.input = Some(input);
        self
    }
}

/// `executeInDocker($container, $command)`.
fn in_docker(container: &str, command: &str) -> String {
    format!(
        "docker exec {container} bash -c '{}'",
        command.replace('\'', "'\\''")
    )
}

/// How a deployment ends early.
#[derive(Debug)]
enum Fail {
    /// `Deployment cancelled by user` (code 69420): the status stays `cancelled-by-user`.
    Cancelled,
    /// A failure; `logged` when the message was already written to the log.
    Error {
        message: String,
        code: i64,
        logged: bool,
    },
}

impl Fail {
    fn new(message: impl Into<String>) -> Self {
        Fail::Error {
            message: message.into(),
            code: 0,
            logged: false,
        }
    }
}

type Step = Result<(), Fail>;

/// The build state of a git deployment (`$this->build_pack`, `$this->source`, image names, …).
#[derive(Default)]
struct BuildState {
    restart_only: bool,
    rollback: bool,
    force_rebuild: bool,
    disable_build_cache: bool,
    /// `$this->addHosts`.
    add_hosts: String,
    /// `$this->buildTarget`.
    build_target: Option<String>,
    source: Option<git::Source>,
    kind: git::DeploymentType,
    repository: String,
    port: String,
    branch: String,
    full_repo_url: String,
    github_token: Option<String>,
    coolify_variables: String,
    production_image: String,
    build_image: String,
    dockerfile_location: String,
    compose_location: String,
    buildkit: bool,
    buildx: bool,
    /// `$this->pull_request_id`.
    pull_request_id: i64,
    /// `$this->git_type`.
    git_type: Option<String>,
    /// `$this->dockerImagePreviewTag`.
    preview_tag: Option<String>,
    /// `$this->preview`.
    preview: Option<coolify_db::records::Record>,
    /// `$this->dockerSecretsSupported`.
    secrets_supported: bool,
    /// `$this->build_secrets`.
    build_secrets: String,
    variables: Option<Vec<Var>>,
    /// `$remote_secrets_cache`.
    remote_secrets: Option<crate::integrations::secret_managers::Secrets>,
    env_args: build::Ordered,
    build_args: Vec<String>,
    /// `$this->useBuildtimeEnvironmentLauncher`.
    launcher: bool,
    nixpacks_plan: Option<Value>,
    /// Set by the Docker Compose build pack before `save_runtime_environment()`.
    compose_env: Option<compose::ComposeEnv>,
    compose_fingerprint: Option<String>,
    compose_build_env: Option<Vec<String>>,
    /// `$this->docker_compose_custom_start_command`.
    compose_start_command: Option<String>,
    /// `$this->docker_compose_custom_build_command`.
    compose_build_command: Option<String>,
}

impl BuildState {
    /// `$this->pull_request_id !== 0`.
    fn is_preview(&self) -> bool {
        self.pull_request_id != 0
    }
}

/// The state of one deployment.
struct Run {
    jobs: Jobs,
    queue_id: i64,
    dep: String,
    app: ApplicationRow,
    /// `$this->server`: follows the build server while the image is built.
    server: ServerInfo,
    server_name: String,
    target: Arc<Target>,
    /// `$this->mainServer`: the server of this deployment's destination.
    main_server: ServerInfo,
    main_name: String,
    main_target: Arc<Target>,
    /// `$this->build_server`, when `$this->use_build_server` was ever set.
    build_server: Option<(ServerInfo, String, Arc<Target>)>,
    use_build_server: bool,
    /// `$this->destination->network`.
    network: String,
    /// `$this->destination->id`.
    destination_id: i64,
    /// `$this->is_this_additional_server`.
    is_additional: bool,
    only_this_server: bool,
    parent_deployment_uuid: Option<String>,
    container: String,
    commit: String,
    workdir: String,
    cfg: String,
    stop_timeout: u32,
    known: Vec<String>,
    locked: Vec<String>,
    saved: HashMap<String, String>,
    generated_compose: Option<String>,
    healthy: bool,
    b: BuildState,
    /// `$hiddenEnvValuesHintAdded`, shared with the output writers.
    hint_added: Arc<std::sync::atomic::AtomicBool>,
}

/// `ApplicationDeploymentQueue::HIDDEN_ENV_VALUES_HINT`.
const HIDDEN_ENV_VALUES_HINT: &str = "Some environment variable values are hidden in this log. To show values that are not locked, go to Environment Variables > Environment values in logs and select \"Hide locked values only\".";

/// `SensitiveDataRedactor::containsKnownSecret($text, $unlocked)`.
fn contains_known_secret(text: &str, known: &[String], locked: &[String]) -> bool {
    known
        .iter()
        .filter(|s| !locked.contains(s))
        .any(|s| !s.trim().is_empty() && s.len() >= 8 && text.contains(s.as_str()))
}

/// `EnvironmentVariable::logRedactionValues()` from a resolved value.
fn log_redaction_values(v: &Var) -> Vec<String> {
    let Some(resolved) = v.resolved.clone().filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    let literal = v.is_literal || v.is_multiline;
    let json = (resolved.starts_with('{') || resolved.starts_with('['))
        && serde_json::from_str::<Value>(&resolved).is_ok();
    let legacy = if json {
        resolved.clone()
    } else if literal {
        format!("'{resolved}'")
    } else {
        crate::databases::env::escape_env_variables(&resolved)
    };
    let mut values = vec![legacy.clone(), resolved];
    if literal {
        let unquoted = if legacy.len() >= 2 && legacy.starts_with('\'') && legacy.ends_with('\'') {
            legacy[1..legacy.len() - 1].to_owned()
        } else {
            legacy.clone()
        };
        values.push(unquoted.clone());
        values.push(format!("'{}'", unquoted.replace('\'', "'\\''")));
        values.push(unquoted.replace('\'', "'\\''"));
        values.push(unquoted.replace("\r\n", "\\n").replace(['\r', '\n'], "\\n"));
        if v.is_multiline {
            values.extend(unquoted.split(['\r', '\n']).map(str::to_owned));
        }
    }
    let mut out: Vec<String> = Vec::new();
    for x in values {
        if !x.is_empty() && !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

impl Run {
    fn redact(&self, text: &str) -> String {
        let known: Vec<&str> = self.known.iter().map(String::as_str).collect();
        let locked: Vec<&str> = self.locked.iter().map(String::as_str).collect();
        coolify_compat::redact::redact_text(text, &known, &locked)
    }

    /// `addLogEntry($message, $type, $hidden)` (batch 1, no command).
    async fn log(&self, message: &str, kind: &'static str, hidden: bool) {
        let hint = !self.hint_added.load(std::sync::atomic::Ordering::SeqCst)
            && contains_known_secret(&format!("{} ", message.trim()), &self.known, &self.locked);
        let mut output = self.redact(message.trim());
        if output.starts_with('╔') {
            output = format!("\n{output}");
        }
        let kind = if kind == "error" { "stderr" } else { kind };
        let entry = LogEntry {
            command: None,
            output,
            kind,
            timestamp: timestamp(),
            hidden,
            batch: 1,
        };
        let _ = apps::append_log(&self.jobs.0.db, self.queue_id, &[entry], now()).await;
        if hint {
            self.add_hidden_values_hint().await;
        }
    }

    /// The hint entry after the first line that hides an unlocked environment value.
    async fn add_hidden_values_hint(&self) {
        if self
            .hint_added
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let entry = LogEntry {
            command: None,
            output: HIDDEN_ENV_VALUES_HINT.into(),
            kind: "stdout",
            timestamp: timestamp(),
            hidden: false,
            batch: 1,
        };
        let _ = apps::append_log(&self.jobs.0.db, self.queue_id, &[entry], now()).await;
    }

    /// `EnvironmentVariable::logRedactionSecretsFor($application)`: values hidden in the log.
    async fn init_redaction(&mut self) {
        let (known, locked) = self
            .jobs
            .deployment_log_redaction_secrets(&self.app, self.main_server.id, self.b.is_preview())
            .await;
        self.known.extend(known);
        self.locked.extend(locked);
    }

    async fn info(&self, message: &str) {
        self.log(message, "stdout", false).await;
    }

    async fn status(&self) -> String {
        apps::deployment_by_id(&self.jobs.0.db, self.queue_id)
            .await
            .ok()
            .flatten()
            .map(|d| d.status)
            .unwrap_or_default()
    }

    /// `checkForCancellation()`.
    async fn check_cancelled(&self) -> Step {
        if self.status().await == "cancelled-by-user" {
            self.info("Deployment cancelled by user, stopping execution.")
                .await;
            return Err(Fail::Cancelled);
        }
        Ok(())
    }

    /// `instant_remote_process($commands, $server)`.
    async fn instant(&self, commands: &[&str], throw: bool) -> Result<Option<String>, Fail> {
        match self
            .jobs
            .remote()
            .run(&self.target, commands, RunOptions::op("deployment.instant"))
            .await
        {
            Ok(out) => Ok(out),
            Err(e) if throw => Err(Fail::Error {
                message: e.message,
                code: e.code.unwrap_or(1).into(),
                logged: false,
            }),
            Err(_) => Ok(None),
        }
    }

    /// `execute_remote_command(...$commands)`.
    async fn exec(&mut self, commands: Vec<Cmd>) -> Step {
        let batch = BATCH.fetch_add(1, Ordering::SeqCst) + 1;
        for cmd in commands {
            if self.status().await == "cancelled-by-user" {
                return Err(Fail::Cancelled);
            }
            let script = match &cmd.input {
                // Upstream pipes the input into ssh; here it travels inside the script.
                Some(input) => {
                    format!("echo '{}' | base64 -d | {}", B64.encode(input), cmd.command)
                }
                None => cmd.command.clone(),
            };
            if cmd.command_hidden && !cmd.skip_command_log {
                self.log(
                    &format!("[CMD]: {}", self.redact(&cmd.command)),
                    "stdout",
                    true,
                )
                .await;
            }
            // `$maxRetries` of `execute_remote_command`: a dropped SSH connection runs the command again.
            let mut attempt = 0u32;
            loop {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
                let db = self.jobs.0.db.clone();
                let queue_id = self.queue_id;
                let command_for_log = (!cmd.skip_command_log && !cmd.command_hidden)
                    .then(|| self.redact(&cmd.command));
                // `$message.' '.$command`: the command of the entry, which these two do not have.
                let raw_command = if cmd.skip_command_log || cmd.command_hidden {
                    String::new()
                } else {
                    cmd.command.clone()
                };
                let hint_added = self.hint_added.clone();
                let known = self.known.clone();
                let locked = self.locked.clone();
                let (hidden, kind) = (cmd.hidden, cmd.kind);
                let skip = cmd.skip_command_log;
                let writer = tokio::spawn(async move {
                    let mut chunks: Vec<String> = Vec::new();
                    let mut stdout = String::new();
                    while let Some((stream, chunk)) = rx.recv().await {
                        chunks.push(chunk.clone());
                        if stream == Stream::Out {
                            stdout.push_str(&chunk);
                        }
                        let k: Vec<&str> = known.iter().map(String::as_str).collect();
                        let l: Vec<&str> = locked.iter().map(String::as_str).collect();
                        let hint = !hint_added.load(std::sync::atomic::Ordering::SeqCst)
                            && contains_known_secret(
                                &format!("{} {raw_command}", chunk.trim()),
                                &known,
                                &locked,
                            );
                        let mut text = chunk.trim().to_owned();
                        if skip {
                            text = redact_base64_payloads(&text);
                        }
                        // `takeCompleteOutput()`: a non-empty chunk is logged even when it trims to nothing.
                        if chunk.is_empty() {
                            continue;
                        }
                        let mut output = coolify_compat::redact::redact_text(&text, &k, &l);
                        if output.starts_with('╔') {
                            output = format!("\n{output}");
                        }
                        let entry = LogEntry {
                            command: command_for_log.clone(),
                            output,
                            kind: kind.unwrap_or(if stream == Stream::Err {
                                "stderr"
                            } else {
                                "stdout"
                            }),
                            timestamp: timestamp(),
                            hidden,
                            batch,
                        };
                        let _ = apps::append_log(&db, queue_id, &[entry], now()).await;
                        if hint && !hint_added.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            let hint_entry = LogEntry {
                                command: None,
                                output: HIDDEN_ENV_VALUES_HINT.into(),
                                kind: "stdout",
                                timestamp: timestamp(),
                                hidden: false,
                                batch,
                            };
                            let _ = apps::append_log(&db, queue_id, &[hint_entry], now()).await;
                        }
                    }
                    (chunks, stdout)
                });
                let result = self
                    .jobs
                    .remote()
                    .run_streaming(
                        &self.target,
                        std::slice::from_ref(&script),
                        RunOptions::op("deployment.command").timeout(COMMAND_TIMEOUT),
                        tx,
                    )
                    .await;
                let (chunks, stdout) = writer.await.unwrap_or_default();
                if let Err(e) = &result
                    && !cmd.ignore_errors
                    && attempt + 1 < coolify_remote::retry::DEFAULT_MAX_ATTEMPTS
                    && coolify_remote::retry::is_retryable_failure(e.code, &e.message)
                {
                    // `addRetryLogEntry` and the cancellation check while waiting.
                    let (message, _) = self.command_failure(&cmd, e, &stdout);
                    let delay = coolify_remote::retry::retry_delay(attempt);
                    attempt += 1;
                    self.log(
                    &format!(
                        "SSH connection failed. Retrying... (Attempt {attempt}/{}, waiting {}s)\nError: {message}",
                        coolify_remote::retry::DEFAULT_MAX_ATTEMPTS,
                        delay.as_secs()
                    ),
                    "stdout",
                    false,
                )
                .await;
                    if self.status().await == "cancelled-by-user" {
                        return Err(Fail::Cancelled);
                    }
                    tokio::time::sleep(delay).await;
                    continue;
                }
                if let Some(key) = &cmd.save {
                    let value = if cmd.append {
                        chunks.concat()
                    } else {
                        chunks
                            .last()
                            .map(|c| c.trim().to_owned())
                            .unwrap_or_default()
                    };
                    if cmd.append {
                        self.saved.entry(key.clone()).or_default().push_str(&value);
                    } else {
                        self.saved.insert(key.clone(), value);
                    }
                }
                if let Err(e) = result
                    && !cmd.ignore_errors
                {
                    if self.status().await == "cancelled-by-user" {
                        return Err(Fail::Cancelled);
                    }
                    let (message, code) = self.command_failure(&cmd, &e, &stdout);
                    return Err(Fail::Error {
                        message,
                        code: code.into(),
                        logged: false,
                    });
                }
                break;
            }
        }
        Ok(())
    }

    /// The `DeploymentException` of a failed command: its message and exit code.
    fn command_failure(
        &self,
        cmd: &Cmd,
        e: &coolify_remote::RemoteError,
        stdout: &str,
    ) -> (String, i32) {
        let code = e.code.unwrap_or(1);
        let error = if e.message.starts_with("SSH command failed with exit code") {
            let out = stdout.trim();
            if out.is_empty() {
                "Command failed with no error output".to_owned()
            } else {
                out.to_owned()
            }
        } else {
            e.message.clone()
        };
        let shown = if cmd.skip_command_log {
            "[command hidden because it contains sensitive data]".to_owned()
        } else {
            self.redact(&cmd.command)
        };
        (
            format!(
                "Command execution failed (exit code {code}): {shown}\nError: {}",
                self.redact(&error)
            ),
            code,
        )
    }

    /// `dockerStopCommand($timeout, escapeshellarg($name), $server)`.
    fn stop_command(&self, name: &str) -> String {
        crate::databases::docker_stop_command(
            self.stop_timeout,
            &quote(name),
            self.server.docker_version(),
        )
    }

    /// `graceful_shutdown_container($name, $skipRemove)`.
    async fn graceful_shutdown(&mut self, name: &str, skip_remove: bool) {
        let stop = Cmd::new(self.stop_command(name)).hidden().ignore_errors();
        if let Err(Fail::Error { message, .. }) = self.exec(vec![stop]).await {
            self.log(
                &format!("Error stopping container {name}: {message}"),
                "stderr",
                false,
            )
            .await;
            return;
        }
        if !skip_remove {
            self.remove_container(name).await;
        }
    }

    /// `removeContainerWithTimeout($name)`.
    async fn remove_container(&mut self, name: &str) {
        let script = format!(
            "if command -v timeout >/dev/null 2>&1; then output=$(timeout -k 10s 60s docker rm -f {q} 2>&1); exit_code=$?; else output=''; exit_code=124; fi; if [ \"$exit_code\" -eq 124 ]; then echo '__COOLIFY_CONTAINER_REMOVE_TIMEOUT__'; elif [ \"$exit_code\" -ne 0 ] && printf '%s' \"$output\" | grep -q 'No such container:'; then exit 0; elif [ \"$exit_code\" -ne 0 ]; then printf '%s\\n' \"$output\" >&2; else printf '%s\\n' \"$output\"; fi; exit $exit_code",
            q = quote(name)
        );
        let key = format!("container_remove_{}", md5_hex(name));
        let cmd = Cmd::new(format!("bash -c {}", quote(&script)))
            .hidden()
            .ignore_errors()
            .save(key.clone(), false);
        if self.exec(vec![cmd]).await.is_ok()
            && self
                .saved
                .get(&key)
                .is_some_and(|o| o.contains("__COOLIFY_CONTAINER_REMOVE_TIMEOUT__"))
        {
            self.log(
                &format!("Warning: Removing container {name} timed out after 60 seconds. The deployment will continue and cleanup will be retried in 5 minutes."),
                "stderr",
                false,
            )
            .await;
        }
    }

    /// `getCurrentApplicationContainerStatus($server, $application, 0)`: containers that belong
    /// to the application (running and stopped).
    async fn current_containers(&self) -> Vec<Value> {
        let u = &self.app.uuid;
        let lines = [
            format!(
                "docker ps -a --filter 'label=coolify.applicationUuid={u}' --format '{{{{json .}}}}'"
            ),
            format!(
                "docker ps -a --filter 'label=coolify.applicationId' --filter 'label=com.docker.compose.project={u}' --format '{{{{json .}}}}'"
            ),
            format!(
                "docker ps -a --filter 'label=coolify.applicationId' --filter 'name={u}' --format '{{{{json .}}}}'"
            ),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let out = self
            .instant(&refs, false)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        let mut containers = Vec::new();
        for line in out.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let id = v
                .get("ID")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let labels = v.get("Labels").and_then(Value::as_str).unwrap_or_default();
            let base = !labels.contains("coolify.pullRequestId=")
                || labels.contains("coolify.pullRequestId=0,")
                || labels.ends_with("coolify.pullRequestId=0");
            let keep = if self.b.is_preview() {
                !base
                    && labels
                        .split(',')
                        .any(|l| l == format!("coolify.pullRequestId={}", self.b.pull_request_id))
            } else {
                base
            };
            if keep {
                containers.push(v);
            }
        }
        containers
    }
}

fn md5_hex(s: &str) -> String {
    use md5::Digest as _;
    md5::Md5::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `skip_command_log`: base64 payloads of the command that decode to text are not shown.
fn redact_base64_payloads(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"[A-Za-z0-9+/]{16,}={0,2}").unwrap_or_else(|_| unreachable!())
    });
    RE.replace_all(text, |c: &regex::Captures<'_>| {
        let m = &c[0];
        match B64.decode(m).ok().and_then(|b| String::from_utf8(b).ok()) {
            Some(t) if t.chars().all(|ch| !ch.is_control() || ch.is_whitespace()) => {
                "<REDACTED>".to_owned()
            }
            _ => m.to_owned(),
        }
    })
    .into_owned()
}

impl Jobs {
    /// Runs the queued deployment `queue_id` (the `ApplicationDeploymentJob` queue worker).
    pub async fn run_application_deployment(&self, queue_id: i64) {
        let Ok(Some(q)) = apps::deployment_by_id(&self.0.db, queue_id).await else {
            return;
        };
        let Ok(Some(app)) = apps::find_by_id(&self.0.db, q.application_id).await else {
            return;
        };
        let server_id = q.int("server_id").unwrap_or_default();
        let pull_request_id = q.int("pull_request_id").unwrap_or(0);
        let Ok(Some(server_row)) = servers::find_by_id(&self.0.db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&server_row);
        let server_name = server_row
            .record
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let target = match server.target(self).await {
            Ok(t) => Arc::new(t),
            Err(e) => {
                tracing::warn!(deployment = %q.uuid, error = %e, "deployment cannot reach its server");
                let _ = apps::transition_status(&self.0.db, queue_id, "failed", now()).await;
                return;
            }
        };
        // `$this->server->destinations()->where('id', $queue->destination_id)->first()`.
        let destination_network = apps::destination_network(
            &self.0.db,
            server_id,
            q.int("destination_id").unwrap_or_default(),
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| app.network().to_owned());
        let is_additional = coolify_db::resource_ops::additional_server_ids(&self.0.db, app.id)
            .await
            .unwrap_or_default()
            .contains(&server_id);
        let base = app
            .str("base_directory")
            .unwrap_or("/")
            .trim_end_matches('/')
            .to_owned();
        let grace = app
            .setting_int("stop_grace_period")
            .filter(|g| (1..=3600).contains(g))
            .unwrap_or(30);
        let created = q
            .str("created_at")
            .and_then(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f").ok())
            .unwrap_or_else(now);
        let restart_only = q.bool("restart_only")
            && !matches!(
                app.str("build_pack"),
                Some("dockerimage" | "dockerfile" | "dockercompose")
            );
        let disable_build_cache = app.setting_bool("disable_build_cache");
        let b = BuildState {
            restart_only,
            rollback: q.bool("rollback"),
            force_rebuild: q.bool("force_rebuild") || disable_build_cache,
            disable_build_cache,
            dockerfile_location: "/Dockerfile".into(),
            compose_location: "/docker-compose.yaml".into(),
            pull_request_id,
            git_type: q.str("git_type").map(str::to_owned),
            preview_tag: q.str("docker_registry_image_tag").map(str::to_owned),
            ..Default::default()
        };
        let mut run = Run {
            jobs: self.clone(),
            queue_id,
            dep: q.uuid.clone(),
            container: if pull_request_id == 0 {
                compose::container_name(&app, created)
            } else {
                compose::preview_container_name(&app, pull_request_id)
            },
            commit: q.str("commit").unwrap_or("HEAD").to_owned(),
            workdir: format!("/artifacts/{}{base}", q.uuid),
            cfg: format!("/data/coolify/applications/{}", app.uuid),
            stop_timeout: u32::try_from(grace).unwrap_or(30),
            network: destination_network,
            destination_id: q.int("destination_id").unwrap_or_default(),
            is_additional,
            only_this_server: q.bool("only_this_server"),
            parent_deployment_uuid: q
                .str("parent_deployment_uuid")
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
            app,
            main_server: server.clone(),
            main_name: server_name.clone(),
            main_target: target.clone(),
            build_server: None,
            use_build_server: false,
            server,
            server_name,
            target,
            known: Vec::new(),
            locked: Vec::new(),
            saved: HashMap::new(),
            generated_compose: None,
            healthy: false,
            b,
            hint_added: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        run.init_redaction().await;
        if pull_request_id != 0
            && let Err(e) = run.load_preview().await
        {
            run.failed(&e).await;
            run.finally().await;
            return;
        }

        // handle()
        if q.status == "cancelled-by-user" {
            run.info("Deployment was cancelled before starting.").await;
            return;
        }
        if let Err(e) = run.check_multiple_servers().await {
            run.failed(&e).await;
            return;
        }
        let host = std::env::var("HOSTNAME")
            .ok()
            .or_else(hostname)
            .unwrap_or_default();
        let _ = apps::update_deployment(
            &self.0.db,
            queue_id,
            &[
                ("status", ColumnValue::Text(Some("in_progress".into()))),
                ("horizon_job_worker", ColumnValue::Text(Some(host))),
            ],
            now(),
        )
        .await;
        if !run.server.is_functional() {
            run.info("Server is not functional.").await;
            run.failed(&Fail::new("Server is not functional.")).await;
            return;
        }
        run.update_pull_request(crate::applications::previews::PullRequestStatus::InProgress);
        let result = Box::pin(run.deploy()).await;
        if let Err(e) = &result {
            if matches!(e, Fail::Error { .. }) {
                run.update_pull_request(crate::applications::previews::PullRequestStatus::Error);
            }
            run.failed(e).await;
        }
        run.finally().await;
    }
}

impl Jobs {
    /// `app:init`: deployments interrupted by a restart are marked failed. Upstream fails every
    /// in-progress and queued row; coolify-rs only fails the rows this host was running, so a
    /// second instance on the same database keeps its own deployments.
    pub async fn fail_interrupted_deployments(&self) -> usize {
        let host = std::env::var("HOSTNAME")
            .ok()
            .or_else(hostname)
            .unwrap_or_default();
        let count = coolify_db::git_sources::fail_in_progress_for_worker(&self.0.db, &host, now())
            .await
            .unwrap_or(0);
        if count > 0 {
            tracing::info!(count, "marked stuck deployments as failed");
        }
        count
    }
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
}

impl Jobs {
    /// `EnvironmentVariable::logRedactionSecretsFor($application, includePreview: ...)`:
    /// the known and the locked values, without remote secrets (not ported).
    pub(crate) async fn log_redaction_secrets(
        &self,
        app: &ApplicationRow,
        server_id: i64,
        include_preview: bool,
    ) -> (Vec<String>, Vec<String>) {
        self.application_log_redaction_secrets(app, server_id, include_preview, true)
            .await
    }

    /// `logRedactionSecretsFor(..., resolveRemoteSecrets: false)`: a deployment registers the
    /// secrets it fetches itself (`redactRemoteSecrets()`).
    async fn deployment_log_redaction_secrets(
        &self,
        app: &ApplicationRow,
        server_id: i64,
        include_preview: bool,
    ) -> (Vec<String>, Vec<String>) {
        self.application_log_redaction_secrets(app, server_id, include_preview, false)
            .await
    }

    async fn application_log_redaction_secrets(
        &self,
        app: &ApplicationRow,
        server_id: i64,
        include_preview: bool,
        resolve_remote: bool,
    ) -> (Vec<String>, Vec<String>) {
        let owner = Owner {
            morph: apps::MORPH,
            id: app.id,
            team_id: app.team_id,
            project_id: app.project.get("id").and_then(Value::as_i64),
            environment_id: app.environment.get("id").and_then(Value::as_i64),
        };
        let redact_all = app
            .settings
            .get("redact_all_env_values_in_logs")
            .and_then(Value::as_bool)
            != Some(false);
        self.owner_log_redaction_secrets(
            &owner,
            redact_all,
            Some(server_id),
            include_preview,
            resolve_remote,
        )
        .await
    }

    /// `logRedactionSecretsFor($service)`.
    pub async fn service_log_redaction_secrets(
        &self,
        service_id: i64,
        team_id: i64,
        server_id: i64,
    ) -> (Vec<String>, Vec<String>) {
        let Ok(Some((environment_id, project_id, redact_all))) =
            coolify_db::scheduled_tasks::service_log_context(&self.0.db, service_id).await
        else {
            return (Vec::new(), Vec::new());
        };
        let owner = Owner {
            morph: "App\\Models\\Service",
            id: service_id,
            team_id,
            project_id,
            environment_id,
        };
        self.owner_log_redaction_secrets(&owner, redact_all, Some(server_id), false, true)
            .await
    }

    /// The loop of `logRedactionSecretsFor()` with `remoteSecretLogRedactionValuesFor()`. A failed
    /// fetch is not repeated, and the reference text is hidden in its place.
    pub(crate) async fn owner_log_redaction_secrets(
        &self,
        owner: &Owner<'_>,
        redact_all: bool,
        server_id: Option<i64>,
        include_preview: bool,
        resolve_remote: bool,
    ) -> (Vec<String>, Vec<String>) {
        let (mut known, mut locked) = (Vec::new(), Vec::new());
        let Some(enc) = self.0.encrypter.clone() else {
            return (known, locked);
        };
        let Ok(variables) = env::load_raw_for(&self.0.db, &enc, owner, server_id).await else {
            return (known, locked);
        };
        let db = &self.0.db;
        let mut link = None;
        let mut fetched: Option<secret_managers::Secrets> = None;
        let mut failed = false;
        let mut unresolved: Vec<&str> = Vec::new();
        if resolve_remote
            && variables
                .iter()
                .any(|v| secret_managers::contains_reference(v.resolved.as_deref()))
        {
            link = secret_managers::link_of(db, owner.morph, owner.id)
                .await
                .ok()
                .flatten();
        }
        for v in variables
            .iter()
            .filter(|v| include_preview || !v.is_preview)
        {
            let value = v.resolved.as_deref().unwrap_or_default();
            if resolve_remote
                && secret_managers::contains_reference(Some(value))
                && (!v.is_literal || link.is_some())
            {
                if fetched.is_none() && !failed {
                    match &link {
                        Some(l) => match l.fetch(db, &enc, owner.team_id, "").await {
                            Ok(s) => fetched = Some(s),
                            Err(_) => failed = true,
                        },
                        None => failed = true,
                    }
                }
                match &fetched {
                    Some(s) if secret_managers::missing_keys(value, s).is_empty() => {
                        let resolved = secret_managers::substitute(value, s);
                        if !resolved.is_empty() {
                            locked.extend(remote_redaction_values(&resolved));
                        }
                    }
                    other => {
                        if let Some(s) = other {
                            for key in secret_managers::referenced_keys(value) {
                                if let Some(x) = s.get(&key).filter(|x| !x.is_empty()) {
                                    locked.extend(remote_redaction_values(x));
                                }
                            }
                        }
                        unresolved.push(&v.key);
                        locked.extend(log_redaction_values(v));
                    }
                }
            }
            if v.is_shown_once {
                locked.extend(log_redaction_values(v));
            } else if redact_all {
                known.extend(log_redaction_values(v));
            }
        }
        if !unresolved.is_empty() {
            tracing::warn!(
                variables = ?unresolved,
                "Could not resolve remote secrets for log redaction; their resolved values are not hidden."
            );
        }
        (known, locked)
    }
}

impl Run {
    async fn deploy(&mut self) -> Step {
        let net = quote(&self.network);
        let containers = self
            .instant(
                &[&format!(
                    "docker network inspect {net} -f '{{{{json .Containers}}}}' "
                )],
                true,
            )
            .await?;
        if let Some(out) = containers {
            self.b.add_hosts = self.add_host_flags(&out).await;
        }
        if let Some(target) = self
            .app
            .str("dockerfile_target_build")
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
        {
            static TARGET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$")
                    .unwrap_or_else(|_| unreachable!())
            });
            if !TARGET.is_match(&target) {
                return Err(Fail::new(
                    "Invalid dockerfile_target_build: contains forbidden characters.",
                ));
            }
            self.b.build_target = Some(format!(" --target {target} "));
        }
        // customRepository()
        self.load_source().await?;
        self.select_build_server().await?;
        self.detect_buildkit().await;
        self.decide_what_to_do().await
    }

    /// `addHostFlags(collect($allContainers[0]))`.
    async fn add_host_flags(&self, output: &str) -> String {
        let Some(first) = output
            .split('\n')
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .and_then(|v| v.into_iter().next())
        else {
            return String::new();
        };
        let Value::Object(map) = first else {
            return String::new();
        };
        let mut containers: Vec<Value> = map.into_iter().map(|(_, v)| v).collect();
        let field = |c: &Value, k: &str| {
            c.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        containers.sort_by(|a, b| {
            (field(a, "Name"), field(a, "EndpointID"))
                .cmp(&(field(b, "Name"), field(b, "EndpointID")))
        });
        let names: Vec<String> = containers
            .iter()
            .map(|c| field(c, "Name"))
            .filter(|n| !n.is_empty())
            .collect();
        let helpers = coolify_db::git_sources::existing_deployment_uuids(&self.jobs.0.db, &names)
            .await
            .unwrap_or_default();
        static GENERATED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(-\d{8}T\d{6}|-\d{12})$").unwrap_or_else(|_| unreachable!())
        });
        let mut ips: Vec<(String, String)> = Vec::new();
        for c in &containers {
            let name = field(c, "Name");
            if name == "coolify-proxy" || GENERATED.is_match(&name) || helpers.contains(&name) {
                continue;
            }
            let ip = field(c, "IPv4Address");
            if !name.is_empty() && !ip.is_empty() {
                let ip = ip.split('/').next().unwrap_or_default().to_owned();
                match ips.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = ip,
                    None => ips.push((name, ip)),
                }
            }
        }
        ips.iter()
            .map(|(name, ip)| format!("--add-host {name}:{ip}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `detectBuildKitCapabilities()`.
    async fn detect_buildkit(&mut self) {
        // `$serverToCheck`: the helper has not moved to the build server yet.
        let restore = self.use_build_server && !self.on_build_server();
        if restore {
            self.switch_to_build_server();
        }
        self.detect_buildkit_here().await;
        if restore {
            self.switch_to_main_server();
        }
    }

    async fn detect_buildkit_here(&mut self) {
        let kind = if self.use_build_server {
            "build server"
        } else {
            "deployment server"
        };
        let name = format!("{kind} ({})", self.server_name);
        let version = match self
            .instant(&["docker version --format '{{.Server.Version}}'"], true)
            .await
        {
            Ok(v) => v.unwrap_or_default(),
            Err(Fail::Error { message, .. }) => {
                self.info(&format!(
                    "Could not detect BuildKit capabilities on {name}: {message}"
                ))
                .await;
                return;
            }
            Err(Fail::Cancelled) => return,
        };
        let parsed = crate::checks::parse_docker_engine_version(Some(&version));
        let n = now();
        let _ = servers::update_settings(
            &self.jobs.0.db,
            self.server.id,
            &[
                ("docker_version", ColumnValue::Text(parsed.clone())),
                ("docker_version_checked_at", ColumnValue::Time(n)),
            ],
            n,
        )
        .await;
        if let Some(p) = &parsed {
            self.server
                .settings
                .insert("docker_version".into(), Value::String(p.clone()));
        }
        let mut parts = version.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
        let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        if major < 18 || (major == 18 && minor < 9) {
            self.info(&format!(
                "Docker {version} on {name} does not support BuildKit (requires 18.09+)."
            ))
            .await;
            return;
        }
        let buildx = self
            .instant(&["docker buildx version >/dev/null 2>&1 && echo 'available' || echo 'not-available'"], true)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if buildx == "available" {
            self.b.buildkit = true;
            self.b.buildx = true;
            self.info(&format!(
                "Docker {version} with BuildKit and Buildx detected on {name}."
            ))
            .await;
        } else {
            let progress = self
                .instant(
                    &["DOCKER_BUILDKIT=1 docker build --help 2>&1 | grep -q '\\-\\-progress' && echo 'supported' || echo 'not-supported'"],
                    true,
                )
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            if progress == "supported" {
                self.b.buildkit = true;
                self.info(&format!(
                    "Docker {version} with BuildKit support detected on {name}."
                ))
                .await;
            } else {
                self.info(&format!(
                    "Docker {version} on {name} does not support BuildKit. Build output progress will be limited."
                ))
                .await;
            }
        }
        if self.app.setting_bool("use_build_secrets") && self.b.buildkit {
            let secrets = self
                .instant(
                    &["docker build --help 2>&1 | grep -q 'secret' && echo 'supported' || echo 'not-supported'"],
                    true,
                )
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            if secrets == "supported" {
                self.b.secrets_supported = true;
                self.info("Build secrets are enabled and will be used for enhanced security.")
                    .await;
            } else {
                self.info(&format!(
                    "Docker on {name} does not support build secrets. Using traditional build arguments."
                ))
                .await;
            }
        }
    }

    /// `deploy_dockerimage_buildpack()`.
    async fn deploy_docker_image(&mut self) -> Step {
        let image_name = self
            .app
            .str("docker_registry_image_name")
            .unwrap_or_default()
            .to_owned();
        // resolveDockerImageTag(): a rollback runs the tag in `commit`.
        let preview_tag = self.b.preview_tag.clone().filter(|t| !t.is_empty());
        let tag = if let Some(tag) = preview_tag.filter(|_| self.b.is_preview()) {
            tag
        } else if self.b.rollback && !self.commit.trim().is_empty() {
            self.commit.clone()
        } else {
            self.app
                .str("docker_registry_image_tag")
                .filter(|t| !t.is_empty())
                .unwrap_or("latest")
                .to_owned()
        };
        let (display, image) = match tag.strip_prefix("sha256-") {
            Some(hash) => (
                format!("{image_name}@sha256:{hash}"),
                format!("{image_name}@sha256:{hash}"),
            ),
            None => (format!("{image_name}:{tag}"), format!("{image_name}:{tag}")),
        };
        // generate_image_names(): a leftover inline Dockerfile wins.
        let image = if self.app.str("dockerfile").is_some_and(|d| !d.is_empty()) {
            format!(
                "{}:latest",
                self.app
                    .str("docker_registry_image_name")
                    .filter(|n| !n.is_empty())
                    .unwrap_or(&self.app.uuid)
            )
        } else {
            image
        };
        self.info(&format!(
            "Starting deployment of {display} to {}.",
            self.server_name
        ))
        .await;
        self.prepare_builder_image().await?;
        self.generate_compose_file(&image).await?;
        self.save_runtime_environment().await?;
        self.rolling_update().await
    }

    /// `prepare_builder_image()`.
    async fn prepare_builder_image(&mut self) -> Step {
        self.prepare_builder_image_again(true).await
    }

    /// `prepare_builder_image($firstTry)`.
    async fn prepare_builder_image_again(&mut self, first_try: bool) -> Step {
        self.check_cancelled().await?;
        let settings = coolify_db::instance::deployment_settings(&self.jobs.0.db)
            .await
            .unwrap_or_default();
        let registry = settings
            .docker_registry_url
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "docker.io".into());
        let version = match settings.helper_version.filter(|v| !v.is_empty()) {
            Some(v) if version_gt(&v, HELPER_VERSION) => v,
            _ => HELPER_VERSION.to_owned(),
        };
        let helper = format!("{registry}/coollabsio/coolify-helper:{version}");
        let home = self
            .instant(&["echo $HOME"], true)
            .await?
            .unwrap_or_default();
        self.instant(&[&format!("mkdir -p {home}/.docker/buildx")], true)
            .await?;
        let config = self
            .instant(
                &[&format!(
                    "test -f {home}/.docker/config.json && echo 'OK' || echo 'NOK'"
                )],
                true,
            )
            .await?
            .unwrap_or_default();
        let env_flags = self.generate_docker_env_flags_for_secrets().await?;
        let buildx = format!("-v {home}/.docker/buildx:/root/.docker/buildx");
        let net = quote(&self.network);
        let dep = &self.dep;
        // seccompProfileMountFlags(): docker compose runs in this helper and reads the profiles.
        let seccomp = if self.b.is_preview() {
            String::new()
        } else {
            docker_run::seccomp_profile_mount_flags(self.app.str("custom_docker_run_options"))
        };
        let run_command = if self.use_build_server {
            if config == "NOK" {
                return Err(Fail::new(format!(
                    "Docker config file (~/.docker/config.json) of the SSH user ({}) not found on the build server ({}). Log in to your registry under Server \u{2192} Docker Registries on the build server. On a non-root server, a login made with \"sudo docker login\" is saved for root and is not used for builds.",
                    self.server.user, self.server_name
                )));
            }
            format!(
                "docker run -d --name {dep} {env_flags} --rm -v {home}/.docker/config.json:/root/.docker/config.json:ro {buildx} -v /var/run/docker.sock:/var/run/docker.sock {helper}"
            )
        } else if config == "OK" {
            format!(
                "docker run -d --network {net} --name {dep} {env_flags} --rm -v {home}/.docker/config.json:/root/.docker/config.json:ro {buildx} -v /var/run/docker.sock:/var/run/docker.sock {seccomp} {helper}"
            )
        } else {
            format!(
                "docker run -d --network {net} --name {dep} {env_flags} --rm {buildx} -v /var/run/docker.sock:/var/run/docker.sock {seccomp} {helper}"
            )
        };
        if first_try {
            self.info(&format!("Preparing container with helper image: {helper}"))
                .await;
        } else {
            self.info("Preparing container with helper image with updated envs.")
                .await;
        }
        let dep = self.dep.clone();
        self.graceful_shutdown(&dep, true).await;
        let mkdir = in_docker(&dep, &format!("mkdir -p /artifacts/{dep}"));
        let mut run = Cmd::new(run_command).hidden();
        if !env_flags.is_empty() {
            run = run.skip_command_log();
        }
        self.exec(vec![run, Cmd::new(mkdir)]).await?;
        self.run_pre_deployment_command().await
    }

    /// `generateLabelsApplication($this->application, $this->preview)`.
    fn generated_labels(&self, server: &servers::ServerRow) -> Result<Vec<String>, Fail> {
        let enc = self
            .jobs
            .0
            .encrypter
            .as_ref()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        let mut input = LabelInput::new(&self.app, server)
            .with_basic_auth(&self.app, server, enc)
            .map_err(Fail::new)?;
        if self.b.is_preview() {
            let overrides = self
                .b
                .preview
                .as_ref()
                .and_then(|p| p.get("domain_port_overrides"))
                .and_then(|v| match v {
                    Value::String(s) => serde_json::from_str::<Value>(s).ok(),
                    other => Some(other.clone()),
                })
                .and_then(|v| v.as_object().cloned())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| {
                            v.as_i64()
                                .or_else(|| v.as_str()?.parse().ok())
                                .map(|p| (k.clone(), p))
                        })
                        .collect()
                })
                .unwrap_or_default();
            input = input.for_preview(
                self.b.pull_request_id,
                self.deployed_fqdn().as_deref(),
                overrides,
            );
        }
        Ok(labels::application(&input))
    }

    /// `generate_compose_file()`.
    async fn generate_compose_file(&mut self, image: &str) -> Step {
        self.check_cancelled().await?;
        self.create_workdir().await?;
        let db = self.jobs.0.db.clone();
        let server_row = servers::find_by_id(&db, self.main_server.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
            .ok_or_else(|| Fail::new("Server not found."))?;
        let mut labels: Vec<String> = match compose::stored_labels(self.app.str("custom_labels")) {
            Some(text) => {
                let text = match text {
                    Ok(t) => t,
                    Err(()) => self.generated_labels(&server_row)?.join("\n"),
                };
                // parseContainerLabels() saves the base64 form; `coolify.` lines are dropped.
                let kept: Vec<String> = text
                    .split("\r\n")
                    .flat_map(|l| l.split(['\n', '\r']))
                    .filter(|l| !l.starts_with("coolify."))
                    .map(str::to_owned)
                    .collect();
                let stored = B64.encode(kept.join("\n"));
                let _ = apps::update_if_changed(
                    &db,
                    self.app.id,
                    &[("custom_labels", ColumnValue::Text(Some(stored.clone())))],
                    now(),
                )
                .await;
                self.app
                    .raw
                    .insert("custom_labels".into(), Value::String(stored));
                kept
            }
            None if self.app.setting_bool("is_container_label_readonly_enabled") => {
                self.generated_labels(&server_row)?
            }
            None if self.b.is_preview() => Vec::new(),
            None => return Err(Fail::new("Undefined variable $labels")),
        };
        if self.b.is_preview() {
            labels = self.generated_labels(&server_row)?;
        }
        // useDestinationNetworkInCaddyLabels()
        let primary = format!("caddy_ingress_network={}", self.app.network());
        labels = labels
            .into_iter()
            .map(|l| {
                if l == primary {
                    format!("caddy_ingress_network={}", self.network)
                } else {
                    l
                }
            })
            .collect();
        // traefikHostRulesWithoutIpv6Brackets
        labels = labels
            .into_iter()
            .map(|l| strip_ipv6_brackets(&l))
            .collect();
        if self
            .app
            .settings
            .get("is_container_label_escape_enabled")
            .and_then(Value::as_bool)
            != Some(false)
        {
            labels = labels.into_iter().map(|l| l.replace('$', "$$")).collect();
        }
        let version = self.jobs.version();
        labels.extend(labels::default_labels(
            &self.app.uuid,
            self.app
                .project
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            self.app.str("name").unwrap_or_default(),
            self.app
                .environment
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &version,
            self.b.pull_request_id,
        ));
        // Custom HEALTHCHECK of the repository's Dockerfile.
        if self.app.str("build_pack") == Some("dockerfile")
            || self.app.str("dockerfile").is_some_and(|d| !d.is_empty())
        {
            self.read_dockerfile_healthcheck().await?;
        }
        let mut volumes = apps::persistent_volumes(&db, self.app.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        if self.b.is_preview() {
            // generate_local_persistent_volumes(): the preview suffix on the name or the host path.
            let suffixes = apps::preview_suffix_volumes(&db, self.app.id)
                .await
                .map_err(|e| Fail::new(e.to_string()))?;
            let pr = self.b.pull_request_id;
            for (name, _, host) in &mut volumes {
                let enabled = suffixes
                    .iter()
                    .find(|(n, _)| n == name)
                    .is_none_or(|(_, on)| *on);
                if enabled {
                    *name = crate::compose_parser::volumes::add_preview_deployment_suffix(name, pr);
                    if let Some(h) = host.as_mut().filter(|h| !h.is_empty()) {
                        *h = crate::compose_parser::volumes::add_preview_deployment_suffix(h, pr);
                    }
                }
            }
        }
        // `$persistent_file_volumes`: every file storage is mounted.
        let file_volumes: Vec<String> = apps::file_volume_rows(&db, self.app.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
            .into_iter()
            .map(|(_, fs_path, mount_path, _)| {
                format!("{fs_path}:{}", mount_path.unwrap_or_default())
            })
            .collect();
        let network = self.network.clone();
        let options = self.app.str("custom_docker_run_options").map(str::to_owned);
        let mut custom = docker_run::convert_docker_run_to_compose(options.as_deref());
        if self.app.setting_bool("is_gpu_enabled") {
            let options = self.app.setting_str("gpu_options").map(str::to_owned);
            if docker_run::parse_gpu_options(options.as_deref()).is_none() {
                self.info(
                    "GPU options are not a valid key=value list or YAML mapping. They are ignored.",
                )
                .await;
            }
            let forget = custom
                .get_mut("deploy")
                .and_then(|d| d.get_mut("resources"))
                .and_then(|r| r.get_mut("reservations"));
            if let Some(reservations) = forget {
                reservations.remove("devices");
                self.info("GPU access is enabled in the GPU settings, so --gpus in Custom Docker Options is ignored.")
                    .await;
            }
        }
        let unsupported = docker_run::unsupported_docker_run_options(options.as_deref());
        if !unsupported.is_empty() {
            self.log(
                &format!(
                    "Warning: These custom Docker options are not supported and were not applied: {}. See https://coolify.io/docs/applications/builds/custom-docker-options for the supported options.",
                    unsupported.join(", ")
                ),
                "stderr",
                false,
            )
            .await;
        }
        let yaml = compose::compose(&compose::ComposeInput {
            a: &self.app,
            container_name: &self.container,
            image,
            network: &network,
            labels,
            volumes: &volumes,
            file_volumes: &file_volumes,
            custom,
            swarm: self.main_server.is_swarm(),
            preview: self.b.is_preview(),
        });
        self.generated_compose = Some(yaml);
        Ok(())
    }

    /// `generate_runtime_environment_variables()`: the runtime `.env` lines and a warning for the
    /// deployment log. Uses (and clears) the Docker Compose part in `self.b.compose_env`.
    pub(super) async fn runtime_environment_lines(
        &mut self,
    ) -> Result<(Vec<String>, Option<String>), Fail> {
        // Server variables come from the server the application runs on, not its build server.
        let mut variables: Vec<Var> = if self.main_server.id == self.server.id {
            self.variables().await?
        } else {
            let enc =
                self.jobs.0.encrypter.as_ref().ok_or_else(|| {
                    Fail::new("No application encryption key has been specified.")
                })?;
            let owner = Owner {
                morph: apps::MORPH,
                id: self.app.id,
                team_id: self.app.team_id,
                project_id: self.app.project.get("id").and_then(Value::as_i64),
                environment_id: self.app.environment.get("id").and_then(Value::as_i64),
            };
            let mut vars =
                env::load_raw_for(&self.jobs.0.db, enc, &owner, Some(self.main_server.id))
                    .await
                    .map_err(Fail::new)?;
            self.resolve_remote_secrets(&mut vars).await?;
            vars
        };
        if self.b.is_preview() {
            variables = self.deployed_variables().await?;
        }
        let deployed_fqdn = self.deployed_fqdn();
        let project = self
            .app
            .project
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let environment = self
            .app
            .environment
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let ip = self.main_server.ip.clone();
        let (envs, warning) = compose::runtime_env(
            &self.app,
            &variables,
            &compose::EnvContext {
                commit: &self.commit,
                branch: Some(self.b.branch.as_str()).filter(|b| !b.is_empty()),
                compose: self.b.compose_env.take(),
                container_name: &self.container,
                project_name: project.as_deref(),
                environment_name: environment.as_deref(),
                server_ip: Some(&ip),
                preview: self.b.is_preview().then_some(deployed_fqdn.as_deref()),
            },
        );
        Ok((envs, warning))
    }

    /// `save_runtime_environment_variables()`.
    async fn save_runtime_environment(&mut self) -> Step {
        let (envs, warning) = self.runtime_environment_lines().await?;
        if let Some(w) = warning {
            self.log(&w, "stderr", false).await;
        }
        self.log(
            &format!("Creating {ENV_FILE} file with runtime variables for container."),
            "stdout",
            true,
        )
        .await;
        let b64 = B64.encode(envs.join("\n"));
        let w = &self.workdir;
        let write_work = in_docker(
            &self.dep,
            &format!(
                "rm -f {w}/{ENV_FILE} && echo '{b64}' | base64 -d | tee {w}/{ENV_FILE} > /dev/null"
            ),
        );
        self.exec(vec![Cmd::new(write_work).skip_command_log()])
            .await?;
        let c = self.cfg.clone();
        let build = self.on_build_server();
        if build {
            self.switch_to_main_server();
        }
        let written = self
            .exec(vec![
                Cmd::new(format!("rm -f {c}/{ENV_FILE}")).hidden(),
                Cmd::new(format!(
                    "echo '{b64}' | base64 -d | tee {c}/{ENV_FILE} > /dev/null"
                ))
                .skip_command_log(),
            ])
            .await;
        if build {
            self.switch_to_build_server();
        }
        written
    }

    /// `rolling_update()`.
    async fn rolling_update(&mut self) -> Step {
        let inner = async {
            self.check_cancelled().await?;
            // write_compose_file_to_workdir()
            let w = self.workdir.clone();
            let dep = self.dep.clone();
            let yaml = self.generated_compose.clone().unwrap_or_default();
            self.exec(vec![
                Cmd::new(format!("docker exec {dep} rm -f {w}/docker-compose.yaml")).hidden(),
                Cmd::new(format!(
                    "docker exec -i {dep} tee {w}/docker-compose.yaml > /dev/null"
                ))
                .input(yaml)
                .hidden(),
            ])
            .await?;
            if self.server.is_swarm() {
                self.info("Rolling update started.").await;
                let u = self.app.uuid.clone();
                self.exec(vec![Cmd::new(in_docker(
                    &dep,
                    &format!(
                        "docker stack deploy --detach=true --with-registry-auth -c {w}{} {u}",
                        self.b.compose_location
                    ),
                ))])
                .await?;
                self.info("Rolling update completed.").await;
                return Ok(());
            }
            if self.use_build_server {
                self.write_deployment_configurations().await?;
                self.switch_to_main_server();
            }
            let options = self
                .app
                .str("custom_docker_run_options")
                .unwrap_or_default()
                .to_owned();
            let mapped = self
                .app
                .str("ports_mappings")
                .is_some_and(|p| !p.is_empty());
            let consistent = self
                .app
                .setting_bool("is_consistent_container_name_enabled");
            let preview = self.b.is_preview();
            if mapped || consistent || preview || options.contains("--ip") {
                self.info(SEPARATOR).await;
                if mapped {
                    self.info("Application has ports mapped to the host system, rolling update is not supported.").await;
                }
                if consistent {
                    self.info("Consistent container name feature enabled, rolling update is not supported.").await;
                }
                if consistent
                    && self
                        .app
                        .setting_str("custom_internal_name")
                        .is_some_and(|n| !n.is_empty())
                {
                    self.info("Custom internal name is set, rolling update is not supported.")
                        .await;
                }
                if preview {
                    self.app.settings.insert(
                        "is_consistent_container_name_enabled".into(),
                        Value::Bool(true),
                    );
                    self.info("Pull request deployment, rolling update is not supported.")
                        .await;
                }
                if options.contains("--ip") {
                    self.info("Custom IP address is set, rolling update is not supported.")
                        .await;
                }
                self.stop_running_container(true).await?;
                self.start_by_compose_file().await
            } else {
                self.info(SEPARATOR).await;
                self.info("Rolling update started.").await;
                self.start_by_compose_file().await?;
                self.health_check().await?;
                self.wait_for_proxy().await;
                self.stop_running_container(false).await?;
                self.info("Rolling update completed.").await;
                Ok(())
            }
        };
        match inner.await {
            Err(Fail::Error {
                message,
                code,
                logged,
            }) => Err(Fail::Error {
                message: format!(
                    "Rolling update failed (App\\Exceptions\\DeploymentException): {message}"
                ),
                code,
                logged,
            }),
            other => other,
        }
    }

    /// `write_missing_content_files()`.
    pub(super) async fn write_missing_content_files(&mut self) -> Step {
        if self.app.setting_bool("is_preserve_repository_enabled") {
            return Ok(());
        }
        let (files, owner) = self.file_storages().await?;
        let lines = self
            .jobs
            .ensure_content_files_on_server(&owner, &files, self.main_server.id)
            .await
            .map_err(Fail::new)?;
        for (line, kind) in lines {
            self.log(&line, kind, false).await;
        }
        Ok(())
    }

    /// `start_by_compose_file()`.
    async fn start_by_compose_file(&mut self) -> Step {
        self.write_missing_content_files().await?;
        let net = quote(&self.network);
        let c = self.cfg.clone();
        let started = async {
            self.exec(vec![
                Cmd::new(format!("touch {c}/{ENV_FILE}")).hidden(),
                Cmd::new(format!(
                    "docker network connect {net} coolify-proxy >/dev/null 2>&1 || true"
                ))
                .hidden()
                .ignore_errors(),
            ])
            .await?;
            let u = self.app.uuid.clone();
            let w = self.workdir.clone();
            let dep = self.dep.clone();
            let vars = self.b.coolify_variables.clone();
            if self.app.str("build_pack") == Some("dockerimage") {
                self.info("Pulling latest images from the registry.").await;
                self.exec(vec![
                    Cmd::new(in_docker(
                        &dep,
                        &format!("docker compose --project-name {u} --project-directory {w} pull"),
                    ))
                    .hidden(),
                    // `{$this->coolify_variables}` is empty here: the command starts with a space.
                    Cmd::new(in_docker(
                        &dep,
                        &format!(
                            "{vars} docker compose --project-name {u} --project-directory {w} up --build -d"
                        ),
                    ))
                    .hidden(),
                ])
                .await?;
            } else if self.use_build_server {
                let loc = self.b.compose_location.clone();
                let prefix = servers_multi::docker_config_prefix(&self.server);
                self.exec(vec![
                    Cmd::new(format!(
                        "{prefix}{vars} docker compose --project-name {u} --project-directory {c} -f {c}{loc} up --pull always --build -d"
                    ))
                    .hidden(),
                ])
                .await?;
            } else {
                let loc = self.b.compose_location.clone();
                self.exec(vec![
                    Cmd::new(in_docker(
                        &dep,
                        &format!(
                            "{vars} docker compose --project-name {u} --project-directory {w} -f {w}{loc} up --build -d"
                        ),
                    ))
                    .hidden(),
                ])
                .await?;
            }
            self.info("New container started.").await;
            Ok(())
        };
        match started.await {
            Err(Fail::Error {
                message,
                code,
                logged,
            }) => Err(Fail::Error {
                message: format!("Failed to start container: {message}"),
                code,
                logged,
            }),
            other => other,
        }
    }

    /// `health_check()`.
    async fn health_check(&mut self) -> Step {
        if self.server.is_swarm() {
            // Implement healthcheck for swarm
            return Ok(());
        }
        let enabled = self
            .app
            .raw
            .get("health_check_enabled")
            .and_then(Value::as_bool)
            != Some(false);
        let custom = self.app.bool("custom_healthcheck_found");
        if !enabled && !custom {
            self.healthy = true;
            return Ok(());
        }
        if !enabled {
            self.info("Using the custom healthcheck from the Dockerfile.")
                .await;
        }
        self.info("Waiting for healthcheck to pass on the new container.")
            .await;
        if let Some((_, full)) = compose::healthcheck_command(&self.app).filter(|_| enabled) {
            let what = if self.app.str("health_check_type") == Some("cmd") {
                "command"
            } else {
                "URL"
            };
            self.info(&format!(
                "Healthcheck {what} (inside the container): {full}"
            ))
            .await;
        }
        let start_period = self.app.int("health_check_start_period").unwrap_or(5);
        let interval = self.app.int("health_check_interval").unwrap_or(5);
        let retries = self.app.int("health_check_retries").unwrap_or(10);
        self.info(&format!(
            "Waiting for the start period ({start_period} seconds) before starting healthcheck."
        ))
        .await;
        tokio::time::sleep(Duration::from_secs(
            u64::try_from(start_period).unwrap_or(0),
        ))
        .await;
        let cn = quote(&self.container);
        let mut status = String::new();
        let mut counter = 1;
        while counter <= retries {
            self.exec(vec![
                Cmd::new(format!(
                    "docker inspect --format='{{{{json .State.Health.Status}}}}' {cn}"
                ))
                .hidden()
                .save("health_check", false),
                Cmd::new(format!(
                    "docker inspect --format='{{{{json .State.Health.Log}}}}' {cn}"
                ))
                .hidden()
                .save("health_check_logs", false),
            ])
            .await?;
            status = self.saved.get("health_check").cloned().unwrap_or_default();
            self.info(&format!(
                "Attempt {counter} of {retries} | Healthcheck status: {status}"
            ))
            .await;
            if let Some(last) = self
                .saved
                .get("health_check_logs")
                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|v| v.as_array().and_then(|a| a.last().cloned()))
            {
                let output = last
                    .get("Output")
                    .and_then(Value::as_str)
                    .filter(|o| !o.is_empty())
                    .unwrap_or("(no logs)")
                    .to_owned();
                let code = last
                    .get("ExitCode")
                    .map_or_else(|| "(no return code)".to_owned(), |c| c.to_string());
                self.info(&format!("Healthcheck logs: {output} | Return code: {code}"))
                    .await;
            }
            match status.trim_matches('"') {
                "healthy" => {
                    self.healthy = true;
                    let _ = apps::update_if_changed(
                        &self.jobs.0.db,
                        self.app.id,
                        &[(
                            "status",
                            ColumnValue::Text(Some("running:unhealthy".into())),
                        )],
                        now(),
                    )
                    .await;
                    self.info("New container is healthy.").await;
                    break;
                }
                "unhealthy" => {
                    self.healthy = false;
                    self.log("New container is unhealthy.", "error", false)
                        .await;
                    self.query_logs().await;
                    break;
                }
                _ => {
                    counter += 1;
                    tokio::time::sleep(Duration::from_secs(u64::try_from(interval).unwrap_or(5)))
                        .await;
                }
            }
        }
        if status.trim_matches('"') == "starting" {
            self.query_logs().await;
        }
        Ok(())
    }

    /// `query_logs()`.
    async fn query_logs(&mut self) {
        self.info(SEPARATOR).await;
        self.info("Container logs:").await;
        let cn = quote(&self.container);
        let _ = self
            .exec(vec![
                Cmd::new(format!("docker logs -n 100 {cn}"))
                    .kind("stderr")
                    .ignore_errors(),
            ])
            .await;
        self.info(SEPARATOR).await;
    }

    /// `wait_for_proxy_to_route_new_container()`.
    async fn wait_for_proxy(&mut self) {
        let seconds = self
            .app
            .setting_int("rolling_update_drain_seconds")
            .unwrap_or(5)
            .clamp(0, 300);
        if seconds == 0 || !self.healthy {
            return;
        }
        let old: Vec<String> = self
            .current_containers()
            .await
            .into_iter()
            .filter(|c| c.get("State").and_then(Value::as_str) == Some("running"))
            .filter_map(|c| c.get("Names").and_then(Value::as_str).map(str::to_owned))
            .filter(|n| *n != self.container)
            .collect();
        if old.is_empty() {
            return;
        }
        let names: Vec<String> = std::iter::once(self.container.clone())
            .chain(old)
            .map(|n| quote(&n))
            .collect();
        let out = self
            .instant(
                &[&format!(
                    "docker inspect --format '{{{{json .Config.Labels}}}}' {}",
                    names.join(" ")
                )],
                false,
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut sets: Vec<String> = Vec::new();
        for line in out.trim().lines() {
            let labels: serde_json::Map<String, Value> =
                serde_json::from_str(line).unwrap_or_default();
            let mut proxy: Vec<(String, Value)> = labels
                .into_iter()
                .filter(|(k, _)| k.starts_with("traefik.") || k.starts_with("caddy"))
                .collect();
            proxy.sort_by(|a, b| a.0.cmp(&b.0));
            let key = serde_json::to_string(&proxy).unwrap_or_default();
            if !sets.contains(&key) {
                sets.push(key);
            }
        }
        if sets.len() > 1 {
            self.info("The proxy labels changed, so the old containers stop without waiting.")
                .await;
            return;
        }
        self.info(&format!(
            "Waiting {seconds} seconds for the proxy to send traffic to the new container."
        ))
        .await;
        tokio::time::sleep(Duration::from_secs(u64::try_from(seconds).unwrap_or(5))).await;
    }

    /// `stop_running_container($force)`.
    async fn stop_running_container(&mut self, force: bool) -> Step {
        self.info("Removing old containers.").await;
        if self.healthy || force {
            let consistent = self
                .app
                .setting_bool("is_consistent_container_name_enabled");
            let mut names: Vec<String> = self
                .current_containers()
                .await
                .into_iter()
                .filter_map(|c| c.get("Names").and_then(Value::as_str).map(str::to_owned))
                .collect();
            if consistent {
                names.push(self.container.clone());
                names.dedup();
            } else {
                names.retain(|n| *n != self.container);
            }
            for name in names {
                self.graceful_shutdown(&name, false).await;
            }
            return Ok(());
        }
        if self.app.str("dockerfile").is_some_and(|d| !d.is_empty())
            || matches!(
                self.app.str("build_pack"),
                Some("dockerfile" | "dockerimage")
            )
        {
            self.info(SEPARATOR).await;
            self.info("WARNING: Dockerfile or Docker Image based deployment detected. The healthcheck needs a curl or wget command to check the health of the application. Please make sure that it is available in the image or turn off healthcheck on Coolify's UI.").await;
            self.info(SEPARATOR).await;
        }
        self.info("New container is not healthy, rolling back to the old container.")
            .await;
        self.fail_deployment().await;
        let cn = self.container.clone();
        self.graceful_shutdown(&cn, false).await;
        Ok(())
    }

    /// `ApplicationPullRequestUpdateJob::dispatch(...)`, for a preview of a GitHub App source.
    fn update_pull_request(&self, status: crate::applications::previews::PullRequestStatus) {
        if let Some(preview) = &self.b.preview
            && self.b.is_preview()
            && self.app.str("source_type") == Some(r"App\Models\GithubApp")
        {
            self.jobs.spawn_pull_request_update(
                self.app.clone(),
                preview.clone(),
                status,
                self.dep.clone(),
            );
        }
    }

    /// `failDeployment()` → `transitionToStatus(FAILED)`.
    async fn fail_deployment(&self) {
        let prev = apps::transition_status(&self.jobs.0.db, self.queue_id, "failed", now())
            .await
            .ok()
            .flatten();
        if !matches!(
            prev.as_deref(),
            Some("finished" | "failed" | "cancelled-by-user")
        ) {
            // `handleFailedDeployment()`
            if let Some(parent) = self.parent_deployment_uuid.clone() {
                self.send_multi_server_notification(&parent).await;
            } else {
                self.send_deployment_notification(false).await;
            }
            self.jobs
                .queue_next_deployment(self.app.id, self.main_server.id)
                .await;
        }
    }

    /// `sendDeploymentNotification($notificationClass)`.
    async fn send_deployment_notification(&self, success: bool) {
        crate::notifications::lookup::notify_deployment(
            &self.jobs.0.db,
            self.jobs.0.encrypter.as_ref(),
            self.app.id,
            self.queue_id,
            &self.commit,
            success,
        )
        .await;
    }

    /// `post_deployment()`.
    async fn post_deployment(&mut self) {
        self.complete_deployment().await;
        // GetContainersStatus::dispatch($server)
        let jobs = self.jobs.clone();
        let server_id = self.server.id;
        tokio::spawn(async move { jobs.check_server_applications(server_id).await });
        self.update_pull_request(crate::applications::previews::PullRequestStatus::Finished);
        if let Err(Fail::Error { message, .. }) = self.run_post_deployment_command().await {
            tracing::warn!("Post deployment command failed for {}: {message}", self.dep);
        }
    }

    /// `completeDeployment()` → `transitionToStatus(FINISHED)`.
    async fn complete_deployment(&mut self) {
        let prev = apps::transition_status(&self.jobs.0.db, self.queue_id, "finished", now())
            .await
            .ok()
            .flatten();
        if !matches!(
            prev.as_deref(),
            Some("finished" | "failed" | "cancelled-by-user")
        ) {
            self.handle_successful_deployment().await;
            self.jobs
                .queue_next_deployment(self.app.id, self.main_server.id)
                .await;
        }
    }

    /// `handleSuccessfulDeployment()`.
    async fn handle_successful_deployment(&mut self) {
        let db = self.jobs.0.db.clone();
        let n = now();
        if self.b.is_preview() {
            if let Some(id) = self
                .b
                .preview
                .as_ref()
                .and_then(|p| p.get("id"))
                .and_then(Value::as_i64)
            {
                let _ = coolify_db::application_ops::reset_preview_restart_limit(&db, id, n).await;
            }
        } else {
            let _ = apps::update_if_changed(
                &db,
                self.app.id,
                &[
                    ("restart_count", ColumnValue::Int(0)),
                    ("last_restart_at", ColumnValue::Text(None)),
                    ("last_restart_type", ColumnValue::Text(None)),
                    ("restart_limit_reached", ColumnValue::Bool(false)),
                ],
                n,
            )
            .await;
        }
        // markDeploymentConfigurationApplied($this->application_deployment_queue)
        if let Some(enc) = self.jobs.0.encrypter.as_ref()
            && let Err(e) =
                snapshot::mark_applied(&db, enc, self.app.id, self.queue_id, self.b.pull_request_id)
                    .await
        {
            tracing::warn!(
                "Failed to mark configuration as applied for deployment {}: {e}",
                self.dep
            );
        }
        if let Some(parent) = self.parent_deployment_uuid.clone() {
            self.send_multi_server_notification(&parent).await;
            return;
        }
        let queued = if self.only_this_server {
            0
        } else {
            self.deploy_to_additional_destinations().await
        };
        if queued == 0 {
            self.send_deployment_notification(true).await;
            return;
        }
        multi_server_mark_queued(&self.dep);
        let dep = self.dep.clone();
        self.send_multi_server_notification(&dep).await;
    }

    /// `failed($e)`: the failure lines (after `failDeployment()`).
    async fn failed(&mut self, e: &Fail) {
        let (message, code, logged) = match e {
            Fail::Cancelled => return,
            Fail::Error {
                message,
                code,
                logged,
            } => (message.clone(), *code, *logged),
        };
        self.fail_deployment().await;
        self.log("========================================", "stderr", false)
            .await;
        if logged {
            self.log("Deployment failed.", "stderr", false).await;
        } else {
            let shown = if message.is_empty() {
                "Unknown error occurred"
            } else {
                message.as_str()
            };
            self.log(&format!("Deployment failed: {shown}"), "stderr", false)
                .await;
        }
        self.log(
            "Error type: App\\Exceptions\\DeploymentException",
            "stderr",
            true,
        )
        .await;
        self.log(&format!("Error code: {code}"), "stderr", true)
            .await;
        self.log("========================================", "stderr", false)
            .await;
        if self.app.str("build_pack") != Some("dockercompose")
            && code != 69420
            && !self.b.is_preview()
            && !self
                .app
                .setting_bool("is_consistent_container_name_enabled")
        {
            if self.healthy {
                self.log(
                    "Deployment failed after the new version became healthy. Keeping the new version of your application running.",
                    "stderr",
                    false,
                )
                .await;
            } else {
                self.log(
                    "Deployment failed. Removing the new version of your application.",
                    "stderr",
                    false,
                )
                .await;
                let name = self.container.clone();
                self.remove_container(&name).await;
            }
        }
    }

    /// `write_deployment_configurations()`: the repository when it is preserved, then the
    /// generated compose file and README in the configuration directory on the server.
    async fn write_deployment_configurations(&mut self) -> Step {
        let finished = now();
        if self.app.setting_bool("is_preserve_repository_enabled") {
            if self.use_build_server {
                self.switch_to_main_server();
            }
            let (c, dep, w) = (self.cfg.clone(), self.dep.clone(), self.workdir.clone());
            self.exec(vec![
                Cmd::new(format!("mkdir -p {c}")),
                Cmd::new(format!("docker cp {dep}:{w}/. {c}")),
            ])
            .await?;
            let (files, owner) = self.file_storages().await?;
            for file in files
                .iter()
                .filter(|f| !f.is_host_file && !f.is_based_on_git && !f.is_directory)
            {
                self.jobs
                    .save_storage_on(&owner, file, self.main_server.id)
                    .await
                    .map_err(Fail::new)?;
            }
            if self.use_build_server {
                self.switch_to_build_server();
            }
        }
        if let Some(yaml) = self.generated_compose.clone() {
            if self.use_build_server {
                self.switch_to_main_server();
            }
            let name = sanitize_string(self.app.str("name").unwrap_or_default());
            let readme = format!(
                "Resource name: {name}\nLatest Deployment Date: {}",
                finished.format("%Y-%m-%d %H:%M:%S")
            );
            let c = self.cfg.clone();
            let file = if self.b.is_preview() {
                let file = crate::compose_parser::volumes::add_preview_deployment_suffix(
                    "docker-compose",
                    self.b.pull_request_id,
                );
                self.b.compose_location = format!("/{file}.yaml");
                format!("{file}.yaml")
            } else {
                "docker-compose.yaml".to_owned()
            };
            self.exec(vec![
                Cmd::new(format!("mkdir -p {c}")),
                Cmd::new(format!("rm -f {c}/{file} {c}/README.md")).hidden(),
                Cmd::new(format!("tee {c}/{file} > /dev/null")).input(yaml),
                Cmd::new(format!("echo '{readme}' | tee {c}/README.md > /dev/null")),
            ])
            .await?;
            if self.use_build_server {
                self.switch_to_build_server();
            }
        }
        Ok(())
    }

    /// `$this->application->fileStorages` and the owner the storage helpers take.
    pub(super) async fn file_storages(&self) -> Result<(Vec<FileStorage>, StorageOwner), Fail> {
        let db = self.jobs.0.db.clone();
        let enc = self
            .jobs
            .0
            .encrypter
            .clone()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        let rows = coolify_db::storages::list(
            &db,
            coolify_db::storages::StorageKind::File,
            apps::MORPH,
            self.app.id,
        )
        .await
        .map_err(|e| Fail::new(e.to_string()))?;
        let files = rows
            .iter()
            .map(|r| FileStorage::from_record(r, &enc))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Fail::new)?;
        let owner = StorageOwner::for_application(&db, &self.app)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        Ok((files, owner))
    }

    /// The `finally` block of `handle()`.
    async fn finally(&mut self) {
        let db = self.jobs.0.db.clone();
        let finished = now();
        let _ = apps::update_deployment(
            &db,
            self.queue_id,
            &[("finished_at", ColumnValue::Time(finished))],
            finished,
        )
        .await;
        // Log but don't fail: configuration writing errors shouldn't prevent status updates.
        let written = if self.use_build_server {
            self.switch_to_build_server();
            Ok(())
        } else {
            self.write_deployment_configurations().await
        };
        if let Err(Fail::Error { message, .. }) = written {
            self.log(
                &format!("Warning: Failed to write deployment configurations: {message}"),
                "stderr",
                false,
            )
            .await;
        }
        let dep = self.dep.clone();
        self.info(&format!("Gracefully shutting down build container: {dep}"))
            .await;
        self.graceful_shutdown(&dep, true).await;
    }
}

/// `sanitize_string()`: strip_tags, htmlspecialchars (ENT_QUOTES | ENT_HTML5), no control characters.
fn sanitize_string(s: &str) -> String {
    let stripped = coolify_domain::naming::strip_tags(Some(s));
    let escaped = stripped
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    escaped
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `traefikHostRulesWithoutIpv6Brackets` for one label.
fn strip_ipv6_brackets(label: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"Host\(`\[([0-9A-Fa-f:.]+)\]`\)").unwrap_or_else(|_| unreachable!())
    });
    RE.replace_all(label, |c: &regex::Captures<'_>| {
        if c[1].parse::<std::net::Ipv6Addr>().is_ok() {
            format!("Host(`{}`)", &c[1])
        } else {
            c[0].to_owned()
        }
    })
    .into_owned()
}

/// `version_compare($a, $b, '>')` for dotted numeric versions.
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.split('.')
            .map(|x| x.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(a) > parse(b)
}
