//! The Docker Compose build pack of `ApplicationDeploymentJob`
//! (`deploy_docker_compose_buildpack()`, pull request 0): clone, `loadComposeFile()`,
//! `applicationParser()`, build, pull, stop the old containers, `docker compose up`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::applications as apps;
use coolify_remote::shell::quote;

use super::build::BUILD_TIME_ENV_PATH;
use super::{Cmd, ENV_FILE, Fail, Run, Step, in_docker};
use crate::applications::compose::ComposeEnv;
use crate::compose_parser::{self, LoadError, ParsedCompose};

/// `BUILD_FINGERPRINT_LABEL`.
const BUILD_FINGERPRINT_LABEL: &str = "coolify.build-fingerprint";

fn get<'a>(v: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    match v {
        PhpValue::Array(items) => items
            .iter()
            .find(|(k, _)| k.as_string() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

fn is_list(items: &[(Key, PhpValue)]) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(i, (k, _))| *k == Key::Int(i64::try_from(i).unwrap_or(-1)))
}

/// `html_entity_decode($message, ENT_QUOTES | ENT_HTML5)` for the entities `e()` produces.
fn html_decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `lcfirst()`.
fn lcfirst(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `resolveComposeDockerfilePath($build)`.
fn compose_dockerfile_path(build: &PhpValue) -> Option<String> {
    static REMOTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^[a-z][a-z0-9+.-]*://|^git@").unwrap_or_else(|_| unreachable!())
    });
    let (context, dockerfile) = match build {
        PhpValue::Str(s) => (Some(s.clone()), Some("Dockerfile".to_owned())),
        PhpValue::Array(_) => {
            if get(build, "dockerfile_inline").is_some() {
                return None;
            }
            let ctx = match get(build, "context") {
                None => Some(".".to_owned()),
                Some(PhpValue::Str(s)) => Some(s.clone()),
                _ => None,
            };
            let df = match get(build, "dockerfile") {
                None => Some("Dockerfile".to_owned()),
                Some(PhpValue::Str(s)) => Some(s.clone()),
                _ => None,
            };
            (ctx, df)
        }
        _ => return None,
    };
    let (context, dockerfile) = (context?, dockerfile?);
    if context.is_empty()
        || format!("{context}{dockerfile}").contains('$')
        || REMOTE.is_match(&context)
    {
        return None;
    }
    Some(if dockerfile.starts_with('/') {
        dockerfile
    } else {
        format!("{}/{dockerfile}", context.trim_end_matches('/'))
    })
}

/// `composeArgInjectionSkippedMessage($service, $build, $variables)`.
fn arg_injection_skipped_message(service: &str, build: &PhpValue, keys: &[String]) -> String {
    static REMOTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^[a-z][a-z0-9+.-]*://|^git@").unwrap_or_else(|_| unreachable!())
    });
    let context = match build {
        PhpValue::Str(s) => Some(s.as_str()),
        _ => match get(build, "context") {
            Some(PhpValue::Str(s)) => Some(s.as_str()),
            _ => None,
        },
    };
    let dockerfile = match get(build, "dockerfile") {
        Some(PhpValue::Str(s)) if matches!(build, PhpValue::Array(_)) => Some(s.as_str()),
        _ => None,
    };
    let reason = if matches!(build, PhpValue::Array(_)) && get(build, "dockerfile_inline").is_some()
    {
        "the service uses an inline Dockerfile (dockerfile_inline)"
    } else if context.is_some_and(|c| REMOTE.is_match(c)) {
        "the build context is a remote Git URL"
    } else if context.is_some_and(|c| c.contains('$'))
        || dockerfile.is_some_and(|d| d.contains('$'))
    {
        "the build context or Dockerfile path uses variables"
    } else {
        "Coolify cannot read the build definition"
    };
    // Valid names, COOLIFY_* last (a stable sort).
    let mut names: Vec<&String> = keys
        .iter()
        .filter(|k| super::build::valid_env_key(k))
        .collect();
    names.sort_by_key(|k| k.starts_with("COOLIFY_"));
    let message = format!(
        "Skipping ARG injection for service {service}: {reason}. Docker ignores build-time variables that the Dockerfile does not declare, so add 'ARG <NAME>' lines to the Dockerfile for the ones you need"
    );
    if names.is_empty() {
        return format!("{message}.");
    }
    let listed = names
        .iter()
        .take(10)
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let more = names.len().saturating_sub(10);
    if more > 0 {
        format!("{message}: {listed} and {more} more.")
    } else {
        format!("{message}: {listed}.")
    }
}

