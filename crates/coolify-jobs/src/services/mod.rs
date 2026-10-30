//! One-click services on servers: `app/Actions/Service/*`, `Service::saveComposeConfigs()`,
//! `Service::isConfigurationChanged()`, `DeleteResourceJob` for services,
//! `ResourceStatusRefresher::refreshService()` and the services part of `GetContainersStatus`.
//!
//! Not ported: `CleanupDocker::dispatchAfterStop`, maintenance pages, secret managers and the
//! proxies of public service databases.

use std::time::Duration;

use base64::Engine as _;
use coolify_db::databases as dbs;
use coolify_db::servers::{self, ColumnValue};
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_remote::{RunOptions, Stream, Target, shell::quote};
use md5::Digest as _;
use serde_json::{Value, json};

use crate::Jobs;
use crate::compose_parser::service_parser::{self, ServiceRun};
use crate::compose_parser::{ParsedCompose, php, validate, volumes};
use crate::databases::docker_stop_command;
use crate::databases::env::{self as vars, Owner};
use crate::server::ServerInfo;
use coolify_compat::yaml::PhpValue;

pub mod status;
pub mod storage;
pub mod templates;

/// `service_configuration_dir()`.
pub const CONFIGURATION_DIR: &str = "/data/coolify/services";

/// `$service->workdir()`.
pub fn workdir(uuid: &str) -> String {
    format!("{CONFIGURATION_DIR}/{uuid}")
}

const ENV_FILE: &str = concat!(".", "env");

fn hrtime() -> i64 {
    chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
}

fn owner(s: &ServiceRow) -> Owner<'_> {
    Owner {
        morph: svc::MORPH,
        id: s.id,
        team_id: s.team_id,
        project_id: s.project.get("id").and_then(Value::as_i64),
        environment_id: s.environment.get("id").and_then(Value::as_i64),
    }
}

/// `serverDockerConfigPrefix($server)`.
fn docker_config_prefix(server: &ServerInfo) -> &'static str {
    if server.is_non_root() {
        "DOCKER_CONTEXT=default DOCKER_CONFIG=\"$(find \"$HOME/.docker/config.json\" -maxdepth 0 -exec dirname {} \\; 2>/dev/null)\" "
    } else {
        ""
    }
}

/// `getTopLevelNetworks($service)->count() > 0`: always true for a service: the parser adds its
/// own network, and an invalid compose file still answers with the uuid.
fn has_networks(_service: &ServiceRow) -> bool {
    true
}

/// `StartService::predefinedNetworkConnectCommand($service, $serviceName, $serviceConfig)`.
pub fn predefined_network_connect_command(
    uuid: &str,
    network: &str,
    service_name: &str,
    config: Option<&PhpValue>,
) -> Option<String> {
    if config
        .and_then(|c| php::get(c, "network_mode"))
        .is_some_and(|m| *m != PhpValue::Null)
    {
        return None;
    }
    let net = quote(network);
    let container = quote(&format!("{service_name}-{uuid}"));
    let warning = quote(&format!(
        "Warning: Could not connect {service_name} to the {network} network."
    ));
    let connected = format!(
        "docker container inspect --format '{{{{range $name, $_ := .NetworkSettings.Networks}}}}{{{{println $name}}}}{{{{end}}}}' {container}"
    );
    Some(format!(
        "{connected} | grep -qxF {net} || docker network connect --alias {container} {net} {container} || echo {warning}"
    ))
}

