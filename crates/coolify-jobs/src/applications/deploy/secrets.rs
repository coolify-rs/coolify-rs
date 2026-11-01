//! Build secrets of `ApplicationDeploymentJob` (`use_build_secrets` on a server with BuildKit):
//! `generate_docker_env_flags_for_secrets()`, `generate_build_secrets()`,
//! `modify_dockerfile_for_secrets()`, `add_build_secrets_to_compose()`.

use base64::Engine as _;
use serde_json::Value;

use coolify_compat::yaml::{Key, PhpValue};
use coolify_remote::shell::quote;

use coolify_db::applications as apps;

use super::build::{php_string, reserved_docker_client_key, valid_env_key};
use super::{Cmd, Fail, Run, Step, git, in_docker};
use crate::compose_parser::ParsedCompose;
use crate::databases::env::Var;
use crate::integrations::secret_managers;

impl Run {
    /// `generate_docker_env_flags_for_secrets()`: the `-e` flags of the helper container.
    pub(super) async fn generate_docker_env_flags_for_secrets(&mut self) -> Result<String, Fail> {
        if !self.app.setting_bool("use_build_secrets") || self.build_pack() == "dockerimage" {
            return Ok(String::new());
        }
        if self.b.env_args.0.is_empty() {
            self.generate_env_variables().await?;
        }
        let railpack = self.build_pack() == "railpack";
        let variables: Vec<(String, String)> = self
            .b
            .env_args
            .0
            .iter()
            .filter(|(k, _)| valid_env_key(k) && !(railpack && reserved_docker_client_key(k)))
            .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
            .collect();
        if variables.is_empty() {
            return Ok(String::new());
        }
        let vars = self.variables().await?;
        let mut flags = Vec::new();
        for (key, value) in variables {
            self.validated_buildtime_key(&key, "the build secret environment")
                .await?;
            let multiline = vars
                .iter()
                .find(|v| v.is_preview == self.b.is_preview() && v.is_buildtime && v.key == key)
                .is_some_and(|v| v.is_multiline);
            // generateDockerEnvFlags()
            let value = if multiline {
                value.trim_matches('\'').to_owned()
            } else {
                value
            };
            flags.push(format!("-e {}", quote(&format!("{key}={value}"))));
        }
        Ok(flags.join(" "))
    }

    /// `generate_build_secrets($variables)`.
    pub(super) async fn generate_build_secrets(&mut self, variables: &[(String, String)]) -> Step {
        if variables.is_empty() {
            self.b.build_secrets = String::new();
            return Ok(());
        }
        let mut flags = Vec::new();
        for (key, _) in variables {
            self.validated_buildtime_key(key, "the build secret environment")
                .await?;
            flags.push(format!(
                "--secret {}",
                quote(&format!("id={key},env={key}"))
            ));
        }
        self.b.build_secrets = format!(
            "{} --build-arg {}",
            flags.join(" "),
            quote(&format!(
                "COOLIFY_BUILD_SECRETS_HASH={}",
                self.secrets_hash(variables)
            ))
        );
        Ok(())
    }