impl Run {
    /// `deploy_docker_compose_buildpack()`.
    pub(super) async fn deploy_docker_compose(&mut self) -> Step {
        if let Some(loc) = self
            .app
            .str("docker_compose_location")
            .filter(|l| !l.is_empty())
        {
            self.b.compose_location = super::build::validate_path(loc, "docker_compose_location")?;
        }
        let preserve = self.app.setting_bool("is_preserve_repository_enabled");
        if let Some(command) = self
            .app
            .str("docker_compose_custom_start_command")
            .filter(|c| !c.is_empty())
        {
            validate_shell_safe_command(command, "docker_compose_custom_start_command")?;
            let project = if preserve { &self.cfg } else { &self.workdir };
            self.b.compose_start_command = Some(with_project_directory(command, project));
        }
        if let Some(command) = self
            .app
            .str("docker_compose_custom_build_command")
            .filter(|c| !c.is_empty())
        {
            validate_shell_safe_command(command, "docker_compose_custom_build_command")?;
            self.b.compose_build_command = Some(with_project_directory(command, &self.workdir));
        }
        let name = self.app.str("name").unwrap_or_default().to_owned();
        if self.b.is_preview() {
            self.info(&format!(
                "Starting pull request (#{}) deployment of {}:{} to {}.",
                self.b.pull_request_id,
                self.b.repository,
                self.branch_name(),
                self.server_name
            ))
            .await;
        } else {
            self.info(&format!(
                "Starting deployment of {name} to {}.",
                self.server_name
            ))
            .await;
        }
        self.prepare_builder_image().await?;
        self.check_git_if_build_needed().await?;
        self.clone_repository().await?;
        if preserve {
            self.sync_file_storages_with_repository().await?;
        }
        self.generate_image_names();
        self.cleanup_git().await?;
        self.generate_build_env_variables().await?;

        // loadComposeFileForDeployment()
        if let Err(e) = compose_parser::load_compose_file(&self.jobs, self.app.id, false).await {
            return Err(match e {
                LoadError::Deployment(message) => {
                    let line = format!("Deployment stopped: {}", lcfirst(&html_decode(&message)));
                    self.log(&line, "stderr", false).await;
                    Fail::Error {
                        message: line,
                        code: 0,
                        logged: true,
                    }
                }
                other => Fail::new(other.message().to_owned()),
            });
        }
        self.reload_app().await?;

        let raw_mode = self.app.setting_bool("is_raw_compose_deployment_enabled");
        let (yaml, compose_file) = if raw_mode {
            let raw = compose_parser::old_raw_parser(&self.jobs, self.app.id)
                .await
                .map_err(Fail::new)?;
            self.app
                .raw
                .insert("docker_compose_raw".into(), Value::String(raw.clone()));
            // Users define the secrets in their own compose file.
            if self.b.secrets_supported && !self.b.build_secrets.is_empty() {
                self.info("Build secrets are configured. Ensure your docker-compose file includes build.secrets configuration for services that need them.")
                    .await;
            }
            (raw, None)
        } else {
            let mut parsed = compose_parser::application_parser(
                &self.jobs,
                self.app.id,
                self.b.pull_request_id,
                Some(&self.commit),
            )
            .await
            .map_err(Fail::new)?;
            // `parseComposeFileForDeployment()`: no services means the file could not be parsed; an
            // empty file would fail later with Docker's unclear "no service selected".
            if parsed.services().is_empty() {
                return Err(Fail::new(format!(
                    "The Docker Compose file has no services, or Coolify could not parse it. Check that {} is valid YAML and has a services section.",
                    self.app.str("docker_compose_location").unwrap_or_default()
                )));
            }
            // One sync of the file storages with the server, which loads the files that come from
            // the repository. A preview uses its own copies of the files.
            if !self.b.is_preview() {
                self.jobs.spawn_application_files_from_server(self.app.id);
            }
            for w in parsed.warnings.clone() {
                self.log(&format!("Warning: {w}"), "stderr", false).await;
            }
            self.reload_app().await?;
            // Always add .env file to services
            if let Some(services) = parsed.services_mut() {
                for (_, service) in services.iter_mut() {
                    service.set("env_file", PhpValue::strings([ENV_FILE]));
                }
            }
            // Add build secrets to compose file if enabled and BuildKit is supported
            if self.b.secrets_supported && !self.b.build_secrets.is_empty() {
                self.add_build_secrets_to_compose(&mut parsed).await?;
            }
            let build_env = self.generate_buildtime_environment_variables().await?;
            let fingerprint = self.compose_build_fingerprint(&build_env).await?;
            add_build_fingerprint(&mut parsed, &fingerprint);
            self.b.compose_fingerprint = Some(fingerprint);
            self.b.compose_build_env = Some(build_env);
            (parsed.to_yaml(4), Some(parsed))
        };
        self.generated_compose = Some(yaml.clone());
        let (dep, w, loc) = (
            self.dep.clone(),
            self.workdir.clone(),
            self.b.compose_location.clone(),
        );
        self.exec(vec![
            Cmd::new(format!("docker exec {dep} rm -f {w}{loc}")).hidden(),
            Cmd::new(format!("docker exec -i {dep} tee {w}{loc} > /dev/null"))
                .input(yaml)
                .hidden(),
        ])
        .await?;

        let reuse = match &compose_file {
            Some(c) => self.can_reuse_compose_images(c).await?,
            None => false,
        };
        if !reuse {
            self.build_compose_images(compose_file.as_ref()).await?;
        }

        self.b.compose_env = Some(self.compose_runtime_env().await?);
        self.save_runtime_environment().await?;
        self.pull_compose_images().await?;
        self.stop_running_container(true).await?;
        self.info("Starting new application.").await;
        let network = if self.b.is_preview() {
            format!("{}-{}", self.app.uuid, self.b.pull_request_id)
        } else {
            self.app.uuid.clone()
        };
        self.exec(vec![
            Cmd::new(format!(
                "docker network inspect '{network}' >/dev/null 2>&1 || docker network create --attachable '{network}' >/dev/null || true"
            ))
            .hidden()
            .ignore_errors(),
            Cmd::new(format!("docker network connect {network} coolify-proxy >/dev/null 2>&1 || true"))
                .hidden()
                .ignore_errors(),
        ])
        .await?;
        self.start_compose_services().await?;
        self.info("New container started.").await;
        Ok(())
    }

