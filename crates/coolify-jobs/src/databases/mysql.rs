//! `StartMysql::handle` and `StartMariadb::handle`: the same shape with their
//! own variable names, healthcheck and configuration column.

use coolify_compat::yaml::{self, PhpValue};
use coolify_db::databases::Kind;
use serde_json::Value;

use super::StartContext;
use super::common::{
    CONFIGURATION_DIR, b64, ca_mount, cpuset, database_labels, finish_commands, healthcheck,
    limits, networks, php_filled, ports, volume_entries,
};
use super::env::{add_coolify_variables, has_key};

pub(super) fn start_commands(ctx: &StartContext<'_>) -> (Vec<String>, String) {
    let d = ctx.db;
    let mysql = d.kind == Kind::Mysql;
    // Column and variable prefixes: `mysql_root_password` / `MYSQL_ROOT_PASSWORD`, ...
    let (col, var) = if mysql {
        ("mysql", "MYSQL")
    } else {
        ("mariadb", "MARIADB")
    };
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let mut commands: Vec<String> = vec![
        "echo 'Starting database.'".into(),
        "echo 'Creating directories.'".into(),
        format!("mkdir -p {dir}"),
        "echo 'Directories created successfully.'".into(),
    ];
    commands.extend(ctx.ssl_commands.iter().cloned());
    commands.extend(ctx.ensure_commands.iter().cloned());
    let (mut volume_list, volume_names) = volume_entries(&ctx.volumes);
    volume_list.extend(ctx.file_mounts.iter().map(|m| PhpValue::from(m.as_str())));

    // generate_environment_variables
    let root_password = ctx.secret(&format!("{col}_root_password"));
    let mut health_password = root_password.clone();
    let mut env: Vec<String> = Vec::new();
    for v in &ctx.variables {
        if mysql {
            let raw = v.resolved.clone().unwrap_or_default();
            env.push(format!(
                "{}={}",
                v.key,
                v.format(Some(&raw)).unwrap_or_default()
            ));
            if v.key == "MYSQL_ROOT_PASSWORD" {
                health_password = v.compose_file_value(&raw, false);
            }
        } else {
            env.push(format!(
                "{}={}",
                v.key,
                v.container_value().unwrap_or_default()
            ));
        }
    }
    for (key, value) in [
        (format!("{var}_ROOT_PASSWORD"), root_password),
        (format!("{var}_DATABASE"), s(&format!("{col}_database"))),
        (format!("{var}_USER"), s(&format!("{col}_user"))),
        (
            format!("{var}_PASSWORD"),
            ctx.secret(&format!("{col}_password")),
        ),
    ] {
        if !has_key(&env, &key) {
            env.push(format!("{key}={value}"));
        }
    }
    add_coolify_variables(&mut env, ctx);

    // add_custom_mysql / add_custom_mariadb
    let conf = php_filled(d.str(&format!("{col}_conf"))).map(str::to_owned);
    if let Some(c) = &conf {
        commands.push(format!(
            "echo '{}' | base64 -d | tee {dir}/custom-config.cnf > /dev/null",
            b64(c)
        ));
        commands.push(format!("chmod 644 {dir}/custom-config.cnf"));
    }

    let root_flag = format!("-p{health_password}");
    let test: Vec<&str> = if mysql {
        vec![
            "CMD",
            "mysqladmin",
            "ping",
            "-h",
            "localhost",
            "-u",
            "root",
            &root_flag,
        ]
    } else {
        vec!["CMD", "healthcheck.sh", "--connect", "--innodb_initialized"]
    };
    let mut service = PhpValue::map([
        ("image", PhpValue::from(s("image"))),
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
    ctx.apply_logging(&mut service);
    let mut volumes = PhpValue::list(volume_list);
    if d.bool("enable_ssl") {
        volumes.push(ca_mount("/etc/mysql/certs/coolify-ca.crt"));
    }
    if conf.is_some() {
        volumes.push(PhpValue::map([
            ("type", PhpValue::from("bind")),
            ("source", PhpValue::from(format!("{dir}/custom-config.cnf"))),
            (
                "target",
                PhpValue::from("/etc/mysql/conf.d/custom-config.cnf"),
            ),
            ("read_only", PhpValue::Bool(true)),
        ]));
    }
    service.set("volumes", volumes);
    ctx.apply_docker_options(&mut service);
    if d.bool("enable_ssl") {
        service.set(
            "command",
            PhpValue::strings([
                if mysql { "mysqld" } else { "mariadbd" },
                "--ssl-cert=/etc/mysql/certs/server.crt",
                "--ssl-key=/etc/mysql/certs/server.key",
                "--ssl-ca=/etc/mysql/certs/coolify-ca.crt",
                "--require-secure-transport=1",
            ]),
        );
    }
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
    let extra: Vec<String> = if d.bool("enable_ssl") {
        vec![format!(
            "docker compose -f {dir}/docker-compose.yml run --rm --no-deps --user root --entrypoint chown {name} mysql:mysql /etc/mysql/certs/server.key /etc/mysql/certs/server.crt < /dev/null"
        )]
    } else {
        Vec::new()
    };
    finish_commands(&mut commands, d, ctx.server, &compose_yaml, ctx.now, &extra);
    (commands, compose_yaml)
}
