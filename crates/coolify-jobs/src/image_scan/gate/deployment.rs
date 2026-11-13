//! `App\Services\ImageScan\DeploymentImageScan`: the image scan steps of one application
//! deployment: the deploy gate (spec 15.8), the start pins of security updates, and the scan
//! after the start. The deployment job creates one per job and calls it at fixed points, see the
//! guide in [`super`].
//!
//! The job changes its servers and its Compose file while it deploys, so this type reads them
//! from the job when a step needs them ([`DeploymentContext`]), and writes the Compose file back
//! through the job.

use std::future::Future;
use std::time::{Duration, Instant};

use chrono::Utc;
use coolify_compat::yaml::{self, PhpValue};
use coolify_db::image_scans::gate::{self as queries, GateDeployment};
use coolify_db::image_scans::{self, APPLICATION_MORPH, ImageScanRow, ScanOwner};
use coolify_remote::shell::quote;
use coolify_remote::text::php_trim;
use serde_json::{Map, Value, json};

use super::security_gate;
use crate::Jobs;
use crate::image_scan::ScanServer;
use crate::image_scan::candidates;
use crate::image_scan::error::ErrorCode;
use crate::image_scan::image_ref::ImageRef;
use crate::image_scan::policy::{self, GateMode, ResolvedPolicy};
use crate::image_scan::resource_scans;
use crate::image_scan::rules::{IgnoreMatcher, RelevanceRules};
use crate::image_scan::scan_image::{self, ScanDeployment, ScanOptions};
use crate::image_scan::scan_jobs::ScanResourceImages;
use crate::image_scan::update::{dockerfile_base_image, image_start_pin};

/// The type of a visible deployment log line (`addLogEntry($message)`).
pub const STDOUT: &str = "stdout";
/// The type of a warning or error line (`addLogEntry($message, 'stderr')`).
pub const STDERR: &str = "stderr";

/// The key of `saved_outputs` that holds the output of the gate's `docker image inspect`.
pub const INSPECT_OUTPUT: &str = "image_scan_inspect";

/// `Yaml::parse()` without arguments: Symfony's limit of collection aliases.
const YAML_MAX_ALIASES: usize = 128;

/// One command of an `execute_remote_command()` call, with the options upstream passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCommand {
    pub command: String,
    /// `'hidden' => true`.
    pub hidden: bool,
    /// `'ignore_errors' => true`.
    pub ignore_errors: bool,
    /// `'save' => $key`: the output goes to `saved_outputs[$key]`.
    pub save: Option<String>,
    /// `'append'`: true unless the command says otherwise, like upstream.
    pub append: bool,
    /// `'input' => $content`: sent on the command's stdin.
    pub input: Option<String>,
}

impl GateCommand {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            hidden: false,
            ignore_errors: false,
            save: None,
            append: true,
            input: None,
        }
    }

    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    pub fn ignore_errors(mut self) -> Self {
        self.ignore_errors = true;
        self
    }

    pub fn save(mut self, key: impl Into<String>, append: bool) -> Self {
        self.save = Some(key.into());
        self.append = append;
        self
    }

    pub fn input(mut self, input: impl Into<String>) -> Self {
        self.input = Some(input.into());
        self
    }
}

/// What `DeploymentImageScan` needs of the running deployment job: upstream's `read`, `write`,
/// `run` and `checkForCancellation` closures. The getters read the job property of the same name
/// at the time of the call; the job changes several of them while it deploys.
///
/// The futures are `Send`, so a deployment can run on any worker thread; an implementation writes
/// them as `async fn`.
pub trait DeploymentContext: Send {
    /// How the deployment job fails (a failed command, a cancelled deployment). The gate passes
    /// it through unchanged in [`GateStop::Deployment`].
    type Error: Send;

    /// `$this->server->id`: the server the commands run on now (the build server while the
    /// image is built there).
    fn server_id(&self) -> i64;
    /// `$this->server->name`.
    fn server_name(&self) -> String;
    /// `serverDockerConfigPrefix($this->server)`: empty for a root SSH user.
    fn server_docker_config_prefix(&self) -> String;
    /// `$this->mainServer->id`: the server of the destination of this deployment.
    fn main_server_id(&self) -> i64;
    /// `$this->mainServer->isSwarm()`.
    fn main_server_is_swarm(&self) -> bool;
    /// `$this->build_server?->id`: `None` before the build server is chosen.
    fn build_server_id(&self) -> Option<i64>;
    /// `$this->use_build_server`.
    fn use_build_server(&self) -> bool;
    /// `$this->deployment_uuid`.
    fn deployment_uuid(&self) -> String;
    /// `$this->pull_request_id`: 0 unless the deployment is a preview.
    fn pull_request_id(&self) -> i64;
    /// `$this->commit`.
    fn commit(&self) -> String;
    /// `$this->rollback`.
    fn rollback(&self) -> bool;
    /// `$this->skip_build`.
    fn skip_build(&self) -> bool;
    /// `$this->is_this_additional_server`.
    fn is_this_additional_server(&self) -> bool;
    /// `$this->container_name`.
    fn container_name(&self) -> String;
    /// `$this->production_image_name`: `None` before the image is named.
    fn production_image_name(&self) -> Option<String>;
    /// `$this->docker_compose`.
    fn docker_compose(&self) -> String;
    /// `$this->generated_docker_compose`.
    fn generated_docker_compose(&self) -> String;
    /// `$this->docker_compose_custom_start_command`.
    fn docker_compose_custom_start_command(&self) -> Option<String>;
    /// `$this->docker_compose_custom_build_command`.
    fn docker_compose_custom_build_command(&self) -> Option<String>;
    /// `$this->workdir`.
    fn workdir(&self) -> String;
    /// `$this->docker_compose_location`.
    fn docker_compose_location(&self) -> String;
    /// `$this->saved_outputs->get($key)`.
    fn saved_output(&self, key: &str) -> Option<String>;
    /// `$this->timeout`: the deployment timeout in seconds (`dynamic_timeout` of the server).
    fn timeout(&self) -> i64;
    /// `$this->handleStartedAt`: Unix time when the worker started the job.
    fn started_at(&self) -> Option<i64>;

    /// `$this->docker_compose = $yaml`.
    fn set_docker_compose(&mut self, yaml: String);
    /// `$this->generated_docker_compose = $yaml`.
    fn set_generated_docker_compose(&mut self, yaml: String);

