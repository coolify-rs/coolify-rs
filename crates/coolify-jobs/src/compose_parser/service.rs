//! Per-service helpers shared by the Compose parsers (bootstrap/helpers/docker.php and
//! shared.php): `isDatabaseImage()`, `firstDockerComposeServicePort()` and
//! `generate_log_drain_configuration()`.

use coolify_compat::yaml::{Key, PhpValue};
use serde_json::Value;

use super::php;

/// `DATABASE_DOCKER_IMAGES`.
const DATABASE_DOCKER_IMAGES: [&str; 33] = [
    "bitnami/mariadb",
    "bitnami/mongodb",
    "bitnami/redis",
    "bitnamilegacy/mariadb",
    "bitnamilegacy/mongodb",
    "bitnamilegacy/redis",
    "bitnamisecure/mariadb",
    "bitnamisecure/mongodb",
    "bitnamisecure/redis",
    "mysql",
    "bitnami/mysql",
    "bitnamilegacy/mysql",
    "bitnamisecure/mysql",
    "mysql/mysql-server",
    "mariadb",
    "postgis/postgis",
    "postgres",
    "bitnami/postgresql",
    "bitnamilegacy/postgresql",
    "bitnamisecure/postgresql",
    "supabase/postgres",
    "elestio/postgres",
    "mongo",
    "redis",
    "memcached",
    "couchdb",
    "neo4j",
    "influxdb",
    "clickhouse/clickhouse-server",
    "timescaledb/timescaledb",
    "timescaledb",
    "timescaledb-ha",
    "pgvector/pgvector",
];

/// `isDatabaseImage($image, $serviceConfig)`.
pub fn is_database_image(image: &str, service: &PhpValue) -> bool {
    let image = if image.contains(':') {
        image.to_owned()
    } else {
        format!("{image}:latest")
    };
    let image_name = php::before(&image, ":");
    let base = if image_name.contains('/') {
        php::after_last(&image_name, "/")
    } else {
        image_name.clone()
    };
    let known = DATABASE_DOCKER_IMAGES.iter().any(|d| {
        let db_base = if d.contains('/') {
            php::after_last(d, "/")
        } else {
            (*d).to_owned()
        };
        base == db_base
    });
    if !known {
        return false;
    }
    if matches!(service, PhpValue::Array(_)) {
        return is_database_image_with_context(&image_name, service);
    }
    true
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|n| !n.is_empty() && haystack.contains(n))
}