    /// `modify_dockerfile_for_secrets($dockerfile_path)`. The output is saved with `append`, as
    /// upstream's is: the second Dockerfile of a Compose file starts with the first one.
    pub(super) async fn modify_dockerfile_for_secrets(&mut self, path: &str) -> Step {
        if self.b.build_secrets.is_empty() {
            return Ok(());
        }
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &format!("cat {path}")))
                .hidden()
                .save("dockerfile_content", true),
        ])
        .await?;
        let content = self
            .saved
            .get("dockerfile_content")
            .cloned()
            .unwrap_or_default();
        let mut lines: Vec<String> = content.trim().split('\n').map(str::to_owned).collect();
        if !lines.first().is_some_and(|l| l.starts_with("# syntax=")) {
            lines.insert(0, "# syntax=docker/dockerfile:1".to_owned());
        }
        if self.b.env_args.0.is_empty() {
            self.generate_env_variables().await?;
        }
        let keys: Vec<String> = if self.build_pack() == "nixpacks" {
            plan_variables(self.b.nixpacks_plan.as_ref())
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        } else {
            self.b.env_args.0.iter().map(|(k, _)| k.clone()).collect()
        };
        if keys.is_empty() {
            return Ok(());
        }
        let mut mounts = Vec::new();
        for key in &keys {
            self.validated_buildtime_key(key, "the generated Dockerfile")
                .await?;
            mounts.push(format!("--mount=type=secret,id={key},env={key}"));
        }
        let mounts = mounts.join(" ");
        let instructions = git::dockerfile_instructions(&lines);
        let runs: Vec<usize> = instructions
            .iter()
            .filter(|i| i.0 == "RUN")
            .map(|i| i.1)
            .collect();
        let from_ends: Vec<usize> = instructions
            .iter()
            .filter(|i| i.0 == "FROM")
            .map(|i| i.2)
            .collect();
        let mut modified = false;
        let mut out = Vec::with_capacity(lines.len());
        for (index, line) in lines.into_iter().enumerate() {
            let mut line = line;
            if runs.contains(&index) && !line.contains("--mount=type=secret") {
                let original = line
                    .trim_start()
                    .get(3..)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                line = format!("RUN {mounts} {original}");
                modified = true;
            }
            out.push(line);
            // A RUN step reads every ARG of its stage, so a changed hash rebuilds it.
            if from_ends.contains(&index) {
                modified = true;
                out.push("ARG COOLIFY_BUILD_SECRETS_HASH".to_owned());
            }
        }
        if modified {
            let b64 = base64::engine::general_purpose::STANDARD.encode(out.join("\n"));
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!("echo '{b64}' | base64 -d | tee {path} > /dev/null"),
                ))
                .hidden(),
            ])
            .await?;
        }
        Ok(())
    }

    /// `add_build_secrets_to_compose($composeFile)`.
    pub(super) async fn add_build_secrets_to_compose(
        &mut self,
        compose: &mut ParsedCompose,
    ) -> Step {
        if self.b.env_args.0.is_empty() {
            self.generate_env_variables().await?;
        }
        let variables: Vec<(String, String)> = self
            .b
            .env_args
            .0
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
            .collect();
        if variables.is_empty() {
            return Ok(());
        }
        for (key, _) in &variables {
            self.validated_buildtime_key(key, "the Compose build secret environment")
                .await?;
        }
        let hash = self.secrets_hash(&variables);
        if let Some(services) = compose.services_mut() {
            for (_, service) in services.iter_mut() {
                add_build_secrets_to_service(service, &variables, &hash);
            }
        }
        let mut secrets = compose
            .top_level
            .get_mut("secrets")
            .map_or(PhpValue::Array(Vec::new()), |s| s.clone());
        for (key, _) in &variables {
            secrets.set(
                key,
                PhpValue::map([("environment", PhpValue::Str(key.clone()))]),
            );
        }
        compose.top_level.set("secrets", secrets);
        self.info(
            "Added build secrets configuration to docker-compose file (using environment variables).",
        )
        .await;
        Ok(())
    }
}

/// One service of the loop of `add_build_secrets_to_compose()`.
fn add_build_secrets_to_service(
    service: &mut PhpValue,
    variables: &[(String, String)],
    hash: &str,
) {
    let Some(build) = service.get_mut("build").filter(|b| **b != PhpValue::Null) else {
        return;
    };
    if let PhpValue::Str(context) = build {
        *build = PhpValue::map([("context", PhpValue::Str(context.clone()))]);
    }
    let mut secrets = match build.get_mut("secrets") {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    for (key, _) in variables {
        if !secrets
            .iter()
            .any(|(_, v)| *v == PhpValue::Str(key.clone()))
        {
            let next = secrets
                .iter()
                .filter_map(|(k, _)| match k {
                    Key::Int(i) => Some(i + 1),
                    Key::Str(_) => None,
                })
                .max()
                .unwrap_or(0);
            secrets.push((Key::Int(next), PhpValue::Str(key.clone())));
        }
    }
    build.set("secrets", PhpValue::Array(secrets));
    // Docker ignores secret values for the build cache, so the hash goes in as a build arg.
    let mut args = match build.get_mut("args") {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let is_list = args
        .iter()
        .enumerate()
        .all(|(i, (k, _))| *k == Key::Int(i64::try_from(i).unwrap_or(-1)));
    if !args.is_empty() && is_list {
        args.push((
            Key::Int(i64::try_from(args.len()).unwrap_or(0)),
            PhpValue::Str(format!("COOLIFY_BUILD_SECRETS_HASH={hash}")),
        ));
        build.set("args", PhpValue::Array(args));
    } else {
        let mut map = PhpValue::Array(args);
        map.set("COOLIFY_BUILD_SECRETS_HASH", PhpValue::Str(hash.to_owned()));
        build.set("args", map);
    }
}

/// `collect($this->nixpacks_plan_json->get('variables'))`.
pub(super) fn plan_variables(plan: Option<&Value>) -> Vec<(String, String)> {
    match plan.and_then(|p| p.get("variables")) {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), php_string(v))).collect(),
        _ => Vec::new(),
    }
}