    /// `$this->application_deployment_queue->addLogEntry($message, $type)`: a visible line.
    /// `kind` is [`STDOUT`] or [`STDERR`].
    fn add_log_entry(
        &mut self,
        message: &str,
        kind: &'static str,
    ) -> impl Future<Output = ()> + Send;
    /// `$this->execute_remote_command(...$commands)`: one call with all commands, in order.
    fn execute_remote_command(
        &mut self,
        commands: Vec<GateCommand>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
    /// `$this->checkForCancellation()`.
    fn check_for_cancellation(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Why a step of [`DeploymentImageScan`] stops the deployment.
#[derive(Debug)]
pub enum GateStop<E> {
    /// `DeploymentException::alreadyLogged($headline)`: the gate blocks the deploy. The message
    /// is the headline of the gate result, which is already a visible line of the deployment
    /// log, so the failure handler must not show it again.
    Blocked(String),
    /// An error upstream lets escape as it is (the database, a Compose file that is not valid
    /// YAML): the deployment fails with this message.
    Failed(String),
    /// The deployment job's own error from [`DeploymentContext`] (a failed command, a cancel).
    Deployment(E),
}

impl<E> GateStop<E> {
    /// The message of the exception upstream throws; `None` for the job's own error.
    pub fn message(&self) -> Option<&str> {
        match self {
            GateStop::Blocked(message) | GateStop::Failed(message) => Some(message),
            GateStop::Deployment(_) => None,
        }
    }

    /// `DeploymentException::isMessageAlreadyLogged()`.
    pub fn is_message_already_logged(&self) -> bool {
        matches!(self, GateStop::Blocked(_))
    }
}

/// What `docker image inspect` says about the image the gate checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedForScan {
    /// The local image ID (`sha256:{hex}`).
    pub id: String,
    /// The registry digest reference (`nginx@sha256:…`). A locally built image (classic store,
    /// no push) has none.
    pub repo_digest: Option<String>,
}

/// The arguments of `applyGate()`.
#[derive(Debug, Default)]
struct GateCheck {
    scan: Option<ImageScanRow>,
    error: Option<ErrorCode>,
    error_context: Map<String, Value>,
    image_ref: Option<String>,
    repo_digest: Option<String>,
    /// The scan of the image the application ran before this deployment.
    running: Option<ImageScanRow>,
    /// The app was deployed before, but Coolify has no earlier scan to compare with.
    baseline_missing: bool,
    /// The local image ID, stored so a later deploy of a blocked image stays blocked.
    image_id: Option<String>,
}

/// How `scanWhenLockFree()` ends without a scan.
#[derive(Debug)]
enum ScanStop<E> {
    /// A cancel while the gate waits for the lock.
    Deployment(E),
    /// An unexpected error: a scan that could not run.
    Other(String),
}

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// `DeploymentImageScan`.
#[derive(Debug)]
pub struct DeploymentImageScan {
    jobs: Jobs,
    application: ScanOwner,
    deployment: GateDeployment,
    /// The resolved rules with the scan server they were resolved for.
    policy: Option<(Option<i64>, ResolvedPolicy)>,
    rollback_ran_before: Option<bool>,
    /// The worst gate result of this deployment (blocked, then error, then pass).
    gate_result: Option<Value>,
    /// The gate decided for the whole deployment (bypass, rollback, or a scan that cannot run).
    gate_settled: bool,
    /// Images whose scan ran or was reused before the start, so they need no scan after it.
    scanned_images: Vec<String>,
    /// Repo digest reference that replaces the image tag in the compose file.
    pinned_image: Option<String>,
    /// Whether the scanned build of a "Pull latest" security update has the image tag now, so no
    /// pull may replace it.
    start_pinned: bool,
    /// The base image of the final stage of a Dockerfile build (spec 7.4).
    base_image_ref: Option<String>,
    /// Compose images that the gate checked on the server.
    checked_images: Vec<String>,
    /// IDs of stored scans that the gate reused; they join this deployment when it finishes.
    reused_scans: Vec<i64>,
    /// Image IDs that the gate blocked in earlier deployments, read once per job.
    blocked_image_ids: Option<Vec<String>>,
}

impl DeploymentImageScan {
    /// `new DeploymentImageScan($application, $deployment, …)`: `application` is the application
    /// of the deployment as a scan owner, `deployment` its row in
    /// `application_deployment_queues`.
    pub fn new(jobs: &Jobs, application: ScanOwner, deployment: GateDeployment) -> Self {
        Self {
            jobs: jobs.clone(),
            application,
            deployment,
            policy: None,
            rollback_ran_before: None,
            gate_result: None,
            gate_settled: false,
            scanned_images: Vec::new(),
            pinned_image: None,
            start_pinned: false,
            base_image_ref: None,
            checked_images: Vec::new(),
            reused_scans: Vec::new(),
            blocked_image_ids: None,
        }
    }

    /// [`Self::new`] with the application and the deployment read from the database.
    pub async fn load(
        jobs: &Jobs,
        application_id: i64,
        deployment_queue_id: i64,
    ) -> Result<Self, String> {
        let db = &jobs.0.db;
        let application = image_scans::find_owner(db, APPLICATION_MORPH, application_id)
            .await
            .map_err(err)?
            .ok_or("Application not found.")?;
        let deployment = queries::deployment(db, deployment_queue_id)
            .await
            .map_err(err)?
            .ok_or("Deployment not found.")?;
        Ok(Self::new(jobs, application, deployment))
    }

    fn db(&self) -> &coolify_db::Db {
        &self.jobs.0.db
    }

    fn is_build_pack(&self, build_pack: &str) -> bool {
        self.application.build_pack() == Some(build_pack)
    }

    /// `pinnedImage()`: the repo digest that the start uses instead of the tag, set by the gate.
    pub fn pinned_image(&self) -> Option<&str> {
        self.pinned_image.as_deref()
    }

    /// `startPinned()`: the scanned build of a "Pull latest" security update has the tag, so the
    /// start must not pull it.
    pub fn start_pinned(&self) -> bool {
        self.start_pinned
    }

    /// `baseImageRef()`.
    pub fn base_image_ref(&self) -> Option<&str> {
        self.base_image_ref.as_deref()
    }

    /// `refreshSecurity()`: the gate scanned the new image while the deployment was in progress,
    /// so its stored security status compared it with the image of the previous deployment. Now
    /// that this deployment finished, the status is stored again. Never fails the deployment.
    pub async fn refresh_security<C: DeploymentContext>(&mut self, ctx: &mut C) {
        if ctx.pull_request_id() != 0 {
            return;
        }
        let refreshed = async {
            if !policy::is_scan_on(self.db(), &self.application).await? {
                return Ok(());
            }
            let scans = resource_scans::current_for(self.db(), &self.application).await?;
            resource_scans::refresh_summaries(&self.jobs, scans).await
        };
        let outcome: Result<(), String> = refreshed.await;
        if let Err(e) = outcome {
            tracing::error!(error = %e, "DeploymentImageScan::refreshSecurity failed");
        }
    }

    /// `forgetStaleComposeImageOverrides()`: the compose file from Git decides which image
    /// overrides of security updates still apply. A preview deploys the compose file of its
    /// branch, so it keeps them. Never fails the deployment.
    pub async fn forget_stale_compose_image_overrides<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) {
        if ctx.pull_request_id() != 0 {
            return;
        }
        let Some(overrides) = stale_compose_image_overrides(
            self.application
                .record
                .get("docker_compose_image_overrides"),
            &self.application.string("docker_compose_raw"),
        ) else {
            return;
        };
        let text = overrides.as_ref().map(coolify_compat::php_json::encode);
        let saved = queries::set_compose_image_overrides(
            self.db(),
            self.application.id,
            text.as_deref(),
            Jobs::now(),
        )
        .await;
        match saved {
            Ok(()) => {
                self.application.record.insert(
                    "docker_compose_image_overrides".to_owned(),
                    overrides.unwrap_or(Value::Null),
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "Application::forgetStaleComposeImageOverrides failed");
            }
        }
    }

    /// `checkGate($imageRef)`: the deploy gate (spec 15.8): stops the start of an image with
    /// security problems, by the "Block deploys" mode. Does nothing, and runs no remote command,
    /// unless the gate applies to this application.
    pub async fn check_gate<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
        image_ref: &str,
    ) -> Result<(), GateStop<C::Error>> {
        if self.gate_settled || !self.gate_applies(ctx).await.map_err(GateStop::Failed)? {
            return Ok(());
        }

        ctx.check_for_cancellation()
            .await
            .map_err(GateStop::Deployment)?;

        if self.settle_gate_for_deployment(ctx).await? {
            return self.pin_bypassed_image(ctx).await;
        }

        if self.is_build_pack("dockerimage") && !self.start_pinned {
            ctx.add_log_entry(
                &format!("Pulling image ({image_ref}) to scan it before the start."),
                STDOUT,
            )
            .await;
            let pull = format!(
                "{}docker pull {}",
                ctx.server_docker_config_prefix(),
                quote(image_ref)
            );
            ctx.execute_remote_command(vec![GateCommand::new(pull).hidden()])
                .await
                .map_err(GateStop::Deployment)?;
        }

        let image = self.inspect_image_for_scan(ctx, image_ref).await?;

        if let Some(inspected) = &image
            && ctx.rollback()
            && self
                .image_ran_before(&inspected.id)
                .await
                .map_err(GateStop::Failed)?
        {
            ctx.add_log_entry(
                "Rollback to an image that ran before. The image scan does not block it.",
                STDOUT,
            )
            .await;
            return Ok(());
        }

        if let Some(inspected) = &image {
            if !self.checked_images.iter().any(|image| image == image_ref) {
                self.checked_images.push(image_ref.to_owned());
            }
            self.pin_scanned_image(ctx, inspected.repo_digest.as_deref())
                .await?;
        }

        if let Some((error, error_context)) = self
            .error_before_scan(ctx)
            .await
            .map_err(GateStop::Failed)?
        {
            self.gate_settled = true;
            return self
                .apply_gate(
                    ctx,
                    GateCheck {
                        error: Some(error),
                        error_context,
                        ..GateCheck::default()
                    },
                )
                .await;
        }

        let image_id = image.as_ref().map(|inspected| inspected.id.clone());
        let repo_digest = image
            .as_ref()
            .and_then(|inspected| inspected.repo_digest.clone());

        // Read before the scan: a scan of the same image takes over the row of the running image.
        let mut running = self
            .running_image_scan(image_ref)
            .await
            .map_err(GateStop::Failed)?;
        let mut baseline_missing = false;
        if running.is_none()
            && self
                .has_earlier_finished_deployment()
                .await
                .map_err(GateStop::Failed)?
        {
            running = self
                .latest_earlier_image_scan(image_ref, image_id.as_deref())
                .await
                .map_err(GateStop::Failed)?;
            // An image the gate blocked before is never its own baseline.
            let blocked_before = match &image_id {
                Some(id) => self
                    .blocked_image_ids()
                    .await
                    .map_err(GateStop::Failed)?
                    .contains(id),
                None => false,
            };
            baseline_missing = running.is_none() && !blocked_before;
        }
        let reused = match &image_id {
            Some(id) => self
                .completed_image_scan(id)
                .await
                .map_err(GateStop::Failed)?,
            None => None,
        };
        let reused_id = reused.as_ref().map(|scan| scan.id);
        let scan = match reused {
            Some(scan) => scan,
            None => match self.scan_when_lock_free(ctx, image_ref).await {
                Ok(scan) => scan,
                // A cancel while the gate waits for the lock.
                Err(ScanStop::Deployment(e)) => return Err(GateStop::Deployment(e)),
                // An unexpected error is a scan that could not run: "If the scan cannot run"
                // decides, not the error.
                Err(ScanStop::Other(message)) => {
                    tracing::error!(error = %message, "the image scan of the deploy gate failed");
                    return self
                        .apply_gate(
                            ctx,
                            GateCheck {
                                error: Some(ErrorCode::Unknown),
                                image_ref: Some(image_ref.to_owned()),
                                repo_digest,
                                image_id,
                                ..GateCheck::default()
                            },
                        )
                        .await;
                }
            },
        };
        self.queue_candidates(&scan).await;

        let image_id = image_id.or_else(|| Some(scan.image_id.clone()));
        self.apply_gate(
            ctx,
            GateCheck {
                scan: Some(scan),
                image_ref: Some(image_ref.to_owned()),
                repo_digest,
                running,
                baseline_missing,
                image_id,
                ..GateCheck::default()
            },
        )
        .await?;

        if let Some(id) = reused_id
            && !self.reused_scans.contains(&id)
        {
            self.reused_scans.push(id);
        }
        Ok(())
    }

