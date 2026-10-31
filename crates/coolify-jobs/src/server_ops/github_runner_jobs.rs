//! `GithubRunnerApi`, `GithubRunnerContainer`, `ProvisionGithubRunnerJob`, `CleanupGithubRunnerJob`,
//! `DeregisterGithubRunnerJob`, `ReconcileGithubRunnersJob` and the `workflow_job` webhook logic.

use std::time::Duration;

use base64::Engine as _;
use chrono::NaiveDateTime;
use coolify_compat::encrypter::Encrypter;
use coolify_db::Db;
use coolify_db::github_runners as store;
use coolify_db::github_runners::{Execution, RunnerConfig};
use coolify_db::records::Record;
use coolify_remote::shell::quote;
use reqwest::Method;
use serde_json::{Value, json};

use super::Srv;
use crate::Jobs;
use crate::github;
use crate::server::ServerInfo;

const LABEL: &str = "coolify.githubRunner";
const EXECUTION_LABEL: &str = "coolify.githubRunnerExecution";
const SECRETS_DIRECTORY: &str = "/data/coolify/github-runners";
const RUNNER_DOCKER_GID: i64 = 123;
const RUNNER_UID: i64 = 1001;
const PIDS_LIMIT: i64 = 4096;
const HOOK_PATH: &str = "/home/runner/coolify-job-started.sh";
const DEFAULT_IMAGE: &str = "ghcr.io/actions/actions-runner:latest";
const DIND_IMAGE: &str = "docker:29.8-dind";
const DEFAULT_LABELS: &[&str] = &["self-hosted", "linux"];
const SYSBOX_RUNTIME: &str = "sysbox-runc";
const DOCKER_READY_ATTEMPTS: u32 = 60;
pub const MAX_ATTEMPTS: i64 = 3;
const RETRY_DELAY_SECONDS: u64 = 30;
const PROVISIONING_TIMEOUT_MINUTES: i64 = 20;

/// `GithubRunnerContainer::PULL_REQUEST_EVENTS`.
pub const PULL_REQUEST_EVENTS: &[&str] = &[
    "pull_request",
    "pull_request_target",
    "pull_request_review",
    "pull_request_review_comment",
];

/// `GithubRunnerContainer::nameFor`.
pub fn name_for(execution_uuid: &str) -> String {
    format!("coolify-runner-{execution_uuid}")
}

fn hook_file(execution_uuid: &str) -> String {
    format!("{SECRETS_DIRECTORY}/{execution_uuid}-job-started.sh")
}

/// `GithubRunnerContainer::pullRequestHookScript`.
pub fn pull_request_hook_script() -> String {
    format!(
        "#!/bin/bash\ncase \"${{GITHUB_EVENT_NAME:-}}\" in\n  {}|\"\")\n    echo \"::error::This Coolify runner does not run pull request jobs. Allow pull request jobs in the GitHub runner settings of the server to run them.\"\n    exit 1\n    ;;\nesac\nexit 0\n",
        PULL_REQUEST_EVENTS.join("|")
    )
}

/// `GithubRunnerContainer` for an execution and a config.
pub struct Container<'a> {
    pub uuid: &'a str,
    pub config: &'a RunnerConfig,
    pub server: &'a ServerInfo,
}

impl std::fmt::Debug for Container<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Container")
            .field("uuid", &self.uuid)
            .finish_non_exhaustive()
    }
}

