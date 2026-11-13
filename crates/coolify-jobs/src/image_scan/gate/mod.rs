//! The policy gate ("Block deploys", spec 15.8) and the image scan steps of an application
//! deployment.
//!
//! | Module | Ports |
//! |---|---|
//! | [`security_gate`] | `SecurityGate`: `evaluate`, `log_lines`, `stored`. Pure. |
//! | [`deployment`] | `DeploymentImageScan`, and [`DeploymentContext`] for what it reads of the job |
//!
//! # Guide for the deployment job
//!
//! `ApplicationDeploymentJob::imageScan()` creates one `DeploymentImageScan` per job, lazily.
//! Here: `DeploymentImageScan::load(&jobs, application.id, queue_id).await` once per deployment,
//! kept in the state of the run.
//!
//! The job implements [`DeploymentContext`] (upstream's `read`, `write`, `run` and
//! `checkForCancellation` closures): getters for the job properties named in `imageScan()`,
//! setters for `docker_compose` and `generated_docker_compose`, `add_log_entry`,
//! `execute_remote_command` (every [`GateCommand`] of one call in one batch, with its `hidden`,
//! `ignore_errors`, `save`, `append` and `input`), and `check_for_cancellation`. Every step takes
//! the context as `&mut`, so when the run itself implements the trait, take the
//! `DeploymentImageScan` out of the run for the call (`Option::take`) and put it back, or
//! implement the trait on a small wrapper.
//!
//! The steps that can stop the deployment return [`GateStop`]:
//! `Blocked(message)` is `DeploymentException::alreadyLogged($message)` (fail the deployment with
//! exactly this message; it is already a visible log line, do not log it again), `Failed(message)`
//! is any other exception (fail with the message), `Deployment(e)` is the job's own error from
//! the context (a failed command, a cancel): pass it on unchanged.
//!
//! | Upstream call site | Call |
//! |---|---|
//! | `rolling_update()`, first two lines | `apply_start_pin(ctx).await?`, then `check_gate(ctx, &production_image_name).await?` |
//! | `start_by_compose_file()`, build pack `dockerimage` | when `pinned_image().is_some() \|\| start_pinned()`: run only the `up --build -d` command, without the "Pulling latest images from the registry." line and the `docker compose … pull` |
//! | `start_by_compose_file()`, build server | `--pull always ` is left out of `up` when `pinned_image().is_some()` |
//! | `deploy_docker_compose_buildpack()`, after `loadComposeFileForDeployment()` | `forget_stale_compose_image_overrides(ctx).await` |
//! | `deploy_docker_compose_buildpack()`, after `pull_docker_compose_images()`, before `startDeployPhase()` | `apply_compose_start_pins(ctx).await?`, then `check_compose_gate(ctx).await?` |
//! | `deploy_simple_dockerfile()` and `deploy_dockerfile_buildpack()` after `build_image()`, `deploy_pull_request()` after `build_pull_request_image()` | `remember_dockerfile_base_image(ctx).await` |
//! | `post_deployment()`, right after `completeDeployment()` | `link_reused_scans(ctx).await` |
//! | `post_deployment()`, last two lines | `dispatch_scan_after_start(ctx).await`, then `refresh_security(ctx).await` |
//! | `just_restart()`, after `completeDeployment()` | `link_reused_scans(ctx).await` |
//!
//! `gate_applies(ctx)` is public like upstream's `gateApplies()`; the job does not call it.
//! `base_image_ref()` is the base image `remember_dockerfile_base_image` found.
//!
//! The gate saves the output of its `docker image inspect` under the key
//! [`deployment::INSPECT_OUTPUT`] of `saved_outputs` and reads it back with
//! `DeploymentContext::saved_output`; `remember_dockerfile_base_image` reads the key
//! `dockerfile_from_repo`.

pub mod deployment;
pub mod security_gate;

pub use deployment::{DeploymentContext, DeploymentImageScan, GateCommand, GateStop};
