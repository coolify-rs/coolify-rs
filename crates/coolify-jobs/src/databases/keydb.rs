//! `StartKeydb::handle` and `StartDragonfly::handle`: the same shape, with
//! their own start command, healthcheck and (KeyDB) configuration file.

use coolify_compat::yaml::{self, PhpValue};
use coolify_db::databases::Kind;
use coolify_remote::shell::quote;
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, b64, cpuset, database_labels, finish_commands, healthcheck, limits,
    networks, php_filled, ports, volume_entries,
};
use super::env::{add_coolify_variables, has_key};

pub(super) fn start_commands(ctx: &StartContext<'_>) -> (Vec<String>, String) {
    let d = ctx.db;
    let keydb = d.kind == Kind::Keydb;
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
    let model_password = ctx.secret(if keydb {
        "keydb_password"
    } else {
        "dragonfly_password"
    });
    let mut password = model_password.clone();
    let legacy_quoting = d.bool("legacy_password_quoting");
    // serverPasswordEnvironmentVariable(): legacy databases keep the stored password.
    let password_var = ctx
        .variables
        .iter()
        .find(|v| v.key == "REDIS_PASSWORD")
        .filter(|_| !legacy_quoting);
    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        let raw = v.resolved.clone().unwrap_or_default();
        env.push(format!(
            "{}={}",
            v.key,
            v.format(Some(&raw)).unwrap_or_default()
        ));
        if password_var.is_some_and(|p| p.id == v.id) && !raw.is_empty() {
            password = v.compose_file_value(&raw, true);
        }
    }
    if !has_key(&env, "REDIS_PASSWORD") {
        env.push(format!("REDIS_PASSWORD={model_password}"));
    }
    add_coolify_variables(&mut env, ctx);

    // add_custom_keydb
    let conf = php_filled(d.str("keydb_conf"))
        .filter(|_| keydb)
        .map(str::to_owned);
    if let Some(c) = &conf {
        commands.push(format!(
            "echo '{}' | base64 -d | tee {dir}/keydb.conf > /dev/null",
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
    let conf_path = "/etc/keydb/keydb.conf";
    let command = match (&conf, keydb) {
        (_, false) => format!("dragonfly --requirepass {password_arg}"),
        (Some(c), true) if c.contains("requirepass") => format!("keydb-server {conf_path}"),
        (Some(_), true) => format!("keydb-server {conf_path} --requirepass {password_arg}"),
        (None, true) => format!("keydb-server --requirepass {password_arg} --appendonly yes"),
    };
    let test: Vec<&str> = if keydb {
        vec!["CMD", "keydb-cli", "--pass", &password, "ping"]
    } else {
        vec!["CMD", "redis-cli", "-a", &password, "ping"]
    };

    let mut service = PhpValue::map([
        ("image", PhpValue::from(d.str("image").unwrap_or_default())),
        ("command", PhpValue::from(command)),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([ctx.network.clone()])),
        ("labels", database_labels(d)),
        ("healthcheck", healthcheck(d, &test)),
    ]);
    for (k, v) in limits(d) {
        service.set(k, v);
    }
    if let Some(c) = cpuset(d) {
        service.set("cpuset", c);
    }
    if let Some(p) = ports(d) {
        service.set("ports", p);
    }
    let mut volumes = PhpValue::list(volume_list);
    if conf.is_some() {
        volumes.push(PhpValue::map([
            ("type", PhpValue::from("bind")),
            ("source", PhpValue::from(format!("{dir}/keydb.conf"))),
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
        .map(|_| format!("chown 999:999 {dir}/keydb.conf"))
        .collect();
    finish_commands(&mut commands, d, ctx.server, &compose_yaml, ctx.now, &extra);
    (commands, compose_yaml)
}