impl Container<'_> {
    fn name(&self) -> String {
        name_for(self.uuid)
    }

    fn dind_name(&self) -> String {
        format!("{}-dind", self.name())
    }

    /// `usesDind`.
    pub fn uses_dind(&self) -> bool {
        self.config.docker_mode != "none"
    }

    fn label_flags(&self) -> String {
        format!(
            "--label {LABEL}=true --label {EXECUTION_LABEL}={}",
            self.uuid
        )
    }

    fn limit_flags(&self) -> String {
        let mut flags = vec![format!("--pids-limit {PIDS_LIMIT}")];
        if let Some(cpu) = self
            .config
            .cpu_limit
            .as_deref()
            .filter(|c| !c.trim().is_empty())
        {
            flags.push(format!("--cpus {}", quote(cpu)));
        }
        if let Some(memory) = self
            .config
            .memory_limit
            .as_deref()
            .filter(|c| !c.trim().is_empty())
        {
            let m = quote(memory);
            flags.push(format!("--memory {m} --memory-swap {m}"));
        }
        flags.join(" ")
    }

    fn shared_volume_flags(&self) -> String {
        let name = self.name();
        format!(
            "-v {name}-sock:/var/run -v {name}-work:/home/runner/_work -v {name}-externals:/home/runner/externals"
        )
    }

    fn image(&self) -> String {
        self.config
            .runner_image
            .clone()
            .filter(|i| !i.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_IMAGE.to_owned())
    }

    /// `prepareCommands`.
    pub fn prepare_commands(&self) -> Vec<String> {
        let name = self.name();
        let labels = self.label_flags();
        let image = quote(&self.image());
        let prefix = crate::databases::common::server_docker_config_prefix(self.server);
        let mut commands = vec![
            format!("docker network create {labels} {name}"),
            format!("{prefix}docker pull {image} || docker image inspect {image} > /dev/null"),
        ];
        if !self.uses_dind() {
            return commands;
        }
        for volume in ["sock", "work", "externals"] {
            commands.push(format!("docker volume create {labels} {name}-{volume}"));
        }
        let dind = quote(DIND_IMAGE);
        commands.push(format!(
            "docker image inspect {dind} > /dev/null 2>&1 || docker pull {dind}"
        ));
        let isolation = if self.config.docker_mode == "sysbox" {
            format!("--runtime={SYSBOX_RUNTIME}")
        } else {
            "--privileged".to_owned()
        };
        commands.push(format!(
            "docker run -d {isolation} --name {} --network {name} {labels} {} {} {dind} dockerd --host=unix:///var/run/docker.sock --group={RUNNER_DOCKER_GID}",
            self.dind_name(),
            self.limit_flags(),
            self.shared_volume_flags()
        ));
        commands
    }

    /// `dockerReadyCommand`.
    pub fn docker_ready_command(&self) -> String {
        format!(
            "docker exec {} docker version --format '{{{{.Server.Version}}}}'",
            self.dind_name()
        )
    }

    /// `startCommands`.
    pub fn start_commands(&self, jit_config: &str) -> Vec<String> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let name = self.name();
        let env_file = format!("{SECRETS_DIRECTORY}/{}.{}", self.uuid, concat!("en", "v"));
        let environment = b64.encode(format!("ACTIONS_RUNNER_INPUT_JITCONFIG={jit_config}\n"));
        let mut commands = vec![
            format!("mkdir -p {SECRETS_DIRECTORY}"),
            format!(
                "sh -c 'umask 077 && rm -f {env_file} && echo {environment} | base64 -d > {env_file}'"
            ),
        ];
        let mut flags = format!(
            "--name {name} --network {name} {} {} --env-file {env_file}",
            self.label_flags(),
            self.limit_flags()
        );
        if !self.config.allow_pull_requests {
            let hook = hook_file(self.uuid);
            commands.push(format!(
                "echo {} | base64 -d | tee {hook} > /dev/null",
                b64.encode(pull_request_hook_script())
            ));
            commands.push(format!("chmod 644 {hook}"));
            flags.push_str(&format!(
                " -v {hook}:{HOOK_PATH}:ro -e ACTIONS_RUNNER_HOOK_JOB_STARTED={HOOK_PATH}"
            ));
        }
        if self.uses_dind() {
            commands.push(format!(
                "docker exec {} chown {RUNNER_UID}:{RUNNER_DOCKER_GID} /home/runner/_work",
                self.dind_name()
            ));
            flags.push_str(&format!(
                " -e DOCKER_HOST=unix:///var/run/docker.sock -e RUNNER_WAIT_FOR_DOCKER_IN_SECONDS=120 {}",
                self.shared_volume_flags()
            ));
        }
        commands.push(format!(
            "docker run -d {flags} {} /home/runner/run.sh",
            quote(&self.image())
        ));
        commands.push(format!("rm -f {env_file}"));
        commands
    }
}

/// `GithubRunnerContainer::listCommand`.
pub fn list_command() -> String {
    format!(
        "docker ps -a --filter label={LABEL}=true --format '{{{{.Label \"{EXECUTION_LABEL}\"}}}} {{{{.Names}}}} {{{{.State}}}}'"
    )
}