impl Jobs {
    async fn service_server(&self, s: &ServiceRow) -> Result<ServerInfo, String> {
        let id = s.server_id().ok_or("The service has no server.")?;
        let row = servers::find_by_id(&self.0.db, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        Ok(ServerInfo::from_row(&row))
    }

    async fn service_network(&self, s: &ServiceRow) -> String {
        svc::destination_network(&self.0.db, s)
            .await
            .unwrap_or_else(|_| "coolify".to_owned())
    }

    /// `$service->parse()` and the jobs it dispatches (`ServerStorageSaveJob`,
    /// `ServerFilesFromServerJob`), which run in the background as upstream's queue does.
    pub async fn parse_service(&self, service_id: i64) -> Result<ParsedCompose, String> {
        let (parsed, effects) = self.parse_service_inline(service_id).await?;
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.run_parse_effects(service_id, effects).await;
        });
        Ok(parsed)
    }

    /// `$service->parse(isNew: true)` on the model the request just created: it has no attributes
    /// from column defaults, so labels are escaped only when the request asked for it.
    pub async fn parse_new_service(
        &self,
        service_id: i64,
        label_escape: bool,
    ) -> Result<ParsedCompose, String> {
        let (parsed, effects) = self
            .parse_service_with(service_id, Some(label_escape))
            .await?;
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.run_parse_effects(service_id, effects).await;
        });
        Ok(parsed)
    }

    async fn parse_service_inline(
        &self,
        service_id: i64,
    ) -> Result<(ParsedCompose, service_parser::ParseEffects), String> {
        self.parse_service_with(service_id, None).await
    }

    async fn parse_service_with(
        &self,
        service_id: i64,
        label_escape: Option<bool>,
    ) -> Result<(ParsedCompose, service_parser::ParseEffects), String> {
        let mut run = ServiceRun::load(self, service_id, Jobs::now()).await?;
        run.label_escape_override = label_escape;
        if run.service.parsing_version() < 3 {
            return Err(
                "Compose parsing versions before 3 (parseDockerComposeFile) are not ported.".into(),
            );
        }
        let parsed = service_parser::service_parser(&mut run).await?;
        Ok((parsed, run.effects))
    }

    async fn run_parse_effects(&self, service_id: i64, effects: service_parser::ParseEffects) {
        let Ok(Some(s)) = svc::find_by_id(&self.0.db, service_id).await else {
            return;
        };
        let Ok(server) = self.service_server(&s).await else {
            return;
        };
        if !server.is_functional() {
            return;
        }
        let Ok(target) = server.target(self).await else {
            return;
        };
        for id in &effects.new_file_volumes {
            let _ = storage::save_storage_on_server(self, &s, &target, *id).await;
        }
        let mut seen: Vec<(SubKind, i64)> = Vec::new();
        for pair in &effects.files_from_server {
            if !seen.contains(pair) {
                seen.push(*pair);
                let _ = storage::files_from_server(self, &s, &target, pair.0, pair.1, true).await;
            }
        }
    }

    /// `updateCompose($resource)`.
    pub async fn update_service_compose(
        &self,
        service_id: i64,
        kind: SubKind,
        name: &str,
    ) -> Result<(), String> {
        service_parser::update_compose(self, service_id, kind, name, Jobs::now()).await
    }

    /// `Service::saveComposeConfigs()`.
    pub async fn save_compose_configs(
        &self,
        s: &ServiceRow,
        target: &Target,
    ) -> Result<(), String> {
        let Some(compose) = s
            .str("docker_compose")
            .filter(|c| php::truthy(&PhpValue::Str((*c).to_owned())))
        else {
            return Ok(());
        };
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let dir = workdir(&s.uuid);
        let mut lines: Vec<String> = Vec::new();
        if let Ok(parsed) = validate::parse_docker_compose_yaml(compose)
            && let Some(PhpValue::Array(services)) = php::get(&parsed, "services")
        {
            for (name, _) in services {
                let name = name.as_string();
                lines.push(format!(
                    "SERVICE_NAME_{}={name}",
                    php::upper(&name.replace('-', "_").replace('.', "_"))
                ));
            }
        }
        let server_id = s.server_id();
        let variables = vars::load_for(&self.0.db, enc, &owner(s), server_id).await?;
        let rank = |v: &vars::Var| {
            if v.key.starts_with("SERVICE_") {
                return 1;
            }
            let value = v.value.as_deref().unwrap_or_default();
            if value.starts_with("$SERVICE_") || value.starts_with("${SERVICE_") {
                return 2;
            }
            3
        };
        let mut sorted: Vec<&vars::Var> = variables.iter().collect();
        sorted.sort_by_key(|v| rank(v));
        let raw = s.str("docker_compose_raw").or(s.str("docker_compose"));
        for v in sorted {
            let preserves = v.preserve_compose_null && v.value.is_none();
            if !preserves
                && v.value.is_none()
                && crate::compose_parser::environment_variable_should_be_unset(raw, &v.key)
            {
                continue;
            }
            lines.push(env_file_line(v));
        }
        let tmp = format!("{dir}/{}{ENV_FILE}.tmp", coolify_domain::new_public_id());
        let mkdir = format!("mkdir -p {dir}");
        let opts = || RunOptions::op("service.configs");
        self.remote()
            .run(target, &[mkdir.as_str()], opts())
            .await
            .map_err(|e| e.message)?;
        self.write_remote_file(target, &format!("{dir}/docker-compose.yml"), compose)
            .await?;
        self.write_remote_file(target, &tmp, &lines.join("\n"))
            .await?;
        let mv = format!("mv {tmp} {dir}/{ENV_FILE}");
        self.remote()
            .run(target, &[mv.as_str()], opts())
            .await
            .map_err(|e| e.message)?;
        Ok(())
    }

    /// `instant_remote_write_file($server, $path, $content)`.
    pub(crate) async fn write_remote_file(
        &self,
        target: &Target,
        path: &str,
        content: &str,
    ) -> Result<(), String> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let command = format!("echo '{encoded}' | base64 -d | tee {} > /dev/null", quote(path));
        self.remote()
            .run(
                target,
                &[command.as_str()],
                RunOptions::op("service.write_file"),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.message)
    }

    /// `Service::isConfigurationChanged(save: true)`.
    pub async fn service_configuration_hash(&self, s: &ServiceRow) -> Result<String, String> {
        let db = &self.0.db;
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let apps = svc::subs(db, SubKind::Application, s.id)
            .await
            .map_err(|e| e.to_string())?;
        let databases = svc::subs(db, SubKind::Database, s.id)
            .await
            .map_err(|e| e.to_string())?;
        let mut domains: Vec<String> = apps
            .iter()
            .map(|a| a.str("fqdn").unwrap_or_default().to_owned())
            .collect();
        domains.sort();
        let mut app_images: Vec<String> = apps
            .iter()
            .map(|a| a.str("image").unwrap_or_default().to_owned())
            .collect();
        app_images.sort();
        let mut db_images: Vec<String> = databases
            .iter()
            .map(|a| a.str("image").unwrap_or_default().to_owned())
            .collect();
        db_images.sort();
        let images = app_images
            .into_iter()
            .chain(db_images)
            .collect::<Vec<_>>()
            .join(",");
        let mut storages = String::new();
        for sub in apps.iter().chain(databases.iter()) {
            let mut rows = svc::persistent_storages(db, sub.kind, sub.id)
                .await
                .map_err(|e| e.to_string())?;
            rows.sort_by_key(|r| r.get("id").and_then(Value::as_i64).unwrap_or_default());
            for r in rows {
                storages.push_str(r.get("updated_at").and_then(Value::as_str).unwrap_or_default());
            }
        }
        let mut noindex: Vec<String> = Vec::new();
        for a in &apps {
            if let Some(Value::Array(items)) = a.raw.get("noindex_domains") {
                noindex.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        noindex.sort();
        let mut overrides = serde_json::Map::new();
        for a in &apps {
            overrides.insert(
                a.id.to_string(),
                a.raw
                    .get("domain_port_overrides")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
        }
        let stored = vars::load_stored_for(db, enc, &owner(s)).await?;
        let text = format!(
            "{images}{}{images}{storages}{}{}{}",
            domains.join(","),
            noindex.join(","),
            Value::Object(overrides),
            vars::config_hash_values(&stored),
        );
        Ok(format!("{:x}", md5::Md5::digest(text.as_bytes())))
    }

    async fn save_configuration_hash(&self, s: &ServiceRow) {
        let Ok(hash) = self.service_configuration_hash(s).await else {
            return;
        };
        let _ = svc::update(
            &self.0.db,
            s.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            Jobs::now(),
        )
        .await;
    }

    /// `remote_process($commands, $server, type_uuid: $service->uuid, callEventOnFinish:
    /// 'ServiceStartFinished', ...)`: the activity, the live log and the refresh afterwards.
    async fn run_service_process(
        &self,
        s: &ServiceRow,
        server: &ServerInfo,
        target: &Target,
        commands: Vec<String>,
    ) -> Result<(), String> {
        let db = self.0.db.clone();
        let enc = self.0.encrypter.as_ref();
        let command_text = commands.join("\n");
        let props = json!({
            "server_uuid": server.uuid,
            "command": enc.map(|e| e.encrypt_string(&command_text)),
            "command_encrypted": true,
            "type": "inline",
            "type_uuid": s.uuid,
            "status": "queued",
            "team_id": server.team_id,
        });
        let activity = svc::activity_insert(&db, &props, Jobs::now())
            .await
            .map_err(|e| e.to_string())?;
        let _ = dbs::update_activity(
            &db,
            activity,
            &json!({"status": "in_progress"}),
            None,
            Jobs::now(),
        )
        .await;
        let secrets: Vec<String> = match enc {
            Some(e) => vars::load_stored_for(&db, e, &owner(s))
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|v| v.is_shown_once)
                .filter_map(|v| v.value)
                .filter(|v| v.len() >= 4)
                .collect(),
            None => Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
        let writer_db = db.clone();
        let writer = tokio::spawn(async move {
            let mut entries: Vec<Value> = Vec::new();
            while let Some((stream, chunk)) = rx.recv().await {
                let locked: Vec<&str> = secrets.iter().map(String::as_str).collect();
                let output = coolify_compat::redact::redact_text(&chunk, &[], &locked);
                if output.is_empty() {
                    continue;
                }
                entries.push(json!({"type": stream.as_str(), "output": output, "timestamp": hrtime(), "batch": 1, "order": entries.len() + 1}));
                let _ = dbs::update_activity(
                    &writer_db,
                    activity,
                    &json!({}),
                    Some(&serde_json::to_string(&entries).unwrap_or_default()),
                    Jobs::now(),
                )
                .await;
            }
        });
        let result = self
            .remote()
            .run_streaming(target, &commands, RunOptions::op("service.start"), tx)
            .await;
        let _ = writer.await;
        let outcome = match &result {
            Ok(_) => json!({"status": "finished", "exitCode": 0}),
            Err(e) => json!({"status": "error", "exitCode": e.code}),
        };
        let _ = dbs::update_activity(&db, activity, &outcome, None, Jobs::now()).await;
        // ServiceStartFinished => RefreshServiceStatusAfterStart
        if let Ok(Some(fresh)) = svc::find_by_id(&db, s.id).await {
            self.refresh_service_status(&fresh, server).await;
        }
        result.map(|_| ()).map_err(|e| e.message)
    }

    /// `StartService::handle($service, $pullLatestImages, $stopBeforeStart)`.
    pub async fn start_service(
        &self,
        service_id: i64,
        pull_latest: bool,
        stop_before_start: bool,
    ) -> Result<(), String> {
        let (parsed, effects) = self.parse_service_inline(service_id).await?;
        let s = svc::find_by_id(&self.0.db, service_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Service not found.")?;
        let server = self.service_server(&s).await?;
        let target = server.target(self).await?;
        self.run_parse_effects(service_id, effects).await;
        if let Some(enc) = self.0.encrypter.as_ref() {
            // ensureRemoteSecretsResolvable(): before stopping, so the service keeps running.
            vars::load_for(&self.0.db, enc, &owner(&s), s.server_id()).await?;
        }
        if stop_before_start && !pull_latest {
            self.stop_service(&s, false).await;
        }
        self.save_compose_configs(&s, &target).await?;
        self.save_configuration_hash(&s).await;
        let now = Jobs::now();
        for app in svc::subs(&self.0.db, SubKind::Application, s.id)
            .await
            .map_err(|e| e.to_string())?
        {
            let _ = svc::reset_restart_limit(&self.0.db, app.id, now).await;
        }
        let dir = workdir(&s.uuid);
        let mut commands: Vec<String> = vec![
            format!("echo 'Saved configuration files to {dir}.'"),
            format!("touch {dir}/{ENV_FILE}"),
        ];
        let mut subs = svc::subs(&self.0.db, SubKind::Application, s.id)
            .await
            .map_err(|e| e.to_string())?;
        subs.extend(
            svc::subs(&self.0.db, SubKind::Database, s.id)
                .await
                .map_err(|e| e.to_string())?,
        );
        commands.extend(storage::ensure_content_files(self, &s, &target, &subs).await);
        commands.extend(warning_commands(&parsed.warnings));
        let compose = format!(
            "{}docker compose --project-directory {dir} -f {dir}/docker-compose.yml --project-name {}",
            docker_config_prefix(&server),
            s.uuid
        );
        if pull_latest {
            commands.push("echo 'Pulling images.'".into());
            commands.push(format!("{compose} pull < /dev/null"));
        }
        if has_networks(&s) {
            commands.push("echo 'Creating Docker network.'".into());
            commands.push(format!(
                "docker network inspect {u} >/dev/null 2>&1 || docker network create --attachable {u}",
                u = s.uuid
            ));
        }
        commands.push("echo Starting service.".into());
        if s.bool("connect_to_docker_network") {
            commands.push(format!(
                "{compose} up -d --remove-orphans --force-recreate --build --no-start < /dev/null"
            ));
            let network = self.service_network(&s).await;
            if let Ok(yaml) =
                validate::parse_docker_compose_yaml(s.str("docker_compose").unwrap_or_default())
                && let Some(PhpValue::Array(services)) = php::get(&yaml, "services")
            {
                for (name, config) in services {
                    if let Some(c) = predefined_network_connect_command(
                        &s.uuid,
                        &network,
                        &name.as_string(),
                        Some(config),
                    ) {
                        commands.push(c);
                    }
                }
            }
            commands.push(format!("{compose} start < /dev/null"));
        } else {
            commands.push(format!(
                "{compose} up -d --remove-orphans --force-recreate --build < /dev/null"
            ));
        }
        commands.push(format!(
            "docker network connect {} coolify-proxy >/dev/null 2>&1 || true",
            s.uuid
        ));
        // logDrainNetworkConnectCommands()
        if s.bool("connect_to_docker_network")
            && crate::compose_parser::service::is_fluent_bit_log_drain_enabled(&Value::Object(
                server.settings.clone(),
            ))
        {
            let network = self.service_network(&s).await;
            if !network.trim().is_empty() {
                commands.push(format!(
                    "docker network connect {} coolify-log-drain >/dev/null 2>&1 || true",
                    quote(&network)
                ));
            }
        }
        self.run_service_process(&s, &server, &target, commands).await
    }

    /// `StartService::dispatch($service)`.
    pub fn spawn_service_start(&self, service_id: i64, pull_latest: bool, stop_first: bool) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs.start_service(service_id, pull_latest, stop_first).await {
                tracing::warn!(service_id, error = %e, "service start failed");
            }
        });
    }

    /// `StopService::handle($service, $deleteConnectedNetworks, $dockerCleanup)`.
    pub async fn stop_service(&self, s: &ServiceRow, delete_networks: bool) {
        let db = &self.0.db;
        let now = Jobs::now();
        let _ = svc::cancel_running_activities(db, &s.uuid, now).await;
        let Ok(server) = self.service_server(s).await else {
            return;
        };
        if !server.is_functional() {
            return;
        }
        let Ok(target) = server.target(self).await else {
            return;
        };
        let apps = svc::subs(db, SubKind::Application, s.id)
            .await
            .unwrap_or_default();
        let databases = svc::subs(db, SubKind::Database, s.id)
            .await
            .unwrap_or_default();
        let containers: Vec<String> = apps
            .iter()
            .chain(databases.iter())
            .map(|sub| format!("{}-{}", sub.name, s.uuid))
            .collect();
        if !containers.is_empty() {
            let timeout = if containers.len() > 5 { 10 } else { 30 };
            let list = containers
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(" ");
            let stop = docker_stop_command(timeout, &list, server.docker_version());
            let rm = format!("docker rm -f {list}");
            self.remote()
                .run_quiet(
                    &target,
                    &[stop.as_str(), rm.as_str()],
                    RunOptions::op("service.stop"),
                )
                .await;
        }
        for app in &apps {
            let _ = svc::sub_set_status(db, SubKind::Application, app.id, "exited", now).await;
            let _ = svc::reset_restart_limit(db, app.id, now).await;
        }
        for d in &databases {
            let _ = svc::sub_set_status(db, SubKind::Database, d.id, "exited", now).await;
        }
        if delete_networks {
            self.delete_connected_networks(s, &target).await;
        }
    }

    /// `StopService::dispatch($service, false, $dockerCleanup)`.
    pub fn spawn_service_stop(&self, s: ServiceRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_service(&s, false).await;
        });
    }

    /// `Service::deleteConnectedNetworks()`.
    async fn delete_connected_networks(&self, s: &ServiceRow, target: &Target) {
        let disconnect = format!("docker network disconnect {} coolify-proxy", s.uuid);
        let remove = format!("docker network rm {}", s.uuid);
        for c in [disconnect, remove] {
            self.remote()
                .run_quiet(target, &[c.as_str()], RunOptions::op("service.networks"))
                .await;
        }
    }

    /// `DeleteResourceJob` for a service: stop, remote cleanup, then the rows.
    pub fn spawn_service_delete(
        &self,
        s: ServiceRow,
        delete_volumes: bool,
        delete_networks: bool,
        delete_configurations: bool,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_service(&s, delete_networks).await;
            if let Err(e) = jobs
                .cleanup_remote(&s, delete_volumes, delete_networks, delete_configurations)
                .await
            {
                // A resource can always be deleted from Coolify, also when its server does not respond.
                tracing::warn!(service = %s.uuid, error = %e, "remote cleanup failed while deleting service");
            }
            if let Err(e) = svc::force_delete(&jobs.0.db, s.id).await {
                tracing::warn!(service = %s.uuid, error = %e, "deleting service rows failed");
            }
        });
    }

    /// `DeleteService::cleanupRemote()`.
    async fn cleanup_remote(
        &self,
        s: &ServiceRow,
        delete_volumes: bool,
        delete_networks: bool,
        delete_configurations: bool,
    ) -> Result<(), String> {
        let server = self.service_server(s).await?;
        if !server.is_functional() {
            return Err("Server is not functional.".into());
        }
        let target = server.target(self).await?;
        let script = format!(
            "{}; [ -z \"$container_ids\" ] || docker rm -f $container_ids",
            container_ids_script(&s.uuid)
        );
        let command = format!("sh -c {}", quote(&script));
        self.remote()
            .run(&target, &[command.as_str()], RunOptions::op("service.delete"))
            .await
            .map_err(|e| e.message)?;
        let mut failed: Vec<String> = Vec::new();
        if delete_volumes {
            for kind in [SubKind::Application, SubKind::Database] {
                for sub in svc::subs(&self.0.db, kind, s.id)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    for r in svc::persistent_storages(&self.0.db, kind, sub.id)
                        .await
                        .map_err(|e| e.to_string())?
                    {
                        let name = r.get("name").and_then(Value::as_str).unwrap_or_default();
                        let c = format!("docker volume rm -f {}", quote(name));
                        if let Err(e) = self
                            .remote()
                            .run(&target, &[c.as_str()], RunOptions::op("service.delete"))
                            .await
                        {
                            failed.push(format!("{name} ({})", e.message));
                        }
                    }
                }
            }
        }
        if delete_networks {
            self.delete_connected_networks(s, &target).await;
        }
        if delete_configurations {
            let c = format!("rm -rf {}", workdir(&s.uuid));
            self.remote()
                .run_quiet(&target, &[c.as_str()], RunOptions::op("service.delete"))
                .await;
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Could not remove these volumes: {}",
                failed.join(", ")
            ))
        }
    }

    /// `RestartServiceApplication::handle()`.
    pub fn spawn_sub_restart(&self, s: ServiceRow, sub: SubRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let Ok(server) = jobs.service_server(&s).await else {
                return;
            };
            let Ok(target) = server.target(&jobs).await else {
                return;
            };
            let c = format!("docker restart {}", quote(&format!("{}-{}", sub.name, s.uuid)));
            let _ = jobs
                .remote()
                .run(&target, &[c.as_str()], RunOptions::op("service.restart"))
                .await;
        });
    }

    /// `StopServiceApplication::handle()`.
    pub async fn stop_sub(&self, s: &ServiceRow, sub: &SubRow, reset: bool, remove: bool) {
        let Ok(server) = self.service_server(s).await else {
            return;
        };
        let Ok(target) = server.target(self).await else {
            return;
        };
        let name = quote(&format!("{}-{}", sub.name, s.uuid));
        let c = if remove {
            format!("docker rm -f {name}")
        } else {
            format!("docker stop {name}")
        };
        let result = self
            .remote()
            .run(&target, &[c.as_str()], RunOptions::op("service.stop"))
            .await;
        if result.is_err() && !remove {
            return;
        }
        let now = Jobs::now();
        let _ = svc::sub_set_status(&self.0.db, sub.kind, sub.id, "exited", now).await;
        if reset && sub.is_application() {
            let _ = svc::reset_restart_limit(&self.0.db, sub.id, now).await;
        }
    }

    pub fn spawn_sub_stop(&self, s: ServiceRow, sub: SubRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.stop_sub(&s, &sub, true, false).await;
        });
    }

    /// `DeployServiceApplication::handle($resource, $pullLatestImages, $forceRebuild)`.
    pub async fn deploy_sub(
        &self,
        service_id: i64,
        kind: SubKind,
        name: &str,
        pull_latest: bool,
        force_rebuild: bool,
    ) -> Result<(), String> {
        let (parsed, effects) = self.parse_service_inline(service_id).await?;
        let s = svc::find_by_id(&self.0.db, service_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Service not found.")?;
        let server = self.service_server(&s).await?;
        let target = server.target(self).await?;
        self.run_parse_effects(service_id, effects).await;
        self.save_compose_configs(&s, &target).await?;
        self.save_configuration_hash(&s).await;
        let dir = workdir(&s.uuid);
        let compose_file = format!("{dir}/docker-compose.yml");
        let (safe_dir, safe_file, safe_project, safe_name) = (
            quote(&dir),
            quote(&compose_file),
            quote(&s.uuid),
            quote(name),
        );
        let mut commands = vec![
            format!("echo {}", quote(&format!("Saved configuration files to {dir}."))),
            format!("touch {}", quote(&format!("{dir}/{ENV_FILE}"))),
        ];
        if let Some(sub) = svc::sub_by_name(&self.0.db, kind, s.id, name)
            .await
            .map_err(|e| e.to_string())?
        {
            commands.extend(storage::ensure_content_files(self, &s, &target, &[sub]).await);
        }
        commands.extend(warning_commands(&parsed.warnings));
        let prefix = docker_config_prefix(&server);
        if pull_latest {
            commands.push("echo Pulling image for service.".into());
            commands.push(format!(
                "{prefix}docker compose --project-directory {safe_dir} -f {safe_file} --project-name {safe_project} pull {safe_name} < /dev/null"
            ));
        }
        if has_networks(&s) {
            commands.push("echo Creating Docker network.".into());
            commands.push(format!(
                "docker network inspect {safe_project} >/dev/null 2>&1 || docker network create --attachable {safe_project}"
            ));
        }
        let connect = s.bool("connect_to_docker_network");
        let mut up = format!(
            "{prefix}docker compose --project-directory {safe_dir} -f {safe_file} --project-name {safe_project} up -d --no-deps"
        );
        if force_rebuild {
            up.push_str(" --build");
        }
        if connect {
            up.push_str(" --no-start");
        }
        up.push_str(&format!(" {safe_name} < /dev/null"));
        commands.push("echo Starting service container.".into());
        commands.push(up);
        if connect {
            let network = self.service_network(&s).await;
            let yaml =
                validate::parse_docker_compose_yaml(s.str("docker_compose").unwrap_or_default())
                    .ok();
            let config = yaml
                .as_ref()
                .and_then(|y| php::get(y, "services"))
                .and_then(|v| php::get(v, name));
            if let Some(c) = predefined_network_connect_command(&s.uuid, &network, name, config) {
                commands.push(c);
            }
            commands.push(format!(
                "docker start {} >/dev/null",
                quote(&format!("{name}-{}", s.uuid))
            ));
        }
        commands.push(format!(
            "docker network connect {safe_project} coolify-proxy >/dev/null 2>&1 || true"
        ));
        self.run_service_process(&s, &server, &target, commands).await
    }

    pub fn spawn_sub_deploy(
        &self,
        service_id: i64,
        kind: SubKind,
        name: String,
        pull_latest: bool,
        force_rebuild: bool,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs
                .deploy_sub(service_id, kind, &name, pull_latest, force_rebuild)
                .await
            {
                tracing::warn!(service_id, error = %e, "service part deploy failed");
            }
        });
    }

    /// `getContainerStatus($server, $container)`.
    pub async fn service_container_status(&self, target: &Target, container: &str) -> String {
        let c = format!(
            "docker inspect --format '{{{{json .}}}}' {}",
            quote(container)
        );
        let Some(out) = self
            .remote()
            .run_quiet(target, &[c.as_str()], RunOptions::op("service.status"))
            .await
        else {
            return "exited".into();
        };
        serde_json::from_str::<Value>(out.trim())
            .ok()
            .and_then(|v| {
                v.pointer("/State/Status")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "exited".into())
    }

    /// `getContainerLogs($server, $container, $lines, $showTimestamps)`.
    pub async fn service_container_logs(
        &self,
        target: &Target,
        container: &str,
        lines: &str,
        timestamps: bool,
    ) -> Result<String, String> {
        let mut c = format!("docker logs -n {lines}");
        if timestamps {
            c.push_str(" --timestamps");
        }
        let c = format!("{c} {} 2>&1", quote(container));
        self.remote()
            .run(target, &[c.as_str()], RunOptions::op("service.logs"))
            .await
            .map(Option::unwrap_or_default)
            .map_err(|e| e.message)
    }

    /// `getCurrentServiceSubContainerStatus($server, $service, $name)` found a container.
    pub async fn service_container_exists(&self, target: &Target, uuid: &str, name: &str) -> bool {
        let script = format!(
            "{}; [ -z \"$container_ids\" ] || docker container inspect $container_ids --format '{{{{json .Config.Labels}}}}'",
            container_ids_script(uuid)
        );
        let command = format!("sh -c {}", quote(&script));
        let out = self
            .remote()
            .run_quiet(target, &[command.as_str()], RunOptions::op("service.status"))
            .await
            .unwrap_or_default();
        out.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
            .any(|labels| labels.get("coolify.name").and_then(Value::as_str) == Some(name))
    }

    /// The SSH target of a service's server when it is functional and not a swarm.
    pub async fn service_target(&self, s: &ServiceRow) -> Result<(ServerInfo, Target), String> {
        let server = self.service_server(s).await?;
        let target = server.target(self).await?;
        Ok((server, target))
    }

    /// `Duration` the refresher waits between inspections while a health check is starting.
    pub(crate) const fn health_wait() -> Duration {
        Duration::from_secs(5)
    }
}