    /// `scanWhenLockFree()`: scans the image for the gate. A busy server lock is not a scan
    /// error: another scan holds it, and background scans do not start while the gate waits. The
    /// gate waits in steps, checks for a cancel between them, and stops at
    /// [`gate_lock_wait_seconds`]; only then the busy lock follows "If the scan cannot run".
    async fn scan_when_lock_free<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
        image_ref: &str,
    ) -> Result<ImageScanRow, ScanStop<C::Error>> {
        let server = ScanServer::load(&self.jobs, ctx.server_id())
            .await
            .map_err(ScanStop::Other)?;
        let host = server.host();
        // `gateLockWaitSeconds()`.
        let total = gate_lock_wait_seconds(
            ctx.started_at(),
            Utc::now().timestamp(),
            ctx.timeout(),
            scan_image::gate_scan_reserve_seconds(&host),
            scan_image::lock_ttl(&host),
        );
        let deadline = Instant::now() + Duration::from_secs(total.unsigned_abs());
        let mut wait_logged = false;

        loop {
            let (wait, last_try) = lock_wait_step(seconds_until(deadline));
            let scan = scan_image::handle(
                &self.jobs,
                &server,
                &self.application,
                image_ref,
                ScanOptions {
                    deployment: Some(ScanDeployment {
                        id: self.deployment.id,
                        pull_request_id: self.deployment.pull_request_id,
                    }),
                    lock_wait_seconds: wait,
                    base_image_ref: self.base_image_ref.clone(),
                    queue_candidate_check: false,
                    retry_when_busy: !last_try,
                    pull_when_missing: false,
                },
            )
            .await
            .map_err(ScanStop::Other)?;

            if last_try || !scan_image::lock_was_busy(&scan) {
                return Ok(scan);
            }

            if !wait_logged {
                wait_logged = true;
                let minutes = (seconds_until(deadline) / 60.0).ceil() as i64;
                ctx.add_log_entry(
                    &format!(
                        "Another image scan is running on {}. Waiting up to {minutes} more minutes for it to finish.",
                        server.name()
                    ),
                    STDOUT,
                )
                .await;
            }
            ctx.check_for_cancellation()
                .await
                .map_err(ScanStop::Deployment)?;
        }
    }

    /// `checkComposeGate()`: gates every image of a Compose deployment before the running
    /// containers stop, and keeps Compose from pulling a different image for a checked service.
    pub async fn check_compose_gate<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<(), GateStop<C::Error>> {
        if !self.gate_applies(ctx).await.map_err(GateStop::Failed)? {
            return Ok(());
        }

        if uses_custom_compose_command(ctx) {
            // The command can build or pull other images than the Compose file names, so no
            // image is checked.
            if !self.gate_settled && !self.settle_gate_for_deployment(ctx).await? {
                self.gate_settled = true;
                let (error, error_context) = match self
                    .error_before_scan(ctx)
                    .await
                    .map_err(GateStop::Failed)?
                {
                    Some(found) => found,
                    None => (ErrorCode::CustomCommand, server_context(ctx.server_name())),
                };
                self.apply_gate(
                    ctx,
                    GateCheck {
                        error: Some(error),
                        error_context,
                        ..GateCheck::default()
                    },
                )
                .await?;
            }
            return Ok(());
        }

        let compose =
            compose_gate_images(&ctx.generated_docker_compose()).map_err(GateStop::Failed)?;
        for image in &compose.images {
            self.check_gate(ctx, image).await?;
        }

        if !self.gate_settled && compose.service_without_image {
            // A raw Compose service without an image gets a name that Coolify does not know.
            self.gate_settled = true;
            self.apply_gate(
                ctx,
                GateCheck {
                    error: Some(ErrorCode::Unknown),
                    ..GateCheck::default()
                },
            )
            .await?;
        }

        if self.checked_images.is_empty() || self.is_raw_compose_deployment_enabled() {
            return Ok(());
        }

        let yaml = with_pull_policy_never(&ctx.generated_docker_compose(), &self.checked_images)
            .map_err(GateStop::Failed)?;
        ctx.set_generated_docker_compose(yaml.clone());
        self.write_compose_file_to_workdir(ctx, yaml).await
    }

    /// `gateApplies()`: the gate runs only when the instance allows scanning, the application
    /// scan is on, and its rules block deploys. Scanning off on the server still gates: the scan
    /// cannot run, and "If the scan cannot run" decides.
    pub async fn gate_applies<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<bool, String> {
        if !policy::is_allowed_for(&self.application) {
            return Ok(false);
        }
        let scan_server_id = scan_server_of(ctx);
        Ok(self.policy(scan_server_id).await?.gate_mode != GateMode::Off)
    }

    /// `linkReusedScans()`: the stored scans that the gate reused belong to this deployment once
    /// it finished, so the current scans of the application (all images of a Compose deployment)
    /// include them. Until then they stay with the deployment that ran them: a deployment that is
    /// running or failed never takes them over. Preview deployments keep the link of the
    /// application. Call it right after the deployment is marked finished (`post_deployment()`,
    /// `just_restart()`). Never fails the deployment.
    pub async fn link_reused_scans<C: DeploymentContext>(&mut self, ctx: &mut C) {
        if ctx.pull_request_id() != 0 || self.reused_scans.is_empty() {
            return;
        }
        let linked = async {
            // completeDeployment() does nothing when the deployment was cancelled or failed
            // meanwhile.
            let status = queries::deployment_status(self.db(), self.deployment.id).await?;
            if status.as_deref() != Some("finished") {
                return Ok(false);
            }
            queries::link_scans(
                self.db(),
                self.application.morph,
                self.application.id,
                &self.reused_scans,
                self.deployment.id,
                Jobs::now(),
            )
            .await?;
            Ok(true)
        };
        let outcome: Result<bool, coolify_db::DbError> = linked.await;
        match outcome {
            Ok(true) => self.reused_scans.clear(),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(error = %e, "DeploymentImageScan::linkReusedScans failed");
            }
        }
    }

    /// `applyStartPin()`: a "Pull latest" security update of a Docker image app: pulls the build
    /// that Coolify scanned and gives it the configured tag, so the gate and the start use it and
    /// do not pull the tag again.
    pub async fn apply_start_pin<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<(), GateStop<C::Error>> {
        if !self.is_build_pack("dockerimage") || ctx.pull_request_id() != 0 || ctx.rollback() {
            return Ok(());
        }
        let Some(image_name) = ctx.production_image_name() else {
            return Ok(());
        };

        let taken = image_start_pin::take(&self.application, Some(ctx.server_id()));
        let pins = image_start_pin::for_images(&taken, std::slice::from_ref(&image_name));
        if pins.is_empty() {
            return Ok(());
        }

        ctx.add_log_entry(
            &format!("Pulling the scanned build of {image_name}."),
            STDOUT,
        )
        .await;
        let commands = image_start_pin::commands(&pins, &ctx.server_docker_config_prefix())
            .into_iter()
            .map(|command| GateCommand::new(command).hidden())
            .collect();
        ctx.execute_remote_command(commands)
            .await
            .map_err(GateStop::Deployment)?;
        self.start_pinned = true;
        Ok(())
    }

    /// `applyComposeStartPins()`: a "Pull latest" security update of a Compose app: after the
    /// pull of the registry images, gives the scanned builds their tags again. `up` uses local
    /// images, so the containers run the scanned builds.
    pub async fn apply_compose_start_pins<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<(), GateStop<C::Error>> {
        if ctx.pull_request_id() != 0
            || truthy(ctx.docker_compose_custom_build_command().as_deref())
        {
            return Ok(());
        }

        let pins = image_start_pin::take(&self.application, Some(ctx.server_id()));
        if pins.is_empty() {
            return Ok(());
        }

        ctx.add_log_entry("Pulling the scanned builds of the security update.", STDOUT)
            .await;
        let commands = image_start_pin::commands(&pins, &ctx.server_docker_config_prefix())
            .into_iter()
            .map(|command| GateCommand::new(command).hidden())
            .collect();
        ctx.execute_remote_command(commands)
            .await
            .map_err(GateStop::Deployment)
    }

    /// `rememberDockerfileBaseImage()`: remembers the base image of the final stage of a
    /// Dockerfile build for the image scan (spec 7.4). Runs no remote command and never fails the
    /// deployment.
    pub async fn remember_dockerfile_base_image<C: DeploymentContext>(&mut self, ctx: &mut C) {
        self.base_image_ref = None;
        let deployment_uuid = ctx.deployment_uuid();
        match self.dockerfile_base_image(ctx).await {
            Ok(base_image_ref) => self.base_image_ref = base_image_ref,
            Err(e) => tracing::warn!(
                "Deployment {deployment_uuid} could not read the base image for the image scan: {e}"
            ),
        }
    }

    async fn dockerfile_base_image<C: DeploymentContext>(
        &self,
        ctx: &mut C,
    ) -> Result<Option<String>, String> {
        if !self.is_build_pack("dockerfile") || !policy::is_allowed_for(&self.application) {
            return Ok(None);
        }
        let server_id = ctx.server_id();
        let main_server_id = ctx.main_server_id();
        if policy::off_state(self.db(), &self.application, Some(server_id))
            .await?
            .is_some()
            && policy::off_state(self.db(), &self.application, Some(main_server_id))
                .await?
                .is_some()
        {
            return Ok(None);
        }

        let dockerfile = match self.application.str("dockerfile") {
            Some(dockerfile) if filled(Some(dockerfile)) => dockerfile.to_owned(),
            _ => ctx.saved_output("dockerfile_from_repo").unwrap_or_default(),
        };
        Ok(dockerfile_base_image::final_stage(
            &dockerfile,
            self.application.str("dockerfile_target_build"),
        ))
    }

    /// `dispatchScanAfterStart()`: queues one scan of the started images when the gate did not
    /// scan them before the start. Restarts, skipped builds, and additional servers run an image
    /// that was scanned before, so they get no scan (spec 4.4), except for a rollback. Never
    /// fails the deployment.
    pub async fn dispatch_scan_after_start<C: DeploymentContext>(&mut self, ctx: &mut C) {
        let deployment_uuid = ctx.deployment_uuid();
        if let Err(e) = self.queue_scan_after_start(ctx).await {
            tracing::warn!("Deployment {deployment_uuid} could not queue the image scan: {e}");
        }
    }

    async fn queue_scan_after_start<C: DeploymentContext>(
        &self,
        ctx: &mut C,
    ) -> Result<(), String> {
        if skips_scan_after_start(
            ctx.skip_build(),
            ctx.is_this_additional_server(),
            ctx.rollback(),
        ) {
            return Ok(());
        }
        let main_server_id = ctx.main_server_id();
        if policy::off_state(self.db(), &self.application, Some(main_server_id))
            .await?
            .is_some()
        {
            return Ok(());
        }

        // Images that the gate scanned before the start need no second scan; the others (a scan
        // that could not run, a rollback image that ran before) are scanned now.
        let images = images_after_start(
            self.is_build_pack("dockercompose"),
            &ctx.generated_docker_compose(),
            ctx.production_image_name().as_deref(),
            self.pinned_image.as_deref(),
            &self.scanned_images,
        )?;

        if !images.is_empty() {
            // `ScanResourceImagesJob::dispatch($application, $deployment->id, $images, false, $baseImageRef)`.
            let mut job = ScanResourceImages::new(&self.application);
            job.deployment_queue_id = Some(self.deployment.id);
            job.image_refs = images;
            job.retry_when_image_missing = false;
            job.base_image_ref = self.base_image_ref.clone();
            self.jobs.dispatch_scan_resource_images(job, 0);
        }
        Ok(())
    }

    /// `writeComposeFileToWorkdir()`: writes the Compose file again after the gate changed it,
    /// with the same commands as the first write.
    async fn write_compose_file_to_workdir<C: DeploymentContext>(
        &self,
        ctx: &mut C,
        yaml: String,
    ) -> Result<(), GateStop<C::Error>> {
        let commands = compose_write_commands(
            &ctx.deployment_uuid(),
            &format!("{}{}", ctx.workdir(), ctx.docker_compose_location()),
            yaml,
        );
        ctx.execute_remote_command(commands)
            .await
            .map_err(GateStop::Deployment)
    }

    /// `queueCandidates()`: queues the check of newer versions and of the base image for the
    /// scanned image. The gate does not wait for it: it reads only the findings.
    async fn queue_candidates(&self, scan: &ImageScanRow) {
        if !scan.exists() {
            return;
        }
        match candidates::should_refresh(&self.jobs, scan).await {
            Ok(true) => candidates::queue_check(&self.jobs, scan),
            Ok(false) => {}
            Err(e) => tracing::error!(error = %e, "RefreshCandidates::shouldRefresh failed"),
        }
    }

    /// `runningImageScan()`: the scan of the image the application ran before this deployment: a
    /// completed scan of the latest finished deployment that is not a preview. The deployment
    /// decides, not the scan time: a redeploy of an older image links its older scan to the newer
    /// deployment. A Compose application matches the image by name. `None` before the first
    /// deploy, so every finding of the new image counts as new.
    async fn running_image_scan(&self, image_ref: &str) -> Result<Option<ImageScanRow>, String> {
        let (morph, id) = (self.application.morph, self.application.id);
        if !self.is_build_pack("dockercompose") {
            return queries::running_scan(self.db(), morph, id)
                .await
                .map_err(err);
        }

        let name = scan_image::image_name(image_ref);
        let scans = queries::running_scan_refs(self.db(), morph, id)
            .await
            .map_err(err)?;
        match scans
            .into_iter()
            .find(|(_, scanned)| scan_image::image_name(scanned) == name)
        {
            Some((scan_id, _)) => image_scans::find_scan(self.db(), scan_id)
                .await
                .map_err(err),
            None => Ok(None),
        }
    }

    /// `hasEarlierFinishedDeployment()`: a finished deployment of this application that is not a
    /// preview ran before this one, so this is not the first deploy.
    async fn has_earlier_finished_deployment(&self) -> Result<bool, String> {
        queries::has_earlier_finished_deployment(self.db(), self.application.id, self.deployment.id)
            .await
            .map_err(err)
    }

    /// `latestEarlierImageScan()`: the gate's baseline when no scan of the running image exists
    /// although the app was deployed before (for example, scanning was turned on after the
    /// deploy): the newest completed scan that is not a preview. The scan of an image the gate
    /// blocked never counts, and neither does the image of this deploy, so trying a blocked image
    /// again cannot pass.
    async fn latest_earlier_image_scan(
        &mut self,
        image_ref: &str,
        image_id: Option<&str>,
    ) -> Result<Option<ImageScanRow>, String> {
        let name = self
            .is_build_pack("dockercompose")
            .then(|| scan_image::image_name(image_ref));
        let mut excluded = self.blocked_image_ids().await?;
        if let Some(id) = image_id.filter(|id| !id.is_empty() && *id != "0")
            && !excluded.iter().any(|blocked| blocked == id)
        {
            excluded.push(id.to_owned());
        }

        let scans = queries::earlier_scans(
            self.db(),
            self.application.morph,
            self.application.id,
            self.deployment.id,
            &excluded,
        )
        .await
        .map_err(err)?;
        Ok(scans.into_iter().find(|scan| {
            name.as_ref()
                .is_none_or(|name| scan_image::image_name(&scan.image_ref) == *name)
        }))
    }

    /// `blockedImageIds()`: the images that the gate blocked in an earlier deployment of this
    /// application that is not a preview: the image ID stored with the result, or a scan linked
    /// to the blocked deployment. The stored ID keeps the block when the scan was removed.
    async fn blocked_image_ids(&mut self) -> Result<Vec<String>, String> {
        if let Some(ids) = &self.blocked_image_ids {
            return Ok(ids.clone());
        }
        let ids = queries::blocked_image_ids(
            self.db(),
            self.application.morph,
            self.application.id,
            self.deployment.id,
        )
        .await
        .map_err(err)?;
        self.blocked_image_ids = Some(ids.clone());
        Ok(ids)
    }

    /// `pinBypassedImage()`: "Deploy anyway" of a Docker image starts the image that was blocked,
    /// not a newer image behind the same tag.
    async fn pin_bypassed_image<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<(), GateStop<C::Error>> {
        if !self.is_build_pack("dockerimage")
            || self
                .gate_bypassed_by()
                .await
                .map_err(GateStop::Failed)?
                .is_none()
        {
            return Ok(());
        }

        let mut pinned = repo_digest_of(self.deployment.scan_gate_result.as_ref());
        if pinned.is_none()
            && let Some(parent) = self
                .deployment
                .parent_deployment_uuid
                .clone()
                .filter(|parent| filled(Some(parent)))
        {
            let application_id = self.deployment.application_id.clone().unwrap_or_default();
            pinned = queries::parent_gate(self.db(), &parent, &application_id)
                .await
                .map_err(|e| GateStop::Failed(err(e)))?
                .and_then(|(result, _)| repo_digest_of(result.as_ref()));
        }
        let Some(pinned) = pinned else {
            return Ok(());
        };
        let Some(image) = ctx
            .production_image_name()
            .and_then(|configured| bypassed_pin(&pinned, &configured))
        else {
            return Ok(());
        };

        if !self.pin_compose(ctx, &image).map_err(GateStop::Failed)? {
            return Ok(());
        }

        ctx.add_log_entry(
            &format!("Image scan: the deployment starts the blocked image {image}."),
            STDOUT,
        )
        .await;
        Ok(())
    }

    /// `pinScannedImage()`: starts the scanned image where the start could pull another one: a
    /// Docker image, an image from a build server, or a Swarm service. Without a repo digest the
    /// image stays as it is.
    async fn pin_scanned_image<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
        repo_digest: Option<&str>,
    ) -> Result<(), GateStop<C::Error>> {
        let Some(repo_digest) = repo_digest else {
            return Ok(());
        };
        if self.is_build_pack("dockercompose") {
            return Ok(());
        }
        if !self.is_build_pack("dockerimage")
            && !ctx.use_build_server()
            && !ctx.main_server_is_swarm()
        {
            return Ok(());
        }

        if !self
            .pin_compose(ctx, repo_digest)
            .map_err(GateStop::Failed)?
        {
            return Ok(());
        }

        ctx.add_log_entry(
            &format!("Image scan: the deployment starts {repo_digest}."),
            STDOUT,
        )
        .await;
        Ok(())
    }

    /// `pinCompose()`: sets the image of the application service in the job's Compose file.
    /// False when the file has no such service.
    fn pin_compose<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
        image: &str,
    ) -> Result<bool, String> {
        let Some(yaml) = compose_with_image(&ctx.docker_compose(), &ctx.container_name(), image)?
        else {
            return Ok(false);
        };
        ctx.set_generated_docker_compose(yaml.clone());
        ctx.set_docker_compose(yaml);
        self.pinned_image = Some(image.to_owned());
        Ok(true)
    }

    /// `policy()`: the image is scanned on the build server when it is there, else on the main
    /// server. The rules always resolve through the main destination server. `scan_server_id` is
    /// [`scan_server_of`] the job.
    async fn policy(&mut self, scan_server_id: Option<i64>) -> Result<ResolvedPolicy, String> {
        if let Some((server_id, policy)) = &self.policy
            && *server_id == scan_server_id
        {
            return Ok(policy.clone());
        }
        let policy = policy::resolve(self.db(), &self.application, scan_server_id).await?;
        self.policy = Some((scan_server_id, policy.clone()));
        Ok(policy)
    }

    /// `(bool) $this->application->settings->is_raw_compose_deployment_enabled`.
    fn is_raw_compose_deployment_enabled(&self) -> bool {
        self.application
            .settings
            .as_ref()
            .and_then(|settings| settings.get("is_raw_compose_deployment_enabled"))
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// `settleGateForDeployment()`: decisions for the whole deployment that need no image:
    /// "Deploy anyway", and a rollback to a commit that ran before.
    async fn settle_gate_for_deployment<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<bool, GateStop<C::Error>> {
        if let Some(user_id) = self.gate_bypassed_by().await.map_err(GateStop::Failed)? {
            self.gate_settled = true;
            self.log_gate_bypass(ctx, user_id)
                .await
                .map_err(GateStop::Failed)?;
            return Ok(true);
        }

        if ctx.rollback() {
            let commit = ctx.commit();
            let pull_request_id = ctx.pull_request_id();
            if self
                .rollback_commit_ran_before(&commit, pull_request_id)
                .await
                .map_err(GateStop::Failed)?
            {
                self.gate_settled = true;
                ctx.add_log_entry(
                    "Rollback to an image that ran before. The image scan does not block it.",
                    STDOUT,
                )
                .await;
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// `errorBeforeScan()`: the reason the gate cannot scan: scanning is off where the image is,
    /// or a custom Compose command. With the context of the error message (`server`).
    async fn error_before_scan<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
    ) -> Result<Option<(ErrorCode, Map<String, Value>)>, String> {
        let on_build_server = image_is_on_build_server(ctx);
        let custom_command = uses_custom_compose_command(ctx);
        let job_server_id = ctx.server_id();
        let job_server_name = ctx.server_name();
        let policy = self.policy(scan_server_of(ctx)).await?;

        let Some(error) =
            error_before_scan_code(policy.scan_enabled, on_build_server, custom_command)
        else {
            return Ok(None);
        };
        // `$policy->scanServer?->name ?? $this->job('server')->name` when scanning is off.
        let server = match policy.scan_server_id {
            Some(id) if error != ErrorCode::CustomCommand && id != job_server_id => {
                queries::server_name(self.db(), id)
                    .await
                    .map_err(err)?
                    .unwrap_or(job_server_name)
            }
            _ => job_server_name,
        };
        Ok(Some((error, server_context(server))))
    }

    /// `rollbackCommitRanBefore()`: a rollback is not gated when a finished deployment of this
    /// application ran its commit (spec 4.4). A local tag alone does not count: the image of a
    /// blocked deployment stays tagged on the server.
    async fn rollback_commit_ran_before(
        &mut self,
        commit: &str,
        pull_request_id: i64,
    ) -> Result<bool, String> {
        if !is_rollback_commit(commit) {
            return Ok(false);
        }
        if let Some(ran_before) = self.rollback_ran_before {
            return Ok(ran_before);
        }
        let ran_before =
            queries::commit_ran_before(self.db(), self.application.id, commit, pull_request_id)
                .await
                .map_err(err)?;
        self.rollback_ran_before = Some(ran_before);
        Ok(ran_before)
    }

    /// `imageRanBefore()`: an image ran before when a finished deployment of this application
    /// scanned it.
    async fn image_ran_before(&self, image_id: &str) -> Result<bool, String> {
        queries::image_ran_before(
            self.db(),
            self.application.morph,
            self.application.id,
            image_id,
        )
        .await
        .map_err(err)
    }

    /// `completedImageScan()`.
    async fn completed_image_scan(&self, image_id: &str) -> Result<Option<ImageScanRow>, String> {
        Ok(image_scans::find_scan_of_image(
            self.db(),
            self.application.morph,
            self.application.id,
            image_id,
        )
        .await
        .map_err(err)?
        .filter(ImageScanRow::is_completed))
    }

    /// `inspectImageForScan()`: the local image ID and the registry digest reference of the
    /// image, read with one `docker image inspect`.
    async fn inspect_image_for_scan<C: DeploymentContext>(
        &self,
        ctx: &mut C,
        image_ref: &str,
    ) -> Result<Option<InspectedForScan>, GateStop<C::Error>> {
        let Ok(image) = ImageRef::parse(image_ref) else {
            return Ok(None);
        };

        ctx.execute_remote_command(vec![inspect_command(image_ref)])
            .await
            .map_err(GateStop::Deployment)?;

        let output = ctx.saved_output(INSPECT_OUTPUT).unwrap_or_default();
        Ok(parse_inspect_output(&output, &image))
    }

    /// `applyGate()`: writes the result on the deployment, logs it, and stops the deployment when
    /// the gate blocks.
    async fn apply_gate<C: DeploymentContext>(
        &mut self,
        ctx: &mut C,
        check: GateCheck,
    ) -> Result<(), GateStop<C::Error>> {
        let GateCheck {
            scan,
            mut error,
            mut error_context,
            image_ref,
            repo_digest,
            running,
            baseline_missing,
            image_id,
        } = check;
        let completed = scan.as_ref().is_some_and(ImageScanRow::is_completed);

        if let Some(scan) = &scan
            && !completed
            && error.is_none()
        {
            error = Some(
                scan.error_code
                    .as_deref()
                    .and_then(ErrorCode::from_value)
                    .unwrap_or(ErrorCode::Unknown),
            );
            error_context = match &scan.error_context {
                Some(Value::Object(context)) => context.clone(),
                _ => Map::new(),
            };
        }

        let policy = self
            .policy(scan_server_of(ctx))
            .await
            .map_err(GateStop::Failed)?;
        let ignores = IgnoreMatcher::for_resource(self.db(), &self.application, Jobs::now())
            .await
            .map_err(GateStop::Failed)?;
        let rules = RelevanceRules::new(ignores, policy.count_medium);
        let result = security_gate::evaluate(
            scan.as_ref(),
            running.as_ref(),
            policy.gate_mode,
            &rules,
            error,
            &error_context,
            policy.on_scan_error,
            baseline_missing,
        );
        let mut stored = security_gate::stored(&result);
        if result.blocked
            && completed
            && let Some(scan) = &scan
        {
            // The action that fixes the blocked image, for the gate popover.
            match crate::image_scan::views::card::blocked_image_action(
                &self.jobs,
                scan,
                &self.application,
            )
            .await
            {
                Ok(Some(action)) => {
                    if let Value::Object(map) = &mut stored {
                        map.insert("action".to_owned(), action.to_value());
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(error = %e, "the action of a blocked image could not be read");
                }
            }
        }

        // A Compose deployment gates several images: keep the worst result (blocked, then error,
        // then pass).
        if gate_rank(Some(&stored)) >= gate_rank(self.gate_result.as_ref()) {
            // The digest lets "Deploy anyway" start the image that was blocked.
            let gate_result =
                stored_gate_result(stored, repo_digest.as_deref(), image_id.as_deref());
            image_scans::save_gate_result(
                self.db(),
                self.deployment.id,
                Some(&gate_result),
                Jobs::now(),
            )
            .await
            .map_err(|e| GateStop::Failed(err(e)))?;
            self.deployment.scan_gate_result = Some(gate_result.clone());
            self.gate_result = Some(gate_result);
        }
        if result.scan_ran
            && let Some(image_ref) = image_ref
            && !self.scanned_images.contains(&image_ref)
        {
            self.scanned_images.push(image_ref);
        }

        // A pass logs "passed" first; the lines after it warn about problems that the running
        // image has too.
        let failed = result.blocked || result.error.is_some();
        let lines = security_gate::log_lines(&result);
        for (index, line) in lines.iter().enumerate() {
            ctx.add_log_entry(line, log_kind(failed, index)).await;
        }

        if result.blocked {
            return Err(GateStop::Blocked(result.headline));
        }
        Ok(())
    }

    /// `gateBypassedBy()`: the user who chose "Deploy anyway" for this deployment, or for the
    /// main deployment that queued it.
    async fn gate_bypassed_by(&self) -> Result<Option<i64>, String> {
        if let Some(user_id) = self.deployment.scan_gate_bypassed_by {
            return Ok(Some(user_id));
        }
        let Some(parent) = self
            .deployment
            .parent_deployment_uuid
            .as_deref()
            .filter(|parent| filled(Some(parent)))
        else {
            return Ok(None);
        };
        let application_id = self.deployment.application_id.clone().unwrap_or_default();
        Ok(queries::parent_gate(self.db(), parent, &application_id)
            .await
            .map_err(err)?
            .and_then(|(_, user_id)| user_id))
    }

    /// `logGateBypass()`.
    async fn log_gate_bypass<C: DeploymentContext>(
        &self,
        ctx: &mut C,
        user_id: i64,
    ) -> Result<(), String> {
        // `User::query()->find($userId)`.
        let user = coolify_db::audit::actor_user(self.db(), user_id)
            .await
            .map_err(err)?;
        let name = user
            .as_ref()
            .map_or("a deleted user", |(name, _)| name.as_str());

        ctx.add_log_entry(&format!("Deploy anyway by {name}."), STDERR)
            .await;

        let actor_name = user.as_ref().map(|(name, _)| name.clone());
        let actor_email = user.as_ref().map(|(_, email)| email.clone());
        let actor_id = user.as_ref().map(|_| user_id);
        let context = json!({
            "team_id": self.application.team_id,
            "application_uuid": self.application.uuid,
            "deployment_uuid": ctx.deployment_uuid(),
            "user_id": user_id,
            "actor_id": actor_id,
            "actor_name": actor_name,
            "actor_email": actor_email,
        });
        // `auditLog('ui.application.image_scan_gate_bypassed', …)` outside a request: the context
        // names the actor.
        let actor = coolify_db::audit::Actor {
            user_id: actor_id,
            name: actor_name,
            email: actor_email,
            from_context: true,
            ..Default::default()
        };
        if let Err(e) = coolify_db::audit::record(
            self.db(),
            self.application.team_id,
            "ui.application.image_scan_gate_bypassed",
            &context,
            "info",
            &actor,
            None,
            Jobs::now(),
        )
        .await
        {
            tracing::warn!(error = %e, "audit log of the image scan gate bypass failed");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------

/// `imageIsOnBuildServer()`: the image is on the build server when the deployment built there. A
/// restart and a Docker image app keep the image on the deployment server, even with a build
/// server.
fn image_is_on_build_server<C: DeploymentContext>(ctx: &C) -> bool {
    ctx.use_build_server() && ctx.build_server_id() == Some(ctx.server_id())
}

/// The server argument of `ImageScanPolicyResolver::for()` in `policy()`: the build server when
/// the image is there, else `None` (the main server of the application).
fn scan_server_of<C: DeploymentContext>(ctx: &C) -> Option<i64> {
    if image_is_on_build_server(ctx) {
        ctx.build_server_id()
    } else {
        None
    }
}

/// `usesCustomComposeCommand()`.
fn uses_custom_compose_command<C: DeploymentContext>(ctx: &C) -> bool {
    filled(ctx.docker_compose_custom_start_command().as_deref())
        || filled(ctx.docker_compose_custom_build_command().as_deref())
}

/// `filled($value)` of a string.
fn filled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// PHP truthiness of a string: null, empty and `"0"` are false.
fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// `['server' => $name]`.
fn server_context(name: String) -> Map<String, Value> {
    let mut context = Map::new();
    context.insert("server".to_owned(), Value::String(name));
    context
}

/// The error code of `errorBeforeScan()`.
fn error_before_scan_code(
    scan_enabled: bool,
    on_build_server: bool,
    custom_command: bool,
) -> Option<ErrorCode> {
    if !scan_enabled {
        return Some(if on_build_server {
            ErrorCode::BuildServerOff
        } else {
            ErrorCode::ScanningOff
        });
    }
    custom_command.then_some(ErrorCode::CustomCommand)
}

/// The commit rule of `rollbackCommitRanBefore()`: a blank commit and `HEAD` never ran before.
fn is_rollback_commit(commit: &str) -> bool {
    filled(Some(commit)) && commit != "HEAD"
}

/// `gateLockWaitSeconds()`: how long the gate waits in total for the server lock: until the lock
/// expires at the latest, and only while the deployment keeps time for its own scan before its
/// timeout. At least one lock wait. `reserve` is `ScanImage::gateScanReserveSeconds($server)`,
/// `lock_ttl` is `ScanImage::lockTtl($server)`.
fn gate_lock_wait_seconds(
    started_at: Option<i64>,
    now: i64,
    timeout: i64,
    reserve: i64,
    lock_ttl: i64,
) -> i64 {
    let elapsed = started_at.map_or(0, |at| (now - at).max(0));
    let left = timeout - elapsed - reserve;
    (scan_image::GATE_LOCK_WAIT_SECONDS as i64).max(lock_ttl.min(left))
}

/// One step of `scanWhenLockFree()`: the lock wait of the next scan, and whether it is the last
/// try, for the seconds left until the deadline.
fn lock_wait_step(seconds_left: f64) -> (u64, bool) {
    let wait = seconds_left
        .ceil()
        .min(scan_image::GATE_LOCK_WAIT_SECONDS as f64)
        .max(1.0);
    (wait as u64, wait >= seconds_left)
}

/// `now()->diffInSeconds($deadline, false)`: negative after the deadline.
fn seconds_until(deadline: Instant) -> f64 {
    let now = Instant::now();
    match deadline.checked_duration_since(now) {
        Some(left) => left.as_secs_f64(),
        None => -now.duration_since(deadline).as_secs_f64(),
    }
}

/// The rank of a stored gate result in `applyGate()`: blocked, then error, then pass.
fn gate_rank(gate_result: Option<&Value>) -> i32 {
    match gate_result {
        None => -1,
        Some(result) if result.get("blocked").and_then(Value::as_bool) == Some(true) => 2,
        Some(result) if result.get("error").is_some_and(|error| !error.is_null()) => 1,
        Some(_) => 0,
    }
}

/// The stored result with the digest and the image ID of the checked image.
fn stored_gate_result(stored: Value, repo_digest: Option<&str>, image_id: Option<&str>) -> Value {
    let mut result = match stored {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    if let Some(repo_digest) = repo_digest {
        result.insert("repo_digest".to_owned(), json!(repo_digest));
    }
    if let Some(image_id) = image_id.filter(|id| !id.is_empty()) {
        result.insert("image_id".to_owned(), json!(image_id));
    }
    Value::Object(result)
}

/// The type of a log line of the gate result: a pass logs its first line as output, every other
/// line is a warning.
fn log_kind(failed: bool, index: usize) -> &'static str {
    if failed || index > 0 { STDERR } else { STDOUT }
}

/// `data_get($gateResult, 'repo_digest')` when it is a string.
fn repo_digest_of(gate_result: Option<&Value>) -> Option<String> {
    gate_result?
        .get("repo_digest")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The image `pinBypassedImage()` starts: the stored digest reference, when it is a digest of
/// the configured image.
fn bypassed_pin(pinned: &str, production_image_name: &str) -> Option<String> {
    let reference = ImageRef::parse(pinned).ok()?;
    let configured = ImageRef::parse(production_image_name).ok()?;
    (reference.digest.is_some() && reference.qualified_name() == configured.qualified_name())
        .then(|| reference.to_string())
}

/// The command of `inspectImageForScan()`.
fn inspect_command(image_ref: &str) -> GateCommand {
    let format = quote("{{.Id}} {{range .RepoDigests}}{{.}} {{end}}");
    GateCommand::new(format!(
        "docker image inspect --format {format} {} 2>/dev/null",
        quote(image_ref)
    ))
    .hidden()
    .ignore_errors()
    .save(INSPECT_OUTPUT, false)
}

/// The output of [`inspect_command`]: the image ID, then the repo digests. `None` when the image
/// is not on the server.
fn parse_inspect_output(output: &str, image: &ImageRef) -> Option<InspectedForScan> {
    let mut parts = output.split_whitespace();
    let id = parts.next().filter(|id| ImageRef::is_valid_digest(id))?;
    let repo_digest = parts
        .filter_map(|candidate| ImageRef::parse(candidate).ok())
        .find(|parsed| parsed.digest.is_some() && parsed.qualified_name() == image.qualified_name())
        .map(|parsed| parsed.to_string());
    Some(InspectedForScan {
        id: id.to_owned(),
        repo_digest,
    })
}

/// The commands of `writeComposeFileToWorkdir()`.
fn compose_write_commands(deployment_uuid: &str, path: &str, yaml: String) -> Vec<GateCommand> {
    vec![
        GateCommand::new(format!("docker exec {deployment_uuid} rm -f {path}")).hidden(),
        GateCommand::new(format!(
            "docker exec -i {deployment_uuid} tee {path} > /dev/null"
        ))
        .input(yaml)
        .hidden(),
    ]
}

/// `Yaml::parse($yaml)`; the error is the message of the `ParseException`.
fn parse_compose(yaml: &str) -> Result<PhpValue, String> {
    coolify_compat::yaml_parse::parse(yaml, YAML_MAX_ALIASES).map_err(|e| e.message())
}

/// `$array[$key] ?? null` of a PHP array.
fn array_get<'a>(value: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    match value {
        PhpValue::Array(items) => items
            .iter()
            .find(|(k, _)| k.as_string() == key)
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, PhpValue::Null)),
        _ => None,
    }
}

/// `collect(data_get($compose, 'services', []))`: a value that is not an array is one item.
fn compose_services(compose: &PhpValue) -> Vec<&PhpValue> {
    match array_get(compose, "services") {
        Some(PhpValue::Array(services)) => services.iter().map(|(_, service)| service).collect(),
        Some(other) => vec![other],
        None => Vec::new(),
    }
}

/// `data_get($service, 'image')` when it is a string.
fn service_image(service: &PhpValue) -> Option<&str> {
    match array_get(service, "image") {
        Some(PhpValue::Str(image)) => Some(image.as_str()),
        _ => None,
    }
}

/// What `checkComposeGate()` reads of the Compose file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposeGateImages {
    /// The images of the services, each once, in the order of the file.
    images: Vec<String>,
    /// A service has no image (a raw Compose service that is built).
    service_without_image: bool,
}

fn compose_gate_images(yaml: &str) -> Result<ComposeGateImages, String> {
    let compose = parse_compose(yaml)?;
    let services = compose_services(&compose);
    let mut images: Vec<String> = Vec::new();
    for image in services.iter().filter_map(|service| service_image(service)) {
        if !image.is_empty() && !images.iter().any(|known| known == image) {
            images.push(image.to_owned());
        }
    }
    Ok(ComposeGateImages {
        images,
        service_without_image: services
            .iter()
            .any(|service| service_image(service).is_none()),
    })
}

/// The Compose file of `checkComposeGate()` with `pull_policy: never` for every service whose
/// image the gate checked, so Compose cannot pull a different image for it.
fn with_pull_policy_never(yaml: &str, checked_images: &[String]) -> Result<String, String> {
    let mut compose = parse_compose(yaml)?;
    if let Some(PhpValue::Array(services)) = compose.get_mut("services") {
        for (_, service) in services.iter_mut() {
            let checked = service_image(service)
                .is_some_and(|image| checked_images.iter().any(|checked| checked == image));
            if checked {
                service.set("pull_policy", PhpValue::Str("never".to_owned()));
            }
        }
    }
    Ok(yaml::dump(&compose, 10))
}

/// The Compose file of `pinCompose()` with the image of the service `container`. `None` when the
/// file has no such service.
fn compose_with_image(yaml: &str, container: &str, image: &str) -> Result<Option<String>, String> {
    let mut compose = parse_compose(yaml)?;
    let Some(PhpValue::Array(services)) = compose.get_mut("services") else {
        return Ok(None);
    };
    let Some((_, service)) = services.iter_mut().find(|(name, service)| {
        name.as_string() == container && !matches!(service, PhpValue::Null)
    }) else {
        return Ok(None);
    };
    if !matches!(service, PhpValue::Array(_)) {
        return Err("Cannot use a scalar value as an array".to_owned());
    }
    service.set("image", PhpValue::Str(image.to_owned()));
    Ok(Some(yaml::dump(&compose, 10)))
}

/// The skip rule of `dispatchScanAfterStart()`: restarts, skipped builds, and additional servers
/// run an image that was scanned before, except for a rollback.
fn skips_scan_after_start(skip_build: bool, is_additional_server: bool, rollback: bool) -> bool {
    (skip_build || is_additional_server) && !rollback
}

/// The images `dispatchScanAfterStart()` scans: the started images that the gate did not scan
/// before the start.
fn images_after_start(
    is_compose: bool,
    generated_docker_compose: &str,
    production_image_name: Option<&str>,
    pinned_image: Option<&str>,
    scanned_images: &[String],
) -> Result<Vec<String>, String> {
    let scanned = |image: &str| scanned_images.iter().any(|known| known == image);
    if is_compose {
        let compose = parse_compose(generated_docker_compose)?;
        let mut images: Vec<String> = Vec::new();
        for image in compose_services(&compose)
            .into_iter()
            .filter_map(service_image)
        {
            if !image.is_empty() && !scanned(image) && !images.iter().any(|known| known == image) {
                images.push(image.to_owned());
            }
        }
        return Ok(images);
    }
    Ok(match production_image_name {
        Some(image_name) if !scanned(image_name) => {
            vec![pinned_image.unwrap_or(image_name).to_owned()]
        }
        _ => Vec::new(),
    })
}

/// `data_get($overrides, $serviceName)`: a dot in the name walks into nested arrays, like
/// upstream's `data_get`.
fn data_get<'a>(target: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let mut segments = key.split('.');
    let mut current = target.get(segments.next()?)?;
    for segment in segments {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(list) => list.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current).filter(|value| !value.is_null())
}

/// `Application::composeImageOverride($serviceName, $composeImage)`: the image that a security
/// update set for a Compose service, while the compose file still names the image it replaced.
fn compose_image_override<'a>(
    overrides: &'a Map<String, Value>,
    service_name: &str,
    compose_image: Option<&str>,
) -> Option<&'a str> {
    let stored = data_get(overrides, service_name)?;
    let from = stored.get("from")?.as_str()?;
    let to = stored.get("to")?.as_str()?;
    (php_trim(compose_image?) == php_trim(from)).then_some(to)
}