/// `GithubRunnerConfig::registeredLabels`.
pub fn registered_labels(custom: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in DEFAULT_LABELS
        .iter()
        .map(|l| (*l).to_owned())
        .chain(custom.iter().map(|l| l.to_lowercase()))
    {
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// `GithubRunnerConfig::matchesLabels`.
pub fn matches_labels(custom: &[String], requested: &[String]) -> bool {
    let requested: Vec<String> = requested.iter().map(|l| l.to_lowercase()).collect();
    let custom_lower: Vec<String> = custom.iter().map(|l| l.to_lowercase()).collect();
    if !requested.iter().any(|r| custom_lower.contains(r)) {
        return false;
    }
    let registered = registered_labels(custom);
    requested.iter().all(|r| registered.contains(r))
}

fn encrypter(jobs: &Jobs) -> Result<&Encrypter, String> {
    jobs.0
        .encrypter
        .as_ref()
        .ok_or_else(|| "No application encryption key has been specified.".to_owned())
}

/// `organization()`.
fn organization(app: &Record) -> Result<String, String> {
    let org = app
        .get("organization")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if org.is_empty() {
        return Err(
            "GitHub Actions runners need a GitHub App that belongs to an organization.".into(),
        );
    }
    Ok(crate::gitea::raw_url_encode(org))
}

/// `errorMessage`.
fn api_error(status: u16, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("HTTP {status}"))
}

fn int_field(app: &Record, key: &str) -> i64 {
    app.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn text_field<'a>(app: &'a Record, key: &str) -> &'a str {
    app.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `ensureRunnerGroup`: the group id and whether it is the organization's Default group.
pub async fn ensure_runner_group(
    db: &Db,
    enc: &Encrypter,
    app: &Record,
    now: NaiveDateTime,
) -> Result<(i64, bool), String> {
    let org = organization(app)?;
    let group = int_field(app, "runner_group_id");
    if group != 0 {
        let (status, body) = github::api(
            db,
            enc,
            app,
            Method::GET,
            &format!("/orgs/{org}/actions/runner-groups/{group}"),
            None,
        )
        .await?;
        if (200..300).contains(&status) {
            let is_default = body.get("default").and_then(Value::as_bool) == Some(true);
            return Ok((group, is_default));
        }
        if status != 404 {
            return Err(format!(
                "Could not read the runner group: {}",
                api_error(status, &body)
            ));
        }
    }
    let (status, body) = github::api(
        db,
        enc,
        app,
        Method::POST,
        &format!("/orgs/{org}/actions/runner-groups"),
        Some(&json!({
            "name": format!("Coolify {}", text_field(app, "uuid")),
            "visibility": "all",
            "allows_public_repositories": true,
        })),
    )
    .await?;
    let id = body.get("id").and_then(Value::as_i64).filter(|i| *i != 0);
    let Some(id) = id.filter(|_| (200..300).contains(&status)) else {
        return Err(format!(
            "Could not create a runner group: {}",
            api_error(status, &body)
        ));
    };
    store::set_runner_group(db, int_field(app, "id"), id, now)
        .await
        .map_err(|e| e.to_string())?;
    Ok((id, false))
}

/// `generateJitConfig`.
async fn generate_jit_config(
    db: &Db,
    enc: &Encrypter,
    app: &Record,
    runner_name: &str,
    labels: &[String],
    group: i64,
) -> Result<(i64, String), String> {
    let org = organization(app)?;
    let (status, body) = github::api(
        db,
        enc,
        app,
        Method::POST,
        &format!("/orgs/{org}/actions/runners/generate-jitconfig"),
        Some(&json!({
            "name": runner_name,
            "runner_group_id": group,
            "labels": labels,
            "work_folder": "_work",
        })),
    )
    .await?;
    let jit = body
        .get("encoded_jit_config")
        .and_then(Value::as_str)
        .filter(|j| !j.trim().is_empty());
    match jit.filter(|_| (200..300).contains(&status)) {
        Some(j) => Ok((
            body.pointer("/runner/id")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            j.to_owned(),
        )),
        None => Err(format!(
            "Could not register the runner on GitHub: {}",
            api_error(status, &body)
        )),
    }
}

/// `workflowRunEvent`.
pub async fn workflow_run_event(
    db: &Db,
    enc: &Encrypter,
    app: &Record,
    repository: &str,
    run_id: i64,
) -> Result<String, String> {
    let encoded = repository
        .splitn(2, '/')
        .map(crate::gitea::raw_url_encode)
        .collect::<Vec<_>>()
        .join("/");
    let (status, body) = github::api(
        db,
        enc,
        app,
        Method::GET,
        &format!("/repos/{encoded}/actions/runs/{run_id}"),
        None,
    )
    .await?;
    match body.get("event").and_then(Value::as_str) {
        Some(e) if (200..300).contains(&status) => Ok(e.to_owned()),
        _ => Err(format!(
            "Could not read the workflow run: {}",
            api_error(status, &body)
        )),
    }
}

/// `deleteRunner`.
async fn delete_runner(
    db: &Db,
    enc: &Encrypter,
    app: &Record,
    runner_id: i64,
) -> Result<(), String> {
    let org = organization(app)?;
    let (status, body) = github::api(
        db,
        enc,
        app,
        Method::DELETE,
        &format!("/orgs/{org}/actions/runners/{runner_id}"),
        None,
    )
    .await?;
    if !(200..300).contains(&status) && status != 404 {
        return Err(format!(
            "Could not remove the runner from GitHub: {}",
            api_error(status, &body)
        ));
    }
    Ok(())
}

/// `remove_iip`.
fn remove_iip(text: &str) -> String {
    coolify_compat::redact::redact_text(text, &[], &[])
}

impl Jobs {
    /// `ProvisionGithubRunnerJob::dispatch($id)`.
    pub fn spawn_provision_runner(&self, execution_id: i64, delay: Option<Duration>) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            jobs.provision_runner(execution_id).await;
        });
    }

    /// `ProvisionGithubRunnerJob::dispatchQueued`.
    pub async fn dispatch_queued_runners(&self, github_app_id: i64) {
        let ids = store::queued_ids(&self.0.db, github_app_id, 10)
            .await
            .unwrap_or_default();
        for id in ids {
            self.spawn_provision_runner(id, None);
        }
    }

    /// `reserveServer`.
    async fn reserve_runner_server(&self, id: i64) -> Option<Execution> {
        let db = &self.0.db;
        let execution = store::execution_by_id(db, id).await.ok().flatten()?;
        if execution.status != "queued" {
            return None;
        }
        let configs = store::enabled_configs(db, execution.github_app_id)
            .await
            .ok()?;
        let mut candidates = Vec::new();
        for config in configs {
            if !matches_labels(&config.labels, &execution.labels)
                || (!config.allow_pull_requests && execution.is_pull_request)
            {
                continue;
            }
            let Ok(srv) = Srv::load(self, config.server_id).await else {
                continue;
            };
            if !srv.info.can_build() || srv.is_localhost() || !srv.info.is_functional() {
                continue;
            }
            let occupied = store::occupied_count(db, config.id).await.ok()?;
            if occupied < config.max_runners {
                candidates.push((occupied, config));
            }
        }
        candidates.sort_by_key(|(o, _)| *o);
        let (_, config) = candidates.into_iter().next()?;
        let name = name_for(&execution.uuid);
        store::reserve_execution(db, id, config.id, config.server_id, &name, Self::now())
            .await
            .ok()
            .filter(|changed| *changed)?;
        store::execution_by_id(db, id).await.ok().flatten()
    }

    /// `ProvisionGithubRunnerJob::handle`.
    pub async fn provision_runner(&self, id: i64) {
        let Some(execution) = self.reserve_runner_server(id).await else {
            return;
        };
        if let Err(e) = self.provision_reserved(&execution).await {
            self.runner_provision_failed(&execution, &e).await;
        }
    }

    async fn provision_reserved(&self, execution: &Execution) -> Result<(), String> {
        let db = &self.0.db;
        let enc = encrypter(self)?;
        let config_id = execution
            .github_runner_config_id
            .ok_or("No configuration.")?;
        let config = store::config_by_id(db, config_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("No configuration.")?;
        let server_id = execution.server_id.ok_or("No server.")?;
        let srv = Srv::load(self, server_id).await?;
        let app = coolify_db::git_sources::github_app(db, execution.github_app_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("GitHub App not found.")?;
        let group = match int_field(&app, "runner_group_id") {
            0 => ensure_runner_group(db, enc, &app, Self::now()).await?.0,
            g => g,
        };
        let container = Container {
            uuid: &execution.uuid,
            config: &config,
            server: &srv.info,
        };
        srv.run(&container.prepare_commands(), "github_runner.prepare")
            .await?;
        if container.uses_dind() {
            let mut ready = false;
            for _ in 0..DOCKER_READY_ATTEMPTS {
                let out = srv
                    .run_quiet(
                        &[container.docker_ready_command()],
                        "github_runner.docker_ready",
                    )
                    .await;
                if out.is_some_and(|o| !o.trim().is_empty()) {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if !ready {
                return Err(format!(
                    "The Docker sidecar did not start within {DOCKER_READY_ATTEMPTS} seconds."
                ));
            }
        }
        if self.runner_stop_if_finished(execution).await {
            return Ok(());
        }
        let (runner_id, jit) = generate_jit_config(
            db,
            enc,
            &app,
            &name_for(&execution.uuid),
            &registered_labels(&config.labels),
            group,
        )
        .await?;
        store::set_runner_id(db, execution.id, runner_id, Self::now())
            .await
            .map_err(|e| e.to_string())?;
        if self.runner_stop_if_finished(execution).await {
            return Ok(());
        }
        let srv = Srv::load(self, server_id).await?;
        let container = Container {
            uuid: &execution.uuid,
            config: &config,
            server: &srv.info,
        };
        srv.run(&container.start_commands(&jit), "github_runner.start")
            .await?;
        if self.runner_stop_if_finished(execution).await {
            return Ok(());
        }
        store::mark_ready(db, execution.id, Self::now())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `stopIfFinished`.
    async fn runner_stop_if_finished(&self, execution: &Execution) -> bool {
        let db = &self.0.db;
        let Ok(Some(fresh)) = store::execution_by_id(db, execution.id).await else {
            let runner = execution.runner_id.filter(|r| *r != 0);
            if let Some(r) = runner {
                self.spawn_deregister_runner(execution.github_app_id, r);
            }
            return true;
        };
        let active = is_active(&fresh.status);
        let transferred = match fresh.server_id {
            Some(id) => Srv::load(self, id).await.is_ok_and(|s| s.info.transferred),
            None => false,
        };
        if active && !transferred {
            return false;
        }
        if active {
            let _ = store::finish_execution(
                db,
                fresh.id,
                "cancelled",
                "The server was transferred to another Coolify instance.",
                Self::now(),
            )
            .await;
        }
        self.spawn_cleanup_runner(fresh.id);
        true
    }

    /// `handleFailure`.
    async fn runner_provision_failed(&self, execution: &Execution, error: &str) {
        if self.runner_stop_if_finished(execution).await {
            return;
        }
        let db = &self.0.db;
        let Ok(Some(fresh)) = store::execution_by_id(db, execution.id).await else {
            return;
        };
        let attempt = fresh.provision_attempts;
        if attempt >= MAX_ATTEMPTS {
            let message = remove_iip(&format!(
                "Could not start a runner after {attempt} attempts: {error}"
            ));
            let _ = store::fail_provisioning(db, fresh.id, &message, Self::now()).await;
            self.spawn_cleanup_runner(fresh.id);
            return;
        }
        // The retry keeps the execution uuid, so its Docker resources must be gone before it starts again.
        self.cleanup_runner(fresh.id).await;
        let message = remove_iip(&format!(
            "Attempt {attempt} of {MAX_ATTEMPTS} failed: {error}"
        ));
        if store::requeue_provisioning(db, fresh.id, &message, Self::now())
            .await
            .unwrap_or(false)
        {
            self.spawn_provision_runner(
                fresh.id,
                Some(Duration::from_secs(
                    RETRY_DELAY_SECONDS * attempt.max(1) as u64,
                )),
            );
        }
    }

    /// `CleanupGithubRunnerJob::dispatch`.
    pub fn spawn_cleanup_runner(&self, id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move { jobs.cleanup_runner(id).await });
    }

    /// `CleanupGithubRunnerJob::handle`.
    pub async fn cleanup_runner(&self, id: i64) {
        let db = &self.0.db;
        let Ok(Some(execution)) = store::execution_by_id(db, id).await else {
            return;
        };
        let srv = match execution.server_id {
            Some(sid) => Srv::load(self, sid).await.ok(),
            None => None,
        };
        if srv.as_ref().is_some_and(|s| s.info.transferred) {
            tracing::info!(execution = %execution.uuid, "Skipped GitHub runner cleanup: the server was transferred to another instance");
            Box::pin(self.dispatch_queued_runners(execution.github_app_id)).await;
            return;
        }
        if let Some(s) = &srv {
            s.run_quiet(
                &super::github_runners::cleanup_commands(&execution.uuid),
                "github_runner.cleanup",
            )
            .await;
        }
        if let (Some(runner), Ok(enc)) = (execution.runner_id.filter(|r| *r != 0), encrypter(self))
            && let Ok(Some(app)) =
                coolify_db::git_sources::github_app(db, execution.github_app_id).await
            && let Err(e) = delete_runner(db, enc, &app, runner).await
        {
            tracing::warn!(execution = %execution.uuid, error = %e, "Could not remove GitHub runner");
        }
        Box::pin(self.dispatch_queued_runners(execution.github_app_id)).await;
    }

    /// `DeregisterGithubRunnerJob::dispatch`.
    pub fn spawn_deregister_runner(&self, github_app_id: i64, runner_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let db = &jobs.0.db;
            let Ok(enc) = encrypter(&jobs) else {
                return;
            };
            if let Ok(Some(app)) = coolify_db::git_sources::github_app(db, github_app_id).await
                && let Err(e) = delete_runner(db, enc, &app, runner_id).await
            {
                tracing::warn!(github_app_id, runner_id, error = %e, "Could not remove GitHub runner");
            }
        });
    }

    /// `ReconcileGithubRunnersJob::handle`.
    pub async fn reconcile_github_runners(&self) {
        let db = &self.0.db;
        for server_id in store::reconcile_server_ids(db).await.unwrap_or_default() {
            if let Ok(srv) = Srv::load(self, server_id).await
                && srv.info.is_functional()
            {
                self.reconcile_runner_server(&srv).await;
            }
        }
        // removeRunnersOfDeletedServers
        for (app, runner) in store::occupying_without_server(db)
            .await
            .unwrap_or_default()
        {
            if let Some(r) = runner {
                self.spawn_deregister_runner(app, r);
            }
        }
        let _ = store::delete_occupying_without_server(db).await;
        // reconcileQueued
        for (app_id, executions) in store::queued_by_app(db).await.unwrap_or_default() {
            let timeout = store::max_capacity_wait(db, app_id).await.unwrap_or(0);
            for (id, queued_at) in executions {
                if queued_at < Self::now() - chrono::Duration::minutes(timeout) {
                    let _ = store::finish_execution(
                        db,
                        id,
                        "timed_out",
                        &format!("No runner server had free capacity within {timeout} minutes."),
                        Self::now(),
                    )
                    .await;
                }
            }
            self.dispatch_queued_runners(app_id).await;
        }
    }

    async fn reconcile_runner_server(&self, srv: &Srv) {
        let db = &self.0.db;
        let Some(output) = srv.run_quiet(&[list_command()], "github_runner.list").await else {
            return;
        };
        let containers = parse_containers(&output);
        let executions = store::occupying_on_server(db, srv.id())
            .await
            .unwrap_or_default();
        let now = Self::now();
        for e in &executions {
            let config = match e.github_runner_config_id {
                Some(c) => store::config_by_id(db, c).await.ok().flatten(),
                None => None,
            };
            if let Some((status, message)) = failure_for(e, config.as_ref(), &containers, now) {
                let _ = store::finish_execution(db, e.id, status, &message, now).await;
                self.spawn_cleanup_runner(e.id);
            }
        }
        let known: Vec<&str> = executions.iter().map(|e| e.uuid.as_str()).collect();
        for (uuid, _, _) in &containers {
            if !known.contains(&uuid.as_str()) {
                srv.run_quiet(
                    &super::github_runners::cleanup_commands(uuid),
                    "github_runner.cleanup",
                )
                .await;
            }
        }
    }

    /// `GithubRunnerExecution::assignJob`: a new queued execution for the original job when a job
    /// without a pending runner took this runner.
    pub async fn assign_runner_job(
        &self,
        execution: &Execution,
        job_id: i64,
        details: &store::JobDetails,
    ) -> Option<i64> {
        let db = &self.0.db;
        let now = Self::now();
        let fresh = store::execution_by_id(db, execution.id)
            .await
            .ok()
            .flatten()?;
        if fresh.status != "provisioning" && fresh.status != "idle" {
            return None;
        }
        let other = if fresh.trigger_workflow_job_id == Some(job_id) {
            Some(fresh.clone())
        } else {
            store::by_trigger(db, fresh.github_app_id, job_id)
                .await
                .ok()
                .flatten()
        };
        if other.as_ref().is_some_and(|o| is_active(&o.status)) {
            store::mark_running(db, fresh.id, job_id, None, details, now)
                .await
                .ok()?;
            return None;
        }
        if let Some(o) = &other {
            store::delete_execution(db, o.id).await.ok()?;
        }
        store::mark_running(db, fresh.id, job_id, Some(job_id), details, now)
            .await
            .ok()?;
        if fresh.provision_attempts >= MAX_ATTEMPTS {
            tracing::warn!(
                github_app_id = fresh.github_app_id,
                "GitHub runner taken by another job too often; no new runner is started."
            );
            return None;
        }
        let configs = store::enabled_configs(db, fresh.github_app_id).await.ok()?;
        let matching = configs.iter().any(|c| {
            matches_labels(&c.labels, &fresh.labels)
                && (c.allow_pull_requests || !fresh.is_pull_request)
        });
        if !matching {
            return None;
        }
        store::create_replacement(db, &fresh, now).await.ok()
    }
}

/// `GithubRunnerStatus::isActive`.
pub fn is_active(status: &str) -> bool {
    matches!(status, "queued" | "provisioning" | "idle" | "running")
}

/// `parseContainers`: `(uuid, name, state)`.
pub fn parse_containers(output: &str) -> Vec<(String, String, String)> {
    output
        .trim()
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [uuid, name, state] = parts[..] else {
                return None;
            };
            let ok = !uuid.is_empty()
                && uuid
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            ok.then(|| (uuid.to_owned(), name.to_owned(), state.to_owned()))
        })
        .collect()
}

