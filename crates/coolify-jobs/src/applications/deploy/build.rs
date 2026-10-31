//! The git build packs of `ApplicationDeploymentJob` (pull request 0, no build
//! server): Dockerfile, static and Nixpacks. compat/notes/deployment-job-git.md
//! describes upstream's steps; the commands and log lines are the same.

use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;
use serde_json::{Map, Value};

use coolify_db::applications as apps;
use coolify_db::git_sources;
use coolify_db::servers::ColumnValue;
use coolify_remote::shell::quote;

use super::git::{self, DeploymentType, GitImport, ImportInput, ImportSettings};
use super::{B64, Cmd, Fail, Run, SEPARATOR, Step, in_docker, now};
use crate::applications::domains;
use crate::databases::env::{self, Owner, Var};

pub(super) const BUILD_TIME_ENV_PATH: &str = concat!("/artifacts/build-time.", "env");
const BUILD_TIME_SHELL_ENV_PATH: &str = concat!("/artifacts/build-time-shell.", "env");
const BUILD_TIME_ENV_LAUNCHER_PATH: &str = "/artifacts/run-with-build-time-env";
const BUILD_SCRIPT_PATH: &str = "/artifacts/build.sh";
const NIXPACKS_PLAN_PATH: &str = "/artifacts/thegameplan.json";
const RAILPACK_REPOSITORY_CONFIG_PATH: &str = "railpack.json";
const RAILPACK_GENERATED_CONFIG_PATH: &str = ".coolify/railpack.generated.json";
/// `DOCKER_CLIENT_ENV_KEYS`.
const DOCKER_CLIENT_ENV_KEYS: &[&str] = &[
    "BUILDKIT_HOST",
    "BUILDX_BUILDER",
    "BUILDX_CONFIG",
    "DOCKER_API_VERSION",
    "DOCKER_BUILDKIT",
    "DOCKER_CERT_PATH",
    "DOCKER_CLI_EXPERIMENTAL",
    "DOCKER_CONFIG",
    "DOCKER_CONTEXT",
    "DOCKER_HOST",
    "DOCKER_TLS",
    "DOCKER_TLS_VERIFY",
];

/// `is_reserved_docker_client_env_key($key)`.
fn reserved_docker_client_key(key: &str) -> bool {
    !key.is_empty() && DOCKER_CLIENT_ENV_KEYS.contains(&key.to_uppercase().as_str())
}

/// `ValidationPatterns::isValidEnvironmentVariableKey()`.
pub(super) fn valid_env_key(key: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!())
    });
    RE.is_match(key)
}

/// `SHELL_ENVIRONMENT_VARIABLE_KEY_PATTERN`.
fn shell_env_key(key: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[A-Za-z_][A-Za-z0-9_]*\z").unwrap_or_else(|_| unreachable!())
    });
    RE.is_match(key)
}

/// `withoutBuildpackControlVariables()`.
fn buildpack_control(key: &str) -> bool {
    key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_")
}

/// An ordered map with PHP `Collection::put` semantics (replace in place, else append).
#[derive(Debug, Default, Clone)]
pub(super) struct Ordered(pub Vec<(String, Option<String>)>);

impl Ordered {
    fn put(&mut self, key: &str, value: Option<String>) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_owned(), value)),
        }
    }
}

/// `EnvironmentVariableAnalyzer::getProblematicBuildVariables()`: `(key, affects, issue, recommendation)`.
const PROBLEMATIC: &[(&str, &str, &str, &str)] = &[
    (
        "NODE_ENV",
        "Node.js/npm/yarn/bun/pnpm",
        "Skips devDependencies installation which are often required for building (webpack, typescript, etc.)",
        "Uncheck \"Available at Buildtime\" or use \"development\" during build",
    ),
    (
        "NPM_CONFIG_PRODUCTION",
        "npm/pnpm",
        "Forces npm to skip devDependencies",
        "Remove from build-time variables or set to false",
    ),
    (
        "YARN_PRODUCTION",
        "Yarn/pnpm",
        "Forces yarn to skip devDependencies",
        "Remove from build-time variables or set to false",
    ),
    (
        "COMPOSER_NO_DEV",
        "PHP/Composer",
        "Skips require-dev packages which may include build tools",
        "Set as \"Runtime only\" or remove from build-time variables",
    ),
    (
        "MIX_ENV",
        "Elixir/Phoenix",
        "Production mode may skip development dependencies needed for compilation",
        "Use \"dev\" for build or set as \"Runtime only\"",
    ),
    (
        "RAILS_ENV",
        "Ruby on Rails",
        "May affect asset precompilation and dependency handling",
        "Consider using \"development\" for build phase",
    ),
    (
        "RACK_ENV",
        "Ruby/Rack",
        "May affect dependency handling and build behavior",
        "Consider using \"development\" for build phase",
    ),
    (
        "BUNDLE_WITHOUT",
        "Ruby/Bundler",
        "Excludes gem groups that may contain build dependencies",
        "Remove from build-time variables or adjust groups",
    ),
    (
        "FLASK_ENV",
        "Python/Flask",
        "May affect debug mode and development tools availability",
        "Usually safe, but consider \"development\" for complex builds",
    ),
    (
        "DJANGO_SETTINGS_MODULE",
        "Python/Django",
        "Production settings may disable debug tools needed during build",
        "Use development settings for build phase",
    ),
    (
        "APP_ENV",
        "Laravel/Symfony",
        "May affect dependency installation and build optimizations",
        "Consider using \"local\" or \"development\" for build",
    ),
    (
        "ASPNETCORE_ENVIRONMENT",
        ".NET/ASP.NET Core",
        "May affect build-time configurations and optimizations",
        "Usually safe, but verify build requirements",
    ),
    (
        "CI",
        "Various tools",
        "Changes behavior in many tools (disables interactivity, changes caching)",
        "Usually beneficial for builds, but be aware of behavior changes",
    ),
];

/// `json_decode($json, true)` then `json_encode()`: empty objects and lists keyed `0..n` become arrays.
fn php_assoc(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let sequential =
                !m.is_empty() && m.keys().enumerate().all(|(i, k)| k == &i.to_string());
            if m.is_empty() {
                Value::Array(Vec::new())
            } else if sequential {
                Value::Array(m.into_iter().map(|(_, v)| php_assoc(v)).collect())
            } else {
                Value::Object(m.into_iter().map(|(k, v)| (k, php_assoc(v))).collect())
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(php_assoc).collect()),
        other => other,
    }
}