/// `isDatabaseImageWithContext($imageName, $serviceConfig)`.
fn is_database_image_with_context(image_name: &str, service: &PhpValue) -> bool {
    const APPLICATION_PATTERNS: [&str; 15] = [
        "supertokens/supertokens-mysql",
        "supertokens/supertokens-postgresql",
        "supertokens/supertokens-mongodb",
        "registry.supertokens.io/supertokens/supertokens-mysql",
        "registry.supertokens.io/supertokens/supertokens-postgresql",
        "registry.supertokens.io/supertokens/supertokens-mongodb",
        "registry.supertokens.io/supertokens",
        "metabase/metabase",
        "amancevice/superset",
        "nocodb/nocodb",
        "ghcr.io/umami-software/umami",
        "infisical/infisical",
        "postgrest/postgrest",
        "supabase/postgres-meta",
        "bluewaveuptime/uptime_redis",
    ];
    if APPLICATION_PATTERNS.iter().any(|p| image_name.contains(p)) {
        return false;
    }
    let ports = php::get(service, "ports");
    let has_db_port = match ports {
        Some(PhpValue::Array(items)) => items.iter().any(|(_, p)| {
            contains_any(
                &php::to_string(p),
                &[
                    "3306", "5432", "27017", "6379", "8086", "9200", "7687", "8123",
                ],
            )
        }),
        _ => false,
    };
    let (mut db_env, mut app_env) = (false, false);
    if let Some(PhpValue::Array(items)) = php::get(service, "environment") {
        for (_, env) in items {
            let upper = php::to_string(env).to_ascii_uppercase();
            if contains_any(
                &upper,
                &[
                    "MYSQL_ROOT_PASSWORD",
                    "POSTGRES_PASSWORD",
                    "MONGO_INITDB_ROOT_PASSWORD",
                    "REDIS_PASSWORD",
                ],
            ) {
                db_env = true;
            }
            if contains_any(
                &upper,
                &["SERVICE_FQDN", "API_KEYS", "APP_", "APPLICATION_"],
            ) {
                app_env = true;
            }
        }
    }
    let healthcheck = match php::data_get(service, "healthcheck.test") {
        Some(PhpValue::Array(items)) => items
            .iter()
            .map(|(_, v)| php::to_string(v))
            .collect::<Vec<_>>()
            .join(" "),
        Some(PhpValue::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let (mut db_hc, mut app_hc) = (false, false);
    if php::truthy(&PhpValue::Str(healthcheck.clone())) {
        let upper = healthcheck.to_ascii_uppercase();
        db_hc = contains_any(
            &upper,
            &["PG_ISREADY", "MYSQLADMIN PING", "MONGO", "REDIS-CLI PING"],
        );
        app_hc = contains_any(
            &upper,
            &[
                "CURL", "WGET", "HTTP://", "HTTPS://", "/HEALTH", "/API/", "/HELLO",
            ],
        );
    }
    let depends_on_db = match php::get(service, "depends_on") {
        Some(PhpValue::Array(items)) => items.iter().any(|(k, _)| {
            contains_any(
                &k.as_string(),
                &["mysql", "postgres", "mongo", "redis", "mariadb"],
            )
        }),
        _ => false,
    };
    if app_env && depends_on_db {
        return false;
    }
    if app_hc {
        return false;
    }
    if has_db_port && db_hc {
        return true;
    }
    if db_env {
        return true;
    }
    !depends_on_db
}

/// `firstDockerComposeServicePort($service)`.
pub fn first_docker_compose_service_port(service: &PhpValue) -> Option<i64> {
    let definitions = php::merge(
        &php::collect(php::get(service, "expose")),
        &php::collect(php::get(service, "ports")),
    );
    for (_, def) in definitions {
        let (protocol, port) = match &def {
            PhpValue::Array(_) => (
                match php::get(&def, "protocol") {
                    None => PhpValue::Str("tcp".into()),
                    Some(p) => p.clone(),
                },
                php::get(&def, "target").cloned().unwrap_or(PhpValue::Null),
            ),
            other => {
                let text = php::to_string(other);
                let protocol = if text.contains('/') {
                    php::after_last(&text, "/")
                } else {
                    "tcp".into()
                };
                let port = php::after_last(&php::before(&text, "/"), ":");
                (PhpValue::Str(protocol), PhpValue::Str(port))
            }
        };
        if protocol != PhpValue::Str("tcp".into()) {
            continue;
        }
        if php::is_numeric_value(&port) {
            let n = match &port {
                PhpValue::Int(i) => *i,
                PhpValue::Float(f) => *f as i64,
                other => php::to_string(other)
                    .trim()
                    .parse::<f64>()
                    .map_or(0, |f| f as i64),
            };
            if (1..=65535).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

fn setting_flag(settings: &Value, key: &str) -> bool {
    settings.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `Server::isFluentBitLogDrainEnabled()`.
pub fn is_fluent_bit_log_drain_enabled(settings: &Value) -> bool {
    [
        "is_logdrain_newrelic_enabled",
        "is_logdrain_highlight_enabled",
        "is_logdrain_axiom_enabled",
        "is_logdrain_custom_enabled",
    ]
    .iter()
    .any(|k| setting_flag(settings, k))
}

/// `Server::isLogDrainEnabled()`.
pub fn is_log_drain_enabled(settings: &Value) -> bool {
    is_fluent_bit_log_drain_enabled(settings)
        || setting_flag(settings, "is_logdrain_cloudwatch_enabled")
}

/// `generate_log_drain_configuration($server)`.
pub fn generate_log_drain_configuration(settings: &Value) -> PhpValue {
    if is_fluent_bit_log_drain_enabled(settings) {
        return PhpValue::map([
            ("driver", PhpValue::from("fluentd")),
            (
                "options",
                PhpValue::map([
                    ("fluentd-address", PhpValue::from("tcp://127.0.0.1:24224")),
                    ("fluentd-async", PhpValue::from("true")),
                    ("fluentd-sub-second-precision", PhpValue::from("true")),
                    (
                        "env",
                        PhpValue::from(
                            "COOLIFY_APP_NAME,COOLIFY_PROJECT_NAME,COOLIFY_SERVER_IP,COOLIFY_ENVIRONMENT_NAME",
                        ),
                    ),
                ]),
            ),
        ]);
    }
    let s = |k: &str| settings.get(k).and_then(Value::as_str).map(str::to_owned);
    let options: Vec<(Key, PhpValue)> = [
        ("awslogs-region", s("logdrain_cloudwatch_region")),
        ("awslogs-group", s("logdrain_cloudwatch_group")),
        ("awslogs-create-group", Some("true".into())),
        ("mode", Some("non-blocking".into())),
        (
            "tag",
            Some(format!(
                "{}{{{{.Name}}}}",
                s("logdrain_cloudwatch_stream_prefix").unwrap_or_default()
            )),
        ),
    ]
    .into_iter()
    .filter_map(|(k, v)| {
        v.filter(|v| !v.trim().is_empty())
            .map(|v| (Key::Str(k.into()), PhpValue::Str(v)))
    })
    .collect();
    PhpValue::map([
        ("driver", PhpValue::from("awslogs")),
        ("options", PhpValue::Array(options)),
    ])
}
