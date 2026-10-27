//! Standalone database lifecycle: upstream's `StartDatabase` +
//! `DatabaseStartJob` + `StartPostgresql`, `StopDatabase`, `RestartDatabase`,
//! `DeleteResourceJob`, `ResourceStatusRefresher::refreshDatabase` and the
//! database part of `GetContainersStatus`.
//!
//! Commands and the Docker Compose file are built exactly as upstream builds
//! them, so a database started by either implementation runs the same way.

use std::time::Duration;

use chrono::{NaiveDateTime, SecondsFormat, Utc};
use coolify_compat::yaml::{self, PhpValue};
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::{RunOptions, Stream, shell::quote};
use serde_json::{Value, json};

use crate::Jobs;
use crate::server::ServerInfo;

pub const IN_PROGRESS: &str =
    "Another start, restart or import of this database is already in progress.";
const NOT_FUNCTIONAL: &str = "Server is not functional";
/// `database_configuration_dir()` with upstream's default base path.
const CONFIGURATION_DIR: &str = "/data/coolify/databases";
/// `ResourceStatusRefresher::HEALTH_WAIT_*`.
const HEALTH_WAIT_ATTEMPTS: u32 = 10;
const HEALTH_WAIT_INTERVAL: Duration = Duration::from_secs(3);

fn now() -> NaiveDateTime {
    Jobs::now()
}

/// `hrtime(true)` stands in as a monotonic nanosecond counter for log ordering.
fn hrtime() -> i64 {
    Utc::now().timestamp_nanos_opt().unwrap_or_default()
}

/// `Str::slug($title)` for the label values (ASCII transliteration is not ported:
/// non-ASCII letters are kept, lowercased).
pub fn slug(title: &str) -> String {
    let mut s = title.replace('_', "-");
    s = s.replace('@', "-at-");
    let lowered = s.to_lowercase();
    let kept: String = lowered
        .chars()
        .filter(|c| *c == '-' || c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let mut out = String::new();
    let mut sep = false;
    for c in kept.chars() {
        if c == '-' || c.is_whitespace() {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.push(c);
        }
    }
    out
}

/// `dockerStopCommand($timeout, $containers, $server)`.
fn docker_stop_command(timeout: u32, container: &str, docker_version: Option<&str>) -> String {
    let normalized = crate::checks::parse_docker_engine_version(docker_version);
    let new_flag = normalized.as_deref().is_some_and(|v| {
        let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
        (
            p.next().unwrap_or(0),
            p.next().unwrap_or(0),
            p.next().unwrap_or(0),
        ) >= (28, 0, 0)
    });
    let flag = if new_flag {
        format!("--timeout={timeout}")
    } else {
        format!("--time={timeout}")
    };
    format!("docker stop {flag} {container}")
}

/// `serverDockerConfigPrefix`.
fn server_docker_config_prefix(server: &ServerInfo) -> &'static str {
    if server.is_non_root() {
        r#"DOCKER_CONTEXT=default DOCKER_CONFIG="$(find "$HOME/.docker/config.json" -maxdepth 0 -exec dirname {} \; 2>/dev/null)" "#
    } else {
        ""
    }
}

/// `escapeDollarSign`.
fn escape_dollar(v: &str) -> String {
    v.replace('$', "$$")
}

/// `base64_encode`.
fn b64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// `generate_readme_file($name, now())`: `sanitize_string` keeps the plain text.
fn readme(name: &str, at: NaiveDateTime) -> String {
    format!(
        "Resource name: {name}\nLatest Deployment Date: {}",
        at.format("%Y-%m-%d %H:%M:%S")
    )
}

/// Everything `StartPostgresql::handle` needs, resolved.
#[derive(Debug)]
pub struct PostgresStart<'a> {
    pub db: &'a DatabaseRow,
    pub password: String,
    pub server: &'a ServerInfo,
    pub network: String,
    pub volumes: Vec<(String, String, Option<String>)>,
    pub now: NaiveDateTime,
}