/// A PHP scalar as a string (`(string) $value`).
fn php_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Run {
    fn build_pack(&self) -> String {
        self.app.str("build_pack").unwrap_or_default().to_owned()
    }

    fn branch_name(&self) -> String {
        self.app.str("git_branch").unwrap_or_default().to_owned()
    }

    /// `decide_what_to_do()`.
    pub(super) async fn decide_what_to_do(&mut self) -> Step {
        if self.b.restart_only {
            return self.just_restart().await;
        }
        let bp = self.build_pack();
        if bp == "dockerimage" {
            self.deploy_docker_image().await?;
        } else if self.app.str("dockerfile").is_some_and(|d| !d.is_empty()) {
            self.deploy_simple_dockerfile().await?;
        } else {
            match bp.as_str() {
                "dockerfile" => self.deploy_dockerfile().await?,
                "static" => self.deploy_static().await?,
                "nixpacks" => self.deploy_nixpacks().await?,
                "railpack" => self.deploy_railpack().await?,
                "dockercompose" => self.deploy_docker_compose().await?,
                _ => return Err(Fail::new(format!("Unsupported build pack: {bp}"))),
            }
        }
        self.post_deployment().await;
        Ok(())
    }

    fn starting_line(&self) -> String {
        format!(
            "Starting deployment of {}:{} to {}.",
            self.b.repository,
            self.branch_name(),
            self.server_name
        )
    }

    /// `just_restart()` (nixpacks, static, railpack).
    async fn just_restart(&mut self) -> Step {
        self.info(&format!(
            "Restarting {}:{} on {}.",
            self.b.repository,
            self.branch_name(),
            self.server_name
        ))
        .await;
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.generate_image_names();
        self.check_image_locally_or_remotely().await?;
        Box::pin(self.should_skip_build()).await?;
        self.complete_deployment().await;
        Ok(())
    }

    /// `deploy_dockerfile_buildpack()`.
    async fn deploy_dockerfile(&mut self) -> Step {
        self.info(&self.starting_line()).await;
        if let Some(loc) = self
            .app
            .str("dockerfile_location")
            .filter(|l| !l.is_empty())
        {
            self.b.dockerfile_location = validate_path(loc, "dockerfile_location")?;
        }
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.generate_image_names();
        self.clone_repository().await?;
        if !self.b.force_rebuild {
            self.check_image_locally_or_remotely().await?;
            if Box::pin(self.should_skip_build()).await? {
                return Ok(());
            }
        }
        self.cleanup_git().await?;
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.generate_build_env_variables().await?;
        self.add_build_env_variables_to_dockerfile().await?;
        self.build_image().await?;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `deploy_simple_dockerfile()`.
    async fn deploy_simple_dockerfile(&mut self) -> Step {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(self.app.str("dockerfile").unwrap_or_default());
        self.info(&format!(
            "Starting deployment of {} to {}.",
            self.app.str("name").unwrap_or_default(),
            self.server_name
        ))
        .await;
        self.prepare_builder_image().await?;
        let target = format!("{}{}", self.workdir, self.b.dockerfile_location);
        self.exec(vec![Cmd::new(in_docker(
            &self.dep,
            &format!("echo '{encoded}' | base64 -d | tee {target} > /dev/null"),
        ))])
        .await?;
        self.generate_image_names();
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.generate_build_env_variables().await?;
        self.add_build_env_variables_to_dockerfile().await?;
        self.build_image().await?;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `deploy_static_buildpack()`.
    async fn deploy_static(&mut self) -> Step {
        self.info(&self.starting_line()).await;
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.generate_image_names();
        if !self.b.force_rebuild {
            self.check_image_locally_or_remotely().await?;
            if Box::pin(self.should_skip_build()).await? {
                return Ok(());
            }
        }
        self.clone_repository().await?;
        self.cleanup_git().await?;
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.build_static_image().await?;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `deploy_nixpacks_buildpack()`.
    async fn deploy_nixpacks(&mut self) -> Step {
        self.info(&self.starting_line()).await;
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.generate_image_names();
        if !self.b.force_rebuild {
            self.check_image_locally_or_remotely().await?;
            if Box::pin(self.should_skip_build()).await? {
                return Ok(());
            }
        }
        self.clone_repository().await?;
        self.cleanup_git().await?;
        self.generate_nixpacks_confs().await?;
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.generate_build_env_variables().await?;
        self.build_image().await?;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `deploy_railpack_buildpack()`.
    async fn deploy_railpack(&mut self) -> Step {
        self.info(&self.starting_line()).await;
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.generate_image_names();
        if !self.b.force_rebuild {
            self.check_image_locally_or_remotely().await?;
            if Box::pin(self.should_skip_build()).await? {
                return Ok(());
            }
        }
        self.clone_repository().await?;
        self.cleanup_git().await?;
        let image = self.b.production_image.clone();
        self.generate_compose_file(&image).await?;
        self.save_buildtime_environment_variables().await?;
        self.generate_build_env_variables().await?;
        self.build_railpack_image().await?;
        self.save_runtime_environment().await?;
        self.push_to_docker_registry().await?;
        self.rolling_update().await
    }

    /// `railpack_build_variables()`.
    async fn railpack_build_variables(&mut self) -> Result<Vec<(String, String)>, Fail> {
        let generic = self.buildtime_variables(false).await?;
        let railpack: Vec<Var> = self
            .variables()
            .await?
            .into_iter()
            .filter(|v| !v.is_preview && v.key.starts_with("RAILPACK_") && valid_env_key(&v.key))
            .collect();
        let mut map = Ordered::default();
        for v in generic.iter().chain(railpack.iter()) {
            let literal = v.is_literal || v.is_multiline;
            // `normalize_resolved_build_variable_value()`
            let value = git::resolved_with_server(v.resolved.as_deref(), literal)
                .filter(|r| !r.is_empty())
                .map(|r| {
                    if literal {
                        r.trim_matches('\'').to_owned()
                    } else {
                        r
                    }
                })
                .filter(|r| !r.is_empty());
            if let Some(value) = value {
                map.put(&v.key, Some(value));
            }
        }
        let pairs: Vec<(String, Value)> = map
            .0
            .into_iter()
            .map(|(k, v)| (k, v.map_or(Value::Null, Value::String)))
            .collect();
        let pairs = self.without_dotted_build_variables(pairs, "Railpack").await;
        let mut map = Ordered(
            pairs
                .into_iter()
                .map(|(k, v)| (k, Some(php_string(&v))))
                .collect(),
        );
        if let Some(cmd) = self.app.str("install_command").filter(|c| !c.is_empty()) {
            map.put("RAILPACK_INSTALL_CMD", Some(cmd.to_owned()));
        }
        // merge_railpack_deploy_apt_packages()
        let current = map
            .0
            .iter()
            .find(|(k, _)| k == "RAILPACK_DEPLOY_APT_PACKAGES")
            .and_then(|(_, v)| v.clone())
            .unwrap_or_default();
        let mut packages: Vec<String> = current.split_whitespace().map(str::to_owned).collect();
        if !packages.iter().any(|p| p == "...") {
            packages.insert(0, "...".into());
        }
        for p in ["curl", "wget"] {
            if !packages.iter().any(|x| x == p) {
                packages.push(p.into());
            }
        }
        map.put("RAILPACK_DEPLOY_APT_PACKAGES", Some(packages.join(" ")));
        for (k, v) in self.coolify_env(true).await?.0 {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                map.put(&k, Some(v));
            }
        }
        Ok(map
            .0
            .into_iter()
            .map(|(k, v)| (k, v.unwrap_or_default()))
            .collect())
    }

    /// `generate_railpack_config_file()`: the repository's railpack.json with `$schema`.
    async fn generate_railpack_config_file(&mut self) -> Result<Option<String>, Fail> {
        let w = self.workdir.clone();
        self.saved.remove("railpack_config_exists");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &format!("test -f {w}/{RAILPACK_REPOSITORY_CONFIG_PATH} && echo 'exists' || echo 'missing'")))
                .hidden()
                .save("railpack_config_exists", true),
        ])
        .await?;
        if self.saved.get("railpack_config_exists").map(|v| v.trim()) != Some("exists") {
            return Ok(None);
        }
        self.saved.remove("railpack_repository_config");
        self.exec(vec![
            Cmd::new(in_docker(
                &self.dep,
                &format!("cat {w}/{RAILPACK_REPOSITORY_CONFIG_PATH}"),
            ))
            .hidden()
            .save("railpack_repository_config", true),
        ])
        .await?;
        let raw = self
            .saved
            .get("railpack_repository_config")
            .cloned()
            .unwrap_or_default();
        let config = match serde_json::from_str::<Value>(&raw) {
            Ok(v) => php_assoc(v),
            Err(e) => return Err(Fail::new(format!("Invalid repository railpack.json: {e}"))),
        };
        let mut map = match config {
            Value::Object(m) => m,
            Value::Array(a) if a.is_empty() => return Ok(None),
            _ => {
                return Err(Fail::new(
                    "Invalid repository railpack.json: expected a JSON object.",
                ));
            }
        };
        if !map.contains_key("$schema") {
            map.insert("$schema".into(), Value::from("https://schema.railpack.com"));
        }
        let encoded =
            coolify_compat::php_json::encode_pretty(&Value::Object(map)).replace("\\/", "/");
        let path = format!("{w}/{RAILPACK_GENERATED_CONFIG_PATH}");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &format!("mkdir -p {w}/.coolify"))).hidden(),
            Cmd::new(in_docker(
                &self.dep,
                &format!(
                    "echo '{}' | base64 -d | tee {path} > /dev/null",
                    B64.encode(encoded)
                ),
            ))
            .hidden(),
        ])
        .await?;
        Ok(Some(RAILPACK_GENERATED_CONFIG_PATH.to_owned()))
    }

    /// `build_railpack_image()`.
    async fn build_railpack_image(&mut self) -> Step {
        if !self.b.buildx {
            return Err(Fail::new(
                "Railpack deployments require the Docker buildx CLI plugin on the build server. Install or enable docker buildx and retry the deployment.",
            ));
        }
        self.saved.remove("railpack_helper_buildx_available");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, "DOCKER_CONFIG=/root/.docker docker buildx version >/dev/null 2>&1 && echo available || echo not-available"))
                .hidden()
                .save("railpack_helper_buildx_available", true),
        ])
        .await?;
        if self
            .saved
            .get("railpack_helper_buildx_available")
            .map(|v| v.trim())
            != Some("available")
        {
            return Err(Fail::new(
                "Railpack deployments require the Docker buildx CLI plugin inside the Coolify helper container. The helper could not find buildx at /root/.docker/cli-plugins/docker-buildx. Pull the latest helper image and retry the deployment.",
            ));
        }
        let variables = self.railpack_build_variables().await?;
        let env_args = variables
            .iter()
            .map(|(k, v)| format!("--env {}", git::shell_value(&format!("{k}={v}"))))
            .collect::<Vec<_>>()
            .join(" ");
        let config = self.generate_railpack_config_file().await?;
        let mut prepare = "railpack prepare".to_owned();
        for (flag, column) in [
            ("--build-cmd", "build_command"),
            ("--start-cmd", "start_command"),
        ] {
            if let Some(c) = self.app.str(column).filter(|c| !c.is_empty()) {
                prepare.push_str(&format!(" {flag} {}", git::shell_value(c)));
            }
        }
        if !env_args.is_empty() {
            prepare.push_str(&format!(" {env_args}"));
        }
        if let Some(c) = &config {
            prepare.push_str(&format!(" --config-file {}", git::shell_value(c)));
        }
        prepare.push_str(&format!(
            " --plan-out /artifacts/railpack-plan.json {}",
            self.workdir
        ));
        self.info("Generating Railpack build plan.").await;
        self.saved.remove("railpack_plan");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &prepare))
                .hidden()
                .skip_command_log(),
            Cmd::new(in_docker(&self.dep, "cat /artifacts/railpack-plan.json"))
                .hidden()
                .save("railpack_plan", true),
        ])
        .await?;
        if let Some(raw) = self
            .saved
            .get("railpack_plan")
            .filter(|p| !p.is_empty())
            .cloned()
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
            && let Value::Object(mut plan) = php_assoc(v)
        {
            plan.remove("secrets");
            self.log(
                &format!(
                    "Final Railpack plan: {}",
                    coolify_compat::php_json::encode_pretty(&Value::Object(plan))
                ),
                "stdout",
                true,
            )
            .await;
        }
        self.info("Building docker image with Railpack.").await;
        self.info("To check the current progress, click on Show Debug Logs.")
            .await;
        let is_static = self.app.setting_bool("is_static");
        let image = if is_static {
            self.b.build_image.clone()
        } else {
            self.b.production_image.clone()
        };
        if is_static && self.app.str("static_image").is_some_and(|s| !s.is_empty()) {
            let s = self.static_image()?;
            self.pull_latest_image(&s).await?;
        }
        let build = self.railpack_build_command(&image, &variables);
        self.run_build_script(Vec::new(), &build).await?;
        if is_static {
            let publish = self
                .app
                .str("publish_directory")
                .unwrap_or_default()
                .trim_matches('/')
                .to_owned();
            let publish = if publish.is_empty() {
                String::new()
            } else {
                format!("/{publish}")
            };
            let dockerfile = format!(
                "FROM {}\nWORKDIR /usr/share/nginx/html/\nLABEL coolify.deploymentId={}\nCOPY --from={} /app{publish} .\nCOPY ./nginx.conf /etc/nginx/conf.d/default.conf",
                self.static_image()?,
                self.dep,
                self.b.build_image
            );
            let w = self.workdir.clone();
            let build = if self.b.buildkit {
                format!(
                    "DOCKER_BUILDKIT=1 docker build {} --network host -f {w}/Dockerfile --progress plain -t {} {w}",
                    self.b.add_hosts, self.b.production_image
                )
            } else {
                format!(
                    "docker build {} --network host -f {w}/Dockerfile -t {} {w}",
                    self.b.add_hosts, self.b.production_image
                )
            };
            let before = vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!(
                        "echo '{}' | base64 -d | tee {w}/Dockerfile > /dev/null",
                        B64.encode(dockerfile)
                    ),
                )),
                Cmd::new(in_docker(
                    &self.dep,
                    &format!(
                        "echo '{}' | base64 -d | tee {w}/nginx.conf > /dev/null",
                        B64.encode(self.nginx_config())
                    ),
                )),
            ];
            self.run_build_script(before, &build).await?;
        }
        Ok(())
    }

    /// `railpack_build_command($imageName, $variables)`.
    fn railpack_build_command(&self, image: &str, variables: &[(String, String)]) -> String {
        let variables: Vec<(String, String)> = variables
            .iter()
            .filter(|(k, _)| !reserved_docker_client_key(k))
            .cloned()
            .collect();
        let mut cache = if self.b.force_rebuild {
            "--no-cache".to_owned()
        } else {
            format!("--build-arg cache-key='{}'", self.app.uuid)
        };
        if !variables.is_empty() {
            cache.push_str(&format!(
                " --build-arg secrets-hash={}",
                self.secrets_hash(&variables)
            ));
        }
        let control: Vec<&(String, String)> = variables
            .iter()
            .filter(|(k, _)| buildpack_control(k))
            .collect();
        let prefix = if control.is_empty() {
            String::new()
        } else {
            format!(
                "env {} ",
                control
                    .iter()
                    .map(|(k, v)| git::shell_value(&format!("{k}={v}")))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let secrets = if variables.is_empty() {
            String::new()
        } else {
            format!(
                " {}",
                variables
                    .iter()
                    .map(|(k, _)| format!(
                        "--secret {}",
                        git::shell_value(&format!("id={k},env={k}"))
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let frontend = "ghcr.io/railwayapp/railpack-frontend:v${RAILPACK_VERSION:-$(railpack --version | cut -d \" \" -f 3)}";
        let buildx = format!(
            "{prefix}DOCKER_CONFIG=/root/.docker docker buildx build --builder coolify-railpack {} --network host --build-arg BUILDKIT_SYNTAX=\"{frontend}\" {cache}{secrets} -f /artifacts/railpack-plan.json --progress plain --load -t {image} {}",
            self.b.add_hosts, self.workdir
        );
        let locked = format!(
            "exec 9>/root/.docker/buildx/coolify-railpack.lock; if flock -s 9; then :; else exit $?; fi; ({}); status=$?; if flock -n -x 9; then DOCKER_CONFIG=/root/.docker docker buildx stop coolify-railpack >/dev/null 2>&1; fi; exit $status",
            self.wrap(&buildx)
        );
        format!(
            "DOCKER_CONFIG=/root/.docker docker buildx create --name coolify-railpack --driver docker-container 2>/dev/null || true\n{locked}"
        )
    }

    /// The application's variables (non-preview first in table order), decrypted and resolved.
    pub(super) async fn variables(&mut self) -> Result<Vec<Var>, Fail> {
        if let Some(v) = &self.b.variables {
            return Ok(v.clone());
        }
        let enc = self
            .jobs
            .0
            .encrypter
            .as_ref()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        let owner = Owner {
            morph: apps::MORPH,
            id: self.app.id,
            team_id: self.app.team_id,
            project_id: self.app.project.get("id").and_then(Value::as_i64),
            environment_id: self.app.environment.get("id").and_then(Value::as_i64),
        };
        let vars = env::load_for(&self.jobs.0.db, enc, &owner, Some(self.server.id))
            .await
            .map_err(Fail::new)?;
        self.b.variables = Some(vars.clone());
        Ok(vars)
    }

    /// `$this->application->environment_variables()->withoutBuildpackControlVariables()->where('is_buildtime', true)`,
    /// minus invalid keys, ordered by key or id when `sorted`, else in table order.
    async fn buildtime_variables(&mut self, sorted: bool) -> Result<Vec<Var>, Fail> {
        let vars: Vec<Var> = self
            .variables()
            .await?
            .into_iter()
            .filter(|v| {
                !v.is_preview
                    && v.is_buildtime
                    && !buildpack_control(&v.key)
                    && valid_env_key(&v.key)
            })
            .collect();
        if !sorted {
            return Ok(vars);
        }
        let by_key = self.app.setting_bool("is_env_sorting_enabled");
        let order =
            git_sources::variable_ids_ordered(&self.jobs.0.db, apps::MORPH, self.app.id, by_key)
                .await
                .map_err(|e| Fail::new(e.to_string()))?;
        let mut vars = vars;
        vars.sort_by_key(|v| {
            order
                .iter()
                .position(|id| *id == v.id)
                .unwrap_or(usize::MAX)
        });
        Ok(vars)
    }

    /// `generate_coolify_env_variables($forBuildTime)` (pull request 0).
    async fn coolify_env(&mut self, for_build: bool) -> Result<Ordered, Fail> {
        let vars = self.variables().await?;
        let defined = |k: &str| vars.iter().any(|v| !v.is_preview && v.key == k);
        let mut out = Ordered::default();
        if (!for_build || self.app.setting_bool("include_source_commit_in_build"))
            && !defined("SOURCE_COMMIT")
        {
            out.put("SOURCE_COMMIT", Some(self.commit.clone()));
        }
        let parsing = self.parsing_version();
        let fqdn = self.app.str("fqdn").map(str::to_owned);
        if !defined("COOLIFY_FQDN") {
            out.put(
                if parsing >= 3 {
                    "COOLIFY_URL"
                } else {
                    "COOLIFY_FQDN"
                },
                fqdn.clone(),
            );
        }
        if !defined("COOLIFY_URL") {
            out.put(
                if parsing >= 3 {
                    "COOLIFY_FQDN"
                } else {
                    "COOLIFY_URL"
                },
                Some(hosts_from_fqdn(fqdn.as_deref())),
            );
        }
        let compose_version = self.app.str("compose_parsing_version");
        if self.build_pack() != "dockercompose" || matches!(compose_version, Some("1" | "2")) {
            if !defined("COOLIFY_BRANCH") {
                out.put(
                    "COOLIFY_BRANCH",
                    Some(self.b.branch.clone()).filter(|_| !self.b.branch.is_empty()),
                );
            }
            if !defined("COOLIFY_RESOURCE_UUID") {
                out.put("COOLIFY_RESOURCE_UUID", Some(self.app.uuid.clone()));
            }
            if !for_build && !defined("COOLIFY_CONTAINER_NAME") {
                out.put("COOLIFY_CONTAINER_NAME", Some(self.container.clone()));
            }
        }
        Ok(out)
    }

    fn parsing_version(&self) -> i64 {
        self.app
            .str("compose_parsing_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    /// The source (`$this->source`) and the deploy key of the application.
    pub(super) async fn load_source(&mut self) -> Step {
        let db = &self.jobs.0.db;
        if let (Some(kind), Some(id)) = (self.app.str("source_type"), self.app.int("source_id")) {
            let table = match kind {
                git::GITHUB_APP => Some("github_apps"),
                git::GITLAB_APP => Some("gitlab_apps"),
                git::GITEA_APP => Some("gitea_apps"),
                _ => None,
            };
            if let Some(table) = table {
                let morph = kind.to_owned();
                let row = git_sources::source_row(db, table, id)
                    .await
                    .map_err(|e| Fail::new(e.to_string()))?;
                self.b.source = row.map(|row| git::Source { row, morph });
            }
        }
        let kind = git::deployment_type(&self.app, self.b.source.as_ref());
        let (repo, port) = git::convert_git_url(
            self.app.str("git_repository").unwrap_or_default(),
            kind,
            self.b.source.as_ref(),
        );
        self.b.kind = kind;
        self.b.repository = repo;
        self.b.port = port;
        Ok(())
    }

    /// The decrypted `private_key` of the application (`private_key.private_key`).
    async fn private_key(&self) -> Result<Option<String>, Fail> {
        let Some(id) = self.app.int("private_key_id") else {
            return Ok(None);
        };
        let cipher = git_sources::private_key_ciphertext(&self.jobs.0.db, id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
        let Some(cipher) = cipher else {
            return Ok(None);
        };
        let enc = self
            .jobs
            .0
            .encrypter
            .as_ref()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        enc.decrypt_maybe_serialized(&cipher)
            .map(Some)
            .map_err(|_| Fail::new("The MAC is invalid."))
    }

    /// `generate_git_import_commands()`: sets `$this->branch` and `$this->fullRepoUrl`.
    async fn git_import(&mut self) -> Result<GitImport, Fail> {
        let key = if self.b.kind == DeploymentType::DeployKey {
            self.private_key().await?
        } else {
            None
        };
        if self
            .b
            .source
            .as_ref()
            .is_some_and(|s| s.is_github() && !s.is_public())
            && self.b.kind == DeploymentType::Source
            && self.b.github_token.is_none()
        {
            self.b.github_token = Some(self.github_installation_token().await?);
        }
        let (oauth_token, source_key) = self.oauth_credentials().await?;
        let branch = self.branch_name();
        let settings = ImportSettings::of(&self.app);
        let import = git::generate_git_import_commands(&ImportInput {
            dep: &self.dep,
            branch: &branch,
            repository: &self.b.repository,
            port: &self.b.port,
            kind: self.b.kind,
            source: self.b.source.as_ref(),
            settings,
            commit: &self.commit,
            private_key: key.as_deref(),
            github_token: self.b.github_token.as_deref(),
            oauth_token: oauth_token.as_deref(),
            source_key: source_key.as_deref(),
        })
        .map_err(Fail::new)?;
        self.b.branch = import.branch.clone();
        self.b.full_repo_url = import.full_repo_url.clone();
        // A GitHub App or OAuth token in the URL is redacted in the logs.
        for t in self.b.github_token.iter().chain(oauth_token.iter()) {
            let encoded = git::urlencoding_raw(t);
            if encoded != *t && !self.locked.contains(&encoded) {
                self.locked.push(encoded);
            }
            if !self.locked.contains(t) {
                self.locked.push(t.clone());
            }
        }
        Ok(import)
    }

    /// The clone credentials of a GitLab or Gitea source: `generateGitlabCloneToken()` /
    /// `generateGiteaCloneToken()` when connected, else `gitlabAppPrivateKey()` /
    /// `giteaAppPrivateKey()`.
    async fn oauth_credentials(&self) -> Result<(Option<String>, Option<String>), Fail> {
        let Some(source) = self
            .b
            .source
            .as_ref()
            .filter(|s| !s.is_github() && self.b.kind == DeploymentType::Source)
        else {
            return Ok((None, None));
        };
        let db = &self.jobs.0.db;
        let enc = self
            .jobs
            .0
            .encrypter
            .as_ref()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        if source.is_connected() {
            let token = if source.is_gitea() {
                crate::gitea::clone_token(db, enc, &source.row).await
            } else {
                crate::gitlab::clone_token(db, enc, &source.row).await
            }
            .map_err(Fail::new)?;
            return Ok((Some(token), None));
        }
        Ok((
            None,
            crate::github::team_private_key(db, enc, &source.row).await,
        ))
    }

    /// `generateGithubInstallationToken($source)`.
    async fn github_installation_token(&self) -> Result<String, Fail> {
        crate::github::installation_token(
            &self.jobs.0.db,
            self.jobs.0.encrypter.as_ref(),
            self.b.source.as_ref().map(|s| &s.row),
        )
        .await
        .map_err(Fail::new)
    }

    /// `check_git_if_build_needed()`.
    pub(super) async fn check_git_if_build_needed(&mut self) -> Step {
        if let Some(source) = self.b.source.clone()
            && self.b.kind == DeploymentType::Source
            && source.is_github()
            && !source.is_public()
        {
            // githubApi(repos/{repo}): keeps `repository_project_id` current.
            if self.b.github_token.is_none() {
                self.b.github_token = Some(self.github_installation_token().await?);
            }
            let token = self.b.github_token.clone().unwrap_or_default();
            let id = crate::github::repository_id(
                &self.jobs.0.db,
                &source.row,
                &token,
                &self.b.repository,
            )
            .await
            .map_err(Fail::new)?;
            if let Some(id) = id
                && self.app.int("repository_project_id") != Some(id)
            {
                let _ = apps::update_if_changed(
                    &self.jobs.0.db,
                    self.app.id,
                    &[("repository_project_id", ColumnValue::Int(id))],
                    now(),
                )
                .await;
                self.app
                    .raw
                    .insert("repository_project_id".into(), Value::from(id));
            }
        }
        self.git_import().await?;
        let reference = format!("refs/heads/{}", self.b.branch);
        // `data_get($this->application, 'private_key.private_key')`
        let key = self.private_key().await?;
        let commands = match key {
            Some(k) => {
                let k64 = B64.encode(k);
                let location = format!("/root/.ssh/id_rsa_coolify_{}", self.dep);
                vec![
                    Cmd::new(in_docker(&self.dep, "mkdir -p /root/.ssh")),
                    Cmd::new(in_docker(
                        &self.dep,
                        &format!("echo '{k64}' | base64 -d | tee {location} > /dev/null"),
                    ))
                    .hidden()
                    .skip_command_log(),
                    Cmd::new(in_docker(&self.dep, &format!("chmod 600 {location}"))),
                    Cmd::new(in_docker(
                        &self.dep,
                        &self.ls_remote_command(&reference, Some(&location)),
                    ))
                    .hidden()
                    .save("git_commit_sha", true),
                ]
            }
            None => vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &self.ls_remote_command(&reference, None),
                ))
                .hidden()
                .save("git_commit_sha", true),
            ],
        };
        self.exec(commands).await?;
        let rollback = self.b.rollback;
        let output = self
            .saved
            .get("git_commit_sha")
            .cloned()
            .unwrap_or_default();
        if !output.is_empty()
            && !rollback
            && self.should_resolve_branch_head_commit()
            && output.contains('\t')
        {
            static SHA: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"\b([0-9a-fA-F]{40})\s*\t").unwrap_or_else(|_| unreachable!())
            });
            if let Some(c) = SHA.captures(&output) {
                self.commit = c[1].to_owned();
                let _ = apps::update_deployment(
                    &self.jobs.0.db,
                    self.queue_id,
                    &[("commit", ColumnValue::Text(Some(self.commit.clone())))],
                    now(),
                )
                .await;
            }
        }
        self.reuse_commit_message().await;
        self.set_coolify_variables();
        if self.app.setting_bool("use_build_secrets") && self.commit != "HEAD" {
            return Err(Fail::new(
                "Build secrets are not supported by coolify-rs yet.",
            ));
        }
        Ok(())
    }

    /// `gitLsRemoteCommand($ref, $identityFile)`.
    fn ls_remote_command(&self, reference: &str, identity: Option<&str>) -> String {
        let ssh = git::git_ssh_command(&self.b.port, identity, true);
        format!(
            "GIT_SSH_COMMAND=\"{ssh}\" git ls-remote {} {}",
            quote(&self.b.full_repo_url),
            quote(reference)
        )
    }

    fn should_resolve_branch_head_commit(&self) -> bool {
        let c = self.commit.trim();
        c.is_empty() || c == "HEAD"
    }

    /// `reuse_commit_message()`.
    async fn reuse_commit_message(&mut self) {
        if self.should_resolve_branch_head_commit() {
            return;
        }
        let current = apps::deployment_by_id(&self.jobs.0.db, self.queue_id)
            .await
            .ok()
            .flatten()
            .and_then(|d| d.str("commit_message").map(str::to_owned))
            .unwrap_or_default();
        if !current.is_empty() {
            return;
        }
        if let Ok(Some(message)) =
            git_sources::latest_commit_message(&self.jobs.0.db, self.app.id, &self.commit).await
        {
            let _ = apps::update_deployment(
                &self.jobs.0.db,
                self.queue_id,
                &[("commit_message", ColumnValue::Text(Some(message)))],
                now(),
            )
            .await;
        }
    }

    /// `set_coolify_variables()`.
    fn set_coolify_variables(&mut self) {
        let mut vars = String::new();
        if self.app.setting_bool("include_source_commit_in_build") {
            vars.push_str(&format!(
                "SOURCE_COMMIT={} ",
                git::shell_value(&self.commit)
            ));
        }
        if let Some(fqdn) = self.app.str("fqdn") {
            let domains: Vec<String> = valid_domains(fqdn);
            if !domains.is_empty() {
                let url = domains
                    .iter()
                    .map(|d| domains::without_port(d))
                    .collect::<Vec<_>>()
                    .join(",");
                let hosts = hosts_from_fqdn(Some(fqdn));
                if self.parsing_version() >= 3 {
                    vars.push_str(&format!(
                        "COOLIFY_URL={} COOLIFY_FQDN={} ",
                        git::shell_value(&url),
                        git::shell_value(&hosts)
                    ));
                } else {
                    vars.push_str(&format!(
                        "COOLIFY_URL={} COOLIFY_FQDN={} ",
                        git::shell_value(&hosts),
                        git::shell_value(&url)
                    ));
                }
            }
        }
        if let Some(branch) = self.app.str("git_branch") {
            vars.push_str(&format!("COOLIFY_BRANCH={} ", git::shell_value(branch)));
        }
        vars.push_str(&format!(
            "COOLIFY_RESOURCE_UUID={} ",
            git::shell_value(&self.app.uuid)
        ));
        self.b.coolify_variables = vars;
    }

    /// `clone_repository()` with `import_repository()`.
    pub(super) async fn clone_repository(&mut self) -> Step {
        let basedir = format!("/artifacts/{}", self.dep);
        self.info(&format!("\n{SEPARATOR}")).await;
        self.info(&format!(
            "Importing {}:{} (commit sha {}) to {basedir}.",
            self.b.repository,
            self.branch_name(),
            self.commit
        ))
        .await;
        let max_attempts = 3;
        let mut attempt = 1;
        loop {
            let import = self.git_import().await?;
            match self.exec(import.commands).await {
                Ok(()) => break,
                Err(Fail::Error { code: 255, .. }) if attempt < max_attempts => {
                    let delay = (2u64 << (attempt - 1)).min(30);
                    self.log(
                        &format!(
                            "Lost the SSH connection while importing the repository. Retrying in {delay}s (attempt {}/{max_attempts}).",
                            attempt + 1
                        ),
                        "stderr",
                        false,
                    )
                    .await;
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    let _ = self
                        .exec(vec![
                            Cmd::new(in_docker(
                                &self.dep,
                                &format!("pkill git; rm -rf {}", quote(&basedir)),
                            ))
                            .hidden()
                            .ignore_errors(),
                        ])
                        .await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
        // create_workdir()
        let mkdir = in_docker(&self.dep, &format!("mkdir -p {}", self.workdir));
        let cfg = format!("mkdir -p {}", self.cfg);
        self.exec(vec![Cmd::new(mkdir), Cmd::new(cfg)]).await?;
        let log = in_docker(
            &self.dep,
            &format!(
                "cd {} && git log -1 {} --pretty=%B",
                self.workdir,
                quote(&self.commit)
            ),
        );
        self.exec(vec![Cmd::new(log).hidden().save("commit_message", true)])
            .await?;
        let message = self
            .saved
            .get("commit_message")
            .map(|m| m.trim().to_owned())
            .unwrap_or_default();
        if !message.is_empty() {
            let _ = git_sources::set_commit_message_for_commit(
                &self.jobs.0.db,
                self.app.id,
                &self.commit,
                &message,
                now(),
            )
            .await;
        }
        Ok(())
    }

    /// `cleanup_git()`.
    pub(super) async fn cleanup_git(&mut self) -> Step {
        let cmd = in_docker(&self.dep, &format!("rm -fr /artifacts/{}/.git", self.dep));
        self.exec(vec![Cmd::new(cmd)]).await
    }

    /// `generate_image_names()` for a git application.
    pub(super) fn generate_image_names(&mut self) {
        let tag: String = self.commit.chars().take(128).collect();
        let name = self
            .app
            .str("docker_registry_image_name")
            .filter(|n| !n.is_empty())
            .map_or_else(|| self.app.uuid.clone(), str::to_owned);
        if self.app.str("dockerfile").is_some_and(|d| !d.is_empty()) {
            self.b.build_image = format!("{name}:build");
            self.b.production_image = format!("{name}:latest");
            return;
        }
        self.b.build_image = format!("{name}:{tag}-build");
        self.b.production_image = format!("{name}:{tag}");
    }

    /// `check_image_locally_or_remotely()`.
    async fn check_image_locally_or_remotely(&mut self) -> Step {
        let img = self.b.production_image.clone();
        self.exec(vec![
            Cmd::new(format!("docker images -q {img} 2>/dev/null"))
                .hidden()
                .save("local_image_found", true),
        ])
        .await?;
        let found = self
            .saved
            .get("local_image_found")
            .is_some_and(|v| !v.is_empty());
        if !found
            && self
                .app
                .str("docker_registry_image_name")
                .is_some_and(|n| !n.is_empty())
        {
            self.info(&format!(
                "Image ({img}) not found on the server. Pulling it from the Docker registry."
            ))
            .await;
            self.exec(vec![
                Cmd::new(format!("docker pull {} 2>/dev/null", quote(&img)))
                    .ignore_errors()
                    .hidden(),
            ])
            .await?;
            self.exec(vec![
                Cmd::new(format!("docker images -q {img} 2>/dev/null"))
                    .hidden()
                    .save("local_image_found", true),
            ])
            .await?;
        }
        Ok(())
    }

    /// `should_skip_build()`.
    async fn should_skip_build(&mut self) -> Result<bool, Fail> {
        let found = self
            .saved
            .get("local_image_found")
            .is_some_and(|v| !v.trim().is_empty());
        if found {
            let enc =
                self.jobs.0.encrypter.clone().ok_or_else(|| {
                    Fail::new("No application encryption key has been specified.")
                })?;
            let fresh = apps::find_by_id(&self.jobs.0.db, self.app.id)
                .await
                .map_err(|e| Fail::new(e.to_string()))?
                .ok_or_else(|| Fail::new("Application not found."))?;
            let diff = crate::applications::snapshot::pending_diff(&self.jobs.0.db, &enc, &fresh)
                .await
                .map_err(Fail::new)?;
            if !diff.requires_build() {
                let img = self.b.production_image.clone();
                self.info(&format!(
                    "No build configuration changed & image found ({img}) with the same Git Commit SHA. Build step skipped."
                ))
                .await;
                self.generate_compose_file(&img).await?;
                self.save_runtime_environment().await?;
                self.push_to_docker_registry().await?;
                self.rolling_update().await?;
                return Ok(true);
            } else if diff.is_legacy_fallback() {
                self.info(
                    "The last deployment stored no configuration snapshot. Rebuilding image.",
                )
                .await;
            } else {
                self.info("Build configuration changed. Rebuilding image.")
                    .await;
            }
        } else {
            self.info(&format!(
                "Image not found ({}). Building new image.",
                self.b.production_image
            ))
            .await;
        }
        if self.b.restart_only {
            self.b.restart_only = false;
            Box::pin(self.decide_what_to_do()).await?;
        }
        Ok(false)
    }

    /// `generate_buildtime_environment_variables()`.
    pub(super) async fn generate_buildtime_environment_variables(
        &mut self,
    ) -> Result<Vec<String>, Fail> {
        let mut dict = Ordered::default();
        if self.build_pack() == "nixpacks"
            && let Some(Value::Object(plan_vars)) = self
                .b
                .nixpacks_plan
                .as_ref()
                .and_then(|p| p.get("variables"))
                .cloned()
        {
            for (key, value) in plan_vars {
                if key.starts_with("COOLIFY_") || key.starts_with("SERVICE_") {
                    continue;
                }
                self.validated_buildtime_key(&key, "the Nixpacks plan")
                    .await?;
                dict.put(&key, Some(git::bash_env_value(Some(&php_string(&value)))));
            }
        }
        for (key, value) in self.coolify_env(true).await?.0 {
            dict.put(&key, Some(git::bash_env_value(value.as_deref())));
        }
        let compose = self.build_pack() == "dockercompose";
        if compose {
            let text = if self.app.setting_bool("is_raw_compose_deployment_enabled") {
                self.app.str("docker_compose_raw")
            } else {
                self.app.str("docker_compose")
            }
            .unwrap_or_default()
            .to_owned();
            for name in crate::compose_parser::service_names(&text) {
                dict.put(
                    &format!(
                        "SERVICE_NAME_{}",
                        name.replace(['-', '.'], "_").to_uppercase()
                    ),
                    Some(git::bash_env_value(Some(&name))),
                );
            }
            for (service, entry) in
                crate::compose_parser::php_json_array(self.app.str("docker_compose_domains"))
            {
                let Some(domain) =
                    crate::compose_parser::domain_entry_string(&entry).filter(|d| !d.is_empty())
                else {
                    continue;
                };
                let first = domain.split(',').next().unwrap_or_default().to_owned();
                let Some(parts) = domains::UrlParts::parse(&first) else {
                    continue;
                };
                let key = crate::compose_parser::normalize_service_name(&service.as_string())
                    .to_uppercase();
                dict.put(
                    &format!("SERVICE_URL_{key}"),
                    Some(git::bash_env_value(Some(&domains::without_port(&first)))),
                );
                dict.put(
                    &format!("SERVICE_FQDN_{key}"),
                    Some(git::bash_env_value(Some(&parts.host))),
                );
            }
        }
        let compose_source = self
            .app
            .str("docker_compose_raw")
            .or(self.app.str("docker_compose"))
            .map(str::to_owned);
        let railpack = self.build_pack() == "railpack";
        for v in self.buildtime_variables(true).await? {
            if railpack && reserved_docker_client_key(&v.key) {
                continue;
            }
            if compose
                && (v.key.starts_with("SERVICE_FQDN_")
                    || v.key.starts_with("SERVICE_URL_")
                    || v.key.starts_with("SERVICE_NAME_")
                    || (!v.preserve_compose_null
                        && v.value.is_none()
                        && crate::compose_parser::environment_variable_should_be_unset(
                            compose_source.as_deref(),
                            &v.key,
                        )))
            {
                continue;
            }
            let literal = v.is_literal || v.is_multiline;
            let resolved = git::resolved_with_server(v.resolved.as_deref(), literal);
            let escaped = if literal {
                let raw = resolved.unwrap_or_default();
                git::bash_env_value(Some(raw.trim_matches('\'')))
            } else {
                git::bash_double_quoted(resolved.as_deref())
            };
            dict.put(&v.key, Some(escaped));
        }
        let mut out = Vec::new();
        for (key, value) in dict.0 {
            self.validated_buildtime_key(&key, "the build-time environment")
                .await?;
            out.push(format!("{key}={}", value.unwrap_or_default()));
        }
        Ok(out)
    }

    /// `validatedBuildtimeEnvironmentVariableKey($key, $origin)`.
    async fn validated_buildtime_key(&self, key: &str, origin: &str) -> Step {
        if valid_env_key(key) {
            return Ok(());
        }
        self.log(SEPARATOR, "stderr", false).await;
        self.log(
            &format!("⚠️ Invalid environment variable name from {origin}: {key}"),
            "stderr",
            false,
        )
        .await;
        self.log(
            "Build-time variable names must start with a letter or underscore and contain only letters, numbers, underscores, and dots.",
            "stderr",
            false,
        )
        .await;
        Err(Fail::Error {
            message: "Invalid build-time environment variable key.".into(),
            code: 0,
            logged: true,
        })
    }

    /// `save_buildtime_environment_variables()`.
    async fn save_buildtime_environment_variables(&mut self) -> Step {
        self.save_buildtime_environment_variables_with(None).await
    }

    /// `save_buildtime_environment_variables($environment_variables)`.
    pub(super) async fn save_buildtime_environment_variables_with(
        &mut self,
        lines: Option<Vec<String>>,
    ) -> Step {
        let lines = match lines {
            Some(l) => l,
            None => self.generate_buildtime_environment_variables().await?,
        };
        let (shell, dotted): (Vec<&String>, Vec<&String>) = lines
            .iter()
            .partition(|l| shell_env_key(l.split('=').next().unwrap_or_default()));
        if dotted.is_empty() {
            self.b.launcher = false;
            if !lines.is_empty() {
                let b64 = B64.encode(lines.join("\n"));
                self.log(
                    &format!(
                        "Creating build-time {} file in /artifacts (outside Docker context).",
                        super::ENV_FILE
                    ),
                    "stdout",
                    true,
                )
                .await;
                let cmd = in_docker(
                    &self.dep,
                    &format!("echo '{b64}' | base64 -d | tee {BUILD_TIME_ENV_PATH} > /dev/null"),
                );
                self.exec(vec![Cmd::new(cmd).skip_command_log()]).await?;
            } else if matches!(
                self.build_pack().as_str(),
                "dockercompose" | "dockerfile" | "railpack"
            ) {
                self.log(
                    &format!("Creating empty build-time {} file in /artifacts (no build-time variables defined).", super::ENV_FILE),
                    "stdout",
                    true,
                )
                .await;
                self.exec(vec![Cmd::new(in_docker(
                    &self.dep,
                    &format!("touch {BUILD_TIME_ENV_PATH}"),
                ))])
                .await?;
            }
            return Ok(());
        }
        self.b.launcher = true;
        let mut launcher = vec![
            "#!/bin/bash".to_owned(),
            "set -a".to_owned(),
            format!("source {BUILD_TIME_SHELL_ENV_PATH}"),
            "set +a".to_owned(),
            "exec env \\".to_owned(),
        ];
        for d in &dotted {
            launcher.push(format!("    {d} \\"));
        }
        launcher.push("    \"$@\"".to_owned());
        let files = [
            (BUILD_TIME_ENV_PATH, lines.join("\n")),
            (
                BUILD_TIME_SHELL_ENV_PATH,
                shell
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                BUILD_TIME_ENV_LAUNCHER_PATH,
                format!("{}\n", launcher.join("\n")),
            ),
        ];
        self.log(
            "Creating build-time environment files in /artifacts (outside Docker context).",
            "stdout",
            true,
        )
        .await;
        for (path, contents) in files {
            let cmd = in_docker(
                &self.dep,
                &format!(
                    "echo '{}' | base64 -d | tee {path} > /dev/null",
                    B64.encode(contents)
                ),
            );
            self.exec(vec![Cmd::new(cmd).skip_command_log()]).await?;
        }
        self.exec(vec![Cmd::new(in_docker(
            &self.dep,
            &format!("chmod 700 {BUILD_TIME_ENV_LAUNCHER_PATH}"),
        ))])
        .await
    }

    /// `generate_env_variables()`: `$this->env_args`.
    pub(super) async fn generate_env_variables(&mut self) -> Step {
        let mut args = Ordered::default();
        if self.app.setting_bool("include_source_commit_in_build") {
            args.put("SOURCE_COMMIT", Some(self.commit.clone()));
        }
        for (k, v) in self.coolify_env(true).await?.0 {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                args.put(&k, Some(v));
            }
        }
        let compose = self.build_pack() == "dockercompose";
        let compose_source = self
            .app
            .str("docker_compose_raw")
            .or(self.app.str("docker_compose"))
            .map(str::to_owned);
        for v in self.buildtime_variables(false).await? {
            if compose
                && (v.key.starts_with("SERVICE_FQDN_")
                    || v.key.starts_with("SERVICE_URL_")
                    || v.key.starts_with("SERVICE_NAME_")
                    || (!v.preserve_compose_null
                        && v.value.is_none()
                        && crate::compose_parser::environment_variable_should_be_unset(
                            compose_source.as_deref(),
                            &v.key,
                        )))
            {
                continue;
            }
            if let Some(value) =
                git::resolved_with_server(v.resolved.as_deref(), v.is_literal || v.is_multiline)
            {
                args.put(&v.key, Some(value));
            }
        }
        self.b.env_args = args;
        Ok(())
    }

    /// `generate_secrets_hash($variables)` for a key/value collection.
    fn secrets_hash(&self, pairs: &[(String, String)]) -> String {
        let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let text = sorted
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("|");
        self.jobs
            .0
            .encrypter
            .as_ref()
            .map(|e| e.hmac_with_app_key(text.as_bytes()))
            .unwrap_or_default()
    }

    /// `generate_build_env_variables()` without build secrets.
    pub(super) async fn generate_build_env_variables(&mut self) -> Step {
        let variables: Vec<(String, String)> = if self.build_pack() == "nixpacks" {
            match self
                .b
                .nixpacks_plan
                .as_ref()
                .and_then(|p| p.get("variables"))
            {
                Some(Value::Object(m)) => {
                    m.iter().map(|(k, v)| (k.clone(), php_string(v))).collect()
                }
                _ => Vec::new(),
            }
        } else {
            self.generate_env_variables().await?;
            self.b
                .env_args
                .0
                .iter()
                .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
                .collect()
        };
        if !variables.is_empty() {
            self.analyze_build_time_variables(&variables).await?;
        }
        let hash = (!variables.is_empty()).then(|| self.secrets_hash(&variables));
        let mut args = Vec::new();
        for (k, _) in &variables {
            if !valid_env_key(k) {
                return Err(Fail::new("Invalid environment variable key."));
            }
            args.push(format!("--build-arg {}", quote(k)));
        }
        if let Some(h) = hash {
            args.push(format!(
                "--build-arg {}",
                quote(&format!("COOLIFY_BUILD_SECRETS_HASH={h}"))
            ));
        }
        self.b.build_args = args;
        Ok(())
    }

    /// `analyzeBuildTimeVariables($variables)`.
    async fn analyze_build_time_variables(&mut self, variables: &[(String, String)]) -> Step {
        let user: Vec<String> = self
            .variables()
            .await?
            .into_iter()
            .filter(|v| !v.is_preview && v.is_buildtime)
            .map(|v| v.key)
            .collect();
        let warnings: Vec<(&str, &str, &str, &str, &str)> = variables
            .iter()
            .filter(|(k, _)| user.contains(k))
            .filter_map(|(k, v)| {
                PROBLEMATIC
                    .iter()
                    .find(|p| p.0 == k)
                    .map(|p| (p.0, v.as_str(), p.1, p.2, p.3))
            })
            .collect();
        if warnings.is_empty() {
            return Ok(());
        }
        self.info(SEPARATOR).await;
        for (key, value, affects, issue, recommendation) in warnings {
            for line in [
                format!("⚠️ Build-time environment variable warning: {key}={value}"),
                format!("   Affects: {affects}"),
                format!("   Issue: {issue}"),
                format!("   Recommendation: {recommendation}"),
            ] {
                self.log(&line, "warning", false).await;
            }
            self.info("").await;
        }
        self.log("💡 Tips to resolve build issues:", "info", false)
            .await;
        self.log(
            "   1. Set these variables as \"Runtime only\" in the environment variables settings",
            "info",
            false,
        )
        .await;
        self.log(
            "   2. Use different values for build-time (e.g., NODE_ENV=development for build)",
            "info",
            false,
        )
        .await;
        self.log("   3. Consider using multi-stage Docker builds to separate build and runtime environments", "info", false)
            .await;
        Ok(())
    }

    /// `add_build_env_variables_to_dockerfile()` (no build secrets).
    async fn add_build_env_variables_to_dockerfile(&mut self) -> Step {
        if self
            .app
            .settings
            .get("inject_build_args_to_dockerfile")
            .and_then(Value::as_bool)
            == Some(false)
        {
            self.log(
                "Skipping Dockerfile ARG injection (disabled in settings).",
                "stdout",
                true,
            )
            .await;
            return Ok(());
        }
        let path = format!("{}{}", self.workdir, self.b.dockerfile_location);
        self.saved.remove("dockerfile");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &format!("cat {path}")))
                .hidden()
                .save("dockerfile", true)
                .ignore_errors(),
        ])
        .await?;
        let content = self.saved.get("dockerfile").cloned().unwrap_or_default();
        let mut lines: Vec<String> = content.trim().split('\n').map(str::to_owned).collect();
        let froms: Vec<usize> = git::dockerfile_instructions(&lines)
            .into_iter()
            .filter(|i| i.0 == "FROM")
            .map(|i| i.2)
            .collect();
        if froms.is_empty() {
            return Ok(());
        }
        let envs = self.buildtime_variables(false).await?;
        let mut args: Vec<String> = envs.iter().map(|e| format!("ARG {}", e.key)).collect();
        if !self.b.coolify_variables.is_empty() {
            for token in self
                .b
                .coolify_variables
                .trim()
                .split(' ')
                .filter(|t| !t.is_empty())
            {
                args.push(format!(
                    "ARG {}",
                    shell_assignment_for_dockerfile_arg(token)
                ));
            }
        }
        if !args.is_empty() {
            let pairs: Vec<(String, String)> = envs
                .iter()
                .map(|e| (e.key.clone(), git::bash_env_value(e.resolved.as_deref())))
                .collect();
            args.push(format!(
                "ARG COOLIFY_BUILD_SECRETS_HASH={}",
                self.secrets_hash(&pairs)
            ));
        }
        for from in froms.iter().rev() {
            for (offset, arg) in args.iter().enumerate() {
                lines.insert(from + 1 + offset, arg.clone());
            }
        }
        self.log("Final Dockerfile:", "info", true).await;
        let b64 = B64.encode(lines.join("\n"));
        self.exec(vec![
            Cmd::new(in_docker(
                &self.dep,
                &format!("echo '{b64}' | base64 -d | tee {path} > /dev/null"),
            ))
            .hidden()
            .skip_command_log(),
        ])
        .await
    }

    /// `wrap_build_command_with_env_export($cmd)`.
    fn wrap(&self, cmd: &str) -> String {
        if !self.b.launcher {
            format!(
                "cd {} && set -a && source {BUILD_TIME_ENV_PATH} && set +a && {cmd}",
                self.workdir
            )
        } else {
            format!(
                "cd {} && bash {BUILD_TIME_ENV_LAUNCHER_PATH} /bin/bash -c {}",
                self.workdir,
                git::bash_env_value(Some(cmd))
            )
        }
    }

    /// The build.sh write / cat / run triple, after `before`.
    async fn run_build_script(&mut self, before: Vec<Cmd>, build_command: &str) -> Step {
        let b64 = B64.encode(build_command);
        let mut commands = before;
        commands.push(
            Cmd::new(in_docker(
                &self.dep,
                &format!("echo '{b64}' | base64 -d | tee {BUILD_SCRIPT_PATH} > /dev/null"),
            ))
            .hidden(),
        );
        commands.push(Cmd::new(in_docker(&self.dep, &format!("cat {BUILD_SCRIPT_PATH}"))).hidden());
        commands
            .push(Cmd::new(in_docker(&self.dep, &format!("bash {BUILD_SCRIPT_PATH}"))).hidden());
        self.exec(commands).await
    }

    /// `staticImage()`.
    fn static_image(&self) -> Result<String, Fail> {
        match self.app.str("static_image") {
            Some("nginx:alpine") => Ok("nginx:alpine".into()),
            other => Err(Fail::new(format!(
                "The static image '{}' is not supported. Select a supported web server (nginx:alpine) in the application settings and deploy again.",
                other.unwrap_or_default()
            ))),
        }
    }

    /// `pull_latest_image($image)`.
    async fn pull_latest_image(&mut self, image: &str) -> Step {
        self.info(&format!(
            "Pulling latest image ({image}) from the registry."
        ))
        .await;
        self.exec(vec![
            Cmd::new(in_docker(
                &self.dep,
                &format!("docker pull {}", quote(image)),
            ))
            .hidden(),
        ])
        .await
    }

    /// The nginx config of a static image: `custom_nginx_configuration` or the default.
    fn nginx_config(&self) -> String {
        let custom = self
            .app
            .str("custom_nginx_configuration")
            .and_then(|c| B64.decode(c).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|c| !c.is_empty());
        custom.unwrap_or_else(|| {
            git::default_nginx_configuration(self.app.setting_bool("is_spa")).to_owned()
        })
    }

    /// `build_static_image()`.
    async fn build_static_image(&mut self) -> Step {
        self.info(SEPARATOR).await;
        self.info("Static deployment. Copying static assets to the image.")
            .await;
        if self.app.str("static_image").is_some_and(|s| !s.is_empty()) {
            let image = self.static_image()?;
            self.pull_latest_image(&image).await?;
        }
        let image = self.static_image()?;
        let dockerfile = format!(
            "FROM {image}\n        WORKDIR /usr/share/nginx/html/\n        LABEL coolify.deploymentId={}\n        COPY . .\n        RUN rm -f /usr/share/nginx/html/nginx.conf\n        RUN rm -f /usr/share/nginx/html/Dockerfile\n        RUN rm -f /usr/share/nginx/html/docker-compose.yaml\n        RUN rm -f /usr/share/nginx/html/{}\n        COPY ./nginx.conf /etc/nginx/conf.d/default.conf",
            self.dep,
            super::ENV_FILE
        );
        let w = self.workdir.clone();
        let build = if self.b.buildkit {
            format!(
                "DOCKER_BUILDKIT=1 docker build {} --network host -f {w}/Dockerfile --progress plain -t {} {w}",
                self.b.add_hosts, self.b.production_image
            )
        } else {
            format!(
                "docker build {} --network host -f {w}/Dockerfile -t {} {w}",
                self.b.add_hosts, self.b.production_image
            )
        };
        let before = vec![
            Cmd::new(in_docker(
                &self.dep,
                &format!(
                    "echo '{}' | base64 -d | tee {w}/Dockerfile > /dev/null",
                    B64.encode(dockerfile)
                ),
            )),
            Cmd::new(in_docker(
                &self.dep,
                &format!(
                    "echo '{}' | base64 -d | tee {w}/nginx.conf > /dev/null",
                    B64.encode(self.nginx_config())
                ),
            )),
        ];
        self.run_build_script(before, &build).await?;
        self.info("Building docker image completed.").await;
        Ok(())
    }

    /// `build_image()` without build secrets.
    async fn build_image(&mut self) -> Step {
        for (key, _) in self.coolify_env(true).await?.0 {
            self.validated_buildtime_key(&key, "the Coolify build environment")
                .await?;
            self.b
                .build_args
                .push(format!("--build-arg {}", quote(&key)));
        }
        let build_args = self.b.build_args.join(" ");
        self.info(SEPARATOR).await;
        if self.b.disable_build_cache {
            self.info(
                "Docker build cache is disabled. It will not be used during the build process.",
            )
            .await;
        }
        if self.build_pack() == "static" {
            self.info("Static deployment. Copying static assets to the image.")
                .await;
        } else {
            self.info("Building docker image started.").await;
            self.info("To check the current progress, click on Show Debug Logs.")
                .await;
        }
        let w = self.workdir.clone();
        let hosts = self.b.add_hosts.clone();
        let target = self.b.build_target.clone().unwrap_or_default();
        let df = self.b.dockerfile_location.clone();
        let nc = if self.b.force_rebuild {
            "--no-cache "
        } else {
            ""
        };
        let nixpacks = self.build_pack() == "nixpacks";
        let is_static = self.app.setting_bool("is_static");
        // Static sites build the app first, then copy it into the web server image.
        let first_image = if is_static {
            self.b.build_image.clone()
        } else {
            self.b.production_image.clone()
        };
        if is_static && self.app.str("static_image").is_some_and(|s| !s.is_empty()) {
            let image = self.static_image()?;
            self.pull_latest_image(&image).await?;
            self.info("Continuing with the building process.").await;
        }
        if nixpacks {
            let plan = self
                .b
                .nixpacks_plan
                .as_ref()
                .map(coolify_compat::php_json::encode_pretty)
                .unwrap_or_default();
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!(
                        "echo '{}' | base64 -d | tee {NIXPACKS_PLAN_PATH} > /dev/null",
                        B64.encode(plan)
                    ),
                ))
                .hidden()
                .skip_command_log(),
            ])
            .await?;
            let cache = if self.b.force_rebuild {
                "--no-cache".to_owned()
            } else {
                format!("--cache-key '{}'", self.app.uuid)
            };
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!("nixpacks build -c {NIXPACKS_PLAN_PATH} {cache} --no-error-without-start -n {first_image} {w} -o {w}"),
                ))
                .hidden(),
                Cmd::new(in_docker(&self.dep, &format!("cat {w}/.nixpacks/Dockerfile"))).hidden(),
            ])
            .await?;
            let build = if self.b.buildkit {
                self.wrap(&format!("DOCKER_BUILDKIT=1 docker build {nc}{hosts} --network host -f {w}/.nixpacks/Dockerfile --progress plain -t {first_image} {build_args} {w}"))
            } else {
                self.wrap(&format!("docker build {nc}{hosts} --network host -f {w}/.nixpacks/Dockerfile -t {first_image} {build_args} {w}"))
            };
            self.run_build_script(Vec::new(), &build).await?;
            self.exec(vec![
                Cmd::new(in_docker(&self.dep, &format!("rm {NIXPACKS_PLAN_PATH}"))).hidden(),
            ])
            .await?;
        } else {
            let build = if self.b.buildkit {
                self.wrap(&format!("DOCKER_BUILDKIT=1 docker build {nc}{target} {hosts} --network host -f {w}{df} --progress plain -t {first_image} {build_args} {w}"))
            } else {
                self.wrap(&format!("docker build {nc}{target} {hosts} --network host -f {w}{df} {build_args} -t {first_image} {w}"))
            };
            self.run_build_script(Vec::new(), &build).await?;
        }
        if is_static {
            let publish = self
                .app
                .str("publish_directory")
                .unwrap_or_default()
                .trim_matches('/')
                .to_owned();
            let publish = if publish.is_empty() {
                String::new()
            } else {
                format!("/{publish}")
            };
            let dockerfile = format!(
                "FROM {}\nWORKDIR /usr/share/nginx/html/\nLABEL coolify.deploymentId={}\nCOPY --from={first_image} /app{publish} .\nCOPY ./nginx.conf /etc/nginx/conf.d/default.conf",
                self.static_image()?,
                self.dep
            );
            let image = self.b.production_image.clone();
            let build = if self.b.buildkit {
                self.wrap(&format!("DOCKER_BUILDKIT=1 docker build {hosts} --network host -f {w}/Dockerfile {build_args} --progress plain -t {image} {w}"))
            } else {
                self.wrap(&format!("docker build {hosts} --network host -f {w}/Dockerfile {build_args} -t {image} {w}"))
            };
            let before = vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!(
                        "echo '{}' | base64 -d | tee {w}/Dockerfile > /dev/null",
                        B64.encode(dockerfile)
                    ),
                )),
                Cmd::new(in_docker(
                    &self.dep,
                    &format!(
                        "echo '{}' | base64 -d | tee {w}/nginx.conf > /dev/null",
                        B64.encode(self.nginx_config())
                    ),
                )),
            ];
            self.run_build_script(before, &build).await?;
        }
        self.info("Building docker image completed.").await;
        Ok(())
    }

    /// `push_to_docker_registry()`.
    async fn push_to_docker_registry(&mut self) -> Step {
        let Some(name) = self
            .app
            .str("docker_registry_image_name")
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
        else {
            return Ok(());
        };
        if self.b.restart_only || self.build_pack() == "dockerimage" {
            return Ok(());
        }
        let img = self.b.production_image.clone();
        let pushed = async {
            self.instant(
                &[&format!("docker images --format '{{{{json .}}}}' {img}")],
                true,
            )
            .await?;
            self.info(SEPARATOR).await;
            self.info(&format!("Pushing image to docker registry ({img})."))
                .await;
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!("docker push {}", quote(&img)),
                ))
                .hidden(),
            ])
            .await?;
            if let Some(tag) = self
                .app
                .str("docker_registry_image_tag")
                .filter(|t| !t.trim().is_empty())
                .map(str::to_owned)
            {
                self.info(&format!("Tagging and pushing image with {tag} tag."))
                    .await;
                let tagged = format!("{name}:{tag}");
                self.exec(vec![
                    Cmd::new(in_docker(
                        &self.dep,
                        &format!("docker tag {} {}", quote(&img), quote(&tagged)),
                    ))
                    .ignore_errors()
                    .hidden(),
                    Cmd::new(in_docker(
                        &self.dep,
                        &format!("docker push {}", quote(&tagged)),
                    ))
                    .ignore_errors()
                    .hidden(),
                ])
                .await?;
            }
            Ok::<(), Fail>(())
        };
        match pushed.await {
            Err(Fail::Error { message, code, .. }) => {
                self.info("Failed to push image to docker registry. Please check debug logs for more information.")
                    .await;
                Err(Fail::Error {
                    message: format!("App\\Exceptions\\DeploymentException: {message}"),
                    code,
                    logged: false,
                })
            }
            other => other,
        }
    }

    /// `generate_nixpacks_confs()`.
    async fn generate_nixpacks_confs(&mut self) -> Step {
        let command = self.nixpacks_build_cmd().await?;
        self.info(&format!(
            "Generating nixpacks configuration with: {command}"
        ))
        .await;
        self.saved.remove("nixpacks_plan");
        self.saved.remove("nixpacks_type");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &command))
                .save("nixpacks_plan", true)
                .hidden(),
            Cmd::new(in_docker(
                &self.dep,
                &format!("nixpacks detect {}", self.workdir),
            ))
            .save("nixpacks_type", true)
            .hidden(),
        ])
        .await?;
        let mut kind = String::new();
        if let Some(t) = self.saved.get("nixpacks_type").filter(|t| !t.is_empty()) {
            kind = t.trim().to_owned();
            if kind.is_empty() {
                return Err(Fail::new(
                    "Nixpacks failed to detect the application type. Please check the documentation of Nixpacks: https://nixpacks.com/docs/providers",
                ));
            }
        }
        let Some(raw) = self
            .saved
            .get("nixpacks_plan")
            .filter(|p| !p.is_empty())
            .cloned()
        else {
            return Ok(());
        };
        self.info(&format!("Found application type: {kind}.")).await;
        self.info(&format!("If you need further customization, please check the documentation of Nixpacks: https://nixpacks.com/docs/providers/{kind}"))
            .await;
        let mut parsed = php_assoc(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null));
        if !parsed.is_object() {
            parsed = Value::Object(Map::new());
        }
        self.generate_env_variables().await?;
        // collect(plan variables)->merge($this->env_args)
        let mut merged: Vec<(String, Value)> = match parsed.get("variables") {
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => Vec::new(),
        };
        for (k, v) in &self.b.env_args.0 {
            let value = v.clone().map_or(Value::Null, Value::String);
            match merged.iter_mut().find(|(key, _)| key == k) {
                Some(slot) => slot.1 = value,
                None => merged.push((k.clone(), value)),
            }
        }
        let merged = self
            .without_dotted_build_variables(merged, "Nixpacks")
            .await;
        let mut apt: Vec<Value> = parsed
            .pointer("/phases/setup/aptPkgs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if apt.is_empty() {
            apt = vec![Value::from("curl"), Value::from("wget")];
        } else {
            for p in ["curl", "wget"] {
                if !apt.iter().any(|a| a.as_str() == Some(p)) {
                    apt.push(Value::from(p));
                }
            }
        }
        data_set(
            &mut parsed,
            &["phases", "setup", "aptPkgs"],
            Value::Array(apt),
        );
        let vars_value = if merged.is_empty() {
            Value::Array(Vec::new())
        } else {
            Value::Object(merged.into_iter().collect())
        };
        data_set(&mut parsed, &["variables"], vars_value);
        if parsed.pointer("/variables/IS_LARAVEL").is_some_and(|v| {
            !matches!(v, Value::Null | Value::Bool(false))
                && v.as_str() != Some("")
                && v.as_str() != Some("0")
        }) {
            return Err(Fail::new(
                "Laravel applications built with Nixpacks are not supported by coolify-rs yet.",
            ));
        }
        if kind == "node" && parsed.pointer("/variables/NIXPACKS_NODE_VERSION").is_none() {
            self.info(SEPARATOR).await;
            self.info("⚠️ NIXPACKS_NODE_VERSION not set. Nixpacks will use Node.js 18 by default, which is EOL.").await;
            self.info("You can override this by setting NIXPACKS_NODE_VERSION=22 in your environment variables.").await;
        }
        let mut for_log = parsed.clone();
        if let Some(o) = for_log.as_object_mut() {
            o.remove("variables");
        }
        self.log(
            &format!(
                "Final Nixpacks plan: {}",
                coolify_compat::php_json::encode_pretty(&for_log)
            ),
            "stdout",
            true,
        )
        .await;
        if kind == "rust" {
            let _ = apps::update_if_changed(
                &self.jobs.0.db,
                self.app.id,
                &[("health_check_enabled", ColumnValue::Bool(false))],
                now(),
            )
            .await;
            self.app
                .raw
                .insert("health_check_enabled".into(), Value::Bool(false));
        }
        self.b.nixpacks_plan = Some(parsed);
        Ok(())
    }

    /// `withoutDottedBuildVariables($variables, $builder)`.
    async fn without_dotted_build_variables(
        &self,
        vars: Vec<(String, Value)>,
        builder: &str,
    ) -> Vec<(String, Value)> {
        let dotted: Vec<String> = vars
            .iter()
            .filter(|(k, _)| k.contains('.'))
            .map(|(k, _)| k.clone())
            .collect();
        if dotted.is_empty() {
            return vars;
        }
        for key in &dotted {
            self.log(
                &format!("⚠️ Build-time variable {key} has a dot in its name, which {builder} cannot pass to the build. It is skipped during the build, but is still passed to the container."),
                "stderr",
                false,
            )
            .await;
        }
        self.log("   If the build needs the value, rename the variable. Otherwise, disable \"Available at Buildtime\" for it.", "info", false)
            .await;
        vars.into_iter().filter(|(k, _)| !k.contains('.')).collect()
    }

    /// `nixpacks_build_cmd()` with `generate_nixpacks_env_variables()`.
    async fn nixpacks_build_cmd(&mut self) -> Result<String, Fail> {
        let mut args: Vec<String> = Vec::new();
        for v in self.variables().await? {
            if v.is_preview || !v.key.starts_with("NIXPACKS_") {
                continue;
            }
            let literal = v.is_literal || v.is_multiline;
            if let Some(resolved) =
                git::resolved_with_server(v.resolved.as_deref(), literal).filter(|r| !r.is_empty())
            {
                let value = if literal {
                    resolved.trim_matches('\'').to_owned()
                } else {
                    resolved
                };
                args.push(format!(
                    "--env {}",
                    git::shell_value(&format!("{}={value}", v.key))
                ));
            }
        }
        for (k, v) in self.coolify_env(true).await?.0 {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                args.push(format!("--env {}", git::shell_value(&format!("{k}={v}"))));
            }
        }
        let mut command = format!("nixpacks plan -f json {}", args.join(" "));
        for (flag, column) in [
            ("--build-cmd", "build_command"),
            ("--start-cmd", "start_command"),
            ("--install-cmd", "install_command"),
        ] {
            if let Some(c) = self.app.str(column).filter(|c| !c.is_empty()) {
                command.push_str(&format!(" {flag} {}", git::shell_value(c)));
            }
        }
        command.push_str(&format!(" {}", self.workdir));
        Ok(command)
    }

    /// `parseHealthcheckFromDockerfile()` on the repository's Dockerfile (inside `generate_compose_file()`).
    pub(super) async fn read_dockerfile_healthcheck(&mut self) -> Step {
        let path = format!("{}{}", self.workdir, self.b.dockerfile_location);
        self.saved.remove("dockerfile_from_repo");
        self.exec(vec![
            Cmd::new(in_docker(&self.dep, &format!("cat {path}")))
                .hidden()
                .save("dockerfile_from_repo", true)
                .ignore_errors(),
        ])
        .await?;
        let content = self
            .saved
            .get("dockerfile_from_repo")
            .cloned()
            .unwrap_or_default();
        let disabled = self
            .app
            .raw
            .get("health_check_enabled")
            .and_then(Value::as_bool)
            == Some(false);
        let u = git::parse_healthcheck(
            &content,
            disabled,
            self.app.bool("custom_healthcheck_found"),
        );
        let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
        if u.reset {
            values = vec![
                ("custom_healthcheck_found", ColumnValue::Bool(false)),
                ("health_check_interval", ColumnValue::Int(5)),
                ("health_check_timeout", ColumnValue::Int(5)),
                ("health_check_retries", ColumnValue::Int(10)),
                ("health_check_start_period", ColumnValue::Int(5)),
            ];
        } else if u.found {
            for (col, v) in [
                ("health_check_interval", u.interval),
                ("health_check_timeout", u.timeout),
                ("health_check_start_period", u.start_period),
                ("health_check_retries", u.retries),
            ] {
                if let Some(v) = v {
                    values.push((col, ColumnValue::Int(v)));
                }
            }
            values.push(("custom_healthcheck_found", ColumnValue::Bool(true)));
        }
        if !values.is_empty() {
            for (col, v) in &values {
                let json = match v {
                    ColumnValue::Bool(b) => Value::Bool(*b),
                    ColumnValue::Int(i) => Value::from(*i),
                    _ => Value::Null,
                };
                self.app.raw.insert((*col).to_owned(), json);
            }
            let _ = apps::update_if_changed(&self.jobs.0.db, self.app.id, &values, now()).await;
        }
        Ok(())
    }
}

