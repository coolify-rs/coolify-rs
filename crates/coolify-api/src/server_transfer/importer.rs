//! `ServerTransferImporter`: writes a bundle into this instance.
//!
//! Rows go in through `jsonb_populate_record`, so a value keeps the type of its column. Model
//! hooks that matter are written out where upstream's models run them.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use chrono::{NaiveDateTime, Utc};
use coolify_compat::Encrypter;
use coolify_domain::new_public_id;
use regex::Regex;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, Row};

use super::bundle;
use super::{TransferError, as_int, blank, dg, instance_url, now_iso, text, truthy};
use crate::state::AppState;

type R<T> = Result<T, TransferError>;

const GITHUB_MORPH: &str = "App\\Models\\GithubApp";
const GITLAB_MORPH: &str = "App\\Models\\GitlabApp";
const GITEA_MORPH: &str = "App\\Models\\GiteaApp";
const APPLICATION_MORPH: &str = "App\\Models\\Application";
const PREVIEW_MORPH: &str = "App\\Models\\ApplicationPreview";
const SERVICE_MORPH: &str = "App\\Models\\Service";
const SERVICE_APP_MORPH: &str = "App\\Models\\ServiceApplication";
const SERVICE_DB_MORPH: &str = "App\\Models\\ServiceDatabase";
const SERVER_MORPH: &str = "App\\Models\\Server";
const VOLUME_MORPH: &str = "App\\Models\\LocalPersistentVolume";
const FILE_MORPH: &str = "App\\Models\\LocalFileVolume";
const STANDALONE_DOCKER_MORPH: &str = "App\\Models\\StandaloneDocker";
const SWARM_DOCKER_MORPH: &str = "App\\Models\\SwarmDocker";

/// The models that use `HasSafeStringAttribute`.
const SAFE_STRING_TABLES: &[&str] = &[
    "applications",
    "environments",
    "projects",
    "s3_storages",
    "scheduled_tasks",
    "private_keys",
    "servers",
    "services",
    "standalone_clickhouses",
    "standalone_dragonflies",
    "standalone_mariadbs",
    "standalone_keydbs",
    "standalone_dockers",
    "standalone_mysqls",
    "standalone_redis",
    "standalone_postgresqls",
    "standalone_mongodbs",
    "tags",
    "standalone_sqlites",
];

/// `Application::$parserVersion` and `Service::$parserVersion`.
const PARSER_VERSION: &str = "5";

/// `DATABASE_MODELS`: type, table, encrypted columns.
const DATABASE_MODELS: &[(&str, &str, &[&str])] = &[
    (
        "StandalonePostgresql",
        "standalone_postgresqls",
        &["postgres_password"],
    ),
    (
        "StandaloneMysql",
        "standalone_mysqls",
        &["mysql_password", "mysql_root_password"],
    ),
    (
        "StandaloneMariadb",
        "standalone_mariadbs",
        &["mariadb_password", "mariadb_root_password"],
    ),
    (
        "StandaloneMongodb",
        "standalone_mongodbs",
        &["mongo_initdb_root_password"],
    ),
    ("StandaloneRedis", "standalone_redis", &[]),
    ("StandaloneKeydb", "standalone_keydbs", &["keydb_password"]),
    (
        "StandaloneDragonfly",
        "standalone_dragonflies",
        &["dragonfly_password"],
    ),
    (
        "StandaloneClickhouse",
        "standalone_clickhouses",
        &["clickhouse_admin_password"],
    ),
    ("StandaloneSqlite", "standalone_sqlites", &[]),
];

/// `Application::$fillable`.
pub const APPLICATION_FILLABLE: &[&str] = &[
    "name",
    "description",
    "fqdn",
    "noindex_domains",
    "domain_port_overrides",
    "git_repository",
    "git_branch",
    "git_commit_sha",
    "git_full_url",
    "docker_registry_image_name",
    "docker_registry_image_tag",
    "build_pack",
    "static_image",
    "install_command",
    "build_command",
    "start_command",
    "ports_exposes",
    "ports_mappings",
    "base_directory",
    "publish_directory",
    "health_check_enabled",
    "health_check_path",
    "health_check_port",
    "health_check_host",
    "health_check_method",
    "health_check_return_code",
    "health_check_scheme",
    "health_check_response_text",
    "health_check_interval",
    "health_check_timeout",
    "health_check_retries",
    "health_check_start_period",
    "health_check_type",
    "health_check_command",
    "limits_memory",
    "limits_memory_swap",
    "limits_memory_swappiness",
    "limits_memory_reservation",
    "limits_cpus",
    "limits_cpuset",
    "limits_cpu_shares",
    "status",
    "preview_url_template",
    "dockerfile",
    "dockerfile_location",
    "dockerfile_target_build",
    "custom_labels",
    "custom_docker_run_options",
    "post_deployment_command",
    "post_deployment_command_container",
    "pre_deployment_command",
    "pre_deployment_command_container",
    "manual_webhook_secret_github",
    "manual_webhook_secret_gitlab",
    "manual_webhook_secret_bitbucket",
    "manual_webhook_secret_gitea",
    "docker_compose_location",
    "docker_compose",
    "docker_compose_raw",
    "docker_compose_domains",
    "domain_dns_statuses",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "swarm_replicas",
    "swarm_placement_constraints",
    "watch_paths",
    "redirect",
    "compose_parsing_version",
    "custom_nginx_configuration",
    "custom_network_aliases",
    "custom_healthcheck_found",
    "nixpkgsarchive",
    "is_http_basic_auth_enabled",
    "http_basic_auth_username",
    "http_basic_auth_password",
    "http_basic_auth_hash_algorithm",
    "http_basic_auth_bcrypt_cost",
    "http_basic_auth_argon2id_memory_cost",
    "http_basic_auth_argon2id_time_cost",
    "connect_to_docker_network",
    "force_domain_override",
    "is_container_label_escape_enabled",
    "use_build_server",
    "config_hash",
    "last_online_at",
    "restart_count",
    "max_restart_count",
    "restart_limit_reached",
    "container_present",
    "last_restart_at",
    "last_restart_type",
    "uuid",
    "environment_id",
    "destination_id",
    "destination_type",
    "source_id",
    "source_type",
    "repository_project_id",
    "private_key_id",
];

/// `ApplicationSetting::$fillable`.
pub const APPLICATION_SETTING_FILLABLE: &[&str] = &[
    "is_static",
    "is_git_submodules_enabled",
    "is_git_lfs_enabled",
    "is_auto_deploy_enabled",
    "is_force_https_enabled",
    "is_debug_enabled",
    "is_preview_deployments_enabled",
    "is_log_drain_enabled",
    "is_gpu_enabled",
    "gpu_driver",
    "gpu_count",
    "gpu_device_ids",
    "gpu_options",
    "is_include_timestamps",
    "is_swarm_only_worker_nodes",
    "is_raw_compose_deployment_enabled",
    "is_build_server_enabled",
    "is_consistent_container_name_enabled",
    "is_gzip_enabled",
    "traefik_certificate_resolver",
    "is_stripprefix_enabled",
    "is_permanent_redirect_enabled",
    "connect_to_docker_network",
    "custom_internal_name",
    "custom_container_name_prefix",
    "is_container_label_escape_enabled",
    "is_env_sorting_enabled",
    "include_coolify_default_variables",
    "is_container_label_readonly_enabled",
    "is_preserve_repository_enabled",
    "disable_build_cache",
    "is_spa",
    "is_git_shallow_clone_enabled",
    "is_pr_deployments_public_enabled",
    "use_build_secrets",
    "redact_all_env_values_in_logs",
    "inject_build_args_to_dockerfile",
    "include_source_commit_in_build",
    "docker_images_to_keep",
    "stop_grace_period",
    "rolling_update_drain_seconds",
];

/// `Service::$fillable`.
pub const SERVICE_FILLABLE: &[&str] = &[
    "uuid",
    "name",
    "description",
    "docker_compose_raw",
    "docker_compose",
    "connect_to_docker_network",
    "service_type",
    "config_hash",
    "compose_parsing_version",
    "is_container_label_escape_enabled",
    "redact_all_env_values_in_logs",
    "environment_id",
    "server_id",
    "destination_id",
    "destination_type",
];

/// `ServerSetting::$fillable`, without the columns the import never takes.
const SERVER_SETTING_FILLABLE: &[&str] = &[
    "is_swarm_manager",
    "is_jump_server",
    "is_build_server",
    "server_role",
    "wildcard_domain",
    "is_cloudflare_tunnel",
    "cloudflare_tunnel_id",
    "cloudflare_tunnel_account_id",
    "is_logdrain_newrelic_enabled",
    "logdrain_newrelic_license_key",
    "logdrain_newrelic_base_uri",
    "is_logdrain_highlight_enabled",
    "logdrain_highlight_project_id",
    "is_logdrain_axiom_enabled",
    "logdrain_axiom_dataset_name",
    "logdrain_axiom_api_key",
    "is_swarm_worker",
    "is_logdrain_custom_enabled",
    "logdrain_custom_config",
    "logdrain_custom_config_parser",
    "is_logdrain_cloudwatch_enabled",
    "logdrain_cloudwatch_region",
    "logdrain_cloudwatch_group",
    "logdrain_cloudwatch_stream_prefix",
    "concurrent_builds",
    "dynamic_timeout",
    "is_metrics_enabled",
    "generate_exact_labels",
    "force_docker_cleanup",
    "docker_cleanup_frequency",
    "docker_cleanup_threshold",
    "server_timezone",
    "delete_unused_volumes",
    "delete_unused_networks",
    "is_sentinel_enabled",
    "sentinel_metrics_refresh_rate_seconds",
    "sentinel_metrics_history_days",
    "sentinel_push_interval_seconds",
    "server_disk_usage_notification_threshold",
    "is_sentinel_debug_enabled",
    "server_disk_usage_check_frequency",
    "server_disk_usage_notification_interval_hours",
    "is_terminal_enabled",
    "deployment_queue_limit",
    "backup_compression_cpu_percentage",
    "disable_application_image_retention",
    "connection_timeout",
    "is_traffic_analytics_enabled",
    "traffic_topn",
    "traffic_sample_threshold",
    "traffic_retention_1h_days",
    "traffic_retention_1d_days",
    "is_geoip_enabled",
    "geoip_refresh_days",
    "geoip_maxmind_license_key",
    "traffic_ip_mode",
    "traefik_certificate_resolver",
    "docker_version",
    "docker_version_checked_at",
    "compose_version",
    "compose_version_checked_at",
    "patch_check_frequency",
    "is_auto_security_updates_enabled",
    "auto_security_updates_time",
];

/// An imported row, for the maps that later parts of the import read.
#[derive(Clone, Debug)]
struct Res {
    id: i64,
    uuid: String,
    morph: &'static str,
    name: String,
    environment_id: i64,
}

#[derive(Default)]
struct Maps {
    private_keys: HashMap<String, i64>,
    github: HashMap<String, i64>,
    gitlab: HashMap<String, i64>,
    gitea: HashMap<String, i64>,
    s3: HashMap<String, i64>,
    cloud_tokens: HashMap<String, i64>,
    integration_tokens: HashMap<String, i64>,
    dns_zones: HashMap<String, (i64, i64)>,
    volumes: HashMap<String, (i64, &'static str, String)>,
    applications: HashMap<String, Res>,
    previews: HashMap<String, Res>,
    services: HashMap<String, Res>,
    service_applications: HashMap<String, Res>,
    databases: HashMap<String, Res>,
    pending_sqlite: Vec<(i64, String, String)>,
    pending_dns: Vec<(i64, String, String)>,
    warnings: Vec<String>,
    git_source_applications: Vec<String>,
    columns: HashMap<String, HashSet<String>>,
}

struct Import<'a> {
    enc: &'a Encrypter,
    team_id: i64,
    preserve: bool,
    adopt: bool,
    now: NaiveDateTime,
    m: Maps,
}

fn str_of(v: &Value, path: &str) -> String {
    dg(v, path).map(text).unwrap_or_default()
}

fn list<'v>(v: &'v Value, path: &str) -> Vec<&'v Value> {
    match dg(v, path) {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    }
}

fn count(v: &Value, path: &str) -> i64 {
    list(v, path).len() as i64
}

/// `$a ?: $b`
fn or(v: Option<&Value>, fallback: &str) -> Value {
    match v {
        Some(x) if crate::serialize::php_truthy(x) => x.clone(),
        _ => json!(fallback),
    }
}

fn get_or_null(v: &Value, key: &str) -> Value {
    dg(v, key).cloned().unwrap_or(Value::Null)
}

fn bool_of(v: &Value, key: &str, default: bool) -> Value {
    json!(dg(v, key).map_or(default, crate::serialize::php_truthy))
}

/// `(int)` with a default.
fn int_or(v: &Value, key: &str, default: i64) -> Value {
    json!(dg(v, key).map_or(default, as_int))
}

async fn exists(conn: &mut PgConnection, table: &str, uuid: &str) -> R<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE uuid = $1)"
    ))
    .bind(uuid)
    .fetch_one(&mut *conn)
    .await?)
}

/// `$uuid` when it is free, or a new one.
async fn free_uuid(conn: &mut PgConnection, table: &str, uuid: String) -> R<String> {
    Ok(if exists(conn, table, &uuid).await? {
        new_public_id()
    } else {
        uuid
    })
}

/// `filled(uuid) ? uuid : new_public_id()`.
fn uuid_or_new(payload: &Value) -> String {
    if blank(dg(payload, "uuid")) {
        new_public_id()
    } else {
        str_of(payload, "uuid")
    }
}

