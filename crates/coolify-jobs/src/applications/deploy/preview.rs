//! Preview (pull request) deployments of `ApplicationDeploymentJob`: the constructor's preview
//! part, `deploy_pull_request()`, `previewImageTag()` and `previewVariableIdsWithProductionValues()`.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use coolify_db::application_ops;
use coolify_db::servers::ColumnValue;

use super::build::validate_path;
use super::{Fail, Run, Step, now};
use crate::applications::ops;
use crate::databases::env::Var;

/// The two lists of `warnAboutPreviewVariablesNotPassed()`: the preview variables that are
/// skipped because they still hold the production value (`previewVariableIdsWithProductionValues()`,
/// only when `skips_production_values`), and the production variables without a preview variable
/// that the preview does not get, with ` (at build time)` when only the build misses them.
pub(super) fn preview_variables_not_passed(
    variables: &[Var],
    skips_production_values: bool,
    gets_runtime_fallback: bool,
    passes_buildtime_variables: bool,
    compose: bool,
) -> (Vec<String>, Vec<String>) {
    let production: Vec<&Var> = variables.iter().filter(|v| !v.is_preview).collect();
    let preview: Vec<&Var> = variables.iter().filter(|v| v.is_preview).collect();
    let skipped = if skips_production_values {
        preview
            .iter()
            .filter(|p| {
                production
                    .iter()
                    .rfind(|v| v.key == p.key)
                    .is_some_and(|v| v.value == p.value)
            })
            .map(|p| p.key.clone())
            .collect()
    } else {
        Vec::new()
    };
    // isGeneratedDockerComposeEnvironmentVariable()
    let generated = |key: &str| {
        key.starts_with("SERVICE_FQDN_")
            || key.starts_with("SERVICE_URL_")
            || key.starts_with("SERVICE_NAME_")
    };
    let missing = production
        .iter()
        .filter(|v| !preview.iter().any(|p| p.key == v.key))
        .filter(|v| !(compose && generated(&v.key)))
        .filter_map(|v| {
            let missing_at_runtime = v.is_runtime && !gets_runtime_fallback;
            let missing_at_buildtime = v.is_buildtime && passes_buildtime_variables;
            if !missing_at_runtime && !missing_at_buildtime {
                return None;
            }
            Some(if v.is_runtime && !missing_at_runtime {
                format!("{} (at build time)", v.key)
            } else {
                v.key.clone()
            })
        })
        .collect();
    (skipped, missing)
}

