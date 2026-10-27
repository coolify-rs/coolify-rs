//! `ApplicationConfigurationSnapshot` as far as `config_hash` reads it:
//! `hash('sha256', json_encode(comparableSnapshot(...)))`.

use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::applications::{self as apps, ApplicationRow};
use serde_json::{Map, Value, json};
use sha2::Digest as _;

/// `DEFAULT_STOP_GRACE_PERIOD_SECONDS`.
const DEFAULT_STOP_GRACE_PERIOD: i64 = 30;

/// `normalizeValue`: `''` is null, arrays are sorted recursively.
fn normalize(v: Value) -> Value {
    match v {
        Value::String(s) if s.is_empty() => Value::Null,
        v @ (Value::Array(_) | Value::Object(_)) => sort_recursive(v),
        other => other,
    }
}

/// `Arr::sortRecursive`: objects by key (`ksort`), lists by value (`sort`).
fn sort_recursive(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut entries: Vec<(String, Value)> =
                o.into_iter().map(|(k, v)| (k, sort_recursive(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(a) => {
            let mut items: Vec<Value> = a.into_iter().map(sort_recursive).collect();
            items.sort_by_key(php_sort_key);
            Value::Array(items)
        }
        other => other,
    }
}

fn php_sort_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn php_json(v: &Value) -> String {
    coolify_compat::php_json::encode(v)
}

struct Snapshot<'a> {
    enc: &'a Encrypter,
    sections: Vec<(&'static str, Map<String, Value>)>,
}

impl Snapshot<'_> {
    fn item(
        &mut self,
        section: &'static str,
        key: &str,
        value: Value,
        impact: &str,
        sensitive: bool,
    ) {
        let normalized = normalize(value);
        let compare = if sensitive {
            Value::String(self.hash(&normalized))
        } else {
            normalized
        };
        let entry = json!({"compare_value": compare, "impact": impact});
        match self.sections.iter_mut().find(|(s, _)| *s == section) {
            Some((_, items)) => {
                items.insert(key.to_owned(), entry);
            }
            None => self
                .sections
                .push((section, [(key.to_owned(), entry)].into_iter().collect())),
        }
    }

    /// `sensitiveHash`: `hash_hmac('sha256', json_encode($v), config('app.key'))`.
    fn hash(&self, v: &Value) -> String {
        self.enc.hmac_with_app_key(php_json(v).as_bytes())
    }
}

/// `$application->custom_network_aliases` (the getter returns a comma list).
fn network_aliases(raw: Option<&str>) -> Value {
    match raw {
        None => Value::Null,
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => Value::String(
                a.iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => Value::String(s.to_owned()),
        },
    }
}

/// `$application->config_hash` as `isConfigurationChanged(true)` computes it.
pub async fn config_hash(db: &Db, enc: &Encrypter, a: &ApplicationRow) -> Result<String, String> {
    let col = |k: &str| a.raw.get(k).cloned().unwrap_or(Value::Null);
    let set = |k: &str| a.settings.get(k).cloned().unwrap_or(Value::Null);
    let mut s = Snapshot {
        enc,
        sections: Vec::new(),
    };

    for (k, impact) in [
        ("git_repository", "build"),
        ("git_branch", "build"),
        ("git_commit_sha", "build"),
        ("source_id", "build"),
        ("source_type", "build"),
        ("private_key_id", "build"),
    ] {
        s.item("source", k, col(k), impact, false);
    }

    s.item("build", "build_pack", col("build_pack"), "build", false);
    s.item("build", "is_static", set("is_static"), "build", false);
    s.item("build", "is_spa", set("is_spa"), "build", false);
    for k in [
        "static_image",
        "base_directory",
        "publish_directory",
        "install_command",
        "build_command",
        "dockerfile",
        "dockerfile_location",
        "dockerfile_target_build",
        "docker_compose_location",
        "docker_compose_raw",
        "docker_compose_custom_build_command",
    ] {
        s.item("build", k, col(k), "build", false);
    }
    for k in [
        "is_git_submodules_enabled",
        "is_git_lfs_enabled",
        "is_git_shallow_clone_enabled",
        "is_env_sorting_enabled",
    ] {
        s.item("build", k, set(k), "build", false);
    }
    s.item(
        "build",
        "custom_docker_run_options",
        col("custom_docker_run_options"),
        "redeploy",
        false,
    );
    for k in [
        "use_build_secrets",
        "inject_build_args_to_dockerfile",
        "include_source_commit_in_build",
        "disable_build_cache",
        "is_build_server_enabled",
    ] {
        s.item("build", k, set(k), "build", false);
    }

    for k in [
        "docker_registry_image_name",
        "docker_registry_image_tag",
        "start_command",
        "pre_deployment_command",
        "pre_deployment_command_container",
        "post_deployment_command",
        "post_deployment_command_container",
        "docker_compose_custom_start_command",
        "ports_exposes",
        "ports_mappings",
    ] {
        s.item("runtime", k, col(k), "redeploy", false);
    }
    s.item(
        "runtime",
        "custom_network_aliases",
        network_aliases(a.str("custom_network_aliases")),
        "redeploy",
        false,
    );
    for k in [
        "connect_to_docker_network",
        "custom_internal_name",
        "custom_container_name_prefix",
        "is_consistent_container_name_enabled",
        "is_container_label_escape_enabled",
        "is_container_label_readonly_enabled",
        "is_log_drain_enabled",
        "is_swarm_only_worker_nodes",
    ] {
        s.item("runtime", k, set(k), "redeploy", false);
    }
    let grace = a
        .setting_int("stop_grace_period")
        .filter(|v| *v != DEFAULT_STOP_GRACE_PERIOD);
    s.item(
        "runtime",
        "stop_grace_period",
        grace.map_or(Value::Null, Value::from),
        "redeploy",
        false,
    );
    for k in [
        "is_preserve_repository_enabled",
        "is_raw_compose_deployment_enabled",
        "is_gpu_enabled",
        "gpu_driver",
        "gpu_count",
        "gpu_device_ids",
        "gpu_options",
    ] {
        s.item("runtime", k, set(k), "redeploy", false);
    }
    for k in [
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
        "swarm_replicas",
        "swarm_placement_constraints",
    ] {
        s.item("runtime", k, col(k), "redeploy", false);
    }

    s.item("domains", "fqdn", col("fqdn"), "redeploy", false);
    let overrides = match a.raw.get("domain_port_overrides") {
        Some(Value::String(t)) => serde_json::from_str::<Value>(t).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let overrides = match overrides {
        Value::Object(o) => sort_recursive(Value::Object(o)),
        _ => Value::Array(vec![]),
    };
    s.item(
        "domains",
        "domain_port_overrides",
        overrides,
        "redeploy",
        false,
    );
    let noindex = match a.raw.get("noindex_domains") {
        Some(Value::Array(v)) => Value::Array(v.clone()),
        Some(Value::String(t)) => match serde_json::from_str::<Value>(t) {
            Ok(Value::Array(v)) => Value::Array(v),
            _ => Value::Array(vec![]),
        },
        _ => Value::Array(vec![]),
    };
    s.item("domains", "noindex_domains", noindex, "redeploy", false);
    let compose_domains = a
        .str("docker_compose_domains")
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .filter(|v| v.is_object() || v.is_array())
        .unwrap_or(Value::Null);
    s.item(
        "domains",
        "docker_compose_domains",
        compose_domains,
        "redeploy",
        false,
    );
    s.item("domains", "redirect", col("redirect"), "redeploy", false);
    s.item(
        "domains",
        "custom_labels",
        col("custom_labels"),
        "redeploy",
        false,
    );
    // The getter decodes the stored base64.
    let nginx = a.str("custom_nginx_configuration").map(|v| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(v)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    });
    s.item(
        "domains",
        "custom_nginx_configuration",
        nginx.map_or(Value::Null, Value::String),
        "build",
        false,
    );
    for k in [
        "is_force_https_enabled",
        "is_gzip_enabled",
        "is_stripprefix_enabled",
        "traefik_certificate_resolver",
    ] {
        s.item("domains", k, set(k), "redeploy", false);
    }
    s.item(
        "domains",
        "is_http_basic_auth_enabled",
        col("is_http_basic_auth_enabled"),
        "redeploy",
        false,
    );
    s.item(
        "domains",
        "http_basic_auth_username",
        col("http_basic_auth_username"),
        "redeploy",
        false,
    );
    let password = match a.str("http_basic_auth_password") {
        Some(c) => Value::String(
            enc.decrypt_string(c)
                .map_err(|_| "The MAC is invalid.".to_owned())?,
        ),
        None => Value::Null,
    };
    s.item(
        "domains",
        "http_basic_auth_password",
        password,
        "redeploy",
        true,
    );
    for k in [
        "http_basic_auth_hash_algorithm",
        "http_basic_auth_bcrypt_cost",
        "http_basic_auth_argon2id_memory_cost",
        "http_basic_auth_argon2id_time_cost",
    ] {
        s.item("domains", k, col(k), "redeploy", false);
    }

    // Environment variables: the compare value is a keyed hash of the value and the flags.
    let vars = coolify_db::environment_variables::for_resource(db, apps::MORPH, a.id)
        .await
        .map_err(|e| e.to_string())?;
    let mut env: Vec<(String, Value, &'static str)> = Vec::new();
    for v in vars {
        let value = match v.value_encrypted.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => Value::String(
                enc.decrypt_maybe_serialized(c)
                    .map_err(|_| "The MAC is invalid.".to_owned())?
                    .trim()
                    .to_owned(),
            ),
            None => Value::Null,
        };
        let flag = |k: &str| v.record.get(k).and_then(Value::as_bool).unwrap_or(false);
        let buildtime = flag("is_buildtime");
        let compare = sort_recursive(json!({
            "value_hash": s.hash(&value),
            "is_multiline": flag("is_multiline"),
            "is_literal": flag("is_literal"),
            "is_buildtime": buildtime,
            "is_runtime": flag("is_runtime"),
        }));
        env.push((
            v.key.clone(),
            Value::String(s.hash(&compare)),
            if buildtime { "build" } else { "redeploy" },
        ));
    }
    s.sections.push(("environment", Map::new()));
    for (key, compare, impact) in env {
        if let Some((_, items)) = s.sections.iter_mut().find(|(n, _)| *n == "environment") {
            items.insert(key, json!({"compare_value": compare, "impact": impact}));
        }
    }

    s.sections.push(("storage", Map::new()));
    for (id, name, mount, host) in apps::volume_rows(db, a.id)
        .await
        .map_err(|e| e.to_string())?
    {
        let source = host.filter(|h| !h.is_empty()).unwrap_or(name);
        s.item(
            "storage",
            &format!("volume_{id}"),
            json!({"source": source, "destination": mount}),
            "redeploy",
            false,
        );
    }
    for (id, fs_path, mount, _dir) in apps::file_volume_rows(db, a.id)
        .await
        .map_err(|e| e.to_string())?
    {
        s.item(
            "storage",
            &format!("file_{id}"),
            json!({"source": fs_path, "destination": mount}),
            "redeploy",
            false,
        );
    }

    // comparableSnapshot: sections and items sorted by key; an empty section is `[]`.
    let mut sections = s.sections;
    sections.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Map::new();
    for (name, items) in sections {
        let mut entries: Vec<(String, Value)> = items.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert(
            name.to_owned(),
            if entries.is_empty() {
                Value::Array(vec![])
            } else {
                Value::Object(entries.into_iter().collect())
            },
        );
    }
    let comparable = json!({"schema_version": 1, "sections": Value::Object(out)});
    Ok(sha2::Sha256::digest(php_json(&comparable).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