impl Import<'_> {
    fn preserved_uuid(&self, payload: &Value) -> String {
        if self.preserve {
            uuid_or_new(payload)
        } else {
            new_public_id()
        }
    }

    /// `encrypted` cast.
    fn encrypt(&self, v: Option<&Value>) -> Value {
        match v {
            None | Some(Value::Null) => Value::Null,
            Some(x) => json!(self.enc.encrypt_string(&text(x))),
        }
    }

    /// `Schema::getColumnListing`.
    async fn columns(&mut self, conn: &mut PgConnection, table: &str) -> R<HashSet<String>> {
        if let Some(c) = self.m.columns.get(table) {
            return Ok(c.clone());
        }
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        let set: HashSet<String> = cols.into_iter().collect();
        self.m.columns.insert(table.to_owned(), set.clone());
        Ok(set)
    }

    /// `$model->save()` for a new row. Timestamps are added when the table has them.
    async fn insert(
        &mut self,
        conn: &mut PgConnection,
        table: &str,
        mut attrs: Map<String, Value>,
    ) -> R<i64> {
        let columns = self.columns(conn, table).await?;
        let now = self.now.format("%Y-%m-%dT%H:%M:%S").to_string();
        for stamp in ["created_at", "updated_at"] {
            if columns.contains(stamp) {
                attrs.insert(stamp.to_owned(), json!(now));
            }
        }
        attrs.retain(|k, _| columns.contains(k));
        // `HasSafeStringAttribute`: `strip_tags()`, also of null.
        if SAFE_STRING_TABLES.contains(&table) {
            for field in ["name", "description"] {
                if let Some(v) = attrs.get(field) {
                    let stripped = match v {
                        Value::Null => String::new(),
                        other => coolify_domain::naming::strip_tags(Some(&text(other))),
                    };
                    attrs.insert(field.to_owned(), json!(stripped));
                }
            }
        }
        let names = attrs
            .keys()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {table} ({names}) SELECT {names} FROM jsonb_populate_record(NULL::{table}, $1::text::jsonb) RETURNING id"
        );
        Ok(sqlx::query_scalar::<_, i64>(&sql)
            .bind(Value::Object(attrs).to_string())
            .fetch_one(&mut *conn)
            .await?)
    }

    /// `$model->update($attributes)` by id.
    async fn update(
        &mut self,
        conn: &mut PgConnection,
        table: &str,
        id_column: &str,
        id: i64,
        attrs: Map<String, Value>,
    ) -> R<()> {
        if attrs.is_empty() {
            return Ok(());
        }
        let names = attrs
            .keys()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE {table} SET ({names}) = (SELECT {names} FROM jsonb_populate_record(NULL::{table}, $2::text::jsonb)), updated_at = $3 WHERE {id_column} = $1"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(Value::Object(attrs).to_string())
            .bind(self.now)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// `importPrivateKey`. Returns the id and uuid.
    async fn private_key(&mut self, conn: &mut PgConnection, payload: &Value) -> R<(i64, String)> {
        let material = str_of(payload, "private_key");
        if material.is_empty() {
            return Err(TransferError::Message(
                "Private key material is required.".into(),
            ));
        }
        let info = coolify_compat::sshkey::inspect(&material);
        let fingerprint = info
            .as_ref()
            .map(|i| i.fingerprint.clone())
            .or_else(|| dg(payload, "fingerprint").map(text))
            .filter(|f| !f.is_empty());
        if let Some(fp) = &fingerprint {
            let row = sqlx::query(
                "SELECT id, uuid FROM private_keys WHERE team_id = $1 AND fingerprint = $2 LIMIT 1",
            )
            .bind(self.team_id)
            .bind(fp)
            .fetch_optional(&mut *conn)
            .await?;
            if let Some(r) = row {
                return Ok((r.try_get("id")?, r.try_get("uuid")?));
            }
        }
        let uuid = free_uuid(conn, "private_keys", self.preserved_uuid(payload)).await?;
        let Some(info) = info else {
            return Err(TransferError::Message("Invalid private key".into()));
        };
        let formatted = coolify_compat::sshkey::format_private_key(&material);
        let attrs = obj(json!({
            "uuid": uuid,
            "name": or(dg(payload, "name"), "Imported transfer key"),
            "description": get_or_null(payload, "description"),
            "private_key": self.enc.encrypt_string(&formatted),
            "team_id": self.team_id,
            "fingerprint": info.fingerprint,
            "is_git_related": bool_of(payload, "is_git_related", false),
        }));
        let id = self.insert(conn, "private_keys", attrs).await?;
        Ok((id, uuid))
    }

    /// `importServer`.
    async fn server(
        &mut self,
        conn: &mut PgConnection,
        state: &AppState,
        payload: &Value,
        key_id: i64,
    ) -> R<Res> {
        let uuid = free_uuid(conn, "servers", self.preserved_uuid(payload)).await?;
        let cloud_token = dg(payload, "cloud_provider_token_uuid")
            .and_then(Value::as_str)
            .and_then(|u| self.m.cloud_tokens.get(u).copied());
        let name = dg(payload, "name")
            .filter(|n| crate::serialize::php_truthy(n))
            .map_or_else(crate::routes::private_keys::random_name, text);
        let ip = str_of(payload, "ip");
        let attrs = obj(json!({
            "uuid": uuid,
            "name": name,
            "description": get_or_null(payload, "description"),
            "ip": ip,
            "port": dg(payload, "port").map_or(22, as_int),
            "user": or(dg(payload, "user"), "root"),
            "private_key_id": key_id,
            "team_id": self.team_id,
            "cloud_provider_token_id": cloud_token,
            "hetzner_server_id": get_or_null(payload, "hetzner_server_id"),
            "vultr_instance_id": get_or_null(payload, "vultr_instance_id"),
            "digitalocean_droplet_id": get_or_null(payload, "digitalocean_droplet_id"),
            "hostinger_virtual_machine_id": get_or_null(payload, "hostinger_virtual_machine_id"),
            "proxy_error_page": get_or_null(payload, "proxy_error_page"),
            "swarm_cluster": get_or_null(payload, "swarm_cluster"),
        }));
        let id = self.insert(conn, "servers", attrs).await?;

        // The hooks of a new server: settings, default destination, shared variables, proxy.
        let token = self.enc.encrypt_string(
            &self
                .enc
                .encrypt_serialized(&json!({"server_uuid": uuid}).to_string()),
        );
        let (fqdn, v4, v6) = coolify_db::servers::instance_urls(&state.db)
            .await
            .unwrap_or((None, None, None));
        let sentinel = fqdn
            .filter(|s| !s.trim().is_empty())
            .or(v4
                .filter(|s| !s.trim().is_empty())
                .map(|v| format!("http://{v}:8000")))
            .or(v6
                .filter(|s| !s.trim().is_empty())
                .map(|v| format!("http://[{v}]:8000")))
            .or_else(|| Some(instance_url_sync()));
        self.insert(
            conn,
            "server_settings",
            obj(json!({"server_id": id, "sentinel_token": token, "sentinel_custom_url": sentinel})),
        )
        .await?;
        self.insert(
            conn,
            "standalone_dockers",
            obj(json!({"uuid": new_public_id(), "name": "coolify", "network": "coolify", "server_id": id})),
        )
        .await?;
        for (key, value) in [
            ("COOLIFY_SERVER_UUID", &uuid),
            ("COOLIFY_SERVER_NAME", &name),
        ] {
            self.insert(
                conn,
                "shared_environment_variables",
                obj(json!({
                    "key": key,
                    "value": self.enc.encrypt_string(value),
                    "type": "server",
                    "server_id": id,
                    "team_id": self.team_id,
                    "is_literal": true,
                })),
            )
            .await?;
        }

        // Role.
        let mut role = dg(payload, "server_role").map(text).unwrap_or_default();
        if !matches!(role.as_str(), "both" | "build" | "deployment") {
            role = if truthy(dg(payload, "is_build_server")) {
                "build".into()
            } else {
                "both".into()
            };
        }

        // Proxy.
        let mut proxy =
            obj(json!({"redirect_enabled": true, "type": "TRAEFIK", "status": "exited"}));
        if let Some(Value::Object(p)) = dg(payload, "proxy") {
            for (k, v) in p {
                proxy.insert(k.clone(), v.clone());
            }
        }
        sqlx::query("UPDATE servers SET proxy = $2::text::json WHERE id = $1")
            .bind(id)
            .bind(Value::Object(proxy).to_string())
            .execute(&mut *conn)
            .await?;

        // Settings: imported servers start not validated; claim rebinds Sentinel.
        let mut settings = Map::new();
        if let Some(Value::Object(s)) = dg(payload, "settings") {
            for field in SERVER_SETTING_FILLABLE {
                if let Some(v) = s.get(*field) {
                    let value = match *field {
                        "logdrain_custom_config" | "geoip_maxmind_license_key" => {
                            self.encrypt(Some(v))
                        }
                        "docker_cleanup_frequency" => {
                            json!(coolify_compat::cron::translate_cron_expression(&text(v)))
                        }
                        _ => v.clone(),
                    };
                    settings.insert((*field).to_owned(), value);
                }
            }
            let token_uuid = s.get("cloudflare_tunnel_integration_token_uuid");
            let tunnel_token = self.imported_integration_token(
                token_uuid,
                "The Cloudflare tunnel of this server: its Cloudflare API token was not found on this instance; select it again in the Cloudflare tunnel settings.",
            );
            settings.insert(
                "cloudflare_tunnel_integration_token_id".into(),
                json!(tunnel_token),
            );
        }
        settings.insert("server_role".into(), json!(role));
        settings.insert("is_build_server".into(), json!(role == "build"));
        settings.insert("is_reachable".into(), json!(false));
        settings.insert("is_usable".into(), json!(false));
        settings.insert("force_disabled".into(), json!(false));
        settings.insert("is_sentinel_enabled".into(), json!(false));
        self.update(conn, "server_settings", "server_id", id, settings)
            .await?;

        Ok(Res {
            id,
            uuid,
            morph: SERVER_MORPH,
            name,
            environment_id: 0,
        })
    }

    /// `importDestinations`: original uuid to (id, morph). The first one is the fallback.
    async fn destinations(
        &mut self,
        conn: &mut PgConnection,
        destinations: Vec<&Value>,
        server_id: i64,
    ) -> R<Vec<(String, i64, &'static str)>> {
        let standalone: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, uuid FROM standalone_dockers WHERE server_id = $1 ORDER BY id",
        )
        .bind(server_id)
        .fetch_all(&mut *conn)
        .await?;
        if destinations.is_empty() {
            return Ok(standalone
                .first()
                .map(|(id, uuid)| (uuid.clone(), *id, STANDALONE_DOCKER_MORPH))
                .into_iter()
                .collect());
        }
        let mut map = Vec::new();
        let mut standalone_index = 0;
        for destination in destinations {
            let original = str_of(destination, "uuid");
            let uuid = if self.preserve && !original.is_empty() {
                original.clone()
            } else {
                new_public_id()
            };
            let network = dg(destination, "network").filter(|n| crate::serialize::php_truthy(n));
            let name = dg(destination, "name").filter(|n| crate::serialize::php_truthy(n));
            if dg(destination, "type").and_then(Value::as_str) == Some("swarm") {
                let uuid = free_uuid(conn, "swarm_dockers", uuid).await?;
                let id = self
                    .insert(
                        conn,
                        "swarm_dockers",
                        obj(json!({
                            "uuid": uuid,
                            "name": or(name, "coolify-overlay"),
                            "network": or(network, "coolify-overlay"),
                            "server_id": server_id,
                        })),
                    )
                    .await?;
                let key = if original.is_empty() { uuid } else { original };
                map.push((key, id, SWARM_DOCKER_MORPH));
            } else if let Some((id, current)) = standalone.get(standalone_index) {
                standalone_index += 1;
                let taken: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM standalone_dockers WHERE uuid = $1 AND id <> $2)",
                )
                .bind(&uuid)
                .bind(id)
                .fetch_one(&mut *conn)
                .await?;
                let target = if taken { current.clone() } else { uuid };
                let mut attrs = obj(json!({"uuid": target}));
                if let Some(n) = name {
                    attrs.insert("name".into(), n.clone());
                }
                if let Some(n) = network {
                    attrs.insert("network".into(), n.clone());
                }
                self.update(conn, "standalone_dockers", "id", *id, attrs)
                    .await?;
                let key = if original.is_empty() {
                    target
                } else {
                    original
                };
                map.push((key, *id, STANDALONE_DOCKER_MORPH));
            } else {
                standalone_index += 1;
                let uuid = free_uuid(conn, "standalone_dockers", uuid).await?;
                let id = self
                    .insert(
                        conn,
                        "standalone_dockers",
                        obj(json!({
                            "uuid": uuid,
                            "name": or(name, "coolify"),
                            "network": or(network, "coolify"),
                            "server_id": server_id,
                        })),
                    )
                    .await?;
                let key = if original.is_empty() { uuid } else { original };
                map.push((key, id, STANDALONE_DOCKER_MORPH));
            }
        }
        // The created hook made a default destination; the server gets only those of the bundle.
        if standalone_index == 0 {
            for (id, _) in &standalone {
                sqlx::query("DELETE FROM standalone_dockers WHERE id = $1")
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
            }
        }
        Ok(map)
    }

    /// `createSharedVariable`.
    async fn shared_variable(
        &mut self,
        conn: &mut PgConnection,
        variable: &Value,
        scope: Value,
    ) -> R<()> {
        let mut attrs = obj(json!({
            "key": str_of(variable, "key"),
            "value": self.encrypt(dg(variable, "value")),
            "is_multiline": bool_of(variable, "is_multiline", false),
            "is_literal": bool_of(variable, "is_literal", false),
            "is_shown_once": bool_of(variable, "is_shown_once", false),
            "comment": get_or_null(variable, "comment"),
        }));
        attrs.extend(obj(scope));
        self.insert(conn, "shared_environment_variables", attrs)
            .await?;
        Ok(())
    }

    async fn scoped_shared_variables(
        &mut self,
        conn: &mut PgConnection,
        variables: Vec<&Value>,
        kind: &str,
        column: &str,
        id: i64,
        team_id: i64,
    ) -> R<()> {
        for variable in variables {
            let found: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM shared_environment_variables WHERE type = $1 AND {column} = $2 AND key = $3)"
            ))
            .bind(kind)
            .bind(id)
            .bind(str_of(variable, "key"))
            .fetch_one(&mut *conn)
            .await?;
            if !found {
                self.shared_variable(
                    conn,
                    variable,
                    json!({"type": kind, column: id, "team_id": team_id}),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// `importProject`.
    async fn project(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        created: &mut Map<String, Value>,
    ) -> R<i64> {
        let uuid = self.preserved_uuid(payload);
        let shared = list(payload, "shared_environment_variables");
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT id FROM projects WHERE uuid = $1 AND team_id = $2")
                .bind(&uuid)
                .bind(self.team_id)
                .fetch_optional(&mut *conn)
                .await?;
        let id = if let Some(id) = existing {
            id
        } else {
            let uuid = free_uuid(conn, "projects", uuid).await?;
            let name = dg(payload, "name")
                .filter(|n| crate::serialize::php_truthy(n))
                .map_or_else(crate::routes::private_keys::random_name, text);
            let id = self
                .insert(
                    conn,
                    "projects",
                    obj(json!({
                        "uuid": uuid,
                        "name": name,
                        "description": get_or_null(payload, "description"),
                        "team_id": self.team_id,
                    })),
                )
                .await?;
            // `Project::created`.
            self.insert(conn, "project_settings", obj(json!({"project_id": id})))
                .await?;
            self.insert(
                conn,
                "environments",
                obj(json!({"uuid": new_public_id(), "name": "production", "project_id": id})),
            )
            .await?;
            bump(created, "projects", 1);
            id
        };
        let team = self.team_id;
        self.scoped_shared_variables(conn, shared, "project", "project_id", id, team)
            .await?;
        Ok(id)
    }

    /// `importEnvironment`.
    async fn environment(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        project_id: i64,
        created: &mut Map<String, Value>,
    ) -> R<(i64, String)> {
        let uuid = self.preserved_uuid(payload);
        let team = self.team_id;
        let shared = list(payload, "shared_environment_variables");
        let by_uuid: Option<i64> =
            sqlx::query_scalar("SELECT id FROM environments WHERE uuid = $1 AND project_id = $2")
                .bind(&uuid)
                .bind(project_id)
                .fetch_optional(&mut *conn)
                .await?;
        let name = dg(payload, "name").map_or_else(|| "production".to_owned(), text);
        let id = if let Some(id) = by_uuid {
            id
        } else {
            let by_name: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM environments WHERE project_id = $1 AND name = $2",
            )
            .bind(project_id)
            .bind(&name)
            .fetch_optional(&mut *conn)
            .await?;
            if let Some(id) = by_name {
                let mut attrs = Map::new();
                if self.preserve && !uuid.is_empty() && !exists(conn, "environments", &uuid).await?
                {
                    attrs.insert("uuid".into(), json!(uuid));
                }
                if !blank(dg(payload, "description")) {
                    attrs.insert("description".into(), get_or_null(payload, "description"));
                }
                self.update(conn, "environments", "id", id, attrs).await?;
                id
            } else {
                let uuid = free_uuid(conn, "environments", uuid).await?;
                let id = self
                    .insert(
                        conn,
                        "environments",
                        obj(json!({
                            "uuid": uuid,
                            "name": name,
                            "description": get_or_null(payload, "description"),
                            "project_id": project_id,
                        })),
                    )
                    .await?;
                bump(created, "environments", 1);
                id
            }
        };
        self.scoped_shared_variables(conn, shared, "environment", "environment_id", id, team)
            .await?;
        Ok((id, name))
    }

    /// `importGithubRunnerConfig`.
    async fn github_runner_config(
        &mut self,
        conn: &mut PgConnection,
        payload: Option<&Value>,
        server_id: i64,
    ) -> R<()> {
        let Some(payload) = payload.filter(|p| p.is_object() || p.is_array()) else {
            return Ok(());
        };
        const UNAVAILABLE: &str = "Set up GitHub runners on the server again.";
        let app_uuid = str_of(payload, "github_app_uuid");
        let mut app = self.m.github.get(&app_uuid).copied();
        if app.is_none() && !app_uuid.is_empty() {
            app = sqlx::query_scalar(
                "SELECT id FROM github_apps WHERE uuid = $1 AND (is_system_wide = true OR team_id = $2) ORDER BY is_system_wide DESC LIMIT 1",
            )
            .bind(&app_uuid)
            .bind(self.team_id)
            .fetch_optional(&mut *conn)
            .await?;
        }
        let Some(app) = app else {
            self.m.warnings.push(format!(
                "The GitHub runner settings of this server were not imported, because their GitHub App was not found on this instance. {UNAVAILABLE}"
            ));
            return Ok(());
        };
        if let Some(problem) = runner_config_problem(payload) {
            self.m.warnings.push(format!(
                "The GitHub runner settings of this server were not imported, because they are not valid: {problem} {UNAVAILABLE}"
            ));
            return Ok(());
        }
        let mut uuid = str_of(payload, "uuid");
        if !self.preserve || uuid.is_empty() || exists(conn, "github_runner_configs", &uuid).await?
        {
            uuid = new_public_id();
        }
        let attrs = obj(json!({
            "uuid": uuid,
            "server_id": server_id,
            "github_app_id": app,
            "labels": list(payload, "labels"),
            "is_enabled": false,
            "is_dedicated": bool_of(payload, "is_dedicated", false),
            "allow_pull_requests": bool_of(payload, "allow_pull_requests", false),
            "max_runners": int_or(payload, "max_runners", 1),
            "docker_mode": str_of(payload, "docker_mode"),
            "runner_image": get_or_null(payload, "runner_image"),
            "cpu_limit": get_or_null(payload, "cpu_limit"),
            "memory_limit": get_or_null(payload, "memory_limit"),
            "capacity_wait_timeout": int_or(payload, "capacity_wait_timeout", 1),
            "idle_timeout": int_or(payload, "idle_timeout", 1),
            "job_timeout": int_or(payload, "job_timeout", 1),
        }));
        self.insert(conn, "github_runner_configs", attrs).await?;
        Ok(())
    }

    /// `resolveResourceUuid`.
    async fn resource_uuid(
        &self,
        conn: &mut PgConnection,
        table: &str,
        uuid: String,
        label: &str,
    ) -> R<String> {
        if !exists(conn, table, &uuid).await? {
            return Ok(uuid);
        }
        if self.adopt {
            return Err(TransferError::Validation(vec![(
                "bundle".to_owned(),
                vec![format!(
                    "The {label} UUID {uuid} cannot be used on this instance. Adopt mode needs the same UUID to keep the running containers. If you deleted the {label} on this instance, delete it permanently first."
                )],
            )]));
        }
        Ok(new_public_id())
    }

    /// The team source with this uuid, or a system-wide one.
    async fn git_source(
        &mut self,
        conn: &mut PgConnection,
        table: &str,
        uuid: &str,
    ) -> R<Option<i64>> {
        Ok(sqlx::query_scalar(&format!(
            "SELECT id FROM {table} WHERE uuid = $1 AND (is_system_wide = true OR team_id = $2) ORDER BY is_system_wide DESC LIMIT 1"
        ))
        .bind(uuid)
        .bind(self.team_id)
        .fetch_optional(&mut *conn)
        .await?)
    }

    /// `importApplication`.
    async fn application(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        environment_id: i64,
        destinations: &[(String, i64, &'static str)],
    ) -> R<Res> {
        let destination = resolve_destination(dg(payload, "destination_uuid"), destinations)?;
        let uuid = self.preserved_uuid(payload);
        let uuid = self
            .resource_uuid(conn, "applications", uuid, "application")
            .await?;

        let columns = self.columns(conn, "applications").await?;
        let source_attrs = dg(payload, "attributes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut attrs: Map<String, Value> = source_attrs
            .iter()
            .filter(|(k, _)| {
                columns.contains(*k)
                    && !matches!(
                        k.as_str(),
                        "id" | "environment_id"
                            | "destination_id"
                            | "destination_type"
                            | "source_id"
                            | "source_type"
                            | "private_key_id"
                    )
                    && (APPLICATION_FILLABLE.contains(&k.as_str())
                        || matches!(k.as_str(), "is_maintenance_enabled" | "maintenance_page"))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        attrs.insert("uuid".into(), json!(uuid));
        attrs.insert("environment_id".into(), json!(environment_id));
        attrs.insert("destination_id".into(), json!(destination.1));
        attrs.insert("destination_type".into(), json!(destination.2));
        let name = attrs.get("name").map_or_else(|| uuid.clone(), text);

        // Re-link git source.
        let source_uuid = str_of(payload, "source.uuid");
        let source_type = str_of(payload, "source.type");
        let mut relinked = false;
        if !source_uuid.is_empty() {
            let found = match source_type.as_str() {
                "github_app" => match self.m.github.get(&source_uuid).copied() {
                    Some(id) => Some((id, GITHUB_MORPH)),
                    None => self
                        .git_source(conn, "github_apps", &source_uuid)
                        .await?
                        .map(|id| (id, GITHUB_MORPH)),
                },
                "gitlab_app" => match self.m.gitlab.get(&source_uuid).copied() {
                    Some(id) => Some((id, GITLAB_MORPH)),
                    None => self
                        .git_source(conn, "gitlab_apps", &source_uuid)
                        .await?
                        .map(|id| (id, GITLAB_MORPH)),
                },
                "gitea_app" => match self.m.gitea.get(&source_uuid).copied() {
                    Some(id) => Some((id, GITEA_MORPH)),
                    None => self
                        .git_source(conn, "gitea_apps", &source_uuid)
                        .await?
                        .map(|id| (id, GITEA_MORPH)),
                },
                _ => None,
            };
            if let Some((id, morph)) = found {
                attrs.insert("source_type".into(), json!(morph));
                attrs.insert("source_id".into(), json!(id));
                relinked = true;
            }
        }
        let source_missing = truthy(dg(payload, "had_git_source")) && !relinked;
        if source_missing || dg(payload, "source.system_wide") == Some(&json!(true)) {
            self.m.git_source_applications.push(uuid.clone());
        }
        if source_missing {
            let label = match source_type.as_str() {
                "github_app" => "GitHub App",
                "gitlab_app" => "GitLab App",
                "gitea_app" => "Gitea App",
                _ => "git source",
            };
            self.m.warnings.push(format!(
                "Application {name}: its {label} was not found on this instance; set the source again."
            ));
        }

        // Re-link deploy key.
        let key = dg(payload, "private_key_uuid")
            .and_then(Value::as_str)
            .and_then(|k| self.m.private_keys.get(k).copied());
        if let Some(k) = key {
            attrs.insert("private_key_id".into(), json!(k));
        } else if truthy(dg(payload, "had_private_key")) {
            self.m.warnings.push(format!(
                "Application {name}: its private key was not found on this instance; select the private key again."
            ));
        }

        let status = if self.adopt {
            attrs
                .get("status")
                .filter(|s| crate::serialize::php_truthy(s))
                .map_or_else(|| "exited".to_owned(), text)
        } else {
            "exited".to_owned()
        };
        attrs.insert("status".into(), json!(normalize_status(&status)));

        self.application_setters(&mut attrs);
        let parser = attrs
            .get("compose_parsing_version")
            .filter(|v| crate::serialize::php_truthy(v))
            .cloned()
            .unwrap_or_else(|| json!(PARSER_VERSION));
        attrs.insert("compose_parsing_version".into(), parser);

        let id = self.insert(conn, "applications", attrs).await?;

        // `Application::created`, then the settings of the bundle.
        let mut settings_attrs = obj(json!({"application_id": id}));
        if let Some(Value::Object(s)) = dg(payload, "settings") {
            for field in APPLICATION_SETTING_FILLABLE {
                if let Some(v) = s.get(*field) {
                    settings_attrs.insert((*field).to_owned(), v.clone());
                }
            }
        }
        self.insert(conn, "application_settings", settings_attrs)
            .await?;

        let res = Res {
            id,
            uuid,
            morph: APPLICATION_MORPH,
            name: name.clone(),
            environment_id,
        };
        self.env_vars(conn, list(payload, "environment_variables"), &res, false)
            .await?;
        self.env_vars(
            conn,
            list(payload, "environment_variables_preview"),
            &res,
            true,
        )
        .await?;
        self.persistent_storages(conn, list(payload, "persistent_storages"), &res)
            .await?;
        self.file_storages(conn, list(payload, "file_storages"), &res)
            .await?;
        self.scheduled_tasks(conn, list(payload, "scheduled_tasks"), "application_id", id)
            .await?;
        self.tags(conn, list(payload, "tags"), &res).await?;
        self.previews(conn, list(payload, "previews"), &res).await?;
        self.secret_manager_link(
            conn,
            dg(payload, "secret_manager_link"),
            &res,
            &format!("Application {name}"),
        )
        .await?;
        Ok(res)
    }

    /// The setters and the `creating` and `saving` hooks of `Application`.
    fn application_setters(&self, attrs: &mut Map<String, Value>) {
        for field in [
            "http_basic_auth_password",
            "manual_webhook_secret_github",
            "manual_webhook_secret_gitlab",
            "manual_webhook_secret_bitbucket",
            "manual_webhook_secret_gitea",
        ] {
            let value = match attrs.get(field) {
                Some(v) if !v.is_null() => self.encrypt(Some(v)),
                // `$application->manual_webhook_secret_* ??= Str::random(40)`.
                _ if field != "http_basic_auth_password" => {
                    json!(self.enc.encrypt_string(&random_string(40)))
                }
                _ => Value::Null,
            };
            if field != "http_basic_auth_password" || attrs.contains_key(field) {
                attrs.insert(field.to_owned(), value);
            }
        }
        if let Some(v) = attrs
            .get("custom_nginx_configuration")
            .filter(|v| !v.is_null())
        {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(text(v));
            attrs.insert("custom_nginx_configuration".into(), json!(encoded));
        }
        if let Some(v) = attrs.get("base_directory") {
            let value = format!("/{}", text(v).trim_start_matches('/'));
            attrs.insert("base_directory".into(), json!(value));
        }
        if let Some(v) = attrs.get("publish_directory") {
            let value = if crate::serialize::php_truthy(v) {
                json!(format!("/{}", text(v).trim_start_matches('/')))
            } else {
                Value::Null
            };
            attrs.insert("publish_directory".into(), value);
        }
        if attrs.get("ports_mappings").and_then(Value::as_str) == Some("") {
            attrs.insert("ports_mappings".into(), Value::Null);
        }
        if attrs.get("fqdn").and_then(Value::as_str) == Some("") {
            attrs.insert("fqdn".into(), Value::Null);
        }
        for field in [
            "install_command",
            "build_command",
            "start_command",
            "ports_exposes",
            "ports_mappings",
            "custom_network_aliases",
        ] {
            if let Some(Value::String(s)) = attrs.get(field) {
                let trimmed = s.trim().to_owned();
                attrs.insert(field.to_owned(), json!(trimmed));
            }
        }
    }

    /// `importDatabase`.
    async fn database(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        environment_id: i64,
        destinations: &[(String, i64, &'static str)],
    ) -> R<Res> {
        let kind = dg(payload, "type").map_or_else(|| "StandalonePostgresql".to_owned(), text);
        let Some(&(type_name, table, encrypted)) =
            DATABASE_MODELS.iter().find(|(t, _, _)| *t == kind)
        else {
            return Err(TransferError::Message(format!(
                "Unsupported database type: {kind}"
            )));
        };
        let destination = resolve_destination(dg(payload, "destination_uuid"), destinations)?;
        let uuid = self.preserved_uuid(payload);
        let uuid = self.resource_uuid(conn, table, uuid, "database").await?;
        let columns = self.columns(conn, table).await?;
        let source_attrs = dg(payload, "attributes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut attrs: Map<String, Value> = source_attrs
            .iter()
            .filter(|(k, _)| {
                columns.contains(*k)
                    && !matches!(
                        k.as_str(),
                        "id" | "environment_id" | "destination_id" | "destination_type"
                    )
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for column in encrypted {
            if let Some(v) = attrs.get(*column) {
                let value = self.encrypt(Some(v));
                attrs.insert((*column).to_owned(), value);
            }
        }
        if attrs.get("ports_mappings").and_then(Value::as_str) == Some("") {
            attrs.insert("ports_mappings".into(), Value::Null);
        }
        let status = if self.adopt {
            attrs
                .get("status")
                .filter(|s| crate::serialize::php_truthy(s))
                .map_or_else(|| "exited".to_owned(), text)
        } else {
            "exited".to_owned()
        };
        attrs.insert("status".into(), json!(normalize_status(&status)));
        attrs.insert("uuid".into(), json!(uuid));
        attrs.insert("environment_id".into(), json!(environment_id));
        attrs.insert("destination_id".into(), json!(destination.1));
        attrs.insert("destination_type".into(), json!(destination.2));
        let name = attrs.get("name").map(text).unwrap_or_default();
        let id = self.insert(conn, table, attrs).await?;
        let morph: &'static str = morph_of(type_name);
        let res = Res {
            id,
            uuid,
            morph,
            name: name.clone(),
            environment_id,
        };
        self.env_vars(conn, list(payload, "environment_variables"), &res, false)
            .await?;
        self.persistent_storages(conn, list(payload, "persistent_storages"), &res)
            .await?;
        self.file_storages(conn, list(payload, "file_storages"), &res)
            .await?;
        self.tags(conn, list(payload, "tags"), &res).await?;
        self.scheduled_backups(conn, list(payload, "scheduled_backups"), &res)
            .await?;
        self.secret_manager_link(
            conn,
            dg(payload, "secret_manager_link"),
            &res,
            &format!("Database {name}"),
        )
        .await?;
        Ok(res)
    }

    /// `importService`.
    async fn service(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        environment_id: i64,
        server_id: i64,
        destinations: &[(String, i64, &'static str)],
    ) -> R<Res> {
        let destination = resolve_destination(dg(payload, "destination_uuid"), destinations)?;
        let uuid = self.preserved_uuid(payload);
        let uuid = self
            .resource_uuid(conn, "services", uuid, "service")
            .await?;
        let columns = self.columns(conn, "services").await?;
        let source_attrs = dg(payload, "attributes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut attrs: Map<String, Value> = source_attrs
            .iter()
            .filter(|(k, _)| {
                columns.contains(*k)
                    && (SERVICE_FILLABLE.contains(&k.as_str())
                        || matches!(
                            k.as_str(),
                            "is_maintenance_enabled"
                                | "maintenance_page"
                                | "include_coolify_default_variables"
                        ))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        attrs.insert("uuid".into(), json!(uuid));
        attrs.insert("environment_id".into(), json!(environment_id));
        attrs.insert("server_id".into(), json!(server_id));
        attrs.insert("destination_id".into(), json!(destination.1));
        attrs.insert("destination_type".into(), json!(destination.2));
        let parser = attrs
            .get("compose_parsing_version")
            .filter(|v| crate::serialize::php_truthy(v))
            .cloned()
            .unwrap_or_else(|| json!(PARSER_VERSION));
        attrs.insert("compose_parsing_version".into(), parser);
        let name = attrs.get("name").map(text).unwrap_or_default();
        let id = self.insert(conn, "services", attrs).await?;
        let res = Res {
            id,
            uuid,
            morph: SERVICE_MORPH,
            name: name.clone(),
            environment_id,
        };
        self.env_vars(conn, list(payload, "environment_variables"), &res, false)
            .await?;
        self.scheduled_tasks(conn, list(payload, "scheduled_tasks"), "service_id", id)
            .await?;
        self.tags(conn, list(payload, "tags"), &res).await?;
        self.secret_manager_link(
            conn,
            dg(payload, "secret_manager_link"),
            &res,
            &format!("Service {name}"),
        )
        .await?;
        for app in list(payload, "applications") {
            let sa = self
                .service_child(
                    conn,
                    app,
                    id,
                    "service_applications",
                    SERVICE_APP_MORPH,
                    "service-app",
                    &[
                        "uuid",
                        "environment_variables",
                        "persistent_storages",
                        "file_storages",
                        "attributes",
                    ],
                )
                .await?;
            self.env_vars(conn, list(app, "environment_variables"), &sa, false)
                .await?;
            self.persistent_storages(conn, list(app, "persistent_storages"), &sa)
                .await?;
            self.file_storages(conn, list(app, "file_storages"), &sa)
                .await?;
            let key = dg(app, "uuid").map_or_else(|| sa.uuid.clone(), text);
            self.m.service_applications.insert(key, sa);
        }
        for db in list(payload, "databases") {
            let sd = self
                .service_child(
                    conn,
                    db,
                    id,
                    "service_databases",
                    SERVICE_DB_MORPH,
                    "service-db",
                    &[
                        "uuid",
                        "environment_variables",
                        "persistent_storages",
                        "file_storages",
                        "scheduled_backups",
                        "attributes",
                    ],
                )
                .await?;
            self.persistent_storages(conn, list(db, "persistent_storages"), &sd)
                .await?;
            self.file_storages(conn, list(db, "file_storages"), &sd)
                .await?;
            self.scheduled_backups(conn, list(db, "scheduled_backups"), &sd)
                .await?;
        }
        Ok(res)
    }

    /// `importServiceApplication` and `importServiceDatabase`.
    #[allow(clippy::too_many_arguments)]
    async fn service_child(
        &mut self,
        conn: &mut PgConnection,
        payload: &Value,
        service_id: i64,
        table: &str,
        morph: &'static str,
        default_name: &str,
        thin_except: &[&str],
    ) -> R<Res> {
        let uuid = free_uuid(conn, table, self.preserved_uuid(payload)).await?;
        let mut source = dg(payload, "attributes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if source.is_empty() {
            source = payload.as_object().cloned().unwrap_or_default();
            source.retain(|k, _| !thin_except.contains(&k.as_str()));
        }
        let columns = self.columns(conn, table).await?;
        let mut attrs: Map<String, Value> = source
            .into_iter()
            .filter(|(k, _)| {
                columns.contains(k)
                    && !matches!(
                        k.as_str(),
                        "id" | "service_id" | "created_at" | "updated_at" | "deleted_at"
                    )
            })
            .collect();
        attrs.insert("service_id".into(), json!(service_id));
        let name = attrs
            .get("name")
            .or_else(|| dg(payload, "name"))
            .filter(|n| crate::serialize::php_truthy(n))
            .map_or_else(|| default_name.to_owned(), text);
        attrs.insert("name".into(), json!(name));
        let status = if self.adopt {
            attrs
                .get("status")
                .or_else(|| dg(payload, "status"))
                .filter(|s| crate::serialize::php_truthy(s))
                .map_or_else(|| "running:unknown".to_owned(), text)
        } else {
            "exited:unhealthy".to_owned()
        };
        attrs.insert("status".into(), json!(status));
        attrs.insert("uuid".into(), json!(uuid));
        let id = self.insert(conn, table, attrs).await?;
        Ok(Res {
            id,
            uuid,
            morph,
            name,
            environment_id: 0,
        })
    }

    /// `importEnvVars`.
    async fn env_vars(
        &mut self,
        conn: &mut PgConnection,
        variables: Vec<&Value>,
        resource: &Res,
        is_preview: bool,
    ) -> R<()> {
        for v in variables {
            let uuid = free_uuid(conn, "environment_variables", uuid_or_new(v)).await?;
            let value = match dg(v, "value") {
                None | Some(Value::Null) => Value::Null,
                Some(x) => json!(self.enc.encrypt_serialized(text(x).trim())),
            };
            let mut attrs = obj(json!({
                "uuid": uuid,
                "key": str_of(v, "key"),
                "value": value,
                "is_literal": bool_of(v, "is_literal", false),
                "is_multiline": bool_of(v, "is_multiline", false),
                "is_preview": is_preview || truthy(dg(v, "is_preview")),
                "is_runtime": bool_of(v, "is_runtime", true),
                "is_buildtime": bool_of(v, "is_buildtime", true),
                "is_shown_once": bool_of(v, "is_shown_once", false),
                "is_shared": bool_of(v, "is_shared", false),
                "is_required": bool_of(v, "is_required", false),
                "uses_legacy_escaping": bool_of(v, "uses_legacy_escaping", true),
                "preserve_compose_null": bool_of(v, "preserve_compose_null", false),
                "comment": get_or_null(v, "comment"),
                "order": get_or_null(v, "order"),
                "resourceable_type": resource.morph,
                "resourceable_id": resource.id,
            }));
            if !blank(dg(v, "version")) {
                attrs.insert("version".into(), json!(str_of(v, "version")));
            }
            self.insert(conn, "environment_variables", attrs).await?;
        }
        Ok(())
    }

    /// `importPersistentStorages`.
    async fn persistent_storages(
        &mut self,
        conn: &mut PgConnection,
        volumes: Vec<&Value>,
        resource: &Res,
    ) -> R<()> {
        for v in volumes {
            let uuid = free_uuid(conn, "local_persistent_volumes", uuid_or_new(v)).await?;
            let start = |s: String| {
                let t = s.trim();
                if t.starts_with('/') {
                    t.to_owned()
                } else {
                    format!("/{t}")
                }
            };
            let host_path = match dg(v, "host_path") {
                Some(h) if crate::serialize::php_truthy(h) => json!(start(text(h))),
                other => other.cloned().unwrap_or(Value::Null),
            };
            let name = get_or_null(v, "name");
            let id = self
                .insert(
                    conn,
                    "local_persistent_volumes",
                    obj(json!({
                        "uuid": uuid,
                        "name": name,
                        "mount_path": start(str_of(v, "mount_path")),
                        "host_path": host_path,
                        "is_preview_suffix_enabled": bool_of(v, "is_preview_suffix_enabled", false),
                        "ignores_compose_driver_options": bool_of(v, "ignores_compose_driver_options", true),
                        "resource_type": resource.morph,
                        "resource_id": resource.id,
                    })),
                )
                .await?;
            let entry = (id, VOLUME_MORPH, text(&name));
            self.m.volumes.insert(uuid.clone(), entry.clone());
            if !blank(dg(v, "uuid")) {
                self.m.volumes.insert(str_of(v, "uuid"), entry);
            }
            if !blank(dg(v, "standalone_sqlite_uuid")) {
                self.m
                    .pending_sqlite
                    .push((id, text(&name), str_of(v, "standalone_sqlite_uuid")));
            }
        }
        Ok(())
    }

    /// `importFileStorages`.
    async fn file_storages(
        &mut self,
        conn: &mut PgConnection,
        storages: Vec<&Value>,
        resource: &Res,
    ) -> R<()> {
        static CHOWN: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"\A(?:[A-Za-z_][A-Za-z0-9_.-]*|[0-9]+)(?::(?:[A-Za-z_][A-Za-z0-9_.-]*|[0-9]+))?\z",
            )
            .expect("regex")
        });
        static CHMOD: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\A[0-7]{3,4}\z").expect("regex"));
        for s in storages {
            let is_host = truthy(dg(s, "is_host_file"));
            let Some(fs_path) = dg(s, "fs_path").and_then(Value::as_str) else {
                return Err(TransferError::Message(
                    "Invalid imported file storage path.".into(),
                ));
            };
            let fs_path = if is_host {
                coolify_jobs::services::storage::validate_host_file_mount_path(
                    fs_path,
                    "imported host file source path",
                )?
            } else {
                fs_path.to_owned()
            };
            let (chown, chmod) = (dg(s, "chown"), dg(s, "chmod"));
            if !blank(chown)
                && !chown
                    .and_then(Value::as_str)
                    .is_some_and(|c| CHOWN.is_match(c))
            {
                return Err(TransferError::Message(
                    "Invalid imported file owner.".into(),
                ));
            }
            if !blank(chmod)
                && !chmod
                    .and_then(Value::as_str)
                    .is_some_and(|c| CHMOD.is_match(c))
            {
                return Err(TransferError::Message("Invalid imported file mode.".into()));
            }
            let uuid = free_uuid(conn, "local_file_volumes", uuid_or_new(s)).await?;
            let mount = dg(s, "mount_path");
            let id = self
                .insert(
                    conn,
                    "local_file_volumes",
                    obj(json!({
                        "uuid": uuid,
                        "fs_path": fs_path,
                        "mount_path": mount,
                        "content": self.encrypt(dg(s, "content")),
                        "is_directory": bool_of(s, "is_directory", false),
                        "is_host_file": is_host,
                        "chown": chown,
                        "chmod": chmod,
                        "is_based_on_git": bool_of(s, "is_based_on_git", false),
                        "is_preview_suffix_enabled": bool_of(s, "is_preview_suffix_enabled", false),
                        "resource_type": resource.morph,
                        "resource_id": resource.id,
                    })),
                )
                .await?;
            let entry = (id, FILE_MORPH, mount.map(text).unwrap_or_default());
            self.m.volumes.insert(uuid.clone(), entry.clone());
            if !blank(dg(s, "uuid")) {
                self.m.volumes.insert(str_of(s, "uuid"), entry);
            }
        }
        Ok(())
    }

    /// `importScheduledTasks`.
    async fn scheduled_tasks(
        &mut self,
        conn: &mut PgConnection,
        tasks: Vec<&Value>,
        column: &str,
        id: i64,
    ) -> R<()> {
        for t in tasks {
            let uuid = free_uuid(conn, "scheduled_tasks", uuid_or_new(t)).await?;
            let attrs = obj(json!({
                "uuid": uuid,
                "name": get_or_null(t, "name"),
                "command": get_or_null(t, "command"),
                "frequency": get_or_null(t, "frequency"),
                "container": get_or_null(t, "container"),
                "timeout": get_or_null(t, "timeout"),
                "enabled": bool_of(t, "enabled", true),
                "team_id": self.team_id,
                column: id,
            }));
            self.insert(conn, "scheduled_tasks", attrs).await?;
        }
        Ok(())
    }

    /// `importTags`.
    async fn tags(&mut self, conn: &mut PgConnection, tags: Vec<&Value>, resource: &Res) -> R<()> {
        for t in tags {
            let name = str_of(t, "name").trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            let existing: Option<i64> =
                sqlx::query_scalar("SELECT id FROM tags WHERE team_id = $1 AND name = $2")
                    .bind(self.team_id)
                    .bind(&name)
                    .fetch_optional(&mut *conn)
                    .await?;
            let tag_id = match existing {
                Some(id) => id,
                None => {
                    let uuid = free_uuid(conn, "tags", uuid_or_new(t)).await?;
                    self.insert(
                        conn,
                        "tags",
                        obj(json!({"uuid": uuid, "name": name, "team_id": self.team_id})),
                    )
                    .await?
                }
            };
            sqlx::query(
                "INSERT INTO taggables (tag_id, taggable_id, taggable_type) SELECT $1, $2, $3 \
                 WHERE NOT EXISTS (SELECT 1 FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3)",
            )
            .bind(tag_id)
            .bind(resource.id)
            .bind(resource.morph)
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }

    /// `importedBackupS3StorageIds`.
    fn backup_s3_ids(&self, backup: &Value) -> Vec<i64> {
        let mut ids = Vec::new();
        let first = dg(backup, "s3_storage_uuid").into_iter();
        for uuid in first.chain(list(backup, "s3_storage_uuids")) {
            if let Some(id) = uuid.as_str().and_then(|u| self.m.s3.get(u))
                && !ids.contains(id)
            {
                ids.push(*id);
            }
        }
        ids
    }

    /// `warnAboutMissingS3Storage`.
    fn warn_missing_s3(&mut self, backup: &Value, s3: Option<i64>, of: &str) {
        if !truthy(dg(backup, "had_s3_storage")) || s3.is_some() {
            return;
        }
        self.m.warnings.push(if truthy(dg(backup, "save_s3")) {
            format!(
                "Backup of {of}: S3 upload was turned off because its S3 storage was not found."
            )
        } else {
            format!("Backup of {of}: its S3 storage was not found; select it again.")
        });
    }

    /// `syncS3Storages`.
    async fn sync_s3(
        &mut self,
        conn: &mut PgConnection,
        table: &str,
        column: &str,
        id: i64,
        s3_ids: &[i64],
    ) -> R<()> {
        for s3 in s3_ids {
            sqlx::query(&format!(
                "INSERT INTO {table} ({column}, s3_storage_id, created_at, updated_at) VALUES ($1, $2, $3, $3)"
            ))
            .bind(id)
            .bind(s3)
            .bind(self.now)
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }

    /// `importScheduledBackups`.
    async fn scheduled_backups(
        &mut self,
        conn: &mut PgConnection,
        backups: Vec<&Value>,
        database: &Res,
    ) -> R<()> {
        for b in backups {
            let uuid = free_uuid(conn, "scheduled_database_backups", uuid_or_new(b)).await?;
            let s3_ids = self.backup_s3_ids(b);
            let s3 = s3_ids.first().copied();
            self.warn_missing_s3(b, s3, &format!("database {}", database.name));
            let id = self
                .insert(
                    conn,
                    "scheduled_database_backups",
                    obj(json!({
                        "uuid": uuid,
                        "team_id": self.team_id,
                        "description": get_or_null(b, "description"),
                        "enabled": bool_of(b, "enabled", true),
                        "save_s3": truthy(dg(b, "save_s3")) && s3.is_some(),
                        "frequency": get_or_null(b, "frequency"),
                        "databases_to_backup": get_or_null(b, "databases_to_backup"),
                        "dump_all": bool_of(b, "dump_all", false),
                        "database_backup_retention_amount_locally": dg(b, "database_backup_retention_amount_locally").cloned().unwrap_or(json!(0)),
                        "database_backup_retention_days_locally": get_or_null(b, "database_backup_retention_days_locally"),
                        "database_backup_retention_max_storage_locally": get_or_null(b, "database_backup_retention_max_storage_locally"),
                        "database_backup_retention_amount_s3": get_or_null(b, "database_backup_retention_amount_s3"),
                        "database_backup_retention_days_s3": get_or_null(b, "database_backup_retention_days_s3"),
                        "database_backup_retention_max_storage_s3": get_or_null(b, "database_backup_retention_max_storage_s3"),
                        "timeout": get_or_null(b, "timeout"),
                        "disable_local_backup": bool_of(b, "disable_local_backup", false),
                        "missing_backup_notification_days": int_or(b, "missing_backup_notification_days", 0),
                        "s3_storage_id": s3,
                        "database_type": database.morph,
                        "database_id": database.id,
                    })),
                )
                .await?;
            self.sync_s3(
                conn,
                "scheduled_database_backup_s3_storage",
                "scheduled_database_backup_id",
                id,
                &s3_ids,
            )
            .await?;
        }
        Ok(())
    }

    /// `importedIntegrationTokenId`.
    fn imported_integration_token(&mut self, uuid: Option<&Value>, warning: &str) -> Option<i64> {
        let uuid = uuid.and_then(Value::as_str).filter(|u| !u.is_empty())?;
        let id = self.m.integration_tokens.get(uuid).copied();
        if id.is_none() {
            self.m.warnings.push(warning.to_owned());
        }
        id
    }

    /// `importSecretManagerLink`.
    async fn secret_manager_link(
        &mut self,
        conn: &mut PgConnection,
        payload: Option<&Value>,
        resource: &Res,
        label: &str,
    ) -> R<()> {
        let Some(payload) = payload.filter(|p| p.is_object() || p.is_array()) else {
            return Ok(());
        };
        let Some(token) = self.imported_integration_token(
            dg(payload, "integration_token_uuid"),
            &format!("{label}: its secret manager token was not found on this instance; link the secret manager source again."),
        ) else {
            return Ok(());
        };
        let uuid = free_uuid(conn, "secret_manager_links", uuid_or_new(payload)).await?;
        self.insert(
            conn,
            "secret_manager_links",
            obj(json!({
                "uuid": uuid,
                "resourceable_type": resource.morph,
                "resourceable_id": resource.id,
                "integration_token_id": token,
                "settings": get_or_null(payload, "settings"),
            })),
        )
        .await?;
        Ok(())
    }

    /// `importPreviews`.
    async fn previews(
        &mut self,
        conn: &mut PgConnection,
        previews: Vec<&Value>,
        application: &Res,
    ) -> R<()> {
        for p in previews {
            let mut uuid = self.preserved_uuid(p);
            let mut preview: Option<i64> =
                sqlx::query_scalar("SELECT id FROM application_previews WHERE uuid = $1")
                    .bind(&uuid)
                    .fetch_optional(&mut *conn)
                    .await?;
            if let Some(id) = preview
                && !self.preview_in_team(conn, id).await?
            {
                return Err(TransferError::Validation(vec![(
                    "previews".to_owned(),
                    vec!["An imported preview UUID cannot be used on this instance.".to_owned()],
                )]));
            }
            if preview.is_none() && !dg(p, "pull_request_id").is_none_or(Value::is_null) {
                preview = sqlx::query_scalar(
                    "SELECT id FROM application_previews WHERE application_id = $1 AND pull_request_id = $2",
                )
                .bind(application.id)
                .bind(as_int(dg(p, "pull_request_id").unwrap_or(&Value::Null)))
                .fetch_optional(&mut *conn)
                .await?;
            }
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM application_previews WHERE uuid = $1 AND ($2::bigint IS NULL OR id <> $2))",
            )
            .bind(&uuid)
            .bind(preview)
            .fetch_one(&mut *conn)
            .await?;
            if taken {
                uuid = new_public_id();
            }
            let fqdn = dg(p, "fqdn").cloned().unwrap_or(Value::Null);
            if !blank(Some(&fqdn)) {
                let conflicts: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM application_previews WHERE fqdn = $1 AND ($2::bigint IS NULL OR id <> $2)",
                )
                .bind(text(&fqdn))
                .bind(preview)
                .fetch_all(&mut *conn)
                .await?;
                for c in &conflicts {
                    if !self.preview_in_team(conn, *c).await? {
                        return Err(TransferError::Validation(vec![(
                            "previews".to_owned(),
                            vec!["An imported preview domain conflicts with a preview outside the target team.".to_owned()],
                        )]));
                    }
                }
                for c in conflicts {
                    sqlx::query("UPDATE application_previews SET fqdn = NULL WHERE id = $1")
                        .bind(c)
                        .execute(&mut *conn)
                        .await?;
                }
            }
            let status = if self.adopt {
                or(dg(p, "status"), "exited")
            } else {
                json!("exited")
            };
            let mut attrs = obj(json!({
                "application_id": application.id,
                "pull_request_id": get_or_null(p, "pull_request_id"),
                "pull_request_html_url": or(dg(p, "pull_request_html_url"), ""),
                "pull_request_issue_comment_id": get_or_null(p, "pull_request_issue_comment_id"),
                "fqdn": fqdn,
                "status": status,
                "git_type": get_or_null(p, "git_type"),
                "docker_compose_domains": get_or_null(p, "docker_compose_domains"),
                "docker_registry_image_tag": get_or_null(p, "docker_registry_image_tag"),
                "domain_port_overrides": get_or_null(p, "domain_port_overrides"),
                "max_restart_count": int_or(p, "max_restart_count", 0),
            }));
            let (id, was_created) = if let Some(id) = preview {
                attrs.insert("deleted_at".into(), Value::Null);
                if self.preserve && !blank(dg(p, "uuid")) {
                    attrs.insert("uuid".into(), json!(uuid));
                }
                self.update(conn, "application_previews", "id", id, attrs)
                    .await?;
                (id, false)
            } else {
                attrs.insert("uuid".into(), json!(uuid));
                (
                    self.insert(conn, "application_previews", attrs).await?,
                    true,
                )
            };
            let res = Res {
                id,
                uuid: uuid.clone(),
                morph: PREVIEW_MORPH,
                name: String::new(),
                environment_id: 0,
            };
            let key = dg(p, "uuid").map_or_else(|| uuid.clone(), text);
            self.m.previews.insert(key, res.clone());
            // Avoid duplicating volumes when re-importing the same preview.
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
            )
            .bind(PREVIEW_MORPH)
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
            if was_created || count == 0 {
                self.persistent_storages(conn, list(p, "persistent_storages"), &res)
                    .await?;
            }
        }
        Ok(())
    }

    /// `previewBelongsToTeam`.
    async fn preview_in_team(&self, conn: &mut PgConnection, preview_id: i64) -> R<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM application_previews p JOIN applications a ON a.id = p.application_id \
             JOIN environments e ON e.id = a.environment_id JOIN projects pr ON pr.id = e.project_id \
             WHERE p.id = $1 AND pr.team_id = $2)",
        )
        .bind(preview_id)
        .bind(self.team_id)
        .fetch_one(&mut *conn)
        .await?)
    }

    /// `importResourceLinks`.
    async fn resource_links(
        &mut self,
        conn: &mut PgConnection,
        links: Vec<&Value>,
        environment_id: i64,
        environment_name: &str,
    ) -> R<()> {
        for link in links {
            let source = self.linked_resource(dg(link, "source"));
            let target = self.linked_resource(dg(link, "target"));
            let valid = match (&source, &target) {
                (Some(s), Some(t)) => {
                    !(s.id == t.id && s.morph == t.morph)
                        && s.environment_id == environment_id
                        && t.environment_id == environment_id
                }
                _ => false,
            };
            if !valid {
                self.m.warnings.push(format!(
                    "A link between resources on the canvas of environment {environment_name} was not imported, because one of its resources was not imported."
                ));
                continue;
            }
            let (source, target) = (source.expect("checked"), target.expect("checked"));
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM resource_links WHERE environment_id = $1 AND source_type = $2 AND source_id = $3 AND target_type = $4 AND target_id = $5)",
            )
            .bind(environment_id)
            .bind(source.morph)
            .bind(source.id)
            .bind(target.morph)
            .bind(target.id)
            .fetch_one(&mut *conn)
            .await?;
            if found {
                continue;
            }
            let label = dg(link, "label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.chars().take(64).collect::<String>());
            let mut uuid = str_of(link, "uuid");
            if !self.preserve || uuid.is_empty() || exists(conn, "resource_links", &uuid).await? {
                uuid = new_public_id();
            }
            self.insert(
                conn,
                "resource_links",
                obj(json!({
                    "uuid": uuid,
                    "environment_id": environment_id,
                    "source_type": source.morph,
                    "source_id": source.id,
                    "target_type": target.morph,
                    "target_id": target.id,
                    "label": label,
                    "created_by": null,
                })),
            )
            .await?;
        }
        Ok(())
    }

    /// `linkedResource`.
    fn linked_resource(&self, end: Option<&Value>) -> Option<Res> {
        let end = end?;
        let uuid = str_of(end, "uuid");
        match dg(end, "kind").and_then(Value::as_str) {
            Some("application") => self.m.applications.get(&uuid).cloned(),
            Some("database") => self.m.databases.get(&uuid).cloned(),
            Some("service") => self.m.services.get(&uuid).cloned(),
            _ => None,
        }
    }

    /// `importCanvasPositions`.
    async fn canvas_positions(
        &mut self,
        conn: &mut PgConnection,
        positions: Option<&Value>,
        environment_id: i64,
    ) -> R<()> {
        let Some(Value::Object(positions)) = positions else {
            return Ok(());
        };
        let mut imported = Map::new();
        for (uuid, position) in positions {
            let resource = self
                .m
                .applications
                .get(uuid)
                .or_else(|| self.m.databases.get(uuid))
                .or_else(|| self.m.services.get(uuid));
            if let Some(r) = resource {
                imported.insert(r.uuid.clone(), position.clone());
            }
        }
        if imported.is_empty() {
            return Ok(());
        }
        let current: Option<Value> =
            sqlx::query_scalar("SELECT canvas_positions FROM environments WHERE id = $1")
                .bind(environment_id)
                .fetch_one(&mut *conn)
                .await?;
        let mut merged = current
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        merged.extend(imported);
        self.update(
            conn,
            "environments",
            "id",
            environment_id,
            obj(json!({"canvas_positions": merged})),
        )
        .await
    }

    /// `linkSqliteVolumes`.
    async fn link_sqlite(&mut self, conn: &mut PgConnection) -> R<Vec<String>> {
        let mut warnings = Vec::new();
        for (volume, name, sqlite_uuid) in std::mem::take(&mut self.m.pending_sqlite) {
            let Some(sqlite) = self
                .m
                .databases
                .get(&sqlite_uuid)
                .filter(|d| d.morph == "App\\Models\\StandaloneSqlite")
            else {
                warnings.push(format!(
                    "Volume {name} mounts a SQLite database that is not part of this transfer. Coolify does not protect it when that database or its applications are deleted."
                ));
                continue;
            };
            let id = sqlite.id;
            self.update(
                conn,
                "local_persistent_volumes",
                "id",
                volume,
                obj(json!({"standalone_sqlite_id": id})),
            )
            .await?;
        }
        Ok(warnings)
    }

    /// `importGithubApp`, `importGitlabApp`, `importGiteaApp`: the columns differ.
    async fn git_app(
        &mut self,
        conn: &mut PgConnection,
        table: &str,
        payload: &Value,
        mut attrs: Map<String, Value>,
    ) -> R<(i64, String)> {
        let uuid = self.preserved_uuid(payload);
        let existing: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT id FROM {table} WHERE uuid = $1 AND team_id = $2"
        ))
        .bind(&uuid)
        .bind(self.team_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(id) = existing {
            return Ok((id, uuid));
        }
        let uuid = free_uuid(conn, table, uuid).await?;
        let key = dg(payload, "private_key_uuid")
            .and_then(Value::as_str)
            .and_then(|k| self.m.private_keys.get(k).copied());
        attrs.insert("uuid".into(), json!(uuid));
        attrs.insert("team_id".into(), json!(self.team_id));
        attrs.insert("private_key_id".into(), json!(key));
        attrs.insert("is_system_wide".into(), json!(false));
        attrs.insert("is_public".into(), bool_of(payload, "is_public", false));
        let id = self.insert(conn, table, attrs).await?;
        Ok((id, uuid))
    }

    fn copy(&self, payload: &Value, plain: &[&str], encrypted: &[&str]) -> Map<String, Value> {
        let mut attrs = Map::new();
        for f in plain {
            attrs.insert((*f).to_owned(), get_or_null(payload, f));
        }
        for f in encrypted {
            attrs.insert((*f).to_owned(), self.encrypt(dg(payload, f)));
        }
        attrs
    }

    /// The credentials in front of the server: keys, git sources, S3, tokens.
    async fn shared_dependencies(
        &mut self,
        conn: &mut PgConnection,
        bundle: &Value,
        created: &mut Map<String, Value>,
    ) -> R<()> {
        let mut key_payloads = list(bundle, "private_keys");
        if key_payloads.is_empty() && truthy(dg(bundle, "private_key")) {
            key_payloads = vec![dg(bundle, "private_key").expect("checked")];
        }
        for payload in key_payloads {
            let (id, uuid) = self.private_key(conn, payload).await?;
            self.m
                .private_keys
                .insert(dg(payload, "uuid").map_or_else(|| uuid.clone(), text), id);
            self.m.private_keys.insert(uuid, id);
            bump(created, "private_keys", 1);
        }
        for p in list(bundle, "github_apps") {
            if truthy(dg(p, "is_system_wide")) {
                continue;
            }
            let mut attrs = self.copy(
                p,
                &[
                    "organization",
                    "custom_user",
                    "custom_port",
                    "app_id",
                    "installation_id",
                    "client_id",
                    "contents",
                    "metadata",
                    "pull_requests",
                    "administration",
                    "organization_self_hosted_runners",
                    "actions",
                    "webhook_events",
                    "runner_group_id",
                ],
                &["client_secret", "webhook_secret"],
            );
            attrs.insert("name".into(), or(dg(p, "name"), "Imported GitHub App"));
            attrs.insert(
                "api_url".into(),
                or(dg(p, "api_url"), "https://api.github.com"),
            );
            attrs.insert(
                "html_url".into(),
                or(dg(p, "html_url"), "https://github.com"),
            );
            let (id, uuid) = self.git_app(conn, "github_apps", p, attrs).await?;
            self.m
                .github
                .insert(dg(p, "uuid").map_or_else(|| uuid.clone(), text), id);
            self.m.github.insert(uuid, id);
            bump(created, "github_apps", 1);
        }
        for p in list(bundle, "gitlab_apps") {
            if truthy(dg(p, "is_system_wide")) {
                continue;
            }
            let mut attrs = self.copy(
                p,
                &[
                    "organization",
                    "api_url",
                    "html_url",
                    "custom_port",
                    "custom_user",
                    "app_id",
                    "oauth_id",
                    "client_id",
                    "expires_at",
                    "redirect_uri",
                    "group_name",
                    "public_key",
                    "webhook_token",
                    "deploy_key_id",
                ],
                &[
                    "app_secret",
                    "client_secret",
                    "access_token",
                    "refresh_token",
                ],
            );
            attrs.insert("name".into(), or(dg(p, "name"), "Imported GitLab App"));
            let (id, uuid) = self.git_app(conn, "gitlab_apps", p, attrs).await?;
            self.m
                .gitlab
                .insert(dg(p, "uuid").map_or_else(|| uuid.clone(), text), id);
            self.m.gitlab.insert(uuid, id);
            bump(created, "gitlab_apps", 1);
        }
        for p in list(bundle, "gitea_apps") {
            if truthy(dg(p, "is_system_wide")) {
                continue;
            }
            let mut attrs = self.copy(
                p,
                &[
                    "organization",
                    "api_url",
                    "html_url",
                    "client_id",
                    "expires_at",
                    "redirect_uri",
                ],
                &[
                    "client_secret",
                    "access_token",
                    "refresh_token",
                    "webhook_secret",
                ],
            );
            attrs.insert("name".into(), or(dg(p, "name"), "Imported Gitea App"));
            attrs.insert(
                "custom_port".into(),
                dg(p, "custom_port")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!(22)),
            );
            attrs.insert(
                "custom_user".into(),
                dg(p, "custom_user")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!("git")),
            );
            let (id, uuid) = self.git_app(conn, "gitea_apps", p, attrs).await?;
            self.m
                .gitea
                .insert(dg(p, "uuid").map_or_else(|| uuid.clone(), text), id);
            self.m.gitea.insert(uuid, id);
            bump(created, "gitea_apps", 1);
        }
        for p in list(bundle, "s3_storages") {
            let (id, uuid) = self.s3_storage(conn, p).await?;
            self.m
                .s3
                .insert(dg(p, "uuid").map_or_else(|| uuid.clone(), text), id);
            self.m.s3.insert(uuid, id);
            bump(created, "s3_storages", 1);
        }
        for p in list(bundle, "cloud_provider_tokens") {
            let uuid = self.preserved_uuid(p);
            let existing = self.team_row(conn, "cloud_provider_tokens", &uuid).await?;
            let (id, final_uuid) = if let Some(id) = existing {
                (id, uuid)
            } else {
                let uuid = free_uuid(conn, "cloud_provider_tokens", uuid).await?;
                let attrs = obj(json!({
                    "uuid": uuid,
                    "team_id": self.team_id,
                    "provider": get_or_null(p, "provider"),
                    "token": self.encrypt(dg(p, "token")),
                    "name": or(dg(p, "name"), "Imported cloud token"),
                    "description": get_or_null(p, "description"),
                }));
                (
                    self.insert(conn, "cloud_provider_tokens", attrs).await?,
                    uuid,
                )
            };
            self.m
                .cloud_tokens
                .insert(dg(p, "uuid").map_or_else(|| final_uuid.clone(), text), id);
            self.m.cloud_tokens.insert(final_uuid, id);
            bump(created, "cloud_provider_tokens", 1);
        }
        for p in list(bundle, "integration_tokens") {
            let uuid = self.preserved_uuid(p);
            let existing = self.team_row(conn, "integration_tokens", &uuid).await?;
            let (id, final_uuid) = if let Some(id) = existing {
                (id, uuid)
            } else {
                let uuid = free_uuid(conn, "integration_tokens", uuid).await?;
                let attrs = obj(json!({
                    "uuid": uuid,
                    "team_id": self.team_id,
                    "provider": str_of(p, "provider"),
                    "name": or(dg(p, "name"), "Imported integration token"),
                    "token": self.enc.encrypt_string(&str_of(p, "token")),
                    "capabilities": dg(p, "capabilities").filter(|v| !v.is_null()).cloned().unwrap_or(json!([])),
                    "metadata": get_or_null(p, "metadata"),
                }));
                (self.insert(conn, "integration_tokens", attrs).await?, uuid)
            };
            self.m
                .integration_tokens
                .insert(dg(p, "uuid").map_or_else(|| final_uuid.clone(), text), id);
            self.m.integration_tokens.insert(final_uuid, id);
            bump(created, "integration_tokens", 1);
        }
        Ok(())
    }

    async fn team_row(&self, conn: &mut PgConnection, table: &str, uuid: &str) -> R<Option<i64>> {
        Ok(sqlx::query_scalar(&format!(
            "SELECT id FROM {table} WHERE uuid = $1 AND team_id = $2"
        ))
        .bind(uuid)
        .bind(self.team_id)
        .fetch_optional(&mut *conn)
        .await?)
    }

    /// `importS3Storage`.
    async fn s3_storage(&mut self, conn: &mut PgConnection, p: &Value) -> R<(i64, String)> {
        let uuid = self.preserved_uuid(p);
        if let Some(id) = self.team_row(conn, "s3_storages", &uuid).await? {
            return Ok((id, uuid));
        }
        let trimmed = |key: &str| {
            dg(p, key)
                .filter(|v| crate::serialize::php_truthy(v))
                .map(|v| text(v).trim().to_owned())
        };
        let by_identity = sqlx::query(
            "SELECT id, uuid FROM s3_storages WHERE team_id = $1 AND endpoint IS NOT DISTINCT FROM $2 AND bucket IS NOT DISTINCT FROM $3 LIMIT 1",
        )
        .bind(self.team_id)
        .bind(dg(p, "endpoint").map(text))
        .bind(dg(p, "bucket").map(text))
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(r) = by_identity {
            return Ok((r.try_get("id")?, r.try_get("uuid")?));
        }
        let uuid = free_uuid(conn, "s3_storages", uuid).await?;
        let secret = |key: &str| {
            dg(p, key)
                .filter(|v| !v.is_null())
                .map(|v| json!(self.enc.encrypt_string(text(v).trim())))
                .unwrap_or(Value::Null)
        };
        let attrs = obj(json!({
            "uuid": uuid,
            "team_id": self.team_id,
            "name": or(dg(p, "name"), "Imported S3"),
            "description": get_or_null(p, "description"),
            "region": trimmed("region"),
            "key": secret("key"),
            "secret": secret("secret"),
            "bucket": trimmed("bucket"),
            "endpoint": trimmed("endpoint"),
            "is_usable": bool_of(p, "is_usable", true),
        }));
        let id = self.insert(conn, "s3_storages", attrs).await?;
        Ok((id, uuid))
    }

    /// `importDnsZones`.
    async fn dns_zones(&mut self, conn: &mut PgConnection, zones: Vec<&Value>) -> R<()> {
        for p in zones {
            let Some(token) = self
                .m
                .integration_tokens
                .get(&str_of(p, "integration_token_uuid"))
                .copied()
            else {
                continue;
            };
            let provider_zone = str_of(p, "provider_zone_id");
            let existing: Option<(i64, String)> = sqlx::query_as(
                "SELECT id, uuid FROM dns_provider_zones WHERE integration_token_id = $1 AND provider_zone_id = $2",
            )
            .bind(token)
            .bind(&provider_zone)
            .fetch_optional(&mut *conn)
            .await?;
            let (id, final_uuid) = if let Some(z) = existing {
                z
            } else {
                let uuid = free_uuid(conn, "dns_provider_zones", self.preserved_uuid(p)).await?;
                let attrs = obj(json!({
                    "uuid": uuid,
                    "integration_token_id": token,
                    "provider_zone_id": provider_zone,
                    "name": str_of(p, "name").to_lowercase(),
                    "account_id": get_or_null(p, "account_id"),
                    "account_name": get_or_null(p, "account_name"),
                }));
                (self.insert(conn, "dns_provider_zones", attrs).await?, uuid)
            };
            let key = dg(p, "uuid").map_or(final_uuid, text);
            self.m.dns_zones.insert(key, (id, token));
        }
        Ok(())
    }

    /// `importManagedDnsRecords`.
    async fn managed_dns_records(
        &mut self,
        conn: &mut PgConnection,
        records: Vec<&Value>,
        server: &Res,
    ) -> R<i64> {
        let mut total = 0;
        for p in records {
            let name = str_of(p, "name");
            let zone = self
                .m
                .dns_zones
                .get(&str_of(p, "dns_provider_zone_uuid"))
                .copied();
            let mut resources = Vec::new();
            for reference in list(p, "references") {
                let uuid = str_of(reference, "uuid");
                let r = match dg(reference, "kind").and_then(Value::as_str) {
                    Some("application") => self.m.applications.get(&uuid).cloned(),
                    Some("preview") => self.m.previews.get(&uuid).cloned(),
                    Some("service_application") => self.m.service_applications.get(&uuid).cloned(),
                    Some("server") => Some(server.clone()),
                    _ => None,
                };
                if let Some(r) = r {
                    resources.push(r);
                }
            }
            let Some((zone_id, token_id)) = zone.filter(|_| !resources.is_empty()) else {
                self.m.warnings.push(format!(
                    "DNS record {name}: its DNS zone or resources were not imported, so this instance does not manage it."
                ));
                continue;
            };
            let provider_record = str_of(p, "provider_record_id");
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM managed_dns_records WHERE dns_provider_zone_id = $1 AND provider_record_id = $2",
            )
            .bind(zone_id)
            .bind(&provider_record)
            .fetch_optional(&mut *conn)
            .await?;
            let record_id = if let Some(id) = existing {
                id
            } else {
                let uuid = free_uuid(conn, "managed_dns_records", self.preserved_uuid(p)).await?;
                let previous = dg(p, "ownership_comment")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                // Without the source comment there is no proof of ownership at the provider.
                let owned = truthy(dg(p, "owned")) && !previous.is_empty();
                let id = self
                    .insert(
                        conn,
                        "managed_dns_records",
                        obj(json!({
                            "uuid": uuid,
                            "team_id": self.team_id,
                            "integration_token_id": token_id,
                            "dns_provider_zone_id": zone_id,
                            "provider_record_id": provider_record,
                            "type": str_of(p, "type"),
                            "name": name,
                            "content": str_of(p, "content"),
                            "owned": owned,
                        })),
                    )
                    .await?;
                if owned {
                    self.m.pending_dns.push((id, name.clone(), previous));
                }
                total += 1;
                id
            };
            for r in resources {
                sqlx::query(
                    "INSERT INTO managed_dns_record_references (managed_dns_record_id, resource_type, resource_id, created_at, updated_at) \
                     SELECT $1, $2, $3, $4, $4 WHERE NOT EXISTS (SELECT 1 FROM managed_dns_record_references WHERE managed_dns_record_id = $1 AND resource_type = $2 AND resource_id = $3)",
                )
                .bind(record_id)
                .bind(r.morph)
                .bind(r.id)
                .bind(self.now)
                .execute(&mut *conn)
                .await?;
            }
        }
        Ok(total)
    }

    /// `importSslCertificates`.
    async fn ssl_certificates(
        &mut self,
        conn: &mut PgConnection,
        certs: Vec<&Value>,
        server_id: i64,
    ) -> R<i64> {
        let mut total = 0;
        for c in certs {
            let uuid = str_of(c, "resource_uuid");
            let kind = dg(c, "resource_kind").and_then(Value::as_str);
            let resource = match kind {
                Some("application") => self.m.applications.get(&uuid).cloned(),
                Some("service") => self.m.services.get(&uuid).cloned(),
                _ => self.m.databases.get(&uuid).cloned(),
            };
            let valid_until = dg(c, "valid_until")
                .filter(|v| crate::serialize::php_truthy(v))
                .cloned()
                .unwrap_or_else(|| {
                    json!(
                        (Utc::now() + chrono::Months::new(12))
                            .format("%Y-%m-%dT%H:%M:%S")
                            .to_string()
                    )
                });
            let attrs = obj(json!({
                "ssl_certificate": self.encrypt(dg(c, "ssl_certificate")),
                "ssl_private_key": self.encrypt(dg(c, "ssl_private_key")),
                "configuration_dir": get_or_null(c, "configuration_dir"),
                "mount_path": get_or_null(c, "mount_path"),
                "common_name": or(dg(c, "common_name"), "imported"),
                "subject_alternative_names": get_or_null(c, "subject_alternative_names"),
                "valid_until": valid_until,
                "is_ca_certificate": bool_of(c, "is_ca_certificate", false),
                "server_id": server_id,
                "resource_type": resource.as_ref().map(|r| r.morph),
                "resource_id": resource.as_ref().map(|r| r.id),
            }));
            self.insert(conn, "ssl_certificates", attrs).await?;
            total += 1;
        }
        Ok(total)
    }

    /// `importVolumeBackups`.
    async fn volume_backups(&mut self, conn: &mut PgConnection, backups: Vec<&Value>) -> R<i64> {
        let mut total = 0;
        for b in backups {
            let Some((volume, morph, label)) =
                self.m.volumes.get(&str_of(b, "backupable_uuid")).cloned()
            else {
                continue;
            };
            let uuid = free_uuid(conn, "scheduled_volume_backups", uuid_or_new(b)).await?;
            let s3_ids = self.backup_s3_ids(b);
            let s3 = s3_ids.first().copied();
            self.warn_missing_s3(b, s3, &format!("volume {label}"));
            let id = self
                .insert(
                    conn,
                    "scheduled_volume_backups",
                    obj(json!({
                        "uuid": uuid,
                        "backupable_type": morph,
                        "backupable_id": volume,
                        "team_id": self.team_id,
                        "s3_storage_id": s3,
                        "frequency": get_or_null(b, "frequency"),
                        "enabled": bool_of(b, "enabled", true),
                        "save_s3": truthy(dg(b, "save_s3")) && s3.is_some(),
                        "disable_local_backup": bool_of(b, "disable_local_backup", false),
                        "stop_during_backup": bool_of(b, "stop_during_backup", false),
                        "retention_amount_locally": get_or_null(b, "retention_amount_locally"),
                        "retention_days_locally": get_or_null(b, "retention_days_locally"),
                        "retention_max_storage_locally": get_or_null(b, "retention_max_storage_locally"),
                        "retention_amount_s3": get_or_null(b, "retention_amount_s3"),
                        "retention_days_s3": get_or_null(b, "retention_days_s3"),
                        "retention_max_storage_s3": get_or_null(b, "retention_max_storage_s3"),
                        "timeout": get_or_null(b, "timeout"),
                        "missing_backup_notification_days": int_or(b, "missing_backup_notification_days", 0),
                    })),
                )
                .await?;
            self.sync_s3(
                conn,
                "scheduled_volume_backup_s3_storage",
                "scheduled_volume_backup_id",
                id,
                &s3_ids,
            )
            .await?;
            total += 1;
        }
        Ok(total)
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn bump(created: &mut Map<String, Value>, key: &str, by: i64) {
    let n = created.get(key).map_or(0, as_int);
    created.insert(key.to_owned(), json!(n + by));
}

/// `Str::random`.
fn random_string(len: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn morph_of(model: &str) -> &'static str {
    match model {
        "StandalonePostgresql" => "App\\Models\\StandalonePostgresql",
        "StandaloneMysql" => "App\\Models\\StandaloneMysql",
        "StandaloneMariadb" => "App\\Models\\StandaloneMariadb",
        "StandaloneMongodb" => "App\\Models\\StandaloneMongodb",
        "StandaloneRedis" => "App\\Models\\StandaloneRedis",
        "StandaloneKeydb" => "App\\Models\\StandaloneKeydb",
        "StandaloneDragonfly" => "App\\Models\\StandaloneDragonfly",
        "StandaloneClickhouse" => "App\\Models\\StandaloneClickhouse",
        _ => "App\\Models\\StandaloneSqlite",
    }
}

/// The `status` setter of `Application` and the standalone databases.
fn normalize_status(value: &str) -> String {
    if let Some((status, rest)) = value.split_once('(') {
        let health = rest.split(')').next().unwrap_or("").trim();
        format!("{}:{health}", status.trim())
    } else if let Some((status, health)) = value.split_once(':') {
        format!("{}:{}", status.trim(), health.trim())
    } else {
        format!("{value}:unhealthy")
    }
}

/// `resolveDestination`.
fn resolve_destination(
    uuid: Option<&Value>,
    map: &[(String, i64, &'static str)],
) -> R<(String, i64, &'static str)> {
    if let Some(u) = uuid.and_then(Value::as_str)
        && let Some(d) = map.iter().find(|(k, _, _)| k == u)
    {
        return Ok(d.clone());
    }
    map.first().cloned().ok_or_else(|| {
        TransferError::Message("No destination available for imported resource.".into())
    })
}

fn instance_url_sync() -> String {
    std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost".to_owned())
}

/// The validation of `importGithubRunnerConfig`: the first error.
fn runner_config_problem(payload: &Value) -> Option<String> {
    static LABEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9._-]{0,99}\z").expect("regex"));
    let labels = dg(payload, "labels");
    if blank(labels) {
        return Some("The labels field is required.".into());
    }
    match labels {
        Some(Value::Array(a)) => {
            for (i, l) in a.iter().enumerate() {
                match l.as_str() {
                    None => return Some(format!("The labels.{i} field must be a string.")),
                    Some(s) if !LABEL.is_match(s) => {
                        return Some(format!("The labels.{i} field format is invalid."));
                    }
                    _ => {}
                }
            }
        }
        _ => return Some("The labels field must be an array.".into()),
    }
    for (key, label, min, max) in [("max_runners", "max runners", 1, 32)] {
        if let Some(p) = int_problem(payload, key, label, min, max) {
            return Some(p);
        }
    }
    if blank(dg(payload, "docker_mode")) {
        return Some("The docker mode field is required.".into());
    }
    let mode = str_of(payload, "docker_mode");
    if !matches!(mode.as_str(), "none" | "socket" | "dind") {
        return Some("The selected docker mode is invalid.".into());
    }
    for (key, label, min, max) in [
        ("capacity_wait_timeout", "capacity wait timeout", 1, 1440),
        ("idle_timeout", "idle timeout", 1, 1440),
        ("job_timeout", "job timeout", 1, 7200),
    ] {
        if let Some(p) = int_problem(payload, key, label, min, max) {
            return Some(p);
        }
    }
    None
}

fn int_problem(payload: &Value, key: &str, label: &str, min: i64, max: i64) -> Option<String> {
    let v = dg(payload, key);
    if blank(v) {
        return Some(format!("The {label} field is required."));
    }
    let n = match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    };
    let Some(n) = n else {
        return Some(format!("The {label} field must be an integer."));
    };
    if n < min {
        return Some(format!("The {label} field must be at least {min}."));
    }
    if n > max {
        return Some(format!("The {label} field must not be greater than {max}."));
    }
    None
}

/// `assertValidServerPayload`.
fn assert_valid_server_payload(server: &Value) -> R<()> {
    static USER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+$").expect("regex"));
    let mut errors: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(n) = dg(server, "name").filter(|v| !v.is_null()) {
        match n.as_str() {
            None => errors.push((
                "name".into(),
                vec!["The name field must be a string.".into()],
            )),
            Some(s) if s.chars().count() > 255 => errors.push((
                "name".into(),
                vec!["The name field must not be greater than 255 characters.".into()],
            )),
            _ => {}
        }
    }
    match dg(server, "ip") {
        v if blank(v) => {
            errors.push(("ip".into(), vec!["The server.ip field is required.".into()]))
        }
        Some(v) if !v.is_string() => {
            errors.push((
                "ip".into(),
                vec!["The server.ip field must be a string.".into()],
            ));
        }
        Some(v) => {
            if let Some(m) = crate::routes::servers::valid_server_ip(v) {
                errors.push(("ip".into(), vec![m.replace(":attribute", "server.ip")]));
            }
        }
        None => {}
    }
    match dg(server, "port") {
        v if blank(v) => errors.push((
            "port".into(),
            vec!["The server.port field is required.".into()],
        )),
        Some(v) => {
            let n = match v {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.parse().ok(),
                _ => None,
            };
            match n {
                None => errors.push((
                    "port".into(),
                    vec!["The server.port field must be an integer.".into()],
                )),
                Some(p) if !(1..=65535).contains(&p) => errors.push((
                    "port".into(),
                    vec!["The server.port field must be between 1 and 65535.".into()],
                )),
                _ => {}
            }
        }
        None => {}
    }
    if let Some(u) = dg(server, "user").filter(|v| !v.is_null()) {
        match u.as_str() {
            None => errors.push((
                "user".into(),
                vec!["The server.user field must be a string.".into()],
            )),
            Some(s) if !USER.is_match(s) => {
                errors.push((
                    "user".into(),
                    vec!["The server.user field format is invalid.".into()],
                ));
            }
            _ => {}
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(TransferError::Validation(errors))
    }
}

/// The checks of `importChecks`, as `{field, label, ok, message}`.
pub async fn import_checks(
    state: &AppState,
    server_uuid: &str,
    server_ip: &str,
    team_id: i64,
    preserve: bool,
) -> Result<Vec<Value>, TransferError> {
    let pool = state.db.pool();
    let team_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1)")
        .bind(team_id)
        .fetch_one(pool)
        .await?;
    let archive = if preserve {
        replaceable_archive(state, server_uuid, team_id).await?
    } else {
        None
    };
    let limit_reached = !team_exists;
    let mut uuid_message = None;
    if preserve && archive.is_none() {
        let row: Option<(Option<Value>, Option<NaiveDateTime>)> =
            sqlx::query_as("SELECT server_metadata, deleted_at FROM servers WHERE uuid = $1")
                .bind(server_uuid)
                .fetch_optional(pool)
                .await?;
        uuid_message = row.map(|(meta, deleted)| {
            let transferred = meta.as_ref().and_then(|m| dg(m, "transfer.status")).and_then(Value::as_str) == Some("transferred");
            if deleted.is_some() {
                format!("The server with UUID {server_uuid} is still being deleted on this instance. Wait a minute, then transfer again.")
            } else if transferred {
                "Another team of this instance has the archived copy of this server. Transfer the server into that team, or delete the archive there first.".to_owned()
            } else {
                format!("A server with UUID {server_uuid} already exists on this instance. Delete/transfer it first, or import with preserve_uuids=false.")
            }
        });
    }
    let ip_exists = coolify_db::servers::ip_in_use(
        &state.db,
        &crate::routes::servers::normalize_ip(server_ip),
        archive,
    )
    .await
    .map_err(|e| TransferError::Message(e.to_string()))?;
    Ok(vec![
        json!({
            "field": "server",
            "label": "the team can have one more server",
            "ok": !limit_reached,
            "message": limit_reached.then_some("Server limit reached for your subscription."),
        }),
        json!({
            "field": "server.uuid",
            "label": if archive.is_some() { "the archived copy of this server on this instance is replaced" } else { "no server with the same UUID exists" },
            "ok": uuid_message.is_none(),
            "message": uuid_message,
        }),
        json!({
            "field": "server.ip",
            "label": "no server with the same IP exists",
            "ok": !ip_exists,
            "message": ip_exists.then(|| format!("A server with IP/domain {server_ip} already exists on this instance. Remove it first.")),
        }),
    ])
}

/// `replaceableArchive`: the id of the archive this team kept of a server it transferred away.
async fn replaceable_archive(state: &AppState, uuid: &str, team_id: i64) -> R<Option<i64>> {
    let row: Option<(i64, Option<Value>)> = sqlx::query_as(
        "SELECT id, server_metadata FROM servers WHERE uuid = $1 AND team_id = $2 AND deleted_at IS NULL",
    )
    .bind(uuid)
    .bind(team_id)
    .fetch_optional(state.db.pool())
    .await?;
    Ok(row.and_then(|(id, meta)| {
        (meta
            .as_ref()
            .and_then(|m| dg(m, "transfer.status"))
            .and_then(Value::as_str)
            == Some("transferred"))
        .then_some(id)
    }))
}

/// `postTransferChecklist`.
fn post_transfer_checklist(bundle: &Value, target_url: &str) -> Vec<String> {
    let mut steps = Vec::new();
    for step in list(bundle, "post_transfer_checklist") {
        if let Some(s) = step.as_str() {
            let replaced = s.replace("{target_url}", target_url);
            if !steps.contains(&replaced) {
                steps.push(replaced);
            }
        }
    }
    steps
}

fn unique(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// `ServerTransferImporter::import`.
pub async fn import(
    state: &AppState,
    bundle_map: &Map<String, Value>,
    team_id: i64,
    dry_run: bool,
    preserve: bool,
    adopt: bool,
) -> R<Map<String, Value>> {
    let version = state.version.clone();
    let enc = state
        .encrypter
        .as_ref()
        .ok_or_else(|| TransferError::Message("APP_KEY is not configured.".to_owned()))?;
    let validation = bundle::validate(bundle_map, &version);
    if !validation.errors.is_empty() {
        return Err(TransferError::Validation(vec![(
            "bundle".into(),
            validation.errors,
        )]));
    }
    let bundle = Value::Object(bundle_map.clone());
    assert_valid_server_payload(dg(&bundle, "server").unwrap_or(&Value::Null))?;
    if adopt && !preserve {
        return Err(TransferError::Validation(vec![(
            "preserve_uuids".into(),
            vec!["Adopt mode keeps the running containers, so it needs the original UUIDs. Import with preserve_uuids=true.".into()],
        )]));
    }
    let mut warnings = validation.warnings;
    for w in list(&bundle, "warnings") {
        warnings.push(text(w));
    }
    warnings = unique(warnings);

    let server_uuid = str_of(&bundle, "server.uuid");
    let server_ip = str_of(&bundle, "server.ip");
    let archive = if preserve {
        replaceable_archive(state, &server_uuid, team_id).await?
    } else {
        None
    };
    let uuid_conflict = archive.is_none() && preserve && exists_any(state, &server_uuid).await?;
    let ip_conflict = coolify_db::servers::ip_in_use(
        &state.db,
        &crate::routes::servers::normalize_ip(&server_ip),
        archive,
    )
    .await
    .map_err(|e| TransferError::Message(e.to_string()))?;

    if !dry_run {
        for check in import_checks(state, &server_uuid, &server_ip, team_id, preserve).await? {
            if check["ok"] != json!(true) {
                return Err(TransferError::Validation(vec![(
                    text(&check["field"]),
                    vec![text(&check["message"])],
                )]));
            }
        }
    }
    if uuid_conflict {
        warnings.push(format!(
            "A server with UUID {server_uuid} already exists on this instance."
        ));
    }
    if ip_conflict {
        warnings.push(format!(
            "A server with IP/domain {server_ip} already exists on this instance."
        ));
    }

    let mut created = obj(json!({
        "projects": 0, "environments": 0, "applications": 0, "databases": 0, "services": 0,
        "destinations": 0, "private_keys": 0, "github_apps": 0, "gitlab_apps": 0, "gitea_apps": 0,
        "s3_storages": 0, "cloud_provider_tokens": 0, "integration_tokens": 0,
        "managed_dns_records": 0, "ssl_certificates": 0, "volume_backups": 0, "previews": 0,
    }));
    let target_url = instance_url(&state.db).await;

    if dry_run {
        for project in list(&bundle, "projects") {
            bump(&mut created, "projects", 1);
            for env in list(project, "environments") {
                bump(&mut created, "environments", 1);
                bump(&mut created, "applications", count(env, "applications"));
                bump(&mut created, "databases", count(env, "databases"));
                bump(&mut created, "services", count(env, "services"));
                for app in list(env, "applications") {
                    bump(&mut created, "previews", count(app, "previews"));
                }
            }
        }
        for key in [
            "destinations",
            "github_apps",
            "gitlab_apps",
            "gitea_apps",
            "s3_storages",
            "cloud_provider_tokens",
            "integration_tokens",
            "managed_dns_records",
            "ssl_certificates",
            "volume_backups",
        ] {
            created.insert(key.to_owned(), json!(count(&bundle, key)));
        }
        let single = i64::from(truthy(dg(&bundle, "private_key")));
        created.insert(
            "private_keys".into(),
            json!(count(&bundle, "private_keys").max(single)),
        );
        return Ok(obj(json!({
            "dry_run": true,
            "warnings": warnings,
            "server_uuid": if preserve { json!(server_uuid) } else { Value::Null },
            "private_key_uuid": get_or_null(&bundle, "private_key.uuid"),
            "created": created,
            "preserved_uuids": preserve,
            "export_id": get_or_null(&bundle, "export_id"),
            "claimed": false,
            "claim": null,
            "checklist": post_transfer_checklist(&bundle, &target_url),
        })));
    }

    let mut import = Import {
        enc,
        team_id,
        preserve,
        adopt,
        now: Utc::now().naive_utc(),
        m: Maps::default(),
    };
    let mut tx = state.db.pool().begin().await?;
    let conn: &mut PgConnection = &mut tx;
    // Removed in the same transaction as the import, so a failed import keeps the archive.
    if let Some(id) = archive {
        delete_archive(conn, id).await?;
    }
    sqlx::query("SELECT id FROM teams WHERE id = $1 FOR UPDATE")
        .bind(team_id)
        .execute(&mut *conn)
        .await?;

    import
        .shared_dependencies(conn, &bundle, &mut created)
        .await?;
    let key_payload = dg(&bundle, "private_key").unwrap_or(&Value::Null);
    let key_uuid = str_of(&bundle, "private_key.uuid");
    let key_id = match import.m.private_keys.get(&key_uuid).copied() {
        Some(id) => id,
        None => import.private_key(conn, key_payload).await?.0,
    };
    let private_key_uuid: String =
        sqlx::query_scalar("SELECT uuid FROM private_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&mut *conn)
            .await?;
    let server = import
        .server(
            conn,
            state,
            dg(&bundle, "server").unwrap_or(&Value::Null),
            key_id,
        )
        .await?;
    let destinations = import
        .destinations(conn, list(&bundle, "destinations"), server.id)
        .await?;
    created.insert("destinations".into(), json!(destinations.len()));
    import
        .github_runner_config(conn, dg(&bundle, "github_runner_config"), server.id)
        .await?;

    for v in list(&bundle, "shared_environment_variables.server") {
        import
            .shared_variable(
                conn,
                v,
                json!({"type": "server", "server_id": server.id, "team_id": team_id}),
            )
            .await?;
    }
    for v in list(&bundle, "shared_environment_variables.team") {
        let key = str_of(v, "key");
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM shared_environment_variables WHERE type = 'team' AND team_id = $1 AND key = $2)",
        )
        .bind(team_id)
        .bind(&key)
        .fetch_one(&mut *conn)
        .await?;
        if found {
            import.m.warnings.push(format!(
                "Team shared variable {key} already exists on this instance. It keeps its value here, which can differ from the value on the source instance."
            ));
            continue;
        }
        import
            .shared_variable(conn, v, json!({"type": "team", "team_id": team_id}))
            .await?;
    }

    for project_payload in list(&bundle, "projects") {
        let project_id = import.project(conn, project_payload, &mut created).await?;
        for env_payload in list(project_payload, "environments") {
            let (env_id, env_name) = import
                .environment(conn, env_payload, project_id, &mut created)
                .await?;
            for app in list(env_payload, "applications") {
                let res = import.application(conn, app, env_id, &destinations).await?;
                import
                    .m
                    .applications
                    .insert(dg(app, "uuid").map_or_else(|| res.uuid.clone(), text), res);
                bump(&mut created, "applications", 1);
                bump(&mut created, "previews", count(app, "previews"));
            }
            for db in list(env_payload, "databases") {
                let res = import.database(conn, db, env_id, &destinations).await?;
                import
                    .m
                    .databases
                    .insert(dg(db, "uuid").map_or_else(|| res.uuid.clone(), text), res);
                bump(&mut created, "databases", 1);
            }
            for service in list(env_payload, "services") {
                let res = import
                    .service(conn, service, env_id, server.id, &destinations)
                    .await?;
                import.m.services.insert(
                    dg(service, "uuid").map_or_else(|| res.uuid.clone(), text),
                    res,
                );
                bump(&mut created, "services", 1);
            }
            import
                .canvas_positions(conn, dg(env_payload, "canvas_positions"), env_id)
                .await?;
            import
                .resource_links(conn, list(env_payload, "resource_links"), env_id, &env_name)
                .await?;
        }
    }
    let sqlite_warnings = import.link_sqlite(conn).await?;
    warnings.extend(sqlite_warnings);

    import.dns_zones(conn, list(&bundle, "dns_zones")).await?;
    let dns = import
        .managed_dns_records(conn, list(&bundle, "managed_dns_records"), &server)
        .await?;
    created.insert("managed_dns_records".into(), json!(dns));
    let ssl = import
        .ssl_certificates(conn, list(&bundle, "ssl_certificates"), server.id)
        .await?;
    created.insert("ssl_certificates".into(), json!(ssl));
    let backups = import
        .volume_backups(conn, list(&bundle, "volume_backups"))
        .await?;
    created.insert("volume_backups".into(), json!(backups));
    warnings.append(&mut import.m.warnings);

    let git_sources = unique(std::mem::take(&mut import.m.git_source_applications));
    let metadata = json!({
        "transfer": {
            "status": "imported",
            "export_id": get_or_null(&bundle, "export_id"),
            "source_instance_url": get_or_null(&bundle, "source_instance.url"),
            "imported_at": now_iso(),
            "adopt_mode": adopt,
            "git_source_applications": git_sources,
        }
    });
    sqlx::query("UPDATE servers SET server_metadata = $2::text::json WHERE id = $1")
        .bind(server.id)
        .bind(metadata.to_string())
        .execute(&mut *conn)
        .await?;

    let pending_dns = std::mem::take(&mut import.m.pending_dns);
    tx.commit().await?;

    // `adoptManagedDnsRecords`: provider calls run after the commit. A record that cannot be adopted
    // is no longer owned, so this instance never changes or deletes it.
    for (id, name, previous) in pending_dns {
        let dns = coolify_jobs::integrations::cloudflare::Dns {
            db: &state.db,
            enc,
            now: state.now(),
            source: "system",
        };
        let adopted = match coolify_db::managed_dns::record_by_id(&state.db, id)
            .await
            .map_err(|e| TransferError::Message(e.to_string()))?
        {
            Some(record) => dns.adopt_record(&record, &previous).await,
            None => continue,
        };
        if adopted {
            continue;
        }
        sqlx::query("UPDATE managed_dns_records SET owned = false WHERE id = $1")
            .bind(id)
            .execute(state.db.pool())
            .await?;
        warnings.push(format!(
            "DNS record {name}: this instance could not take ownership; it will not change or delete it."
        ));
    }
    let mut result = obj(json!({
        "dry_run": false,
        "warnings": unique(warnings),
        "server_uuid": server.uuid,
        "private_key_uuid": private_key_uuid,
        "created": created,
        "preserved_uuids": preserve,
        "export_id": get_or_null(&bundle, "export_id"),
        "claimed": false,
        "claim": null,
        "checklist": post_transfer_checklist(&bundle, &target_url),
    }));

    // Claim after the commit so a claim failure cannot roll back rows.
    let claim = match state.jobs.as_ref() {
        Some(jobs) => coolify_jobs::server_transfer::claim(jobs, server.id, &target_url).await,
        None => Err("Remote execution is not configured.".to_owned()),
    };
    match claim {
        Ok(claim) => {
            result.insert("claim".into(), claim);
            result.insert("claimed".into(), json!(true));
            if let Some(jobs) = state.jobs.as_ref() {
                // Same check as the Validate button. A running proxy and Docker are kept as they are.
                jobs.spawn_validation(
                    server.id,
                    team_id,
                    server.uuid.clone(),
                    coolify_jobs::Wording::Api,
                );
                super::checklist::start_after_import(state, server.id, team_id, &server.name).await;
            }
            result.insert(
                "server_url".into(),
                json!(format!("{target_url}/server/{}", server.uuid)),
            );
        }
        Err(e) => {
            if let Some(Value::Array(w)) = result.get_mut("warnings") {
                w.push(json!(format!(
                    "Server imported, but automatic claim failed: {e}"
                )));
            }
        }
    }
    Ok(result)
}

async fn exists_any(state: &AppState, uuid: &str) -> R<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM servers WHERE uuid = $1)")
            .bind(uuid)
            .fetch_one(state.db.pool())
            .await?,
    )
}

