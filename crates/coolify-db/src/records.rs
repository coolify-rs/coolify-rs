//! Model records: rows serialized the way Laravel's `Model::toArray()` does.
//!
//! Rows are fetched with `row_to_json`, which keeps database column order
//! (Laravel keeps it too), native booleans/integers and parsed JSON. Each
//! model spec then applies upstream's `$hidden` list and datetime casts.
//!
//! Unlike upstream, columns not listed in `known` are **dropped** (and
//! logged once): a newer upstream release adding a secret column must not
//! leak it through coolify-rs before the port is reviewed.

use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value};

pub type Record = Map<String, Value>;

#[derive(Debug)]
pub struct ModelSpec {
    pub table: &'static str,
    /// Columns this build knows how to present (baseline + reviewed newer ones).
    pub known: &'static [&'static str],
    /// Model `$hidden` attributes.
    pub hidden: &'static [&'static str],
    /// Columns of type `timestamp`.
    pub timestamps: &'static [&'static str],
    /// Timestamp attributes cast to `datetime` (Carbon). Others render raw.
    pub datetimes: &'static [&'static str],
}

pub static TEAM: ModelSpec = ModelSpec {
    table: "teams",
    known: &[
        "id",
        "name",
        "description",
        "personal_team",
        "created_at",
        "updated_at",
        "show_boarding",
        "custom_server_limit",
        "is_mcp_server_enabled",
        "is_build_server_fallback_enabled",
        "avatar_path",
        "avatar_storage_type",
        "avatar_s3_storage_id",
        // main (2026_10_25_115811), reviewed: boolean flag, not secret.
        "is_two_factor_required",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static USER: ModelSpec = ModelSpec {
    table: "users",
    known: &[
        "id",
        "name",
        "email",
        "email_verified_at",
        "password",
        "remember_token",
        "created_at",
        "updated_at",
        "two_factor_secret",
        "two_factor_recovery_codes",
        "two_factor_confirmed_at",
        "force_password_reset",
        "marketing_emails",
        "pending_email",
        "email_change_code",
        "email_change_code_expires_at",
        "avatar_path",
        "avatar_storage_type",
        "avatar_s3_storage_id",
        "current_team_id",
        "created_before_oauth_identities",
    ],
    hidden: &[
        "password",
        "remember_token",
        "two_factor_recovery_codes",
        "two_factor_secret",
        "created_before_oauth_identities",
    ],
    timestamps: &[
        "created_at",
        "updated_at",
        "email_verified_at",
        "two_factor_confirmed_at",
        "email_change_code_expires_at",
    ],
    datetimes: &[
        "created_at",
        "updated_at",
        "email_verified_at",
        "email_change_code_expires_at",
    ],
};

pub static PROJECT: ModelSpec = ModelSpec {
    table: "projects",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "team_id",
        "created_at",
        "updated_at",
        "icon_path",
        "icon_storage_type",
        "icon_s3_storage_id",
        // main (2026_10_23_182201), reviewed: layout JSON, not secret.
        "canvas_position",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static ENVIRONMENT: ModelSpec = ModelSpec {
    table: "environments",
    known: &[
        "id",
        "name",
        "project_id",
        "created_at",
        "updated_at",
        "description",
        "uuid",
        "canvas_positions",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static PRIVATE_KEY: ModelSpec = ModelSpec {
    table: "private_keys",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "private_key",
        "is_git_related",
        "team_id",
        "created_at",
        "updated_at",
        "fingerprint",
    ],
    // `private_key` is hidden by the model; handlers re-add it (decrypted)
    // only for tokens with `can_read_sensitive`.
    hidden: &["private_key"],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

/// `servers`. Hidden attributes and `makeVisible` depend on the token, so
/// the API layer applies them (see `coolify_api::routes::servers`).
pub static SERVER: ModelSpec = ModelSpec {
    table: "servers",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "ip",
        "port",
        "user",
        "team_id",
        "private_key_id",
        "proxy",
        "created_at",
        "updated_at",
        "unreachable_count",
        "high_disk_usage_notification_sent",
        "log_drain_notification_sent",
        "swarm_cluster",
        "validation_logs",
        "sentinel_updated_at",
        "deleted_at",
        "ip_previous",
        "hetzner_server_id",
        "cloud_provider_token_id",
        "hetzner_server_status",
        "is_validating",
        "detected_traefik_version",
        "traefik_outdated_info",
        "server_metadata",
        "vultr_instance_id",
        "vultr_instance_status",
        "digitalocean_droplet_id",
        "digitalocean_droplet_status",
        "hostinger_virtual_machine_id",
        "hostinger_virtual_machine_status",
        "sentinel_waiting_since",
        "proxy_error_page",
        "canvas_position",
    ],
    hidden: &[],
    timestamps: &[
        "created_at",
        "updated_at",
        "sentinel_updated_at",
        "deleted_at",
        "sentinel_waiting_since",
    ],
    // `deleted_at` via SoftDeletes; `sentinel_updated_at` has no cast.
    datetimes: &[
        "created_at",
        "updated_at",
        "deleted_at",
        "sentinel_waiting_since",
    ],
};

/// `standalone_dockers` (a destination).
pub static STANDALONE_DOCKER: ModelSpec = ModelSpec {
    table: "standalone_dockers",
    known: &[
        "id",
        "name",
        "uuid",
        "network",
        "server_id",
        "created_at",
        "updated_at",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

/// `standalone_postgresqls`. `postgres_password` (encrypted) and
/// `init_scripts` are model-hidden; the API layer re-adds them for
/// `can_read_sensitive`.
pub static STANDALONE_POSTGRESQL: ModelSpec = ModelSpec {
    table: "standalone_postgresqls",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "postgres_user",
        "postgres_password",
        "postgres_db",
        "postgres_initdb_args",
        "postgres_host_auth_method",
        "init_scripts",
        "status",
        "image",
        "is_public",
        "public_port",
        "ports_mappings",
        "limits_memory",
        "limits_memory_swap",
        "limits_memory_swappiness",
        "limits_memory_reservation",
        "limits_cpus",
        "limits_cpuset",
        "limits_cpu_shares",
        "started_at",
        "destination_type",
        "destination_id",
        "environment_id",
        "created_at",
        "updated_at",
        "postgres_conf",
        "is_log_drain_enabled",
        "is_include_timestamps",
        "deleted_at",
        "config_hash",
        "custom_docker_run_options",
        "last_online_at",
        "enable_ssl",
        "ssl_mode",
        "restart_count",
        "last_restart_at",
        "last_restart_type",
        "public_port_timeout",
        "health_check_enabled",
        "health_check_interval",
        "health_check_timeout",
        "health_check_retries",
        "health_check_start_period",
        "redact_all_env_values_in_logs",
        "include_coolify_default_variables",
    ],
    hidden: &["postgres_password", "init_scripts"],
    timestamps: &[
        "started_at",
        "created_at",
        "updated_at",
        "deleted_at",
        "last_online_at",
        "last_restart_at",
    ],
    // SoftDeletes plus the `last_restart_at` datetime cast.
    datetimes: &["created_at", "updated_at", "deleted_at", "last_restart_at"],
};

/// `server_settings` (hidden attributes applied by the API layer, as for servers).
pub static SERVER_SETTING: ModelSpec = ModelSpec {
    table: "server_settings",
    known: &[
        "id",
        "is_swarm_manager",
        "is_jump_server",
        "is_build_server",
        "is_reachable",
        "is_usable",
        "server_id",
        "created_at",
        "updated_at",
        "wildcard_domain",
        "is_cloudflare_tunnel",
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
        "concurrent_builds",
        "dynamic_timeout",
        "force_disabled",
        "is_metrics_enabled",
        "generate_exact_labels",
        "force_docker_cleanup",
        "docker_cleanup_frequency",
        "docker_cleanup_threshold",
        "server_timezone",
        "delete_unused_volumes",
        "delete_unused_networks",
        "is_sentinel_enabled",
        "sentinel_token",
        "sentinel_metrics_refresh_rate_seconds",
        "sentinel_metrics_history_days",
        "sentinel_push_interval_seconds",
        "sentinel_custom_url",
        "server_disk_usage_notification_threshold",
        "is_sentinel_debug_enabled",
        "server_disk_usage_check_frequency",
        "is_terminal_enabled",
        "deployment_queue_limit",
        "disable_application_image_retention",
        "connection_timeout",
        "is_traffic_analytics_enabled",
        "geoip_maxmind_license_key",
        "traffic_topn",
        "traffic_sample_threshold",
        "traffic_retention_1h_days",
        "traffic_retention_1d_days",
        "is_geoip_enabled",
        "geoip_refresh_days",
        "docker_version",
        "docker_version_checked_at",
        "compose_version",
        "compose_version_checked_at",
        "backup_compression_cpu_percentage",
        "server_role",
        "is_logdrain_cloudwatch_enabled",
        "logdrain_cloudwatch_region",
        "logdrain_cloudwatch_group",
        "logdrain_cloudwatch_stream_prefix",
        "server_disk_usage_notification_interval_hours",
        "traffic_ip_mode",
        "cloudflare_tunnel_id",
        "cloudflare_tunnel_account_id",
        "cloudflare_tunnel_integration_token_id",
        "traefik_certificate_resolver",
        "patch_check_frequency",
        "patch_check_result",
        "patch_check_error",
        "patch_checked_at",
        "is_auto_security_updates_enabled",
        "auto_security_updates_time",
    ],
    hidden: &[],
    timestamps: &[
        "created_at",
        "updated_at",
        "docker_version_checked_at",
        "compose_version_checked_at",
        "patch_checked_at",
    ],
    datetimes: &[
        "created_at",
        "updated_at",
        "docker_version_checked_at",
        "compose_version_checked_at",
        "patch_checked_at",
    ],
};

static WARNED: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

impl ModelSpec {
    /// Applies hidden fields, unknown-column filtering and datetime casts.
    pub fn present(&self, raw: Record) -> Record {
        let mut out = Record::new();
        for (k, v) in raw {
            if !self.known.contains(&k.as_str()) {
                warn_unknown(self.table, &k);
                continue;
            }
            if self.hidden.contains(&k.as_str()) {
                continue;
            }
            let v = match v {
                Value::String(s)
                    if self.timestamps.contains(&k.as_str()) && looks_like_pg_timestamp(&s) =>
                {
                    Value::String(if self.datetimes.contains(&k.as_str()) {
                        laravel_datetime(&s)
                    } else {
                        s.replacen('T', " ", 1)
                    })
                }
                other => other,
            };
            out.insert(k, v);
        }
        out
    }

    /// Same as [`present`](Self::present) with extra attributes hidden (controller `makeHidden`).
    pub fn present_without(&self, raw: Record, extra_hidden: &[&str]) -> Record {
        let mut r = self.present(raw);
        for h in extra_hidden {
            r.shift_remove(*h);
        }
        r
    }
}

fn warn_unknown(table: &str, column: &str) {
    let key = format!("{table}.{column}");
    if let Ok(mut w) = WARNED.lock()
        && w.insert(key.clone())
    {
        tracing::warn!(column = %key, "dropping unreviewed upstream column from API output");
    }
}

/// `row_to_json` renders `timestamp(0)` as `YYYY-MM-DDTHH:MM:SS`.
fn looks_like_pg_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 19
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, c)| [4, 7, 10, 13, 16].contains(&i) || c.is_ascii_digit())
}

/// Carbon's default JSON serialization: microseconds and `Z` (UTC).
pub fn laravel_datetime(pg: &str) -> String {
    format!("{pg}.000000Z")
}

pub fn laravel_datetime_from(dt: chrono::NaiveDateTime) -> String {
    format!("{}.000000Z", dt.format("%Y-%m-%dT%H:%M:%S"))
}

/// Parses `row_to_json(...)::text` output into an order-preserving record.
pub fn parse_row(text: &str) -> Result<Record, serde_json::Error> {
    serde_json::from_str(text)
}

/// `applications`: hidden attributes and secrets are handled by the API layer
/// (`removeSensitiveData`), so the spec only formats timestamps.
pub static APPLICATION: ModelSpec = ModelSpec {
    table: "applications",
    known: &[
        "id",
        "repository_project_id",
        "uuid",
        "name",
        "fqdn",
        "config_hash",
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
        "limits_memory",
        "limits_memory_swap",
        "limits_memory_swappiness",
        "limits_memory_reservation",
        "limits_cpus",
        "limits_cpuset",
        "limits_cpu_shares",
        "status",
        "preview_url_template",
        "destination_type",
        "destination_id",
        "source_type",
        "source_id",
        "private_key_id",
        "environment_id",
        "created_at",
        "updated_at",
        "description",
        "dockerfile",
        "health_check_enabled",
        "dockerfile_location",
        "custom_labels",
        "dockerfile_target_build",
        "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab",
        "docker_compose_location",
        "docker_compose",
        "docker_compose_raw",
        "docker_compose_domains",
        "deleted_at",
        "docker_compose_custom_start_command",
        "docker_compose_custom_build_command",
        "swarm_replicas",
        "swarm_placement_constraints",
        "manual_webhook_secret_bitbucket",
        "custom_docker_run_options",
        "post_deployment_command",
        "post_deployment_command_container",
        "pre_deployment_command",
        "pre_deployment_command_container",
        "watch_paths",
        "custom_healthcheck_found",
        "manual_webhook_secret_gitea",
        "redirect",
        "compose_parsing_version",
        "last_online_at",
        "custom_nginx_configuration",
        "custom_network_aliases",
        "is_http_basic_auth_enabled",
        "http_basic_auth_username",
        "http_basic_auth_password",
        "restart_count",
        "last_restart_at",
        "last_restart_type",
        "health_check_type",
        "health_check_command",
        "max_restart_count",
        "noindex_domains",
        "domain_dns_statuses",
        "container_present",
        "restart_limit_reached",
        "domain_port_overrides",
        "is_maintenance_enabled",
        "maintenance_page",
        "http_basic_auth_hash_algorithm",
        "http_basic_auth_bcrypt_cost",
        "http_basic_auth_argon2id_memory_cost",
        "http_basic_auth_argon2id_time_cost",
    ],
    hidden: &[],
    timestamps: &[
        "created_at",
        "updated_at",
        "deleted_at",
        "last_online_at",
        "last_restart_at",
    ],
    datetimes: &["created_at", "updated_at", "deleted_at", "last_restart_at"],
};

/// `application_settings` (the API drops id, application_id and the timestamps).
pub static APPLICATION_SETTING: ModelSpec = ModelSpec {
    table: "application_settings",
    known: &[
        "id",
        "is_static",
        "is_git_submodules_enabled",
        "is_git_lfs_enabled",
        "is_auto_deploy_enabled",
        "is_force_https_enabled",
        "is_debug_enabled",
        "is_preview_deployments_enabled",
        "application_id",
        "created_at",
        "updated_at",
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
        "is_stripprefix_enabled",
        "connect_to_docker_network",
        "custom_internal_name",
        "is_container_label_escape_enabled",
        "is_env_sorting_enabled",
        "is_container_label_readonly_enabled",
        "is_preserve_repository_enabled",
        "disable_build_cache",
        "is_spa",
        "is_git_shallow_clone_enabled",
        "is_pr_deployments_public_enabled",
        "use_build_secrets",
        "stop_grace_period",
        "inject_build_args_to_dockerfile",
        "include_source_commit_in_build",
        "docker_images_to_keep",
        "custom_container_name_prefix",
        "redact_all_env_values_in_logs",
        "traefik_certificate_resolver",
        "include_coolify_default_variables",
        "is_permanent_redirect_enabled",
        "rolling_update_drain_seconds",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

/// `application_deployment_queues`.
pub static APPLICATION_DEPLOYMENT_QUEUE: ModelSpec = ModelSpec {
    table: "application_deployment_queues",
    known: &[
        "id",
        "application_id",
        "deployment_uuid",
        "pull_request_id",
        "force_rebuild",
        "commit",
        "status",
        "is_webhook",
        "created_at",
        "updated_at",
        "logs",
        "current_process_id",
        "restart_only",
        "git_type",
        "server_id",
        "application_name",
        "server_name",
        "deployment_url",
        "destination_id",
        "only_this_server",
        "rollback",
        "commit_message",
        "is_api",
        "build_server_id",
        "horizon_job_id",
        "horizon_job_worker",
        "finished_at",
        "docker_registry_image_tag",
        "configuration_hash",
        "configuration_snapshot",
        "configuration_diff",
        "parent_deployment_uuid",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at", "finished_at"],
    datetimes: &["created_at", "updated_at", "finished_at"],
};

/// `EnvironmentVariable`: `value` is added by the caller when the token may read it.
pub static ENVIRONMENT_VARIABLE: ModelSpec = ModelSpec {
    table: "environment_variables",
    known: &[
        "id",
        "key",
        "value",
        "is_preview",
        "created_at",
        "updated_at",
        "is_shown_once",
        "is_multiline",
        "version",
        "is_literal",
        "uuid",
        "order",
        "is_required",
        "is_shared",
        "resourceable_type",
        "resourceable_id",
        "is_runtime",
        "is_buildtime",
        "comment",
        "uses_legacy_escaping",
        "preserve_compose_null",
    ],
    hidden: &["value", "uses_legacy_escaping"],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static LOCAL_PERSISTENT_VOLUME: ModelSpec = ModelSpec {
    table: "local_persistent_volumes",
    known: &[
        "id",
        "name",
        "mount_path",
        "host_path",
        "container_id",
        "resource_type",
        "resource_id",
        "created_at",
        "updated_at",
        "is_preview_suffix_enabled",
        "uuid",
        "standalone_sqlite_id",
        "ignores_compose_driver_options",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

/// `LocalFileVolume`: `content` stays in the record encrypted; the caller decides.
pub static LOCAL_FILE_VOLUME: ModelSpec = ModelSpec {
    table: "local_file_volumes",
    known: &[
        "id",
        "uuid",
        "fs_path",
        "mount_path",
        "content",
        "resource_type",
        "resource_id",
        "created_at",
        "updated_at",
        "is_directory",
        "chown",
        "chmod",
        "is_based_on_git",
        "is_preview_suffix_enabled",
        "is_host_file",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static TAG: ModelSpec = ModelSpec {
    table: "tags",
    known: &["id", "uuid", "name", "team_id", "created_at", "updated_at"],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presents_like_laravel() {
        let raw = parse_row(
            r#"{"id":1,"name":"A","password":"$2y$","email_verified_at":"2026-10-26T20:26:28",
                "two_factor_confirmed_at":"2026-10-26T20:26:29","created_at":"2026-10-26T20:26:28",
                "secret_new_column":"x","current_team_id":null}"#,
        )
        .unwrap();
        let r = USER.present(raw);
        let keys: Vec<&str> = r.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "name",
                "email_verified_at",
                "two_factor_confirmed_at",
                "created_at",
                "current_team_id"
            ]
        );
        assert_eq!(r["email_verified_at"], "2026-10-26T20:26:28.000000Z");
        // Not cast: Laravel returns the raw database string.
        assert_eq!(r["two_factor_confirmed_at"], "2026-10-26 20:26:29");
    }

    #[test]
    fn non_timestamps_untouched() {
        let raw = parse_row(r#"{"id":1,"name":"2026-10-26T20:26:28","description":null}"#).unwrap();
        let r = PROJECT.present(raw);
        assert_eq!(r["name"], "2026-10-26T20:26:28");
    }
}