impl Run {
    /// `remote_secrets()`: fetches the source once, logs it and hides every fetched value in the
    /// deployment log.
    async fn remote_secrets(&mut self) -> Result<secret_managers::Secrets, Fail> {
        if let Some(s) = &self.b.remote_secrets {
            return Ok(s.clone());
        }
        let db = self.jobs.0.db.clone();
        let link = secret_managers::link_of(&db, apps::MORPH, self.app.id)
            .await
            .map_err(Fail::new)?;
        let Some(link) = link else {
            return Err(Fail::new(
                "Environment variables reference remote secrets ({{vault.KEY}}), but no secret manager source is configured for this application.",
            ));
        };
        let enc = self
            .jobs
            .0
            .encrypter
            .clone()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        let provider = link.provider_name();
        let summary = format!("{}, {}", link.token_name, link.summary());
        let secrets = match link.fetch(&db, &enc, self.app.team_id, "").await {
            Ok(s) => s,
            Err(e) => {
                self.log(
                    &format!("Failed to fetch secrets from {provider} ({summary}): {e}"),
                    "stderr",
                    false,
                )
                .await;
                return Err(Fail::new(format!(
                    "Could not fetch secrets from {provider}. The deployment was stopped so the application does not start with missing secrets."
                )));
            }
        };
        // `redactRemoteSecrets()`
        for value in secrets.values().filter(|v| !v.is_empty()) {
            for variant in remote_redaction_values(value) {
                if !self.locked.contains(&variant) {
                    self.locked.push(variant);
                }
            }
        }
        self.info(&format!(
            "Fetched {} secrets from {provider} ({summary}).",
            secrets.len()
        ))
        .await;
        self.b.remote_secrets = Some(secrets.clone());
        Ok(secrets)
    }

    /// `uses_remote_secret_references()` and `substitute_remote_secrets()` for the variables this
    /// deployment uses: other variables never fetch secrets.
    pub(super) async fn resolve_remote_secrets(&mut self, vars: &mut [Var]) -> Result<(), Fail> {
        let compose = self.build_pack() == "dockercompose";
        let preview = self.b.is_preview();
        let db = self.jobs.0.db.clone();
        let mut has_source = None;
        for v in vars.iter_mut() {
            if !preview && v.is_preview {
                continue;
            }
            let stored = secret_managers::contains_reference(v.value.as_deref());
            let resolved = secret_managers::contains_reference(v.resolved.as_deref());
            if !(stored || (compose && resolved)) {
                continue;
            }
            if has_source.is_none() {
                has_source = Some(
                    secret_managers::link_of(&db, apps::MORPH, self.app.id)
                        .await
                        .map_err(Fail::new)?
                        .is_some(),
                );
            }
            if v.is_literal && has_source != Some(true) {
                continue;
            }
            let value = v.resolved.clone().unwrap_or_default();
            if !resolved {
                continue;
            }
            let secrets = self.remote_secrets().await?;
            let missing = secret_managers::missing_keys(&value, &secrets);
            if !missing.is_empty() {
                let message = format!(
                    "Missing secret keys: {} (referenced by {}). Check the secret manager source of this application.",
                    missing.join(", "),
                    v.key
                );
                self.log(&message, "stderr", false).await;
                return Err(Fail::new(message));
            }
            v.resolved = Some(secret_managers::substitute(&value, &secrets));
            v.remote = true;
        }
        Ok(())
    }
}

/// `EnvironmentVariable::remoteSecretLogRedactionValues()` for one value.
pub(crate) fn remote_redaction_values(value: &str) -> Vec<String> {
    let mut out = vec![
        value.to_owned(),
        git::bash_env_value(Some(value)),
        value.replace('\'', "'\\''"),
        value.replace("\r\n", "\\n").replace(['\r', '\n'], "\\n"),
    ];
    if value.contains(['\r', '\n']) {
        out.extend(
            value
                .split("\r\n")
                .flat_map(|l| l.split(['\r', '\n']))
                .map(str::to_owned),
        );
    }
    out.push(secret_managers::compose_file(value));
    out.retain(|x| !x.is_empty());
    out
}
