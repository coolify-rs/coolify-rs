//! `StartRedis::handle`.

use coolify_compat::yaml::{self, PhpValue};
use coolify_remote::shell::quote;
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, b64, database_labels, finish_commands, healthcheck, limits, networks,
    volume_entries,
};
use super::env::add_coolify_variables;

/// The `redis_username` accessor: the first REDIS_USERNAME value when filled, else `default`;
/// with its resolved form.
fn username(ctx: &StartContext<'_>) -> (String, String) {
    match ctx.variables.iter().find(|v| v.key == "REDIS_USERNAME") {
        Some(v) if v.value.as_deref().is_some_and(|x| !x.is_empty()) => (
            v.value.clone().unwrap_or_default(),
            v.resolved.clone().unwrap_or_default(),
        ),
        _ => ("default".into(), "default".into()),
    }
}

/// `$env->update(['value' => $this->database->redis_username])` for each non-shared
/// REDIS_USERNAME: written only when the value changes (the `encrypted` cast's dirty check).
pub(crate) fn username_updates(ctx: &StartContext<'_>) -> Vec<(i64, String)> {
    let (name, _) = username(ctx);
    ctx.variables
        .iter()
        .filter(|v| {
            v.key == "REDIS_USERNAME" && !v.is_shared && v.value.as_deref() != Some(name.as_str())
        })
        .map(|v| (v.id, name.clone()))
        .collect()
}

pub(super) fn start_commands(ctx: &StartContext<'_>) -> (Vec<String>, String) {
    let d = ctx.db;
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    let mut commands: Vec<String> = vec![
        "echo 'Starting database.'".into(),
        "echo 'Creating directories.'".into(),
        format!("mkdir -p {dir}"),
        "echo 'Directories created successfully.'".into(),
        // enable_ssl is false (SSL is not ported; the caller refuses it).
        format!("rm -rf {dir}/ssl"),
    ];
    let (volume_list, volume_names) = volume_entries(&ctx.volumes);

    // generate_environment_variables
    let legacy_quoting = d.bool("legacy_password_quoting");
    let password_var = ctx.variables.iter().find(|v| v.key == "REDIS_PASSWORD");
    // The `redis_password` accessor: the stored REDIS_PASSWORD value, unresolved.
    let mut password = password_var
        .and_then(|v| v.value.clone())
        .unwrap_or_default();
    // serverPasswordEnvironmentVariable(): legacy databases keep the stored value.
    let server_password_var = password_var.filter(|_| !legacy_quoting);
    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        if server_password_var.is_some_and(|p| p.id == v.id) {
            let raw = v.resolved.clone().unwrap_or_default();
            if !raw.is_empty() {
                password = v.compose_file_value(&raw, true);
            }
        }
        let line_value = if v.key == "REDIS_USERNAME" && !v.is_shared {
            // The variable now holds `redis_username` (see `username_updates`).
            let mut synced = v.clone();
            synced.resolved = Some(username(ctx).1);
            synced.container_value()
        } else {
            v.container_value()
        };
        env.push(format!("{}={}", v.key, line_value.unwrap_or_default()));
    }
    add_coolify_variables(&mut env, ctx);

    // add_custom_redis
    let conf = d
        .str("redis_conf")
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    if let Some(c) = &conf {
        commands.push(format!(
            "echo '{}' | base64 -d | tee {dir}/redis.conf > /dev/null",
            b64(c)
        ));
    }

    // buildStartCommand / requirePassArgument
    let keeps_unquoted = legacy_quoting
        && password.contains([
            '\\', '\'', '"', ';', '&', '|', '<', '>', ' ', '\t', '\r', '\n',
        ]);
    let password_arg = if keeps_unquoted {
        password.clone()
    } else {
        quote(&password)
    };
    let conf_path = "/usr/local/etc/redis/redis.conf";
    let command = match &conf {
        Some(c) if c.contains("requirepass") => format!("redis-server {conf_path}"),
        Some(_) => format!("redis-server {conf_path} --requirepass {password_arg}"),
        None => format!("redis-server --requirepass {password_arg} --appendonly yes"),
    };

    let mut service = PhpValue::map([
        ("image", PhpValue::from(d.str("image").unwrap_or_default())),
        ("command", PhpValue::from(command)),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([ctx.network.clone()])),
        ("labels", database_labels(d)),
        (
            "healthcheck",
            healthcheck(d, &["CMD-SHELL", "redis-cli", "ping"]),
        ),
    ]);
    for (k, v) in limits(d) {
        service.set(k, v);
    }
    // `! is_null($this->database->limits_cpuset)` (an empty string is kept).
    if let Some(Value::String(cpuset)) = d.raw.get("limits_cpuset") {
        service.set("cpuset", PhpValue::from(cpuset.as_str()));
    }
    if let Some(ports) = d.str("ports_mappings") {
        service.set(
            "ports",
            PhpValue::strings(ports.split(',').map(str::to_owned)),
        );
    }
    let mut volumes = PhpValue::list(volume_list);
    if conf.is_some() {
        volumes.push(PhpValue::map([
            ("type", PhpValue::from("bind")),
            ("source", PhpValue::from(format!("{dir}/redis.conf"))),
            ("target", PhpValue::from(conf_path)),
            ("read_only", PhpValue::Bool(true)),
        ]));
    }
    service.set("volumes", volumes);
    if d.raw.get("health_check_enabled").and_then(Value::as_bool) == Some(false) {
        service.remove("healthcheck");
    }
    let mut compose = PhpValue::map([
        ("services", PhpValue::map([(name, service)])),
        ("networks", networks(&ctx.network)),
    ]);
    if !matches!(&volume_names, PhpValue::Array(v) if v.is_empty()) {
        compose.set("volumes", volume_names);
    }
    let compose_yaml = yaml::dump(&compose, 10);
    let extra: Vec<String> = conf
        .iter()
        .map(|_| format!("chown 999:999 {dir}/redis.conf"))
        .collect();
    finish_commands(&mut commands, d, ctx.server, &compose_yaml, ctx.now, &extra);
    (commands, compose_yaml)
}