/// `StartPostgresql::handle`: the commands, including the Compose file.
pub fn postgres_start_commands(p: &PostgresStart<'_>) -> (Vec<String>, String) {
    let d = p.db;
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let mut commands: Vec<String> = vec![
        "echo 'Starting database.'".into(),
        "echo 'Creating directories.'".into(),
        format!("mkdir -p {dir}"),
        format!("mkdir -p {dir}/docker-entrypoint-initdb.d/"),
        "echo 'Directories created successfully.'".into(),
        // enable_ssl is false (SSL is not ported; the caller refuses it).
        format!("rm -rf {dir}/ssl"),
    ];

    // generate_environment_variables (no runtime variables: the caller refuses them).
    let user = s("postgres_user");
    let db_name = s("postgres_db");
    let mut env: Vec<String> = vec![
        format!("POSTGRES_USER={user}"),
        format!("PGUSER={user}"),
        format!("POSTGRES_PASSWORD={}", p.password),
        format!("PGPASSWORD={}", p.password),
        format!("POSTGRES_DB={db_name}"),
        format!("COOLIFY_RESOURCE_UUID={}", escape_dollar(name)),
        format!("COOLIFY_CONTAINER_NAME={}", escape_dollar(name)),
    ];
    if d.raw
        .get("include_coolify_default_variables")
        .and_then(Value::as_bool)
        != Some(false)
    {
        let defaults = [
            ("COOLIFY_APP_NAME", d.str("name").map(str::to_owned)),
            (
                "COOLIFY_PROJECT_NAME",
                d.project
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ),
            (
                "COOLIFY_ENVIRONMENT_NAME",
                d.environment
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ),
            (
                "COOLIFY_SERVER_IP",
                Some(p.server.ip.clone()).filter(|_| true),
            ),
        ];
        for (k, v) in defaults {
            if let Some(v) = v {
                env.push(format!("{k}={}", escape_dollar(&v)));
            }
        }
    }

    // generate_init_scripts
    commands.push(format!(
        "find {dir}/docker-entrypoint-initdb.d -mindepth 1 -maxdepth 1 -exec rm -rf {{}} + 2>/dev/null || true"
    ));
    let mut init_scripts: Vec<String> = Vec::new();
    if let Some(Value::Array(scripts)) = d.raw.get("init_scripts") {
        for script in scripts {
            let filename = script
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let filename = filename.rsplit('/').next().unwrap_or_default();
            let content = script
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let target = format!("{dir}/docker-entrypoint-initdb.d/{filename}");
            commands.push(format!(
                "echo '{}' | base64 -d | tee {} > /dev/null",
                b64(content),
                quote(&target)
            ));
            init_scripts.push(target);
        }
    }

    // add_custom_conf
    let conf_path = format!("{dir}/custom-postgres.conf");
    let conf = d
        .str("postgres_conf")
        .filter(|c| !c.trim().is_empty())
        .map(|c| {
            if c.contains("listen_addresses") {
                c.to_owned()
            } else {
                format!("{c}\nlisten_addresses = '*'")
            }
        });
    match &conf {
        None => commands.push(format!("rm -f {conf_path}")),
        Some(c) => {
            commands.push(format!(
                "echo '{}' | base64 -d | tee {conf_path} > /dev/null",
                b64(c)
            ));
            commands.push(format!("chmod 644 {conf_path}"));
        }
    }

    let int = |k: &str, default: i64| d.int(k).unwrap_or(default);
    let health = PhpValue::map([
        (
            "test",
            PhpValue::strings(["CMD", "psql", "-U", &user, "-d", &db_name, "-c", "SELECT 1"]),
        ),
        (
            "interval",
            PhpValue::from(format!("{}s", int("health_check_interval", 15))),
        ),
        (
            "timeout",
            PhpValue::from(format!("{}s", int("health_check_timeout", 5))),
        ),
        ("retries", PhpValue::Int(int("health_check_retries", 5))),
        (
            "start_period",
            PhpValue::from(format!("{}s", int("health_check_start_period", 5))),
        ),
    ]);
    let project_name = d
        .project
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let env_name = d
        .environment
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let db_display = s("name");
    let labels = PhpValue::strings([
        "coolify.managed=true".to_owned(),
        "coolify.type=database".to_owned(),
        format!("coolify.databaseUuid={name}"),
        format!("coolify.resourceName={}", slug(&db_display)),
        format!("coolify.serviceName={}", slug(&db_display)),
        format!("coolify.projectName={}", slug(project_name)),
        format!("coolify.environmentName={}", slug(env_name)),
        format!("coolify.database.subType={}", Kind::Postgresql.type_name()),
    ]);
    let cpus: f64 = s("limits_cpus").trim().parse().unwrap_or(0.0);
    let mut service = PhpValue::map([
        ("image", PhpValue::from(s("image"))),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([p.network.clone()])),
        ("labels", labels),
        ("healthcheck", health),
        ("mem_limit", PhpValue::from(s("limits_memory"))),
        ("memswap_limit", PhpValue::from(s("limits_memory_swap"))),
        (
            "mem_swappiness",
            PhpValue::Int(int("limits_memory_swappiness", 60)),
        ),
        (
            "mem_reservation",
            PhpValue::from(s("limits_memory_reservation")),
        ),
        ("cpus", PhpValue::Float(cpus)),
        ("cpu_shares", PhpValue::Int(int("limits_cpu_shares", 1024))),
    ]);
    if let Some(cpuset) = d.str("limits_cpuset").filter(|c| !c.trim().is_empty()) {
        service.set("cpuset", PhpValue::from(cpuset));
    }
    if let Some(ports) = d.str("ports_mappings") {
        service.set(
            "ports",
            PhpValue::strings(ports.split(',').map(str::to_owned)),
        );
    }
    let mut volumes = PhpValue::list([]);
    let mut volume_names = PhpValue::list([]);
    for (vname, mount, host) in &p.volumes {
        match host.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => volumes.push(PhpValue::from(format!("{h}:{mount}"))),
            None => {
                volumes.push(PhpValue::from(format!("{vname}:{mount}")));
                volume_names.set(
                    vname,
                    PhpValue::map([
                        ("name", PhpValue::from(vname.as_str())),
                        ("external", PhpValue::Bool(false)),
                    ]),
                );
            }
        }
    }
    for script in &init_scripts {
        let base = script.rsplit('/').next().unwrap_or_default();
        volumes.push(PhpValue::map([
            ("type", PhpValue::from("bind")),
            ("source", PhpValue::from(script.as_str())),
            (
                "target",
                PhpValue::from(format!("/docker-entrypoint-initdb.d/{base}")),
            ),
            ("read_only", PhpValue::Bool(true)),
        ]));
    }
    let mut command = vec!["postgres".to_owned()];
    if conf.is_some() {
        volumes.push(PhpValue::map([
            ("type", PhpValue::from("bind")),
            ("source", PhpValue::from(conf_path.clone())),
            ("target", PhpValue::from("/etc/postgresql/postgresql.conf")),
            ("read_only", PhpValue::Bool(true)),
        ]));
        command.extend([
            "-c".to_owned(),
            "config_file=/etc/postgresql/postgresql.conf".to_owned(),
        ]);
    }
    service.set("volumes", volumes);
    if command.len() > 1 {
        service.set("command", PhpValue::strings(command));
    }
    if d.raw.get("health_check_enabled").and_then(Value::as_bool) == Some(false) {
        service.remove("healthcheck");
    }
    let mut compose = PhpValue::map([
        ("services", PhpValue::map([(name, service)])),
        (
            "networks",
            PhpValue::map([(
                p.network.clone(),
                PhpValue::map([
                    ("external", PhpValue::Bool(true)),
                    ("name", PhpValue::from(p.network.clone())),
                    ("attachable", PhpValue::Bool(true)),
                ]),
            )]),
        ),
    ]);
    if !matches!(&volume_names, PhpValue::Array(v) if v.is_empty()) {
        compose.set("volumes", volume_names);
    }
    let compose_yaml = yaml::dump(&compose, 10);
    commands.push(format!(
        "echo '{}' | base64 -d | tee {dir}/docker-compose.yml > /dev/null",
        b64(&compose_yaml)
    ));
    commands.push(format!(
        "echo '{}' | tee {dir}/README.md > /dev/null",
        readme(&db_display, p.now)
    ));
    commands.push(format!(
        "echo {}",
        quote(&format!("Pulling {} image.", s("image")))
    ));
    commands.push(format!(
        "{}docker compose -f {dir}/docker-compose.yml pull",
        server_docker_config_prefix(p.server)
    ));
    commands.push(format!(
        "{} 2>/dev/null || true",
        docker_stop_command(10, name, p.server.docker_version())
    ));
    commands.push(format!("docker rm -f {name} 2>/dev/null || true"));
    commands.push(format!("docker compose -f {dir}/docker-compose.yml up -d"));
    commands.push("echo 'Database started.'".into());
    (commands, compose_yaml)
}

