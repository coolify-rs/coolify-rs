//! `StartPostgresql::handle`.

use coolify_compat::yaml::{self, PhpValue};
use coolify_db::databases::Kind;
use coolify_remote::shell::quote;
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, b64, docker_stop_command, readme, server_docker_config_prefix, slug,
};
use super::env::{add_coolify_variables, has_key};

/// `StartPostgresql::handle`: the commands, including the Compose file.
pub(super) fn start_commands(p: &StartContext<'_>) -> (Vec<String>, String) {
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
    ];
    commands.extend(p.ssl_commands.iter().cloned());
    commands.extend(p.ensure_commands.iter().cloned());

    // generate_environment_variables
    let mut user = s("postgres_user");
    let mut db_name = s("postgres_db");
    let model_password = p.secret("postgres_password");
    let mut password = model_password.clone();
    let mut env: Vec<String> = Vec::new();
    for v in &p.variables {
        let raw = v.resolved.clone().unwrap_or_default();
        let resolved = v.format(Some(&raw)).unwrap_or_default();
        let compose_value = v.compose_file_value(&raw, false);
        env.push(format!("{}={resolved}", v.key));
        match v.key.as_str() {
            "POSTGRES_USER" => user = compose_value,
            "POSTGRES_DB" => db_name = compose_value,
            "POSTGRES_PASSWORD" => password = resolved,
            _ => {}
        }
    }
    let model_user = s("postgres_user");
    let model_db = s("postgres_db");
    for (key, value) in [
        ("POSTGRES_USER", model_user.clone()),
        ("PGUSER", model_user),
        ("POSTGRES_PASSWORD", model_password),
        // psql in the healthcheck, backups and restores reads PGPASSWORD.
        ("PGPASSWORD", password),
        ("POSTGRES_DB", model_db),
    ] {
        if !has_key(&env, key) {
            env.push(format!("{key}={value}"));
        }
    }
    add_coolify_variables(&mut env, p);

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
    p.apply_logging(&mut service);
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
    for mount in &p.file_mounts {
        volumes.push(PhpValue::from(mount.as_str()));
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
    if d.bool("enable_ssl") {
        command.extend(
            [
                "-c",
                "ssl=on",
                "-c",
                "ssl_cert_file=/var/lib/postgresql/certs/server.crt",
                "-c",
                "ssl_key_file=/var/lib/postgresql/certs/server.key",
            ]
            .map(str::to_owned),
        );
    }
    service.set("volumes", volumes);
    p.apply_docker_options(&mut service);
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
    if d.bool("enable_ssl") {
        commands.push(format!(
            "docker compose -f {dir}/docker-compose.yml run --rm --no-deps --user root --entrypoint chown {name} postgres:postgres /var/lib/postgresql/certs/server.key /var/lib/postgresql/certs/server.crt < /dev/null"
        ));
    }
    commands.push(format!(
        "{} 2>/dev/null || true",
        docker_stop_command(10, name, p.server.docker_version())
    ));
    commands.push(format!("docker rm -f {name} 2>/dev/null || true"));
    commands.push(format!("docker compose -f {dir}/docker-compose.yml up -d"));
    commands.push("echo 'Database started.'".into());
    (commands, compose_yaml)
}