    /// The loop over `fileStorages` after the clone: a file storage follows what the path is in
    /// the repository.
    async fn sync_file_storages_with_repository(&mut self) -> Step {
        use coolify_db::servers::ColumnValue;
        use coolify_db::storages::{StorageKind, save_dirty};
        let (files, owner) = self.file_storages().await?;
        for mut file in files {
            let real_path = file.fs_path.replace(&self.cfg, &self.workdir);
            let key = format!("file_stat_{}", file.id);
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!("stat -c '%F' {}", quote(&real_path)),
                ))
                .hidden()
                .ignore_errors()
                .save(key.clone(), true),
            ])
            .await?;
            let stat = self
                .saved
                .get(&key)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            let dirty = if stat == "directory" && !file.is_directory {
                file.is_directory = true;
                file.content = None;
                vec![
                    ("is_directory", ColumnValue::Bool(true)),
                    ("content", ColumnValue::Text(None)),
                ]
            } else if stat == "regular file" && file.is_directory {
                file.is_directory = false;
                file.is_based_on_git = true;
                vec![
                    ("is_directory", ColumnValue::Bool(false)),
                    ("is_based_on_git", ColumnValue::Bool(true)),
                ]
            } else {
                continue;
            };
            save_dirty(
                &self.jobs.0.db,
                StorageKind::File,
                file.id,
                &dirty,
                super::now(),
            )
            .await
            .map_err(|e| Fail::new(e.to_string()))?;
            // On the server the application runs on (`$this->mainServer`), at the paths of this
            // deployment (`forPullRequest()`).
            self.jobs
                .delete_storage_on(&owner, &file, self.main_server.id)
                .await
                .map_err(Fail::new)?;
            self.jobs
                .save_storage_on(&owner, &file, self.main_server.id)
                .await
                .map_err(Fail::new)?;
        }
        Ok(())
    }

    /// `$this->application->refresh()` after the parser wrote its columns.
    async fn reload_app(&mut self) -> Step {
        let fresh = apps::find_by_id(&self.jobs.0.db, self.app.id)
            .await
            .map_err(|e| Fail::new(e.to_string()))?
            .ok_or_else(|| Fail::new("Application not found."))?;
        self.app = fresh;
        self.b.variables = None;
        Ok(())
    }

    /// `docker_compose_build_fingerprint($buildEnvironment)`.
    async fn compose_build_fingerprint(&mut self, build_env: &[String]) -> Result<String, Fail> {
        let enc = self
            .jobs
            .0
            .encrypter
            .clone()
            .ok_or_else(|| Fail::new("No application encryption key has been specified."))?;
        let snapshot = crate::applications::snapshot::snapshot(&self.jobs.0.db, &enc, &self.app)
            .await
            .map_err(Fail::new)?;
        let mut settings = serde_json::Map::new();
        for section in snapshot
            .get("sections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for item in section
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if item.get("impact").and_then(Value::as_str) == Some("build")
                    && let Some(key) = item.get("key").and_then(Value::as_str)
                {
                    settings.insert(
                        key.to_owned(),
                        item.get("compare_value").cloned().unwrap_or(Value::Null),
                    );
                }
            }
        }
        let settings = if settings.is_empty() {
            Value::Array(Vec::new())
        } else {
            Value::Object(settings)
        };
        let json = coolify_compat::php_json::encode(&Value::Array(vec![
            settings,
            Value::Array(build_env.iter().map(|l| Value::String(l.clone())).collect()),
            Value::String(self.b.coolify_variables.clone()),
        ]));
        use sha2::Digest as _;
        Ok(sha2::Sha256::digest(json.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    /// `can_reuse_docker_compose_images($composeFile)`.
    async fn can_reuse_compose_images(&mut self, compose: &ParsedCompose) -> Result<bool, Fail> {
        let Some(fingerprint) = self.b.compose_fingerprint.clone().filter(|f| !f.is_empty()) else {
            return Ok(false);
        };
        if self.b.force_rebuild
            || self.b.is_preview()
            || self.b.compose_build_command.is_some()
            || self.commit.trim().is_empty()
            || self.commit == "HEAD"
        {
            return Ok(false);
        }
        let mut images: Vec<String> = Vec::new();
        for (name, service) in compose.services() {
            if get(service, "build").is_none_or(|b| *b == PhpValue::Null) {
                continue;
            }
            let expected = format!("{}_{}:{}", self.app.uuid, name.as_string(), self.commit);
            if get(service, "image") != Some(&PhpValue::Str(expected.clone())) {
                return Ok(false);
            }
            images.push(expected);
        }
        if images.is_empty() {
            return Ok(false);
        }
        let names = images
            .iter()
            .map(|i| quote(i))
            .collect::<Vec<_>>()
            .join(" ");
        let format = quote(&format!(
            "{{{{ index .Config.Labels \"{BUILD_FINGERPRINT_LABEL}\" }}}}"
        ));
        self.saved.remove("compose_image_fingerprints");
        self.exec(vec![
            Cmd::new(format!(
                "docker image inspect --format {format} {names} 2>/dev/null"
            ))
            .hidden()
            .ignore_errors()
            .save("compose_image_fingerprints", true),
        ])
        .await?;
        let out = self
            .saved
            .get("compose_image_fingerprints")
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let lines: Vec<&str> = out.split('\n').map(str::trim).collect();
        if lines.len() != images.len() || lines.iter().any(|l| *l != fingerprint) {
            return Ok(false);
        }
        self.info(
            "Images found with the same Git Commit SHA and build inputs. Build step skipped.",
        )
        .await;
        Ok(true)
    }

    /// `build_docker_compose_images($composeFile, $buildEnvironment)`.
    async fn build_compose_images(&mut self, compose: Option<&ParsedCompose>) -> Step {
        // A raw Compose file is a string for upstream: no services, only the checks before the loop.
        let raw = ParsedCompose::empty();
        self.modify_dockerfiles_for_compose(compose.unwrap_or(&raw))
            .await?;
        self.info("Pulling & building required images.").await;
        // Save build-time .env file BEFORE the build
        let mut build_env = match self.b.compose_build_env.take() {
            Some(lines) => lines,
            None => self.generate_buildtime_environment_variables().await?,
        };
        let extra = self
            .compose_build_interpolation_variables(compose, &build_env)
            .await?;
        build_env.extend(extra);
        self.save_buildtime_environment_variables_with(Some(build_env))
            .await?;
        let (u, w, loc) = (
            self.app.uuid.clone(),
            self.workdir.clone(),
            self.b.compose_location.clone(),
        );
        if let Some(custom) = self.b.compose_build_command.clone() {
            // Auto-inject -f (compose file) and --env-file flags
            let mut build =
                inject_compose_flags(&custom, &format!("{w}{loc}"), BUILD_TIME_ENV_PATH);
            if self.b.buildkit {
                build = format!("DOCKER_BUILDKIT=1 {build}");
            }
            if !self.app.setting_bool("use_build_secrets") && !self.b.build_args.is_empty() {
                let original = build.clone();
                build = inject_build_args(&build, &self.b.build_args.join(" "));
                if build != original {
                    self.info("Adding build arguments to custom Docker Compose build command.")
                        .await;
                }
            }
            let dep = self.dep.clone();
            let result = self
                .exec(vec![
                    Cmd::new(in_docker(&dep, &format!("cd /artifacts/{dep} && {build}"))).hidden(),
                ])
                .await;
            return result.map_err(|e| syntax_error(e, "build", &custom));
        }
        let mut command = format!("{} docker compose", self.b.coolify_variables);
        if self.b.buildkit {
            command = format!("DOCKER_BUILDKIT=1 {command}");
        }
        command.push_str(&format!(" --env-file {BUILD_TIME_ENV_PATH}"));
        command.push_str(&format!(
            " --project-name {u} --project-directory {w} -f {w}{loc} build --pull"
        ));
        if self.b.force_rebuild {
            command.push_str(" --no-cache");
        }
        if !self.app.setting_bool("use_build_secrets") && !self.b.build_args.is_empty() {
            command.push_str(&format!(" {}", self.b.build_args.join(" ")));
            self.info("Adding build arguments to Docker Compose build command.")
                .await;
        }
        let dep = self.dep.clone();
        self.exec(vec![Cmd::new(in_docker(&dep, &command)).hidden()])
            .await
    }

    /// `compose_build_interpolation_variables($composeFile, $buildEnvironment)`: `docker compose build`
    /// interpolates the whole file, so a required runtime-only variable (`${KEY:?}` in `environment:`)
    /// would fail the build. The build gets the runtime `.env` lines of the variables the file uses
    /// outside its build sections; variables a build section or a build secret can read stay out.
    async fn compose_build_interpolation_variables(
        &mut self,
        compose: Option<&ParsedCompose>,
        build_env: &[String],
    ) -> Result<Vec<String>, Fail> {
        // A raw Compose file is the `docker_compose_raw` string, parsed here.
        let parsed;
        let value = match compose {
            Some(c) => &c.top_level,
            None => {
                let raw = self.app.str("docker_compose_raw").unwrap_or_default();
                parsed = compose_parser::validate::parse_docker_compose_yaml(raw)
                    .map_err(|e| Fail::new(e.to_string()))?;
                &parsed
            }
        };
        let keys = compose_build_interpolation_keys(value, build_env);
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let saved = self.b.compose_env.take();
        self.b.compose_env = Some(self.compose_runtime_env().await?);
        let lines = self.runtime_environment_lines().await;
        self.b.compose_env = saved;
        Ok(lines?
            .0
            .into_iter()
            .filter(|l| {
                keys.iter()
                    .any(|k| k == l.split('=').next().unwrap_or_default())
            })
            .collect())
    }

    /// `modify_dockerfiles_for_compose($composeFile)`.
    async fn modify_dockerfiles_for_compose(&mut self, compose: &ParsedCompose) -> Step {
        if self
            .app
            .settings
            .get("inject_build_args_to_dockerfile")
            .and_then(Value::as_bool)
            == Some(false)
        {
            self.log(
                "Skipping Docker Compose Dockerfile ARG injection (disabled in settings).",
                "stdout",
                true,
            )
            .await;
            return Ok(());
        }
        if self.b.env_args.0.is_empty() {
            self.generate_env_variables().await?;
        }
        let keys: Vec<String> = self.b.env_args.0.iter().map(|(k, _)| k.clone()).collect();
        if keys.is_empty() {
            self.info("No build-time variables to add to Dockerfiles.")
                .await;
            return Ok(());
        }
        let basedir = format!("/artifacts/{}", self.dep);
        for (name, service) in compose.services() {
            let name = name.as_string();
            let Some(build) = get(service, "build").filter(|b| **b != PhpValue::Null) else {
                continue;
            };
            let Some(path) = compose_dockerfile_path(build) else {
                self.info(&arg_injection_skipped_message(&name, build, &keys))
                    .await;
                continue;
            };
            let full = quote(&if path.starts_with('/') {
                path.clone()
            } else {
                format!("{}/{path}", self.workdir)
            });
            let check = format!("dockerfile_check_{name}");
            self.saved.remove(&check);
            self.exec(vec![
                Cmd::new(in_docker(
                    &self.dep,
                    &format!("resolved_path=$(realpath {full} 2>/dev/null) && test -f \"$resolved_path\" && printf '%s' \"$resolved_path\" || true"),
                ))
                .hidden()
                .save(check.clone(), true),
            ])
            .await?;
            let resolved = self
                .saved
                .get(&check)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            if !resolved.starts_with(&format!("{basedir}/")) {
                self.info(&format!(
                    "Dockerfile not found for service {name} at {path}, skipping ARG injection."
                ))
                .await;
                continue;
            }
            let full = quote(&resolved);
            let content_key = format!("dockerfile_content_{name}");
            self.saved.remove(&content_key);
            self.exec(vec![
                Cmd::new(in_docker(&self.dep, &format!("cat {full}")))
                    .hidden()
                    .save(content_key.clone(), true),
            ])
            .await?;
            let content = self.saved.get(&content_key).cloned().unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            let mut lines: Vec<String> = content.trim().split('\n').map(str::to_owned).collect();
            let froms: Vec<usize> = super::git::dockerfile_instructions(&lines)
                .into_iter()
                .filter(|i| i.0 == "FROM")
                .map(|i| i.2)
                .collect();
            if froms.is_empty() {
                self.info(&format!(
                    "No FROM instruction found in Dockerfile for service {name}, skipping."
                ))
                .await;
                continue;
            }
            let args: Vec<String> = keys.iter().map(|k| format!("ARG {k}")).collect();
            let mut total = 0;
            let mut offset = 0;
            for (stage, from) in froms.iter().enumerate() {
                let adjusted = from + offset;
                let start = adjusted + 1;
                let end = froms.get(stage + 1).map_or(lines.len(), |n| n + offset);
                let mut existing: Vec<String> = Vec::new();
                for line in lines.iter().take(end).skip(start) {
                    if line.is_empty() || !line.trim().starts_with("ARG") {
                        break;
                    }
                    let mut parts = line.trim().splitn(2, ' ');
                    parts.next();
                    if let Some(arg) = parts.next() {
                        existing.push(arg.split('=').next().unwrap_or_default().to_owned());
                    }
                }
                let add: Vec<String> = args
                    .iter()
                    .filter(|a| !existing.contains(&a.trim_start_matches("ARG ").trim().to_owned()))
                    .cloned()
                    .collect();
                if !add.is_empty() {
                    for (i, a) in add.iter().enumerate() {
                        lines.insert(adjusted + 1 + i, a.clone());
                    }
                    total += add.len();
                    offset += add.len();
                }
            }
            if total > 0 {
                let b64 = {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD.encode(lines.join("\n"))
                };
                self.exec(vec![
                    Cmd::new(in_docker(
                        &self.dep,
                        &format!("echo '{b64}' | base64 -d | tee {full} > /dev/null"),
                    ))
                    .hidden(),
                ])
                .await?;
                let stages = if froms.len() > 1 {
                    format!(" (multi-stage build, added to {} stages)", froms.len())
                } else {
                    String::new()
                };
                self.info(&format!(
                    "Added {total} ARG declarations to Dockerfile for service {name}{stages}."
                ))
                .await;
            } else {
                self.info(&format!(
                    "Service {name}: All required ARG declarations already exist."
                ))
                .await;
            }
            if self.b.secrets_supported && !self.b.build_secrets.is_empty() {
                self.modify_dockerfile_for_secrets(&full).await?;
                self.info(&format!(
                    "Modified Dockerfile for service {name} to use build secrets."
                ))
                .await;
            }
        }
        Ok(())
    }

    /// The Docker Compose part of `generate_runtime_environment_variables()`.
    async fn compose_runtime_env(&mut self) -> Result<ComposeEnv, Fail> {
        let mut lines = Vec::new();
        let preview_domains = self.preview_compose_domains();
        let domains = compose_parser::php_json_array(if self.b.is_preview() {
            preview_domains.as_deref()
        } else {
            self.app.str("docker_compose_domains")
        });
        for (service, entry) in &domains {
            let Some(domain) = compose_parser::domain_entry_string(entry).filter(|d| !d.is_empty())
            else {
                continue;
            };
            let first = domain.split(',').next().unwrap_or_default().to_owned();
            let Some(parts) = crate::applications::domains::UrlParts::parse(&first) else {
                continue;
            };
            let url = crate::applications::domains::without_port(&first);
            let key = compose_parser::normalize_service_name(&service.as_string()).to_uppercase();
            lines.push(format!("SERVICE_URL_{key}={url}"));
            lines.push(format!("SERVICE_FQDN_{key}={}", parts.host));
        }
        let compose_text =
            if self.b.is_preview() || self.app.setting_bool("is_raw_compose_deployment_enabled") {
                self.app.str("docker_compose_raw")
            } else {
                self.app.str("docker_compose")
            }
            .unwrap_or_default()
            .to_owned();
        for name in compose_parser::service_names(&compose_text) {
            lines.push(format!(
                "SERVICE_NAME_{}={}",
                name.replace(['-', '.'], "_").to_uppercase(),
                compose_parser::volumes::add_preview_deployment_suffix(
                    &name,
                    self.b.pull_request_id
                )
            ));
        }
        // Generated SERVICE_* variables and blank variables Compose unsets stay out of the file.
        let source = self
            .app
            .str("docker_compose_raw")
            .or(self.app.str("docker_compose"))
            .map(str::to_owned);
        let excluded_ids = self
            .variables()
            .await?
            .iter()
            .filter(|v| {
                v.key.starts_with("SERVICE_FQDN_")
                    || v.key.starts_with("SERVICE_URL_")
                    || v.key.starts_with("SERVICE_NAME_")
                    || (!v.preserve_compose_null
                        && v.value.is_none()
                        && compose_parser::environment_variable_should_be_unset(
                            source.as_deref(),
                            &v.key,
                        ))
            })
            .map(|v| v.id)
            .collect();
        Ok(ComposeEnv {
            service_lines: lines,
            excluded_ids,
        })
    }

    /// `pull_docker_compose_images()` (no build server).
    async fn pull_compose_images(&mut self) -> Step {
        if self.b.compose_build_command.is_some() {
            self.info("Skipping the image pull before stopping the current deployment, because the custom build command can build images outside of Docker Compose.")
                .await;
            return Ok(());
        }
        let yaml = self.generated_compose.clone().unwrap_or_default();
        let services = compose_parser::registry_image_services(&yaml);
        if services.is_empty() {
            return Ok(());
        }
        self.info("Pulling image-based services before stopping the current deployment.")
            .await;
        let list = services
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(" ");
        let (u, w, loc) = (
            self.app.uuid.clone(),
            self.workdir.clone(),
            self.b.compose_location.clone(),
        );
        let command = in_docker(
            &self.dep,
            &format!(
                "{} docker compose --env-file {w}/{ENV_FILE} --project-name {u} --project-directory {w} -f {w}{loc} pull --ignore-buildable {list}",
                self.b.coolify_variables
            ),
        );
        self.exec(vec![Cmd::new(command).hidden()]).await
    }

    /// `start_docker_compose_services()`.
    async fn start_compose_services(&mut self) -> Step {
        self.write_missing_content_files().await?;
        let server_workdir = self.cfg.clone();
        let (u, w, dep) = (
            self.app.uuid.clone(),
            self.workdir.clone(),
            self.dep.clone(),
        );
        let preserve = self.app.setting_bool("is_preserve_repository_enabled");
        let custom = self.b.compose_start_command.clone();
        let vars = self.b.coolify_variables.clone();
        if self.app.setting_bool("is_raw_compose_deployment_enabled") {
            if let Some(custom) = custom {
                let start = inject_compose_flags(
                    &custom,
                    &format!("{server_workdir}{}", self.b.compose_location),
                    &format!("{server_workdir}/{ENV_FILE}"),
                );
                self.write_deployment_configurations().await?;
                let result = self
                    .exec(vec![
                        Cmd::new(in_docker(&dep, &format!("cd {w} && {start}")))
                            .kind("stdout")
                            .command_hidden(),
                    ])
                    .await;
                return result.map_err(|e| syntax_error(e, "start", &custom));
            }
            // write_deployment_configurations() runs first, then compose starts from the server.
            self.write_deployment_configurations().await?;
            self.b.compose_location = "/docker-compose.yaml".into();
            let command = format!(
                "{vars} docker compose --env-file {server_workdir}/{ENV_FILE} --project-directory {server_workdir} -f {server_workdir}/docker-compose.yaml up -d"
            );
            return self
                .exec(vec![Cmd::new(command).kind("stdout").command_hidden()])
                .await;
        }
        let loc = self.b.compose_location.clone();
        if let Some(custom) = custom {
            let path = if preserve { &server_workdir } else { &w };
            let start = inject_compose_flags(
                &custom,
                &format!("{path}{loc}"),
                &format!("{path}/{ENV_FILE}"),
            );
            self.write_deployment_configurations().await?;
            let command = if preserve {
                let in_workdir = format!("cd {server_workdir} && {start}");
                // A non-root SSH user cannot cd into the resource directory: as one `sh -c`
                // script, the sudo parser runs the cd and the start command as root.
                if self.server.is_non_root() {
                    format!("sh -c {}", quote(&in_workdir))
                } else {
                    in_workdir
                }
            } else {
                in_docker(&dep, &format!("cd /artifacts/{dep} && {start}"))
            };
            return self
                .exec(vec![Cmd::new(command).kind("stdout").command_hidden()])
                .await;
        }
        if preserve {
            let command = format!(
                "{vars} docker compose --env-file {server_workdir}/{ENV_FILE} --project-name {u} --project-directory {server_workdir} -f {server_workdir}{loc} up -d"
            );
            self.write_deployment_configurations().await?;
            return self
                .exec(vec![Cmd::new(command).kind("stdout").command_hidden()])
                .await;
        }
        let command = format!(
            "{vars} docker compose --env-file {w}/{ENV_FILE} --project-name {u} --project-directory {w} -f {w}{loc} up -d"
        );
        self.exec(vec![
            Cmd::new(in_docker(&dep, &command))
                .kind("stdout")
                .command_hidden(),
        ])
        .await?;
        self.write_deployment_configurations().await
    }
}

/// `validateShellSafeCommand($value, $fieldName)`.
/// `Arr::flatten($values)` then `extractDockerComposeEnvironmentVariableReferences` of each value.
fn compose_references(value: &PhpValue, out: &mut Vec<String>) {
    match value {
        PhpValue::Str(s) => {
            out.extend(
                compose_parser::env::extract_docker_compose_environment_variable_references(s),
            );
        }
        PhpValue::Array(items) => {
            for (_, v) in items {
                compose_references(v, out);
            }
        }
        _ => {}
    }
}

/// The variable names of `compose_build_interpolation_variables()`: referenced in the Compose file
/// outside its `build` sections, and neither read by a build (argument, build section, secret) nor
/// already a build-time variable.
fn compose_build_interpolation_keys(compose: &PhpValue, build_env: &[String]) -> Vec<String> {
    let mut compose = compose.clone();
    let mut read_by_builds: Vec<String> = Vec::new();
    // `collect(data_get($compose, 'secrets', []))->pluck('environment')->filter()`
    if let Some(PhpValue::Array(secrets)) = get(&compose, "secrets") {
        for (_, secret) in secrets {
            match get(secret, "environment") {
                Some(PhpValue::Str(s)) if !s.is_empty() && s != "0" => {
                    read_by_builds.push(s.clone());
                }
                Some(PhpValue::Int(i)) if *i != 0 => read_by_builds.push(i.to_string()),
                _ => {}
            }
        }
    }
    if let Some(PhpValue::Array(services)) = compose.get_mut("services") {
        for (_, service) in services.iter_mut() {
            let build = match get(service, "build") {
                None | Some(PhpValue::Null) => continue,
                Some(b) => b.clone(),
            };
            // `data_get($build, 'args', [])`: a string build has no args.
            match get(&build, "args") {
                Some(PhpValue::Array(args)) if is_list(args) => {
                    for (_, arg) in args {
                        let arg = match arg {
                            PhpValue::Str(s) => s.clone(),
                            PhpValue::Int(i) => i.to_string(),
                            PhpValue::Float(f) => f.to_string(),
                            PhpValue::Bool(true) => "1".to_owned(),
                            _ => String::new(),
                        };
                        read_by_builds.push(arg.split('=').next().unwrap_or_default().to_owned());
                    }
                }
                Some(PhpValue::Array(args)) => {
                    read_by_builds.extend(args.iter().map(|(k, _)| k.as_string()));
                }
                None | Some(PhpValue::Null) => {}
                // `array_keys((array) $scalar)`
                Some(_) => read_by_builds.push("0".to_owned()),
            }
            compose_references(&build, &mut read_by_builds);
            service.remove("build");
        }
    }
    let build_keys: Vec<&str> = build_env
        .iter()
        .map(|l| l.split('=').next().unwrap_or_default())
        .collect();
    let mut references = Vec::new();
    compose_references(&compose, &mut references);
    let mut keys: Vec<String> = Vec::new();
    for r in references {
        if !keys.contains(&r) && !read_by_builds.contains(&r) && !build_keys.contains(&r.as_str()) {
            keys.push(r);
        }
    }
    keys
}

fn validate_shell_safe_command(value: &str, field: &str) -> Step {
    static SAFE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"^(?:[ \t]+|&&|\|\||"[^"$`\\\n\r]*"|'[^'\n\r]*'|[a-zA-Z0-9._\-/=:@,+\[\]{}#%^~*?!]+)+$"#,
        )
        .unwrap_or_else(|_| unreachable!())
    });
    if SAFE.is_match(value) {
        Ok(())
    } else {
        Err(Fail::new(format!(
            "Invalid {field}: contains forbidden shell characters."
        )))
    }
}