/// `ContainerStatusAggregator::containerStatus`.
fn container_status(c: &Value) -> String {
    let state = c
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if state == "exited" {
        return state.to_owned();
    }
    let health = c
        .pointer("/State/Health/Status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("{state}:{health}")
}

/// `format_docker_command_output_to_json`: one JSON object per line.
fn json_lines(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.is_object())
        .collect()
}

impl Jobs {
    fn reserve_database(&self, uuid: &str) -> bool {
        self.0
            .db_ops
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(uuid.to_owned())
    }

    fn release_database(&self, uuid: &str) {
        self.0
            .db_ops
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(uuid);
    }

    async fn server_for(&self, d: &DatabaseRow) -> Result<ServerInfo, String> {
        let server_id = d.server_id().ok_or("Database destination not found.")?;
        let row = servers::find_by_id(&self.0.db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        Ok(ServerInfo::from_row(&row))
    }

    /// `StartDatabase::reserveOperation` + `dispatchReserved(StartDatabase::class)`.
    /// The request answers right away; the start runs in the background.
    pub fn spawn_database_start(
        &self,
        d: DatabaseRow,
        user_id: Option<i64>,
    ) -> Result<(), &'static str> {
        if !self.reserve_database(&d.uuid) {
            return Err(IN_PROGRESS);
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let uuid = d.uuid.clone();
            jobs.start_database(&d, user_id).await;
            jobs.release_database(&uuid);
        });
        Ok(())
    }

    /// `RestartDatabase`: stop (without cleanup), then start, under one reservation.
    pub fn spawn_database_restart(
        &self,
        d: DatabaseRow,
        user_id: Option<i64>,
    ) -> Result<(), &'static str> {
        if !self.reserve_database(&d.uuid) {
            return Err(IN_PROGRESS);
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let uuid = d.uuid.clone();
            match jobs.server_for(&d).await {
                Ok(server) if server.is_functional() => {
                    jobs.stop_database(&d, &server).await;
                    jobs.start_database(&d, user_id).await;
                }
                Ok(_) => jobs.record_failed_start(&d, NOT_FUNCTIONAL, user_id).await,
                Err(e) => tracing::warn!(database = %uuid, error = %e, "database restart failed"),
            }
            jobs.release_database(&uuid);
        });
        Ok(())
    }

    /// `StopDatabase::dispatch`.
    pub fn spawn_database_stop(&self, d: DatabaseRow) {
        let jobs = self.clone();
        tokio::spawn(async move {
            match jobs.server_for(&d).await {
                Ok(server) if server.is_functional() => jobs.stop_database(&d, &server).await,
                Ok(_) => {
                    tracing::info!(database = %d.uuid, "stop skipped: server is not functional")
                }
                Err(e) => tracing::warn!(database = %d.uuid, error = %e, "database stop failed"),
            }
        });
    }

    /// `DeleteResourceJob` for a database: stop, remove configuration and
    /// volumes on the server (best effort), then delete the rows.
    pub fn spawn_database_delete(
        &self,
        d: DatabaseRow,
        delete_volumes: bool,
        delete_configurations: bool,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Ok(server) = jobs.server_for(&d).await
                && server.is_functional()
                && let Ok(t) = server.target(&jobs).await
            {
                jobs.stop_database(&d, &server).await;
                let remote = jobs.remote();
                if delete_configurations {
                    remote
                        .run_quiet(
                            &t,
                            &[&format!("rm -rf {CONFIGURATION_DIR}/{}", d.uuid)],
                            RunOptions::op("database.delete"),
                        )
                        .await;
                }
                if delete_volumes
                    && let Ok(volumes) = dbs::persistent_volumes(&jobs.0.db, d.kind, d.id).await
                {
                    for (name, _, _) in volumes {
                        remote
                            .run_quiet(
                                &t,
                                &[&format!("docker volume rm -f {}", quote(&name))],
                                RunOptions::op("database.delete"),
                            )
                            .await;
                    }
                }
            }
            if let Err(e) = dbs::force_delete(&jobs.0.db, d.kind, d.id, delete_volumes).await {
                tracing::warn!(database = %d.uuid, error = %e, "database rows could not be deleted");
            }
        });
    }

    /// `StopDatabase::handle` (no Docker cleanup: `CleanupDocker` is not ported).
    async fn stop_database(&self, d: &DatabaseRow, server: &ServerInfo) {
        if let Ok(t) = server.target(self).await {
            let stop = docker_stop_command(30, &d.uuid, server.docker_version());
            let rm = format!("docker rm -f {}", d.uuid);
            self.remote()
                .run_quiet(
                    &t,
                    &[stop.as_str(), rm.as_str()],
                    RunOptions::op("database.stop"),
                )
                .await;
        }
        let now = now();
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[(
                "status",
                ColumnValue::Text(Some(normalize_status("exited"))),
            )],
            now,
        )
        .await;
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[
                ("restart_count", ColumnValue::Int(0)),
                ("last_restart_at", ColumnValue::Text(None)),
                ("last_restart_type", ColumnValue::Text(None)),
            ],
            now,
        )
        .await;
    }

    async fn record_failed_start(&self, d: &DatabaseRow, message: &str, user_id: Option<i64>) {
        if let Ok(server) = self.server_for(d).await
            && let Ok(id) = self.queue_activity(d, &server, user_id).await
        {
            self.mark_failed(id, message).await;
        }
    }

    async fn queue_activity(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        _user_id: Option<i64>,
    ) -> Result<i64, String> {
        let props = json!({
            "server_uuid": server.uuid,
            "type": "inline",
            "type_uuid": d.uuid,
            "status": "queued",
            "team_id": server.team_id,
            "operation": "database-start",
        });
        dbs::insert_activity(&self.0.db, d.kind, d.id, &props, now())
            .await
            .map_err(|e| e.to_string())
    }

    /// `ResourceStartActivity::markFailed`.
    async fn mark_failed(&self, activity_id: i64, message: &str) {
        let message = coolify_compat::redact::redact_text(message, &[], &[]);
        let Ok(Some((props, desc))) = dbs::activity_by_id(&self.0.db, activity_id).await else {
            return;
        };
        let mut entries: Vec<Value> = serde_json::from_str(&desc).unwrap_or_default();
        let last = entries
            .iter()
            .filter_map(|e| e.get("order").and_then(Value::as_i64))
            .max()
            .unwrap_or(0);
        entries.push(json!({"type": "stderr", "output": format!("\n{message}\n"), "timestamp": hrtime(), "batch": 1, "order": last + 1}));
        let mut merge = json!({
            "status": "error",
            "error": message,
            "failed_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        });
        if props.get("exitCode").is_none_or(Value::is_null) {
            merge["exitCode"] = json!(1);
        }
        let description = serde_json::to_string(&entries).unwrap_or_default();
        let _ =
            dbs::update_activity(&self.0.db, activity_id, &merge, Some(&description), now()).await;
    }

    /// `StartDatabase::queueStart` + `DatabaseStartJob` + `StartPostgresql`.
    async fn start_database(&self, d: &DatabaseRow, user_id: Option<i64>) {
        let server = match self.server_for(d).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(database = %d.uuid, error = %e, "database start failed");
                return;
            }
        };
        if !server.is_functional() {
            self.record_failed_start(d, NOT_FUNCTIONAL, user_id).await;
            return;
        }
        let now_ = now();
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[
                ("restart_count", ColumnValue::Int(0)),
                ("last_restart_at", ColumnValue::Text(None)),
                ("last_restart_type", ColumnValue::Text(None)),
            ],
            now_,
        )
        .await;
        let activity = match self.queue_activity(d, &server, user_id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(database = %d.uuid, error = %e, "start activity could not be written");
                return;
            }
        };
        match self.run_start(d, &server, activity).await {
            Ok(()) => {
                self.refresh_database_status(d, &server).await;
                self.after_start(d).await;
            }
            Err(message) => self.mark_failed(activity, &message).await,
        }
    }

    async fn run_start(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        activity: i64,
    ) -> Result<(), String> {
        let Some(enc) = self.0.encrypter.as_ref() else {
            return Err("Database start failed.".into());
        };
        let password = d
            .str("postgres_password")
            .and_then(|c| enc.decrypt_string(c).ok())
            .ok_or("Database start failed.")?;
        let network = d
            .destination
            .as_ref()
            .and_then(|x| x.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("coolify")
            .to_owned();
        let volumes = dbs::persistent_volumes(&self.0.db, d.kind, d.id)
            .await
            .map_err(|e| e.to_string())?;
        let start = PostgresStart {
            db: d,
            password: password.clone(),
            server,
            network,
            volumes,
            now: now(),
        };
        let (commands, _) = postgres_start_commands(&start);
        let target = server.target(self).await?;

        let _ = dbs::update_activity(
            &self.0.db,
            activity,
            &json!({"status": "in_progress"}),
            None,
            now(),
        )
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
        let db = self.0.db.clone();
        let secrets = [password];
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
                    &db,
                    activity,
                    &json!({}),
                    Some(&serde_json::to_string(&entries).unwrap_or_default()),
                    now(),
                )
                .await;
            }
        });
        let result = self
            .remote()
            .run_streaming(&target, &commands, RunOptions::op("database.start"), tx)
            .await;
        let _ = writer.await;
        match result {
            Ok(_) => {
                let _ = dbs::update_activity(
                    &self.0.db,
                    activity,
                    &json!({"status": "finished", "exitCode": 0}),
                    None,
                    now(),
                )
                .await;
                Ok(())
            }
            Err(e) => {
                let _ = dbs::update_activity(
                    &self.0.db,
                    activity,
                    &json!({"status": "error", "exitCode": e.code}),
                    None,
                    now(),
                )
                .await;
                // DatabaseStartJob::failed: other errors are not shown as they are.
                Err("Database start failed.".into())
            }
        }
    }

    /// `Database\Heading::activityFinished`: `started_at` once running, and the
    /// configuration hash the "restart required" check compares with.
    async fn after_start(&self, d: &DatabaseRow) {
        let Ok(Some(fresh)) = dbs::find_in_team(&self.0.db, d.team_id, &d.uuid).await else {
            return;
        };
        let now = now();
        if fresh.str("status").unwrap_or_default().contains("running")
            && fresh.str("started_at").is_none()
        {
            let _ = dbs::update_if_changed(
                &self.0.db,
                d.kind,
                d.id,
                &[("started_at", ColumnValue::Time(now))],
                now,
            )
            .await;
        }
        let hash = config_hash(&fresh);
        let _ = dbs::update_if_changed(
            &self.0.db,
            d.kind,
            d.id,
            &[("config_hash", ColumnValue::Text(Some(hash)))],
            now,
        )
        .await;
    }

    /// `ResourceStatusRefresher::refreshDatabase`.
    pub async fn refresh_database_status(&self, d: &DatabaseRow, server: &ServerInfo) {
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let command = format!(
            "docker container inspect {} --format '{{{{json .}}}}'",
            quote(&d.uuid)
        );
        for attempt in 1..=HEALTH_WAIT_ATTEMPTS {
            let out = self
                .remote()
                .run_quiet(&t, &[command.as_str()], RunOptions::op("database.status"))
                .await
                .unwrap_or_default();
            let containers = json_lines(&out);
            let Some(c) = containers
                .iter()
                .find(|c| c.get("Name").and_then(Value::as_str) == Some(&format!("/{}", d.uuid)))
            else {
                return;
            };
            let stored = dbs::find_in_team(&self.0.db, d.team_id, &d.uuid)
                .await
                .ok()
                .flatten();
            let stored = stored
                .as_ref()
                .and_then(|r| r.str("status"))
                .unwrap_or_default()
                .to_owned();
            self.store_status(d, &stored, &container_status(c)).await;
            let starting =
                c.pointer("/State/Health/Status").and_then(Value::as_str) == Some("starting");
            if !starting || attempt >= HEALTH_WAIT_ATTEMPTS {
                return;
            }
            tokio::time::sleep(HEALTH_WAIT_INTERVAL).await;
        }
    }

    /// `ResourceStatusRefresher::store` / `GetContainersStatus`: the stored status
    /// (through the model accessor) is compared with the container's raw status.
    async fn store_status(&self, d: &DatabaseRow, stored: &str, raw: &str) {
        let now = now();
        if normalize_status(stored) != raw {
            let value = ColumnValue::Text(Some(normalize_status(raw)));
            let _ =
                dbs::update_if_changed(&self.0.db, d.kind, d.id, &[("status", value)], now).await;
        } else {
            let _ = dbs::touch_online(&self.0.db, d.kind, d.id, now).await;
        }
    }

    /// The database part of `GetContainersStatus` for one server.
    pub async fn check_server_databases(&self, server_id: i64) {
        let Ok(Some(row)) = servers::find_by_id(&self.0.db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(databases) = dbs::list_for_server(&self.0.db, server_id).await else {
            return;
        };
        let databases: Vec<DatabaseRow> =
            databases.into_iter().filter(|d| d.kind.ported()).collect();
        if databases.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        // `Server::getContainers`: every container with its inspect data.
        let script = "docker container inspect $(docker container ls -aq) --format '{{json .}}'";
        let opts = RunOptions::op("server.containers").timeout(Duration::from_secs(30));
        let Some(out) = self.remote().run_quiet(&t, &[script], opts).await else {
            return;
        };
        let containers = json_lines(&out);
        let now = now();
        let mut found = Vec::new();
        for c in &containers {
            let labels = c.pointer("/Config/Labels");
            let uuid = labels
                .and_then(|l| l.get("coolify.databaseUuid"))
                .and_then(Value::as_str);
            let Some(d) = uuid.and_then(|u| databases.iter().find(|d| d.uuid == u)) else {
                continue;
            };
            found.push(d.id);
            self.store_status(d, d.str("status").unwrap_or_default(), &container_status(c))
                .await;
            let restarts = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
            if restarts > d.int("restart_count").unwrap_or(0) {
                let _ = dbs::update_if_changed(
                    &self.0.db,
                    d.kind,
                    d.id,
                    &[
                        ("restart_count", ColumnValue::Int(restarts)),
                        ("last_restart_at", ColumnValue::Time(now)),
                        ("last_restart_type", ColumnValue::Text(Some("crash".into()))),
                    ],
                    now,
                )
                .await;
            }
        }
        if containers.is_empty() {
            return;
        }
        for d in databases.iter().filter(|d| !found.contains(&d.id)) {
            if d.str("status").unwrap_or_default().starts_with("exited") {
                continue;
            }
            let _ = dbs::update_if_changed(
                &self.0.db,
                d.kind,
                d.id,
                &[
                    (
                        "status",
                        ColumnValue::Text(Some(normalize_status("exited"))),
                    ),
                    ("restart_count", ColumnValue::Int(0)),
                    ("last_restart_at", ColumnValue::Text(None)),
                    ("last_restart_type", ColumnValue::Text(None)),
                ],
                now,
            )
            .await;
        }
    }

    /// `ServerManagerJob` (database statuses only): every minute, each functional server.
    pub fn spawn_status_poller(&self) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let Ok(ids) = dbs::servers_with_databases(&jobs.0.db).await else {
                    continue;
                };
                for id in ids {
                    jobs.check_server_databases(id).await;
                }
            }
        });
    }

    /// The database's latest start activity: `(status, log entries)`.
    pub async fn database_start_log(&self, uuid: &str) -> Option<(String, Value)> {
        let (_, props, description) = dbs::latest_start_activity(&self.0.db, uuid)
            .await
            .ok()
            .flatten()?;
        let status = props
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Some((
            status,
            serde_json::from_str(&description).unwrap_or(Value::Array(vec![])),
        ))
    }
}

