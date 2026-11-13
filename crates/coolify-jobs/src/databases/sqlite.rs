//! `StartSqlite::handle`.

use coolify_compat::yaml::{self, PhpValue};
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, cpuset, database_labels, finish_commands, healthcheck, limits, networks,
    volume_entries,
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
    commands.extend(ctx.ensure_commands.iter().cloned());
    let (mut volume_list, volume_names) = volume_entries(&ctx.volumes);
    volume_list.extend(ctx.file_mounts.iter().map(|m| PhpValue::from(m.as_str())));

    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        let raw = v.resolved.clone().unwrap_or_default();
        env.push(format!(
            "{}={}",
            v.key,
            v.format(Some(&raw)).unwrap_or_default()
        ));
    }
    if !has_key(&env, "SQLITE_DATABASES") {
        env.push(format!("SQLITE_DATABASES={}", s("sqlite_databases")));
    }
    add_coolify_variables(&mut env, ctx);

    let mut service = PhpValue::map([
        ("image", PhpValue::from(s("image"))),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([ctx.network.clone()])),
        ("labels", database_labels(d)),
        ("healthcheck", healthcheck(d, &["CMD", "healthcheck.sh"])),
    ]);
    for (k, v) in limits(d) {
        service.set(k, v);
    }
    if let Some(c) = cpuset(d) {
        service.set("cpuset", c);
    }
    ctx.apply_logging(&mut service);
    if !volume_list.is_empty() {
        service.set("volumes", PhpValue::list(volume_list));
    }
    ctx.apply_docker_options(&mut service);
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
    finish_commands(
        &mut commands,
        d,
        ctx.server,
        &compose_yaml,
        ctx.now,
        &[],
        &ctx.start_pins,
    );
    (commands, compose_yaml)
}