/// `ServerTransferClaimer::deleteArchive`: records only. Another instance manages the host, so
/// nothing runs on the server and nothing changes at a provider.
pub async fn delete_archive(conn: &mut PgConnection, server_id: i64) -> R<()> {
    coolify_jobs::server_transfer::forget_dns(conn, server_id).await?;
    let destinations = "(SELECT 'App\\Models\\StandaloneDocker' t, id FROM standalone_dockers WHERE server_id = $1 UNION ALL SELECT 'App\\Models\\SwarmDocker', id FROM swarm_dockers WHERE server_id = $1)";
    // Applications that have the server only as an additional destination stay.
    sqlx::query("DELETE FROM additional_destinations WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *conn)
        .await?;
    let mut resources: Vec<(String, i64, &'static str)> = Vec::new();
    let apps: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT a.id FROM applications a JOIN {destinations} d ON d.t = a.destination_type AND d.id = a.destination_id"
    ))
    .bind(server_id)
    .fetch_all(&mut *conn)
    .await?;
    for id in &apps {
        resources.push(("applications".into(), *id, APPLICATION_MORPH));
        let previews: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM application_previews WHERE application_id = $1")
                .bind(id)
                .fetch_all(&mut *conn)
                .await?;
        for preview in previews {
            resources.push(("application_previews".into(), preview, PREVIEW_MORPH));
        }
    }
    for (type_name, table, _) in DATABASE_MODELS {
        let ids: Vec<i64> = sqlx::query_scalar(&format!(
            "SELECT x.id FROM {table} x JOIN {destinations} d ON d.t = x.destination_type AND d.id = x.destination_id"
        ))
        .bind(server_id)
        .fetch_all(&mut *conn)
        .await?;
        for id in ids {
            resources.push(((*table).to_owned(), id, morph_of(type_name)));
        }
    }
    let services: Vec<i64> = sqlx::query_scalar("SELECT id FROM services WHERE server_id = $1")
        .bind(server_id)
        .fetch_all(&mut *conn)
        .await?;
    for service in services {
        for (table, morph) in [
            ("service_applications", SERVICE_APP_MORPH),
            ("service_databases", SERVICE_DB_MORPH),
        ] {
            let ids: Vec<i64> =
                sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE service_id = $1"))
                    .bind(service)
                    .fetch_all(&mut *conn)
                    .await?;
            for id in ids {
                resources.push((table.to_owned(), id, morph));
            }
        }
        resources.push(("services".into(), service, SERVICE_MORPH));
    }
    for (table, id, morph) in &resources {
        delete_resource_rows(conn, table, *id, morph).await?;
    }
    for sql in [
        "DELETE FROM shared_environment_variables WHERE server_id = $1",
        "DELETE FROM server_settings WHERE server_id = $1",
        "DELETE FROM standalone_dockers WHERE server_id = $1",
        "DELETE FROM swarm_dockers WHERE server_id = $1",
        "DELETE FROM ssl_certificates WHERE server_id = $1",
        "DELETE FROM github_runner_configs WHERE server_id = $1",
        "DELETE FROM servers WHERE id = $1",
    ] {
        sqlx::query(sql).bind(server_id).execute(&mut *conn).await?;
    }
    Ok(())
}