/// `StandalonePostgresql::isConfigurationChanged`'s hash (no environment variables:
/// `json_encode` of the empty collection is `[]`).
pub fn config_hash(d: &DatabaseRow) -> String {
    use md5::Digest as _;
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let i = |k: &str, default: i64| d.int(k).unwrap_or(default);
    let enabled = d
        .raw
        .get("health_check_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let health = format!(
        "{}|{}|{}|{}|{}",
        i64::from(enabled),
        i("health_check_interval", 15),
        i("health_check_timeout", 5),
        i("health_check_retries", 5),
        i("health_check_start_period", 5)
    );
    let input = format!(
        "{}{}{}{}{health}[]",
        s("image"),
        s("ports_mappings"),
        s("postgres_initdb_args"),
        s("postgres_host_auth_method")
    );
    hex(&md5::Md5::digest(input.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `ConfigurationChecker`: the hash differs, one was saved before, and the database is not exited.
pub fn configuration_changed(d: &DatabaseRow) -> bool {
    let saved = d.str("config_hash");
    saved.is_some_and(|h| h != config_hash(d))
        && !d.str("status").unwrap_or_default().starts_with("exited")
}

/// The model's `status` mutator: `running (healthy)` / `running:healthy` / `exited` → `status:health`.
pub fn normalize_status(v: &str) -> String {
    let (status, health) = if let Some(i) = v.find('(') {
        let rest = &v[i + 1..];
        (
            v[..i].trim(),
            rest.split(')').next().unwrap_or_default().trim(),
        )
    } else if let Some((s, h)) = v.split_once(':') {
        (s.trim(), h.trim())
    } else {
        (v, "unhealthy")
    };
    format!("{status}:{health}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("postgresql-database-abc"), "postgresql-database-abc");
        assert_eq!(slug("Databases E2E"), "databases-e2e");
        assert_eq!(slug("My_App @ Home!"), "my-app-at-home");
        assert_eq!(slug("  --x--  "), "x");
    }

    #[test]
    fn statuses() {
        assert_eq!(normalize_status("exited"), "exited:unhealthy");
        assert_eq!(normalize_status("running:healthy"), "running:healthy");
        assert_eq!(normalize_status("running (starting)"), "running:starting");
        assert_eq!(normalize_status("exited:unhealthy"), "exited:unhealthy");
    }

    #[test]
    fn stop_flags() {
        assert_eq!(
            docker_stop_command(10, "x", Some("29.1.3")),
            "docker stop --timeout=10 x"
        );
        assert_eq!(
            docker_stop_command(10, "x", Some("27.5.1")),
            "docker stop --time=10 x"
        );
        assert_eq!(
            docker_stop_command(30, "x", None),
            "docker stop --time=30 x"
        );
    }
}