/// A `domain_port_overrides` column.
fn stored_overrides(value: Option<&Value>) -> BTreeMap<String, i64> {
    let parsed = match value {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    let Some(Value::Object(o)) = parsed else {
        return BTreeMap::new();
    };
    o.iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

/// `previewImageTag($build)`.
pub(super) fn preview_image_tag(
    pull_request_id: i64,
    commit: &str,
    dep: &str,
    build: bool,
) -> String {
    let prefix = format!("pr-{pull_request_id}-");
    let suffix = if build { "-build" } else { "" };
    let max = 128usize.saturating_sub(prefix.len() + suffix.len()).max(1);
    let source = if commit == "HEAD" || commit.trim().is_empty() {
        dep
    } else {
        commit
    };
    let clean: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(max)
        .collect();
    let clean = if clean.is_empty() {
        "HEAD".to_owned()
    } else {
        clean
    };
    format!("{prefix}{clean}{suffix}")
}

impl Run {
    /// The preview part of the constructor: the row, its image tag and its domains.
    pub(super) async fn load_preview(&mut self) -> Step {
        let db = self.jobs.0.db.clone();
        let pull_request_id = self.b.pull_request_id;
        let Some(preview) = application_ops::preview(&db, self.app.id, pull_request_id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
        else {
            return Ok(());
        };
        if self.app.str("build_pack") == Some("dockerimage")
            && self.b.preview_tag.as_deref().is_none_or(str::is_empty)
        {
            self.b.preview_tag = preview
                .get("docker_registry_image_tag")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let id = preview
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if self.app.str("build_pack") == Some("dockercompose") {
            // generate_preview_fqdn_compose(keepExistingDomains: true)
            let raw = self.app.str("docker_compose_raw").unwrap_or_default();
            let names = ops::compose_service_names(raw);
            let existing = stored_overrides(preview.get("domain_port_overrides"));
            let generated = ops::preview_fqdn_compose_keeping(
                self.app.str("docker_compose_domains"),
                preview
                    .get("docker_compose_domains")
                    .and_then(Value::as_str),
                &names,
                self.app.str("preview_url_template").unwrap_or_default(),
                pull_request_id,
                &stored_overrides(self.app.raw.get("domain_port_overrides")),
                Some(&existing),
            );
            application_ops::update_preview(
                &db,
                id,
                &[
                    ("fqdn", ColumnValue::Text(generated.fqdn)),
                    (
                        "domain_port_overrides",
                        ColumnValue::Text(generated.overrides),
                    ),
                    (
                        "docker_compose_domains",
                        ColumnValue::Text(generated.compose),
                    ),
                ],
                now(),
            )
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
            self.b.preview = application_ops::preview(&db, self.app.id, pull_request_id)
                .await
                .map_err(|e| Fail::new(e.to_string()))?;
            return Ok(());
        }
        let fqdn = preview
            .get("fqdn")
            .and_then(Value::as_str)
            .filter(|f| !f.trim().is_empty());
        // generate_preview_fqdn(keepExistingDomains: true)
        let generated = match fqdn {
            Some(existing) => {
                let overrides = stored_overrides(preview.get("domain_port_overrides"));
                Some(ops::normalized_domains(Some(existing), &overrides))
            }
            None => {
                let overrides = stored_overrides(self.app.raw.get("domain_port_overrides"));
                ops::preview_fqdn(
                    self.app.str("fqdn"),
                    self.app.str("preview_url_template").unwrap_or_default(),
                    pull_request_id,
                    &overrides,
                )
                .map(|d| (d.fqdn, d.overrides))
            }
        };
        if let Some((new_fqdn, new_overrides)) = generated {
            let same_overrides = stored_overrides(preview.get("domain_port_overrides"))
                == stored_overrides(new_overrides.clone().map(Value::String).as_ref());
            if new_fqdn.as_deref() != fqdn || !same_overrides {
                application_ops::update_preview(
                    &db,
                    id,
                    &[
                        ("fqdn", ColumnValue::Text(new_fqdn)),
                        ("domain_port_overrides", ColumnValue::Text(new_overrides)),
                    ],
                    now(),
                )
                .await
                .map_err(|e| Fail::new(e.to_string()))?;
            }
        }
        self.b.preview = application_ops::preview(&db, self.app.id, pull_request_id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        Ok(())
    }

    /// `$this->preview->fqdn`, or `$this->application->fqdn` for a production deployment.
    pub(super) fn deployed_fqdn(&self) -> Option<String> {
        if self.b.is_preview() {
            self.b
                .preview
                .as_ref()
                .and_then(|p| p.get("fqdn"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            self.app.str("fqdn").map(str::to_owned)
        }
    }

    /// `$this->preview->docker_compose_domains`.
    pub(super) fn preview_compose_domains(&self) -> Option<String> {
        self.b
            .preview
            .as_ref()
            .and_then(|p| p.get("docker_compose_domains"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// `$this->preview?->keeps_production_values`: previews that existed before the upgrade to
    /// v4.5.3 (`2026_10_28_103619_add_keeps_production_values_to_application_previews_table`).
    pub(super) fn preview_keeps_production_values(&self) -> bool {
        self.b
            .preview
            .as_ref()
            .and_then(|p| p.get("keeps_production_values"))
            .is_some_and(|v| v.as_bool() == Some(true))
    }

    /// `previewUsesProductionValues()`: a preview can run code from a fork when public PR
    /// deployments are on, so it must not get production values. A preview from before v4.5.1
    /// keeps them, because it was deployed with them.
    pub(super) fn preview_uses_production_values(&self) -> bool {
        !self.app.setting_bool("is_pr_deployments_public_enabled")
            || self.preview_keeps_production_values()
    }

    /// `previewGetsProductionRuntimeFallback()`: a preview gets the production runtime variables
    /// that have no preview variable, as before v4.5.1. This needs at least one runtime preview
    /// variable, so previews without preview variables stay without production values.
    /// `variables`: all variables of the application.
    pub(super) fn preview_gets_production_runtime_fallback(&self, variables: &[Var]) -> bool {
        self.b.is_preview()
            && self.preview_uses_production_values()
            && variables.iter().any(|v| {
                v.is_preview
                    && v.is_runtime
                    && !crate::applications::compose::excluded_runtime_key(&v.key)
            })
    }

    /// `warnAboutPreviewVariablesNotPassed()`: logs the variables that this preview deployment
    /// does not get, so users know what to add.
    pub(super) async fn warn_about_preview_variables_not_passed(&mut self) -> Step {
        if !self.b.is_preview() {
            return Ok(());
        }
        let vars = self.variables().await?;
        let (skipped, missing) = preview_variables_not_passed(
            &vars,
            !self.preview_uses_production_values(),
            self.preview_gets_production_runtime_fallback(&vars),
            // deploymentPassesBuildtimeVariables()
            self.build_pack() != "dockerimage",
            self.build_pack() == "dockercompose",
        );
        if !skipped.is_empty() {
            self.log(
                &format!(
                    "\u{26a0}\u{fe0f} Public PR deployments are on, so this preview does not get the preview variables that still have the production value: {}",
                    skipped.join(", ")
                ),
                "stderr",
                false,
            )
            .await;
            self.log(
                "   To use them, give them a preview value in Environment Variables > Preview Deployments.",
                "info",
                false,
            )
            .await;
        }
        if !missing.is_empty() {
            self.log(
                &format!(
                    "\u{26a0}\u{fe0f} These production variables have no preview variable, so this preview does not get them: {}",
                    missing.join(", ")
                ),
                "stderr",
                false,
            )
            .await;
            self.log(
                "   To use them, add them in Environment Variables > Preview Deployments.",
                "info",
                false,
            )
            .await;
        }
        Ok(())
    }

    /// `deployedPreviewVariables()` for a preview (without the variables that hold the production
    /// value when pull requests of forks can deploy), else the production variables.
    pub(super) async fn deployed_variables(&mut self) -> Result<Vec<Var>, Fail> {
        let vars = self.variables().await?;
        if !self.b.is_preview() {
            return Ok(vars.into_iter().filter(|v| !v.is_preview).collect());
        }
        // `previewVariableIdsWithProductionValues()`: a preview from before v4.5.1 keeps them.
        let public = !self.preview_uses_production_values();
        let production: HashMap<String, Option<String>> = vars
            .iter()
            .filter(|v| !v.is_preview)
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect();
        Ok(vars
            .into_iter()
            .filter(|v| v.is_preview)
            .filter(|v| !public || production.get(&v.key).is_none_or(|p| *p != v.value))
            .collect())
    }

    /// `deploy_pull_request()`.
    pub(super) async fn deploy_pull_request(&mut self) -> Step {
        if self.build_pack() != "dockercompose" {
            self.move_switch_to_build_server();
        }
        if self.build_pack() == "dockercompose" {
            return self.deploy_docker_compose().await;
        }
        if self.build_pack() == "dockerfile"
            && let Some(loc) = self
                .app
                .str("dockerfile_location")
                .filter(|l| !l.is_empty())
        {
            self.b.dockerfile_location = validate_path(loc, "dockerfile_location")?;
        }
        // markNewVersionHealthy(true)
        self.healthy = true;
        self.generate_image_names();
        self.info(&format!(
            "Starting pull request (#{}) deployment of {}:{}.",
            self.b.pull_request_id,
            self.b.repository,
            self.branch_name()
        ))
        .await;
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.clone_repository().await?;
        self.cleanup_git().await?;
        let build_pack = self.build_pack();
        if build_pack == "nixpacks" {
            self.generate_nixpacks_confs().await?;
        }
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.generate_build_env_variables().await?;
        if build_pack == "dockerfile" {
            self.add_build_env_variables_to_dockerfile().await?;
        }
        // build_pull_request_image()
        match build_pack.as_str() {
            "railpack" => self.build_railpack_image().await?,
            "static" => self.build_static_image().await?,
            _ => self.build_image().await?,
        }
        self.image_scan_remember_dockerfile_base_image().await;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `resolve_bitbucket_pull_request_commit()`.
    pub(super) async fn resolve_bitbucket_pull_request_commit(&mut self) -> Step {
        let head = self.commit.trim().is_empty() || self.commit == "HEAD";
        if !self.b.is_preview() || self.b.git_type.as_deref() != Some("bitbucket") || !head {
            return Ok(());
        }
        let commit = application_ops::latest_pull_request_commit(
            &self.jobs.0.db,
            self.app.id,
            self.b.pull_request_id,
            self.queue_id,
        )
        .await
        .map_err(|e| Fail::new(e.to_string()))?
        .filter(|c| !c.trim().is_empty());
        let Some(commit) = commit else {
            return Err(Fail::new(format!(
                "Coolify cannot find the commit of Bitbucket pull request #{}, because Bitbucket Cloud has no pull request refs over git. Push a commit to the pull request to deploy it.",
                self.b.pull_request_id
            )));
        };
        self.commit = commit.clone();
        let _ = coolify_db::applications::update_deployment(
            &self.jobs.0.db,
            self.queue_id,
            &[("commit", ColumnValue::Text(Some(commit.clone())))],
            now(),
        )
        .await;
        self.info(&format!(
            "Using commit {commit} of the latest deployment of Bitbucket pull request #{}.",
            self.b.pull_request_id
        ))
        .await;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn var(id: i64, key: &str, value: &str, is_preview: bool) -> Var {
        Var {
            id,
            key: key.into(),
            value: Some(value.into()),
            resolved: Some(value.into()),
            is_literal: false,
            is_multiline: false,
            is_shared: false,
            is_shown_once: false,
            is_runtime: true,
            is_buildtime: true,
            is_preview,
            uses_legacy_escaping: false,
            remote: false,
            preserve_compose_null: false,
            cipher: None,
        }
    }

    /// `warnAboutPreviewVariablesNotPassed()` with public PR deployments: preview variables that
    /// still hold the production value are skipped, and production variables without a preview
    /// variable are missing.
    #[test]
    fn a_public_preview_lists_skipped_and_missing_variables() {
        let vars = [
            var(1, "DATABASE_URL", "postgres://production", false),
            var(2, "API_KEY", "k", false),
            var(3, "ONLY_PRODUCTION", "x", false),
            var(4, "SERVICE_FQDN_WEB", "web.example.com", false),
            var(11, "DATABASE_URL", "postgres://production", true),
            var(12, "API_KEY", "preview-key", true),
        ];
        let (skipped, missing) = preview_variables_not_passed(&vars, true, false, true, true);
        assert_eq!(skipped, vec!["DATABASE_URL"]);
        // The generated compose variable is left out for the Docker Compose build pack only.
        assert_eq!(missing, vec!["ONLY_PRODUCTION"]);
        let (_, missing) = preview_variables_not_passed(&vars, true, false, true, false);
        assert_eq!(missing, vec!["ONLY_PRODUCTION", "SERVICE_FQDN_WEB"]);
    }

    /// With the production runtime fallback, a variable is only missing at build time; a Docker
    /// image deployment passes no build-time variables, so nothing is missing there.
    #[test]
    fn the_runtime_fallback_leaves_only_build_time_gaps() {
        let mut runtime_only = var(2, "RUNTIME_ONLY", "x", false);
        runtime_only.is_buildtime = false;
        let mut build_only = var(3, "BUILD_ONLY", "x", false);
        build_only.is_runtime = false;
        let vars = [
            var(1, "BOTH", "x", false),
            runtime_only,
            build_only,
            var(11, "OTHER", "y", true),
        ];
        let (skipped, missing) = preview_variables_not_passed(&vars, false, true, true, false);
        assert!(skipped.is_empty());
        assert_eq!(missing, vec!["BOTH (at build time)", "BUILD_ONLY"]);
        let (_, missing) = preview_variables_not_passed(&vars, false, true, false, false);
        assert!(missing.is_empty());
        // Without the fallback the runtime variables are missing by name.
        let (_, missing) = preview_variables_not_passed(&vars, false, false, false, false);
        assert_eq!(missing, vec!["BOTH", "RUNTIME_ONLY"]);
    }
}
