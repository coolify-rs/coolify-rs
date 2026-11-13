//! `ApplicationDeploymentJob::imageScan()`: the image scan steps of a deployment (the deploy gate,
//! the start pins of security updates, the scan after the start). The job is the
//! [`DeploymentContext`] of its [`DeploymentImageScan`], which is created on first use and kept in
//! [`Run::image_scan`]. The guide in [`crate::image_scan::gate`] lists the upstream call sites.

use super::{Cmd, Fail, Run, Step};
use crate::image_scan::gate::{DeploymentContext, DeploymentImageScan, GateCommand, GateStop};

/// One command of the gate as a command of `execute_remote_command()`.
fn command_of(gate: GateCommand) -> Cmd {
    let mut cmd = Cmd::new(gate.command);
    cmd.hidden = gate.hidden;
    cmd.ignore_errors = gate.ignore_errors;
    if let Some(key) = gate.save {
        cmd = cmd.save(key, gate.append);
    }
    cmd.input = gate.input;
    cmd
}

/// How a step of the gate fails the deployment: `DeploymentException::alreadyLogged($message)`
/// for a blocked deploy, a `DeploymentException` with the message otherwise, the job's own
/// failure unchanged.
fn failure(stop: GateStop<Fail>) -> Fail {
    match stop {
        GateStop::Blocked(message) => Fail::Error {
            message,
            code: 0,
            logged: true,
        },
        GateStop::Failed(message) => Fail::new(message),
        GateStop::Deployment(fail) => fail,
    }
}

impl DeploymentContext for Run {
    type Error = Fail;

    fn server_id(&self) -> i64 {
        self.server.id
    }

    fn server_name(&self) -> String {
        self.server_name.clone()
    }

    fn server_docker_config_prefix(&self) -> String {
        super::servers_multi::docker_config_prefix(&self.server).to_owned()
    }

    fn main_server_id(&self) -> i64 {
        self.main_server.id
    }

    fn main_server_is_swarm(&self) -> bool {
        self.main_server.is_swarm()
    }

    fn build_server_id(&self) -> Option<i64> {
        self.build_server.as_ref().map(|(server, _, _)| server.id)
    }

    fn use_build_server(&self) -> bool {
        self.use_build_server
    }

    fn deployment_uuid(&self) -> String {
        self.dep.clone()
    }

    fn pull_request_id(&self) -> i64 {
        self.b.pull_request_id
    }

    fn commit(&self) -> String {
        self.commit.clone()
    }

    fn rollback(&self) -> bool {
        self.b.rollback
    }

    fn skip_build(&self) -> bool {
        self.b.skip_build
    }

    fn is_this_additional_server(&self) -> bool {
        self.is_additional
    }

    fn container_name(&self) -> String {
        self.container.clone()
    }

    fn production_image_name(&self) -> Option<String> {
        Some(self.b.production_image.clone()).filter(|name| !name.is_empty())
    }

    /// `$this->docker_compose`: the job keeps one Compose file, which is both the file it
    /// generated and the file it writes (upstream sets both to the same text in
    /// `generate_compose_file()`).
    fn docker_compose(&self) -> String {
        self.generated_compose.clone().unwrap_or_default()
    }

    fn generated_docker_compose(&self) -> String {
        self.generated_compose.clone().unwrap_or_default()
    }

    fn docker_compose_custom_start_command(&self) -> Option<String> {
        self.b.compose_start_command.clone()
    }

    fn docker_compose_custom_build_command(&self) -> Option<String> {
        self.b.compose_build_command.clone()
    }

    fn workdir(&self) -> String {
        self.workdir.clone()
    }

    fn docker_compose_location(&self) -> String {
        self.b.compose_location.clone()
    }

    fn saved_output(&self, key: &str) -> Option<String> {
        self.saved.get(key).cloned()
    }

    fn timeout(&self) -> i64 {
        self.timeout
    }

    fn started_at(&self) -> Option<i64> {
        self.started_at
    }

    fn set_docker_compose(&mut self, yaml: String) {
        self.generated_compose = Some(yaml);
    }

    fn set_generated_docker_compose(&mut self, yaml: String) {
        self.generated_compose = Some(yaml);
    }

    async fn add_log_entry(&mut self, message: &str, kind: &'static str) {
        self.log(message, kind, false).await;
    }

    async fn execute_remote_command(&mut self, commands: Vec<GateCommand>) -> Result<(), Fail> {
        self.exec(commands.into_iter().map(command_of).collect())
            .await
    }

    async fn check_for_cancellation(&mut self) -> Result<(), Fail> {
        self.check_cancelled().await
    }
}

impl Run {
    /// `$this->imageScan()`: takes the scan steps of this deployment out of the run, created on
    /// first use. Every caller puts them back with [`Self::put_image_scan`].
    async fn take_image_scan(&mut self) -> Result<DeploymentImageScan, String> {
        match self.image_scan.take() {
            Some(scan) => Ok(scan),
            None => DeploymentImageScan::load(&self.jobs, self.app.id, self.queue_id).await,
        }
    }

    fn put_image_scan(&mut self, scan: DeploymentImageScan) {
        self.image_scan = Some(scan);
    }

