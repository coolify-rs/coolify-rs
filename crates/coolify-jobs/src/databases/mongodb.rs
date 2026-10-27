//! `StartMongodb::handle`.

use coolify_compat::yaml::{self, PhpValue};
use coolify_remote::shell::quote;
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, b64, cpuset, database_labels, finish_commands, healthcheck, limits,
    networks, php_filled, ports, volume_entries,
};
use super::env::{add_coolify_variables, has_key};

/// `json_encode($v, JSON_UNESCAPED_SLASHES)` of a string.
fn json_string(v: &str) -> String {
    coolify_compat::php_json::encode(&Value::String(v.to_owned())).replace("\\/", "/")
}

/// `health_check_shell_command`: the shell's ping, each word `escapeshellarg`ed.
fn health_shell(shell: &str, uuid: &str) -> String {
    [
        shell,
        "--quiet",
        "--host",
        uuid,
        "--eval",
        "quit(db.adminCommand({ ping: 1 }).ok === 1 ? 0 : 1)",
    ]
    .iter()
    .map(|w| quote(w))
    .collect::<Vec<_>>()
    .join(" ")
}

pub(super) fn start_commands(ctx: &StartContext<'_>) -> (Vec<String>, String) {
    let d = ctx.db;
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
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
    let mut user = s("mongo_initdb_root_username");
    let mut password = ctx.secret("mongo_initdb_root_password");
    let mut database = s("mongo_initdb_database");
    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        let raw = v.resolved.clone().unwrap_or_default();
        env.push(format!(
            "{}={}",
            v.key,
            v.format(Some(&raw)).unwrap_or_default()
        ));
        match v.key.as_str() {
            "MONGO_INITDB_ROOT_USERNAME" => user = raw,
            "MONGO_INITDB_ROOT_PASSWORD" => password = raw,
            "MONGO_INITDB_DATABASE" => database = raw,
            _ => {}
        }
    }
    for (key, value) in [
        (
            "MONGO_INITDB_ROOT_USERNAME",
            s("mongo_initdb_root_username"),
        ),
        (
            "MONGO_INITDB_ROOT_PASSWORD",
            ctx.secret("mongo_initdb_root_password"),
        ),
        ("MONGO_INITDB_DATABASE", s("mongo_initdb_database")),
    ] {
        if !has_key(&env, key) {
            env.push(format!("{key}={value}"));
        }
    }
    add_coolify_variables(&mut env, ctx);

    // add_custom_mongo_conf
    let conf = php_filled(d.str("mongo_conf")).map(str::to_owned);
    if let Some(c) = &conf {
        commands.push(format!(
            "echo '{}' | base64 -d | tee {dir}/mongod.conf > /dev/null",
            b64(c)
        ));
        commands.push(format!("chmod 644 {dir}/mongod.conf"));
    }

    let test = format!(
        "if command -v mongosh >/dev/null 2>&1; then {}; else {}; fi",
        health_shell("mongosh", name),
        health_shell("mongo", name)
    );
    let mut service = PhpValue::map([
        ("image", PhpValue::from(s("image"))),
        ("command", PhpValue::from("mongod")),
        ("container_name", PhpValue::from(name)),
        ("environment", PhpValue::strings(env)),
        ("restart", PhpValue::from("unless-stopped")),
        ("networks", PhpValue::strings([ctx.network.clone()])),
        ("labels", database_labels(d)),
        ("healthcheck", healthcheck(d, &["CMD-SHELL", &test])),
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
            ("source", PhpValue::from(format!("{dir}/mongod.conf"))),
            ("target", PhpValue::from("/etc/mongo/mongod.conf")),
            ("read_only", PhpValue::Bool(true)),
        ]));
        service.set(
            "command",
            PhpValue::strings(["mongod", "--config", "/etc/mongo/mongod.conf"]),
        );
    }

    // add_default_database
    let (db_json, user_json, pwd_json) = (
        json_string(&database),
        json_string(&user),
        json_string(&password),
    );
    let content = format!(
        "db = db.getSiblingDB({db_json});db.createCollection('init_collection');db.createUser({{user: {user_json}, pwd: {pwd_json}, roles: [{{role:\"readWrite\",db:{db_json}}}]}});"
    );
    commands.push(format!("mkdir -p {dir}/docker-entrypoint-initdb.d"));
    commands.push(format!(
        "echo '{}' | base64 -d | tee {dir}/docker-entrypoint-initdb.d/01-default-database.js > /dev/null",
        b64(&content)
    ));
    volumes.push(PhpValue::map([
        ("type", PhpValue::from("bind")),
        (
            "source",
            PhpValue::from(format!("{dir}/docker-entrypoint-initdb.d")),
        ),
        ("target", PhpValue::from("/docker-entrypoint-initdb.d")),
        ("read_only", PhpValue::Bool(true)),
    ]));
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
    finish_commands(&mut commands, d, ctx.server, &compose_yaml, ctx.now, &[]);
    (commands, compose_yaml)
}