/// `str($command)->replaceFirst('compose', 'compose --project-directory '.$dir)` unless present.
fn with_project_directory(command: &str, directory: &str) -> String {
    if command.contains("--project-directory") {
        command.to_owned()
    } else {
        command.replacen(
            "compose",
            &format!("compose --project-directory {directory}"),
            1,
        )
    }
}

/// `injectDockerComposeFlags($command, $composeFilePath, $envFilePath)`.
pub fn inject_compose_flags(command: &str, compose_file: &str, env_file: &str) -> String {
    static FILE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|\s)(?:-f(?:[=\s]|[./~]|$)|--file(?:=|\s))")
            .unwrap_or_else(|_| unreachable!())
    });
    static ENV: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|\s)--env-file(?:=|\s)").unwrap_or_else(|_| unreachable!())
    });
    static COMPOSE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"docker\s+compose").unwrap_or_else(|_| unreachable!()));
    let mut replacement = "docker compose".to_owned();
    if !FILE.is_match(command) {
        replacement.push_str(&format!(" -f {compose_file}"));
    }
    if !ENV.is_match(command) {
        replacement.push_str(&format!(" --env-file {env_file}"));
    }
    COMPOSE
        .replacen(command, 1, regex::NoExpand(&replacement))
        .into_owned()
}