/// `Application::activeComposeImageOverrides()`: the overrides that still apply to the compose
/// file, keyed by the service name.
fn active_compose_image_overrides(
    overrides: &Map<String, Value>,
    docker_compose_raw: &str,
) -> Map<String, Value> {
    let compose = parse_compose(docker_compose_raw).ok();
    let services = compose
        .as_ref()
        .and_then(|compose| array_get(compose, "services"));
    let mut active = Map::new();
    for service_name in overrides.keys() {
        let image = services
            .and_then(|services| array_get(services, service_name))
            .and_then(service_image);
        if compose_image_override(overrides, service_name, image).is_some()
            && let Some(stored) = overrides.get(service_name)
        {
            active.insert(
                service_name.clone(),
                json!({
                    "from": stored.get("from").and_then(Value::as_str).unwrap_or_default(),
                    "to": stored.get("to").and_then(Value::as_str).unwrap_or_default(),
                }),
            );
        }
    }
    active
}

/// `Application::forgetStaleComposeImageOverrides()` without the write: the new value of
/// `docker_compose_image_overrides` when some overrides no longer apply (`Some(None)` for null).
/// `None` when nothing changes: no overrides, a compose file that cannot be read (they are all
/// kept), or all of them still apply.
fn stale_compose_image_overrides(
    stored: Option<&Value>,
    docker_compose_raw: &str,
) -> Option<Option<Value>> {
    let Some(Value::Object(overrides)) = stored else {
        return None;
    };
    if overrides.is_empty() {
        return None;
    }
    let compose = parse_compose(docker_compose_raw).ok()?;
    if !matches!(array_get(&compose, "services"), Some(PhpValue::Array(_))) {
        return None;
    }

    let active = active_compose_image_overrides(overrides, docker_compose_raw);
    if active.len() == overrides.len() {
        return None;
    }
    Some((!active.is_empty()).then_some(Value::Object(active)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATE_IMAGE_ID: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GATE_REPO_DIGEST: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GATE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn image(reference: &str) -> ImageRef {
        ImageRef::parse(reference).unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// The image of a service of a Compose file.
    fn image_of(yaml: &str, service: &str) -> Option<String> {
        let compose = parse_compose(yaml).unwrap();
        array_get(&compose, "services")
            .and_then(|services| array_get(services, service))
            .and_then(service_image)
            .map(str::to_owned)
    }

    /// `pull_policy` of a service of a Compose file.
    fn pull_policy_of(yaml: &str, service: &str) -> Option<String> {
        let compose = parse_compose(yaml).unwrap();
        let policy = array_get(&compose, "services")
            .and_then(|services| array_get(services, service))
            .and_then(|service| array_get(service, "pull_policy"));
        match policy {
            Some(PhpValue::Str(policy)) => Some(policy.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_command_appends_its_output_unless_it_says_otherwise() {
        let plain = GateCommand::new("docker pull 'nginx:1.27'").hidden();
        assert!(plain.hidden && plain.append && !plain.ignore_errors);
        assert_eq!(plain.save, None);
        assert_eq!(plain.input, None);
    }

    #[test]
    fn inspects_the_image_with_one_hidden_command() {
        let command = inspect_command("nginx:1.27");
        assert_eq!(
            command.command,
            "docker image inspect --format '{{.Id}} {{range .RepoDigests}}{{.}} {{end}}' 'nginx:1.27' 2>/dev/null"
        );
        assert!(command.hidden && command.ignore_errors && !command.append);
        assert_eq!(command.save.as_deref(), Some("image_scan_inspect"));
    }

    #[test]
    fn reads_the_image_id_and_the_repo_digest_of_the_inspected_image() {
        // A Docker image: the digest of the configured repository is pinned.
        assert_eq!(
            parse_inspect_output(
                &format!("{GATE_IMAGE_ID} nginx@{GATE_REPO_DIGEST} "),
                &image("nginx:1.27")
            ),
            Some(InspectedForScan {
                id: GATE_IMAGE_ID.to_owned(),
                repo_digest: Some(format!("nginx@{GATE_REPO_DIGEST}")),
            })
        );
        // An image pushed from a build server.
        assert_eq!(
            parse_inspect_output(
                &format!("{GATE_IMAGE_ID} ghcr.io/team/app@{GATE_REPO_DIGEST} \n"),
                &image(&format!("ghcr.io/team/app:{GATE_COMMIT}"))
            )
            .and_then(|inspected| inspected.repo_digest),
            Some(format!("ghcr.io/team/app@{GATE_REPO_DIGEST}"))
        );
        // A built image without a repo digest is gated by its local image ID.
        assert_eq!(
            parse_inspect_output(&format!("{GATE_IMAGE_ID} "), &image("appuuid:abc")),
            Some(InspectedForScan {
                id: GATE_IMAGE_ID.to_owned(),
                repo_digest: None,
            })
        );
        // The digest of another repository is not the digest of this image.
        assert_eq!(
            parse_inspect_output(
                &format!("{GATE_IMAGE_ID} mirror.example.com/nginx@{GATE_REPO_DIGEST} not/a ref"),
                &image("nginx:1.27")
            )
            .and_then(|inspected| inspected.repo_digest),
            None
        );
        // The image is not on the server.
        assert_eq!(parse_inspect_output("", &image("nginx:1.27")), None);
        assert_eq!(
            parse_inspect_output("Error: No such image", &image("nginx:1.27")),
            None
        );
    }

    #[test]
    fn the_gate_waits_for_the_lock_while_the_deployment_keeps_time_for_its_scan() {
        // The default limits: reserve 1920 s, lock 5280 s, deployment timeout 3600 s.
        assert_eq!(
            gate_lock_wait_seconds(Some(1000), 1000, 3600, 1920, 5280),
            1680
        );
        // No start time: nothing elapsed.
        assert_eq!(gate_lock_wait_seconds(None, 1000, 3600, 1920, 5280), 1680);
        // The lock expires before the deployment times out.
        assert_eq!(
            gate_lock_wait_seconds(Some(1000), 1000, 36_000, 1920, 5280),
            5280
        );
        // At least one lock wait, also late in the deployment.
        assert_eq!(
            gate_lock_wait_seconds(Some(1000), 4000, 3600, 1920, 5280),
            120
        );
        // A clock that went back does not add time.
        assert_eq!(
            gate_lock_wait_seconds(Some(2000), 1000, 3600, 1920, 5280),
            1680
        );
    }

    #[test]
    fn the_gate_waits_in_steps_and_stops_at_the_deadline() {
        assert_eq!(lock_wait_step(1680.0), (120, false));
        assert_eq!(lock_wait_step(120.4), (120, false));
        assert_eq!(lock_wait_step(120.0), (120, true));
        assert_eq!(lock_wait_step(60.2), (61, true));
        assert_eq!(lock_wait_step(0.5), (1, true));
        assert_eq!(lock_wait_step(-3.0), (1, true));
    }

    #[test]
    fn scanning_off_and_custom_commands_stop_the_gate_before_the_scan() {
        assert_eq!(
            error_before_scan_code(false, true, false),
            Some(ErrorCode::BuildServerOff)
        );
        assert_eq!(
            error_before_scan_code(false, false, true),
            Some(ErrorCode::ScanningOff)
        );
        assert_eq!(
            error_before_scan_code(true, false, true),
            Some(ErrorCode::CustomCommand)
        );
        assert_eq!(error_before_scan_code(true, true, false), None);
        // The messages of the blocked deploys.
        assert_eq!(
            ErrorCode::BuildServerOff.message(&server_context("build-server".to_owned())),
            "Image scanning is off on the build server."
        );
        assert_eq!(
            ErrorCode::CustomCommand.message(&server_context("main-server".to_owned())),
            "Coolify cannot check the image because this application uses a custom build or start command."
        );
        assert_eq!(
            ErrorCode::LockBusy.message(&server_context("main-server".to_owned())),
            "Another scan is still running on main-server."
        );
    }

    #[test]
    fn a_rollback_to_head_or_without_a_commit_is_gated() {
        assert!(is_rollback_commit(GATE_COMMIT));
        assert!(!is_rollback_commit("HEAD"));
        assert!(!is_rollback_commit(""));
        assert!(!is_rollback_commit("  "));
    }

    #[test]
    fn keeps_the_worst_gate_result_of_a_compose_deploy() {
        let pass = json!({"blocked": false, "scan_ran": true, "error": null});
        let error = json!({"blocked": false, "scan_ran": false, "error": "timeout"});
        let blocked = json!({"blocked": true, "scan_ran": true, "error": null});

        assert_eq!(gate_rank(None), -1);
        assert_eq!(gate_rank(Some(&pass)), 0);
        assert_eq!(gate_rank(Some(&error)), 1);
        assert_eq!(gate_rank(Some(&blocked)), 2);
        // The first image could not be scanned, the second passed: the error stays.
        assert!(gate_rank(Some(&pass)) < gate_rank(Some(&error)));
        // A later result of the same rank replaces the earlier one.
        assert!(gate_rank(Some(&pass)) >= gate_rank(Some(&pass)));
    }

    #[test]
    fn stores_the_digest_and_the_image_id_with_the_gate_result() {
        let stored = json!({"blocked": true, "count": 1});
        assert_eq!(
            stored_gate_result(
                stored.clone(),
                Some("nginx@sha256:bbbb"),
                Some(GATE_IMAGE_ID)
            ),
            json!({"blocked": true, "count": 1, "repo_digest": "nginx@sha256:bbbb", "image_id": GATE_IMAGE_ID})
        );
        // A scan that never inspected the image has no image ID.
        assert_eq!(stored_gate_result(stored.clone(), None, Some("")), stored);
        assert_eq!(stored_gate_result(stored.clone(), None, None), stored);
    }

    #[test]
    fn a_pass_logs_its_first_line_as_output_and_the_rest_as_warnings() {
        assert_eq!(log_kind(false, 0), "stdout");
        assert_eq!(log_kind(false, 1), "stderr");
        assert_eq!(log_kind(true, 0), "stderr");
        assert_eq!(log_kind(true, 3), "stderr");
    }

    #[test]
    fn deploy_anyway_starts_the_blocked_image_by_its_digest() {
        let pinned = format!("nginx@{GATE_REPO_DIGEST}");
        assert_eq!(bypassed_pin(&pinned, "nginx:latest"), Some(pinned.clone()));
        // The image was changed since the block.
        assert_eq!(bypassed_pin(&pinned, "redis:7"), None);
        // Not a digest reference, or not an image reference.
        assert_eq!(bypassed_pin("nginx:1.27", "nginx:latest"), None);
        assert_eq!(bypassed_pin("not an image", "nginx:latest"), None);
        assert_eq!(
            repo_digest_of(Some(&json!({"repo_digest": pinned}))),
            Some(pinned)
        );
        assert_eq!(repo_digest_of(Some(&json!({"repo_digest": 5}))), None);
        assert_eq!(repo_digest_of(Some(&json!({"blocked": true}))), None);
        assert_eq!(repo_digest_of(None), None);
    }

    #[test]
    fn pins_the_scanned_image_in_the_compose_file_of_the_application() {
        let compose = "services:\n    gate-app:\n        image: 'nginx:1.27'\n        container_name: gate-app\n";
        let pinned = format!("nginx@{GATE_REPO_DIGEST}");

        let yaml = compose_with_image(compose, "gate-app", &pinned)
            .unwrap()
            .unwrap();
        assert_eq!(image_of(&yaml, "gate-app"), Some(pinned.clone()));
        assert!(yaml.contains("nginx@sha256:"));
        assert!(yaml.contains("container_name: gate-app"));

        // The file has no such service.
        assert_eq!(compose_with_image(compose, "other", &pinned), Ok(None));
        assert_eq!(
            compose_with_image("version: '3'\n", "gate-app", &pinned),
            Ok(None)
        );
        // A file that is not valid YAML fails the deployment.
        assert!(compose_with_image("services: [\n  web", "gate-app", &pinned).is_err());
    }

    #[test]
    fn reads_the_images_of_a_compose_deploy_each_once() {
        let compose = "services:\n    web:\n        image: 'nginx:1.27'\n    cache:\n        image: 'redis:7'\n    web2:\n        image: 'nginx:1.27'\n";
        assert_eq!(
            compose_gate_images(compose),
            Ok(ComposeGateImages {
                images: strings(&["nginx:1.27", "redis:7"]),
                service_without_image: false,
            })
        );

        // A raw Compose service without an image gets a name that Coolify does not know.
        let built = "services:\n    web:\n        build: .\n";
        assert_eq!(
            compose_gate_images(built),
            Ok(ComposeGateImages {
                images: Vec::new(),
                service_without_image: true,
            })
        );
        assert_eq!(
            compose_gate_images("version: '3'\n"),
            Ok(ComposeGateImages {
                images: Vec::new(),
                service_without_image: false,
            })
        );
    }

    #[test]
    fn stops_compose_from_pulling_the_scanned_images() {
        let compose = format!(
            "services:\n    web:\n        image: 'nginx:1.27'\n    worker:\n        image: 'appuuid_worker:{GATE_COMMIT}'\n        build:\n            context: .\n    cache:\n        image: 'redis:7'\n"
        );
        let checked = strings(&["nginx:1.27", &format!("appuuid_worker:{GATE_COMMIT}")]);

        let yaml = with_pull_policy_never(&compose, &checked).unwrap();

        assert_eq!(pull_policy_of(&yaml, "web").as_deref(), Some("never"));
        assert_eq!(pull_policy_of(&yaml, "worker").as_deref(), Some("never"));
        // An image the gate could not inspect is pulled as usual.
        assert_eq!(pull_policy_of(&yaml, "cache"), None);
        assert_eq!(image_of(&yaml, "web").as_deref(), Some("nginx:1.27"));
    }

    #[test]
    fn writes_the_compose_file_again_with_the_commands_of_the_first_write() {
        let commands = compose_write_commands(
            "gate-deployment",
            "/artifacts/gate-deployment/docker-compose.yaml",
            "services: {}\n".to_owned(),
        );
        assert_eq!(
            commands,
            vec![
                GateCommand::new(
                    "docker exec gate-deployment rm -f /artifacts/gate-deployment/docker-compose.yaml"
                )
                .hidden(),
                GateCommand::new(
                    "docker exec -i gate-deployment tee /artifacts/gate-deployment/docker-compose.yaml > /dev/null"
                )
                .input("services: {}\n")
                .hidden(),
            ]
        );
    }

    #[test]
    fn does_not_queue_a_scan_after_a_skipped_build_or_on_an_additional_server() {
        assert!(skips_scan_after_start(true, false, false));
        assert!(skips_scan_after_start(false, true, false));
        assert!(!skips_scan_after_start(false, false, false));
        // A rollback that skipped the build is scanned.
        assert!(!skips_scan_after_start(true, false, true));
        assert!(!skips_scan_after_start(false, true, true));
    }

    #[test]
    fn scans_after_the_start_what_the_gate_did_not_scan_before_it() {
        let built = format!("appuuid:{GATE_COMMIT}");
        // The gate is off: the started image is scanned after the deploy.
        assert_eq!(
            images_after_start(false, "", Some(&built), None, &[]),
            Ok(vec![built.clone()])
        );
        // The gate scanned it before the start.
        assert_eq!(
            images_after_start(false, "", Some(&built), None, std::slice::from_ref(&built)),
            Ok(Vec::new())
        );
        // The gate pinned the image, but its scan could not run: the pinned image is scanned.
        let pinned = format!("ghcr.io/team/app@{GATE_REPO_DIGEST}");
        assert_eq!(
            images_after_start(false, "", Some(&built), Some(&pinned), &[]),
            Ok(vec![pinned])
        );
        // The image was never named.
        assert_eq!(
            images_after_start(false, "", None, None, &[]),
            Ok(Vec::new())
        );

        // A Compose image whose gate scan could not run is scanned after the start.
        let compose = "services:\n    web:\n        image: 'nginx:1.27'\n    cache:\n        image: 'redis:7'\n    cache2:\n        image: 'redis:7'\n    job:\n        build: .\n";
        assert_eq!(
            images_after_start(
                true,
                compose,
                Some("ignored"),
                None,
                &strings(&["nginx:1.27"])
            ),
            Ok(strings(&["redis:7"]))
        );
        assert_eq!(
            images_after_start(true, compose, None, None, &[]),
            Ok(strings(&["nginx:1.27", "redis:7"]))
        );
    }

    #[test]
    fn a_custom_build_command_keeps_the_scanned_builds_for_later() {
        assert!(truthy(Some("docker compose build")));
        assert!(!truthy(Some("")));
        assert!(!truthy(Some("0")));
        assert!(!truthy(None));
        assert!(filled(Some("docker compose up -d")));
        assert!(filled(Some("0")));
        assert!(!filled(Some("  ")));
        assert!(!filled(None));
    }

    fn overrides() -> Value {
        json!({
            "web": {"from": "nginx:1.27", "to": "nginx:1.29"},
            "db": {"from": "postgres:16", "to": "postgres:16.4"},
        })
    }

    #[test]
    fn drops_an_override_whose_image_the_compose_file_no_longer_names() {
        let raw = "services:\n  web:\n    image: 'nginx:1.30'\n  db:\n    image: 'postgres:16'\n";
        let kept = json!({"db": {"from": "postgres:16", "to": "postgres:16.4"}});

        assert_eq!(
            stale_compose_image_overrides(Some(&overrides()), raw),
            Some(Some(kept.clone()))
        );
        assert_eq!(
            coolify_compat::php_json::encode(&kept),
            r#"{"db":{"from":"postgres:16","to":"postgres:16.4"}}"#
        );

        // So it never turns on again when a later compose file names that image again.
        let again = "services:\n  web:\n    image: 'nginx:1.27'\n";
        let kept = kept.as_object().cloned().unwrap();
        assert!(active_compose_image_overrides(&kept, again).is_empty());
        // The last override is gone: the column is null.
        assert_eq!(
            stale_compose_image_overrides(Some(&Value::Object(kept)), again),
            Some(None)
        );
    }

    #[test]
    fn keeps_the_overrides_that_apply_and_those_of_a_compose_file_that_is_not_valid() {
        // All of them still apply.
        let raw = "services:\n  web:\n    image: 'nginx:1.27'\n  db:\n    image: ' postgres:16 '\n";
        assert_eq!(stale_compose_image_overrides(Some(&overrides()), raw), None);
        // A compose file that is not valid, or has no services.
        assert_eq!(
            stale_compose_image_overrides(Some(&overrides()), "services: [\n  web"),
            None
        );
        assert_eq!(
            stale_compose_image_overrides(Some(&overrides()), "version: '3'\n"),
            None
        );
        // No overrides.
        assert_eq!(stale_compose_image_overrides(None, raw), None);
        assert_eq!(stale_compose_image_overrides(Some(&Value::Null), raw), None);
        assert_eq!(stale_compose_image_overrides(Some(&json!({})), raw), None);
    }

    #[test]
    fn an_override_applies_while_the_compose_file_names_the_replaced_image() {
        let stored = overrides();
        let stored = stored.as_object().unwrap();
        assert_eq!(
            compose_image_override(stored, "web", Some(" nginx:1.27 ")),
            Some("nginx:1.29")
        );
        assert_eq!(
            compose_image_override(stored, "web", Some("nginx:1.30")),
            None
        );
        assert_eq!(compose_image_override(stored, "web", None), None);
        assert_eq!(
            compose_image_override(stored, "other", Some("nginx:1.27")),
            None
        );
        // An override that is not `{from, to}` never applies.
        let broken = json!({"web": {"from": "nginx:1.27"}, "db": "postgres:16"});
        let broken = broken.as_object().unwrap();
        assert_eq!(
            compose_image_override(broken, "web", Some("nginx:1.27")),
            None
        );
        assert_eq!(
            compose_image_override(broken, "db", Some("postgres:16")),
            None
        );
    }

    #[test]
    fn the_gate_stop_carries_the_message_of_the_exception() {
        let blocked: GateStop<()> = GateStop::Blocked(
            "Deploy blocked: this image has a security problem that attackers are using."
                .to_owned(),
        );
        assert_eq!(
            blocked.message(),
            Some("Deploy blocked: this image has a security problem that attackers are using.")
        );
        assert!(blocked.is_message_already_logged());

        let failed: GateStop<()> = GateStop::Failed("A YAML file cannot be parsed.".to_owned());
        assert!(!failed.is_message_already_logged());
        assert_eq!(GateStop::Deployment(()).message(), None);
    }
}