/// `StartService::composeVolumeWarningCommands($service)`.
fn warning_commands(warnings: &[String]) -> Vec<String> {
    warnings
        .iter()
        .map(|w| format!("echo {}", quote(&format!("Warning: {w}"))))
        .collect()
}

/// `containerIdsByOwnerScript('service', $uuid)`.
pub(crate) fn container_ids_script(uuid: &str) -> String {
    format!(
        "container_ids=$({{ docker ps -a --filter {} --format '{{{{.ID}}}}'; docker ps -a --filter 'label=coolify.serviceId' --filter {} --format '{{{{.ID}}}}'; }} | sort -u)",
        quote(&format!("label=coolify.serviceUuid={uuid}")),
        quote(&format!("label=com.docker.compose.project={uuid}")),
    )
}

/// `$service->composeEnvironmentFileLine($variable)`.
fn env_file_line(v: &vars::Var) -> String {
    let Some(value) = v.resolved.as_deref() else {
        return format!("{}=", v.key);
    };
    let formatted = if v.exact() {
        crate::applications::compose::escape_env_file_value(
            Some(value),
            v.allows_interpolation(value),
        )
    } else {
        v.format(Some(value)).unwrap_or_default()
    };
    format!("{}={formatted}", v.key)
}

/// The base configuration directory, for callers outside the parser.
pub fn base_configuration_dir() -> &'static str {
    volumes::BASE_CONFIGURATION_DIR
}

/// Whether an uploaded compose file passes `validateDockerComposeForInjection()`.
pub fn validate_compose(compose: &str) -> Result<(), String> {
    validate::validate_docker_compose_for_injection(compose)
}

/// `parseDockerComposeYaml($compose)` succeeded.
pub fn compose_parses(compose: &str) -> Result<(), String> {
    validate::parse_docker_compose_yaml(compose)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
