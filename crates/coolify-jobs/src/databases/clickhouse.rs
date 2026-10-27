//! `StartClickhouse::handle`.

use coolify_compat::yaml::{self, PhpValue};
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, cpuset, database_labels, finish_commands, healthcheck, limits, networks,
    ports, volume_entries,
};
use super::env::{add_coolify_variables, has_key};

pub(super) fn start_commands(ctx: &StartContext<'_>) -> (Vec<String>, String) {
    let d = ctx.db;
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let mut commands: Vec<String> = vec![
        "echo 'Starting database.'".into(),
        format!("mkdir -p {dir}"),
    ];
    let (volume_list, volume_names) = volume_entries(&ctx.volumes);

    // generate_environment_variables
    let mut user = s("clickhouse_admin_user");
    let mut password = ctx.secret("clickhouse_admin_password");
    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        let raw = v.resolved.clone().unwrap_or_default();
        env.push(format!(
            "{}={}",
            v.key,
            v.format(Some(&raw)).unwrap_or_default()
        ));
        match v.key.as_str() {
            "CLICKHOUSE_USER" => user = v.compose_file_value(&raw, false),
            "CLICKHOUSE_PASSWORD" => password = v.compose_file_value(&raw, false),
            _ => {}
        }
    }
    for (key, value) in [
        ("CLICKHOUSE_USER", s("clickhouse_admin_user")),
        (
            "CLICKHOUSE_PASSWORD",
            ctx.secret("clickhouse_admin_password"),
        ),
        ("CLICKHOUSE_DB", s("clickhouse_db")),
    ] {
        if !has_key(&env, key) {
            env.push(format!("{key}={value}"));
        }
    }
    add_coolify_variables(&mut env, ctx);

    let nofile = PhpValue::map([
        ("soft", PhpValue::Int(262144)),
        ("hard", PhpValue::Int(262144)),
    ]);
    let test = [
        "CMD",
        "clickhouse-client",
        "--user",
        &user,
        "--password",
        &password,
        "--query",
        "SELECT 1",
    ];
    let mut service = PhpValue::map([
        ("image", PhpValue::from(s("image"))),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([ctx.network.clone()])),
        ("ulimits", PhpValue::map([("nofile", nofile)])),
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
    service.set("volumes", PhpValue::list(volume_list));
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
    finish_commands(&mut commands, d, ctx.server, &compose_yaml, ctx.now, &[]);
    (commands, compose_yaml)
}