/// `failureFor`.
fn failure_for(
    e: &Execution,
    config: Option<&RunnerConfig>,
    containers: &[(String, String, String)],
    now: NaiveDateTime,
) -> Option<(&'static str, String)> {
    let minutes = |m: i64| now - chrono::Duration::minutes(m);
    if e.status == "provisioning" {
        return (e.updated_at < minutes(PROVISIONING_TIMEOUT_MINUTES)).then(|| {
            (
                "failed",
                format!("Provisioning did not finish in {PROVISIONING_TIMEOUT_MINUTES} minutes."),
            )
        });
    }
    let name = name_for(&e.uuid);
    let state = containers
        .iter()
        .find(|(u, n, _)| *u == e.uuid && *n == name)
        .map(|(_, _, s)| s.as_str());
    if !matches!(state, Some("running" | "restarting" | "paused")) {
        return Some((
            "failed",
            "The runner container stopped before GitHub reported a job result.".to_owned(),
        ));
    }
    if let Some(c) = config {
        if e.status == "idle" && e.ready_at.unwrap_or(e.updated_at) < minutes(c.idle_timeout) {
            return Some((
                "cancelled",
                format!(
                    "No job was assigned to the runner within {} minutes.",
                    c.idle_timeout
                ),
            ));
        }
        if e.status == "running" && e.started_at.unwrap_or(e.updated_at) < minutes(c.job_timeout) {
            return Some((
                "timed_out",
                format!("The job ran longer than {} minutes.", c.job_timeout),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn labels() {
        let custom = s(&["Gpu", "fast"]);
        assert_eq!(
            registered_labels(&custom),
            ["self-hosted", "linux", "gpu", "fast"]
        );
        assert!(matches_labels(&custom, &s(&["self-hosted", "GPU"])));
        assert!(!matches_labels(&custom, &s(&["self-hosted", "linux"])));
        assert!(!matches_labels(&custom, &s(&["gpu", "windows"])));
    }

    #[test]
    fn containers() {
        let out = "abc123 coolify-runner-abc123 running\nabc123 coolify-runner-abc123-dind running\nBAD x y\nshort line\n";
        assert_eq!(parse_containers(out).len(), 2);
    }
}