/// `data_set($target, 'a.b.c', $value)` on nested objects (missing levels are created).
fn data_set(target: &mut Value, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cur = target;
    for p in parents {
        if !cur.get(*p).is_some_and(Value::is_object)
            && let Some(o) = cur.as_object_mut()
        {
            o.insert((*p).to_owned(), Value::Object(Map::new()));
        }
        let Some(next) = cur.get_mut(*p) else {
            return;
        };
        cur = next;
    }
    if let Some(o) = cur.as_object_mut() {
        o.insert((*last).to_owned(), value);
    }
}

/// `shellAssignmentForDockerfileArg($assignment)`.
fn shell_assignment_for_dockerfile_arg(assignment: &str) -> String {
    let Some((key, value)) = assignment.split_once('=') else {
        return assignment.to_owned();
    };
    let value = if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("'\\''", "'")
    } else {
        value.to_owned()
    };
    format!("{key}={value}")
}

/// `validDomainsFromFqdn($fqdn)`.
fn valid_domains(fqdn: &str) -> Vec<String> {
    fqdn.split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty() && domains::is_valid_domain_url(d))
        .map(str::to_owned)
        .collect()
}

/// `hostsFromFqdn($fqdn)`.
fn hosts_from_fqdn(fqdn: Option<&str>) -> String {
    valid_domains(fqdn.unwrap_or_default())
        .iter()
        .filter_map(|d| domains::UrlParts::parse(d).map(|p| p.host))
        .collect::<Vec<_>>()
        .join(",")
}