/// `injectDockerComposeBuildArgs($command, $buildArgsString)`.
pub fn inject_build_args(command: &str, build_args: &str) -> String {
    static BUILD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"( builder build| buildx build| build)(?:\s|$)")
            .unwrap_or_else(|_| unreachable!())
    });
    if build_args.trim().is_empty() {
        return command.to_owned();
    }
    let Some(end) = BUILD
        .captures(command)
        .and_then(|c| c.get(1))
        .map(|m| m.end())
    else {
        return command.to_owned();
    };
    let (head, tail) = command.split_at(end);
    format!("{head} {build_args}{tail}")
}

/// The `catch (\\RuntimeException)` around a custom command.
fn syntax_error(e: Fail, what: &str, command: &str) -> Fail {
    match &e {
        Fail::Error { message, .. }
            if message.contains("matching `'") || message.contains("unexpected EOF") =>
        {
            Fail::new(format!(
                "Custom {what} command failed due to shell syntax error. Please check your command for special characters (like unmatched quotes): {command}"
            ))
        }
        _ => e,
    }
}

/// `add_build_fingerprint_to_compose($composeFile)`.
fn add_build_fingerprint(compose: &mut ParsedCompose, fingerprint: &str) {
    let Some(services) = compose.services_mut() else {
        return;
    };
    for (_, service) in services.iter_mut() {
        let Some(build) = service.get_mut("build") else {
            continue;
        };
        if let PhpValue::Str(context) = build {
            *build = PhpValue::map([("context", PhpValue::Str(context.clone()))]);
        }
        let mut labels = get(build, "labels")
            .cloned()
            .unwrap_or(PhpValue::Array(Vec::new()));
        match &mut labels {
            PhpValue::Array(items) if is_list(items) => {
                let i = i64::try_from(items.len()).unwrap_or(0);
                items.push((
                    Key::Int(i),
                    PhpValue::Str(format!("{BUILD_FINGERPRINT_LABEL}={fingerprint}")),
                ));
            }
            PhpValue::Array(_) => labels.set(
                BUILD_FINGERPRINT_LABEL,
                PhpValue::Str(fingerprint.to_owned()),
            ),
            _ => labels = PhpValue::strings([format!("{BUILD_FINGERPRINT_LABEL}={fingerprint}")]),
        }
        build.set("labels", labels);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_interpolation_keys_leave_out_build_inputs() {
        let compose = compose_parser::validate::parse_docker_compose_yaml(
            "services:
  app:
    build:
      context: .
      args:
        MAPPED_ARG: '${RUNTIME_IN_BUILD_ARGS}'
        LISTED_FROM_ENV: null
    environment:
      FERNET_KEY: '${FERNET_KEY:?}'
      OPTIONAL_REQUIRED: '${OPTIONAL_REQUIRED?}'
      BUILD_VALUE: '${BUILD_VALUE}'
      SECRET_USER: '$FROM_SECRET'
  worker:
    build:
      context: ./worker
      args:
        - LISTED_ARG
    environment:
      FERNET_KEY: '${FERNET_KEY:?}'
      PLAIN: '$$NOT_A_REFERENCE'
secrets:
  token:
    environment: FROM_SECRET
",
        )
        .unwrap();
        assert_eq!(
            compose_build_interpolation_keys(&compose, &["BUILD_VALUE=\"x\"".to_owned()]),
            ["FERNET_KEY", "OPTIONAL_REQUIRED"]
        );
        // Without a build section, nothing is read by a build.
        let compose = compose_parser::validate::parse_docker_compose_yaml(
            "services:\n  web:\n    image: nginx\n    environment:\n      A: ${A:?}\n",
        )
        .unwrap();
        assert_eq!(compose_build_interpolation_keys(&compose, &[]), ["A"]);
    }

    #[test]
    fn dockerfile_paths() {
        assert_eq!(
            compose_dockerfile_path(&PhpValue::Str("./app".into())),
            Some("./app/Dockerfile".into())
        );
        let b = PhpValue::map([
            ("context", PhpValue::Str("api/".into())),
            ("dockerfile", PhpValue::Str("Dockerfile.prod".into())),
        ]);
        assert_eq!(
            compose_dockerfile_path(&b),
            Some("api/Dockerfile.prod".into())
        );
        assert_eq!(
            compose_dockerfile_path(&PhpValue::Str("https://github.com/a/b.git".into())),
            None
        );
        assert_eq!(lcfirst("Docker Compose file"), "docker Compose file");
    }

    #[test]
    fn custom_command_flags() {
        assert_eq!(
            inject_compose_flags("docker compose build", "/w/c.yaml", "/e"),
            "docker compose -f /w/c.yaml --env-file /e build"
        );
        assert_eq!(
            inject_compose_flags("docker  compose -f x.yml --env-file=y up -d", "/w", "/e"),
            "docker compose -f x.yml --env-file=y up -d"
        );
        assert_eq!(
            inject_compose_flags("docker compose --from up", "/w", "/e"),
            "docker compose -f /w --env-file /e --from up"
        );
        assert_eq!(
            inject_build_args(
                "docker compose -f x build --pull && docker compose push",
                "--build-arg A"
            ),
            "docker compose -f x build --build-arg A --pull && docker compose push"
        );
        assert_eq!(
            inject_build_args("docker buildx build .", "--build-arg A"),
            "docker buildx build --build-arg A ."
        );
        assert_eq!(
            inject_build_args("docker compose push", "--build-arg A"),
            "docker compose push"
        );
        assert_eq!(
            with_project_directory("docker compose up -d", "/w"),
            "docker compose --project-directory /w up -d"
        );
        assert!(validate_shell_safe_command("docker compose up -d && echo 'a b'", "x").is_ok());
        assert!(validate_shell_safe_command("echo $(id)", "x").is_err());
    }
}