    /// The steps that never fail the deployment cannot run when the scan steps cannot be loaded
    /// (the application or the deployment row is gone); this logs why.
    fn image_scan_unavailable(&self, step: &str, error: &str) {
        tracing::warn!(
            deployment = %self.dep,
            error = %error,
            "DeploymentImageScan::{step} skipped: the image scan steps could not be loaded"
        );
    }

    /// The first two lines of `rolling_update()`: `applyStartPin()`, then
    /// `checkGate($this->production_image_name)`.
    pub(super) async fn image_scan_start_pin_and_gate(&mut self) -> Step {
        let mut scan = self.take_image_scan().await.map_err(Fail::new)?;
        let mut result = scan.apply_start_pin(&mut *self).await;
        if result.is_ok() {
            let image = self.b.production_image.clone();
            result = scan.check_gate(&mut *self, &image).await;
        }
        self.put_image_scan(scan);
        result.map_err(failure)
    }

    /// `deploy_docker_compose_buildpack()` before `startDeployPhase()`: `applyComposeStartPins()`,
    /// then `checkComposeGate()`.
    pub(super) async fn image_scan_compose_start_pins_and_gate(&mut self) -> Step {
        let mut scan = self.take_image_scan().await.map_err(Fail::new)?;
        let mut result = scan.apply_compose_start_pins(&mut *self).await;
        if result.is_ok() {
            result = scan.check_compose_gate(&mut *self).await;
        }
        self.put_image_scan(scan);
        result.map_err(failure)
    }

    /// `forgetStaleComposeImageOverrides()`, after `loadComposeFileForDeployment()`.
    pub(super) async fn image_scan_forget_stale_compose_image_overrides(&mut self) {
        match self.take_image_scan().await {
            Ok(mut scan) => {
                scan.forget_stale_compose_image_overrides(&mut *self).await;
                self.put_image_scan(scan);
            }
            Err(e) => self.image_scan_unavailable("forgetStaleComposeImageOverrides", &e),
        }
    }

    /// `rememberDockerfileBaseImage()`, after the image is built.
    pub(super) async fn image_scan_remember_dockerfile_base_image(&mut self) {
        match self.take_image_scan().await {
            Ok(mut scan) => {
                scan.remember_dockerfile_base_image(&mut *self).await;
                self.put_image_scan(scan);
            }
            Err(e) => self.image_scan_unavailable("rememberDockerfileBaseImage", &e),
        }
    }

    /// `linkReusedScans()`, right after `completeDeployment()`.
    pub(super) async fn image_scan_link_reused_scans(&mut self) {
        match self.take_image_scan().await {
            Ok(mut scan) => {
                scan.link_reused_scans(&mut *self).await;
                self.put_image_scan(scan);
            }
            Err(e) => self.image_scan_unavailable("linkReusedScans", &e),
        }
    }

    /// The last two lines of `post_deployment()`: `dispatchScanAfterStart()`, then
    /// `refreshSecurity()`.
    pub(super) async fn image_scan_after_start(&mut self) {
        match self.take_image_scan().await {
            Ok(mut scan) => {
                scan.dispatch_scan_after_start(&mut *self).await;
                scan.refresh_security(&mut *self).await;
                self.put_image_scan(scan);
            }
            Err(e) => self.image_scan_unavailable("dispatchScanAfterStart", &e),
        }
    }

    /// `$this->imageScan()->pinnedImage() !== null`: the gate pinned the start to a repo digest.
    pub(super) fn image_scan_pinned(&self) -> bool {
        self.image_scan
            .as_ref()
            .is_some_and(|scan| scan.pinned_image().is_some())
    }

    /// `pinnedImage() !== null || startPinned()`: the gate pulled the image the start uses, so
    /// the start must not pull the tag again.
    pub(super) fn image_scan_skips_pull(&self) -> bool {
        self.image_scan
            .as_ref()
            .is_some_and(|scan| scan.pinned_image().is_some() || scan.start_pinned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gate_command_keeps_its_options() {
        let cmd = command_of(
            GateCommand::new("docker image inspect x")
                .hidden()
                .ignore_errors()
                .save("image_scan_inspect", false)
                .input("yaml"),
        );
        assert_eq!(cmd.command, "docker image inspect x");
        assert!(cmd.hidden);
        assert!(cmd.ignore_errors);
        assert_eq!(cmd.save.as_deref(), Some("image_scan_inspect"));
        assert!(!cmd.append);
        assert_eq!(cmd.input.as_deref(), Some("yaml"));
        assert!(!cmd.skip_command_log);

        let plain = command_of(GateCommand::new("true"));
        assert!(!plain.hidden && !plain.ignore_errors);
        assert!(plain.save.is_none() && plain.append);
    }

    #[test]
    fn a_blocked_deploy_fails_with_the_message_already_logged() {
        match failure(GateStop::Blocked("Deploy blocked".into())) {
            Fail::Error {
                message, logged, ..
            } => {
                assert_eq!(message, "Deploy blocked");
                assert!(logged);
            }
            Fail::Cancelled => panic!("not a cancel"),
        }
        match failure(GateStop::Failed("Database error".into())) {
            Fail::Error {
                message, logged, ..
            } => {
                assert_eq!(message, "Database error");
                assert!(!logged);
            }
            Fail::Cancelled => panic!("not a cancel"),
        }
        assert!(matches!(
            failure(GateStop::Deployment(Fail::Cancelled)),
            Fail::Cancelled
        ));
    }
}