/// `validatePathField($value, $field)`.
pub(super) fn validate_path(value: &str, field: &str) -> Result<String, Fail> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^/[a-zA-Z0-9._\-/~@+]+$").unwrap_or_else(|_| unreachable!()));
    if !RE.is_match(value) {
        return Err(Fail::new(format!(
            "Invalid {field}: contains forbidden characters."
        )));
    }
    if value.contains("..") {
        return Err(Fail::new(format!(
            "Invalid {field}: path traversal detected."
        )));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assoc_and_args() {
        let v: Value =
            serde_json::from_str(r#"{"a":{},"b":{"0":"x","1":"y"},"c":{"k":1}}"#).unwrap();
        assert_eq!(
            php_assoc(v).to_string(),
            r#"{"a":[],"b":["x","y"],"c":{"k":1}}"#
        );
        assert_eq!(
            shell_assignment_for_dockerfile_arg("COOLIFY_URL='https://a.b'"),
            "COOLIFY_URL=https://a.b"
        );
        assert_eq!(
            shell_assignment_for_dockerfile_arg("X='it'\\''s'"),
            "X=it's"
        );
        assert!(validate_path("/Dockerfile", "dockerfile_location").is_ok());
        assert!(validate_path("/../x", "dockerfile_location").is_err());
    }
}