/// The relation deletes of `DeleteResourceJob(deleteFromCoolifyOnly: true)`.
async fn delete_resource_rows(conn: &mut PgConnection, table: &str, id: i64, morph: &str) -> R<()> {
    for sql in [
        "DELETE FROM scheduled_volume_backup_s3_storage WHERE scheduled_volume_backup_id IN (SELECT id FROM scheduled_volume_backups WHERE (backupable_type = 'App\\Models\\LocalPersistentVolume' AND backupable_id IN (SELECT id FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)) OR (backupable_type = 'App\\Models\\LocalFileVolume' AND backupable_id IN (SELECT id FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2)))",
        "DELETE FROM scheduled_volume_backups WHERE (backupable_type = 'App\\Models\\LocalPersistentVolume' AND backupable_id IN (SELECT id FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2)) OR (backupable_type = 'App\\Models\\LocalFileVolume' AND backupable_id IN (SELECT id FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2))",
        "DELETE FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2",
        "DELETE FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2",
        "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2",
        "DELETE FROM taggables WHERE taggable_type = $1 AND taggable_id = $2",
        "DELETE FROM secret_manager_links WHERE resourceable_type = $1 AND resourceable_id = $2",
        "DELETE FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id IN (SELECT id FROM scheduled_database_backups WHERE database_type = $1 AND database_id = $2)",
        "DELETE FROM scheduled_database_backups WHERE database_type = $1 AND database_id = $2",
        "DELETE FROM resource_links WHERE (source_type = $1 AND source_id = $2) OR (target_type = $1 AND target_id = $2)",
    ] {
        sqlx::query(sql)
            .bind(morph)
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    if morph == APPLICATION_MORPH {
        for sql in [
            "DELETE FROM application_settings WHERE application_id = $1",
            "DELETE FROM scheduled_tasks WHERE application_id = $1",
            "DELETE FROM application_deployment_queues WHERE application_id = $1::text",
        ] {
            sqlx::query(sql).bind(id).execute(&mut *conn).await?;
        }
    }
    if morph == SERVICE_MORPH {
        sqlx::query("DELETE FROM scheduled_tasks WHERE service_id = $1")
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
