//! `App\Services\DeploymentConfiguration`: `ApplicationConfigurationSnapshot` (what an
//! application deploys with), `ConfigurationDiffer`/`ConfigurationDiff` (what changed since
//! the last deployment) and the `Application` methods that store both on the deployment
//! queue (`configuration_snapshot`, `configuration_diff`, `EncryptedArrayCast`).

use std::cmp::Ordering;

use chrono::{NaiveDateTime, SubsecRound as _, Utc};
use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Digest as _;

use super::domains::{normalize_url, without_port};

/// `ApplicationConfigurationSnapshot::SCHEMA_VERSION`.
pub const SCHEMA_VERSION: i64 = 1;

/// `DEFAULT_STOP_GRACE_PERIOD_SECONDS`.
const DEFAULT_STOP_GRACE_PERIOD: i64 = 30;

/// `SummarizesDiffText::SINGLE_LINE_LIMIT`.
const SINGLE_LINE_LIMIT: usize = 40;

/// `ConfigurationDiffer::IGNORED_KEYS`.
const IGNORED_KEYS: [&str; 1] = ["build.docker_compose"];

/// `ConfigurationDiffer::DYNAMIC_SECTIONS`.
const DYNAMIC_SECTIONS: [&str; 2] = ["environment", "storage"];

// ---------------------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------------------

/// An `environment_variables` row as `environmentItem()` reads it (`is_preview = false`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    /// `$environmentVariable->value`: decrypted and trimmed, null when empty.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub is_multiline: Value,
    #[serde(default)]
    pub is_literal: Value,
    #[serde(default)]
    pub is_buildtime: Value,
    #[serde(default)]
    pub is_runtime: Value,
    #[serde(default)]
    pub is_shown_once: Value,
}

/// A `local_persistent_volumes` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub id: i64,
    pub name: String,
    pub mount_path: String,
    #[serde(default)]
    pub host_path: Option<String>,
}

/// A `local_file_volumes` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMount {
    pub id: i64,
    pub fs_path: String,
    #[serde(default)]
    pub mount_path: Option<String>,
    #[serde(default)]
    pub is_directory: bool,
}

/// Everything `ApplicationConfigurationSnapshot::toArray()` reads, already decrypted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInput {
    /// The `applications` row (`row_to_json`).
    pub application: Record,
    /// The `application_settings` row (empty without one).
    #[serde(default)]
    pub settings: Record,
    /// `$application->http_basic_auth_password` (the `encrypted` cast, decrypted).
    #[serde(default)]
    pub http_basic_auth_password: Option<String>,
    /// `$application->environment_variables` in table order.
    #[serde(default)]
    pub environment_variables: Vec<EnvVar>,
    /// Persistent volumes by id.
    #[serde(default)]
    pub volumes: Vec<Volume>,
    /// File mounts by id.
    #[serde(default)]
    pub file_mounts: Vec<FileMount>,
}

/// Loads what `toArray()` reads for an application.
pub async fn load_input(
    db: &Db,
    enc: &Encrypter,
    a: &ApplicationRow,
) -> Result<SnapshotInput, String> {
    let mac = |_| "The MAC is invalid.".to_owned();
    let http_basic_auth_password = match a.str("http_basic_auth_password") {
        Some(c) => Some(enc.decrypt_string(c).map_err(mac)?),
        None => None,
    };
    let mut environment_variables = Vec::new();
    for v in coolify_db::environment_variables::for_resource(db, apps::MORPH, a.id)
        .await
        .map_err(|e| e.to_string())?
    {
        // `environment_variables()` is `where('is_preview', false)`.
        if v.record.get("is_preview") != Some(&Value::Bool(false)) {
            continue;
        }
        // `get_environment_variables()`: `! $value` is null, otherwise the decrypted value
        // without edge whitespace (multiline content is kept as it is).
        let value = match v.value_encrypted.as_deref() {
            None | Some("" | "0") => None,
            Some(c) => Some(
                crate::databases::env::without_edge_whitespace(
                    &enc.decrypt_maybe_serialized(c).map_err(mac)?,
                )
                .to_owned(),
            ),
        };
        let raw = |k: &str| v.record.get(k).cloned().unwrap_or(Value::Null);
        environment_variables.push(EnvVar {
            key: v.key.clone(),
            value,
            is_multiline: raw("is_multiline"),
            is_literal: raw("is_literal"),
            is_buildtime: raw("is_buildtime"),
            is_runtime: raw("is_runtime"),
            is_shown_once: raw("is_shown_once"),
        });
    }
    let volumes = apps::volume_rows(db, a.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, name, mount_path, host_path)| Volume {
            id,
            name,
            mount_path,
            host_path,
        })
        .collect();
    let file_mounts = apps::file_volume_rows(db, a.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, fs_path, mount_path, is_directory)| FileMount {
            id,
            fs_path,
            mount_path,
            is_directory,
        })
        .collect();
    Ok(SnapshotInput {
        application: a.raw.clone(),
        settings: a.settings.clone(),
        http_basic_auth_password,
        environment_variables,
        volumes,
        file_mounts,
    })
}

// ---------------------------------------------------------------------------------------
// ApplicationConfigurationSnapshot
// ---------------------------------------------------------------------------------------

/// The optional arguments of `item()`.
#[derive(Default)]
struct Extra {
    sensitive: bool,
    display_value: Option<String>,
    display_full: Option<String>,
    lines: bool,
}

struct Builder<'a> {
    enc: &'a Encrypter,
    input: &'a SnapshotInput,
}

impl Builder<'_> {
    fn col(&self, k: &str) -> Value {
        self.input
            .application
            .get(k)
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn col_str(&self, k: &str) -> Option<&str> {
        self.input.application.get(k).and_then(Value::as_str)
    }

    /// `data_get($this->application, 'settings.<k>')`.
    fn set(&self, k: &str) -> Value {
        self.input.settings.get(k).cloned().unwrap_or(Value::Null)
    }

    /// `sensitiveHash()`: `hash_hmac('sha256', json_encode($value), config('app.key'))`.
    fn sensitive_hash(&self, v: &Value) -> String {
        self.enc.hmac_with_app_key(php_json(v).as_bytes())
    }

    /// `item()`.
    fn item(&self, key: &str, label: &str, value: Value, impact: &str, extra: Extra) -> Value {
        let normalized = normalize(value);
        let compare = if extra.sensitive {
            Value::String(self.sensitive_hash(&normalized))
        } else {
            normalized.clone()
        };
        let display_value = extra
            .display_value
            .unwrap_or_else(|| display_value(&normalized));
        let display_full = if extra.sensitive {
            None
        } else {
            expandable_text(
                extra
                    .display_full
                    .or_else(|| stringify_value(&normalized))
                    .as_deref(),
            )
        };
        json!({
            "key": key,
            "label": label,
            "impact": impact,
            "sensitive": extra.sensitive,
            "diff_mode": if extra.lines { "lines" } else { "default" },
            "compare_value": compare,
            "display_value": display_value,
            "display_full": display_full,
        })
    }

    fn plain(&self, key: &str, label: &str, value: Value, impact: &str) -> Value {
        self.item(key, label, value, impact, Extra::default())
    }

    /// `sourceItems()`.
    fn source_items(&self) -> Vec<Value> {
        [
            ("git_repository", "Repository"),
            ("git_branch", "Branch"),
            ("git_commit_sha", "Commit SHA"),
            ("source_id", "Source ID"),
            ("source_type", "Source type"),
            ("private_key_id", "Private key"),
        ]
        .into_iter()
        .map(|(k, l)| self.plain(k, l, self.col(k), "build"))
        .collect()
    }

    /// A text item shown summarized, expandable to the full text (`dockerfile`, ...).
    fn text_item(
        &self,
        key: &str,
        label: &str,
        raw: Option<&str>,
        impact: &str,
        lines: bool,
    ) -> Value {
        let value = raw.map_or(Value::Null, |s| Value::String(s.to_owned()));
        self.item(
            key,
            label,
            value,
            impact,
            Extra {
                display_value: Some(summarize_text(raw)),
                display_full: raw.map(str::to_owned),
                lines,
                ..Extra::default()
            },
        )
    }

    /// `buildItems()`.
    fn build_items(&self) -> Vec<Value> {
        let mut out = vec![
            self.plain("build_pack", "Build pack", self.col("build_pack"), "build"),
            self.plain("is_static", "Static site", self.set("is_static"), "build"),
            self.plain(
                "is_spa",
                "Single-page application",
                self.set("is_spa"),
                "build",
            ),
        ];
        for (k, l) in [
            ("static_image", "Static image"),
            ("base_directory", "Base directory"),
            ("publish_directory", "Publish directory"),
            ("install_command", "Install command"),
            ("build_command", "Build command"),
        ] {
            out.push(self.plain(k, l, self.col(k), "build"));
        }
        out.push(self.text_item(
            "dockerfile",
            "Dockerfile",
            self.col_str("dockerfile"),
            "build",
            false,
        ));
        for (k, l) in [
            ("dockerfile_location", "Dockerfile location"),
            ("dockerfile_target_build", "Dockerfile target"),
            ("docker_compose_location", "Docker Compose location"),
        ] {
            out.push(self.plain(k, l, self.col(k), "build"));
        }
        // The generated docker_compose is intentionally excluded (re-rendered on every parse).
        out.push(self.text_item(
            "docker_compose_raw",
            "Docker Compose",
            self.col_str("docker_compose_raw"),
            "build",
            true,
        ));
        out.push(self.plain(
            "docker_compose_custom_build_command",
            "Docker Compose custom build command",
            self.col("docker_compose_custom_build_command"),
            "build",
        ));
        for (k, l) in [
            ("is_git_submodules_enabled", "Git submodules"),
            ("is_git_lfs_enabled", "Git LFS"),
            ("is_git_shallow_clone_enabled", "Shallow clone"),
            ("is_env_sorting_enabled", "Sort environment variables"),
        ] {
            out.push(self.plain(k, l, self.set(k), "build"));
        }
        out.push(self.plain(
            "custom_docker_run_options",
            "Custom Docker run options",
            self.col("custom_docker_run_options"),
            "redeploy",
        ));
        for (k, l) in [
            ("use_build_secrets", "Use build secrets"),
            (
                "inject_build_args_to_dockerfile",
                "Inject build args to Dockerfile",
            ),
            (
                "include_source_commit_in_build",
                "Include source commit in build",
            ),
            (
                "include_network_hosts_in_build",
                "Include network hosts in build",
            ),
            ("disable_build_cache", "Disable build cache"),
            ("is_build_server_enabled", "Build server"),
        ] {
            out.push(self.plain(k, l, self.set(k), "build"));
        }
        out
    }

    /// `runtimeItems()` (with `healthCheckItems()` and `limitItems()`).
    fn runtime_items(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (k, l) in [
            ("docker_registry_image_name", "Docker image"),
            ("docker_registry_image_tag", "Docker image tag or hash"),
            ("start_command", "Start command"),
            ("pre_deployment_command", "Pre-deployment command"),
            (
                "pre_deployment_command_container",
                "Pre-deployment command container",
            ),
            ("post_deployment_command", "Post-deployment command"),
            (
                "post_deployment_command_container",
                "Post-deployment command container",
            ),
            (
                "docker_compose_custom_start_command",
                "Docker Compose custom start command",
            ),
            ("ports_exposes", "Exposed ports"),
            ("ports_mappings", "Port mappings"),
        ] {
            out.push(self.plain(k, l, self.col(k), "redeploy"));
        }
        out.push(self.plain(
            "custom_network_aliases",
            "Network aliases",
            network_aliases(self.col_str("custom_network_aliases")),
            "redeploy",
        ));
        for (k, l) in [
            ("connect_to_docker_network", "Connect to Docker network"),
            ("custom_internal_name", "Custom container name"),
            ("custom_container_name_prefix", "Container name prefix"),
            (
                "is_consistent_container_name_enabled",
                "Consistent container name",
            ),
            (
                "is_container_label_escape_enabled",
                "Escape container labels",
            ),
            (
                "is_container_label_readonly_enabled",
                "Read-only container labels",
            ),
            ("is_log_drain_enabled", "Log drain"),
            ("is_swarm_only_worker_nodes", "Swarm worker nodes only"),
        ] {
            out.push(self.plain(k, l, self.set(k), "redeploy"));
        }
        // `normalizedStopGracePeriod()`.
        let grace = self
            .input
            .settings
            .get("stop_grace_period")
            .and_then(Value::as_i64)
            .filter(|v| *v != DEFAULT_STOP_GRACE_PERIOD);
        out.push(self.plain(
            "stop_grace_period",
            "Stop grace period",
            grace.map_or(Value::Null, Value::from),
            "redeploy",
        ));
        for (k, l) in [
            ("is_preserve_repository_enabled", "Preserve repository"),
            (
                "is_raw_compose_deployment_enabled",
                "Raw Compose deployment",
            ),
            ("is_gpu_enabled", "GPU enabled"),
            ("gpu_driver", "GPU driver"),
            ("gpu_count", "GPU count"),
            ("gpu_device_ids", "GPU device IDs"),
            ("gpu_options", "GPU options"),
        ] {
            out.push(self.plain(k, l, self.set(k), "redeploy"));
        }
        for (k, l) in [
            ("health_check_enabled", "Health check enabled"),
            ("health_check_path", "Health check path"),
            ("health_check_port", "Health check port"),
            ("health_check_host", "Health check host"),
            ("health_check_method", "Health check method"),
            ("health_check_return_code", "Health check return code"),
            ("health_check_scheme", "Health check scheme"),
            ("health_check_response_text", "Health check response text"),
            ("health_check_interval", "Health check interval"),
            ("health_check_timeout", "Health check timeout"),
            ("health_check_retries", "Health check retries"),
            ("health_check_start_period", "Health check start period"),
            ("health_check_type", "Health check type"),
            ("health_check_command", "Health check command"),
            ("limits_memory", "Memory limit"),
            ("limits_memory_swap", "Memory swap limit"),
            ("limits_memory_swappiness", "Memory swappiness"),
            ("limits_memory_reservation", "Memory reservation"),
            ("limits_cpus", "CPU limit"),
            ("limits_cpuset", "CPU set"),
            ("limits_cpu_shares", "CPU shares"),
            ("swarm_replicas", "Swarm replicas"),
            ("swarm_placement_constraints", "Swarm placement constraints"),
        ] {
            out.push(self.plain(k, l, self.col(k), "redeploy"));
        }
        out
    }

    /// `domainItems()`.
    fn domain_items(&self) -> Vec<Value> {
        let mut out = vec![self.plain("fqdn", "Domains", self.col("fqdn"), "redeploy")];
        // `DomainPortOverrides::sorted($this->domain_port_overrides)`.
        let overrides = match php_array(self.col("domain_port_overrides")) {
            Value::Null => Value::Array(vec![]),
            v @ (Value::Array(_) | Value::Object(_)) => sort_keys(v),
            scalar => Value::Array(vec![scalar]),
        };
        out.push(self.plain(
            "domain_port_overrides",
            "Domain port overrides",
            overrides,
            "redeploy",
        ));
        out.push(self.plain(
            "noindex_domains",
            "Search engine indexing",
            self.noindex_domains(),
            "redeploy",
        ));
        let compose_domains = self.decoded_compose_domains();
        let compose_text = compose_domains_text(&compose_domains);
        out.push(self.item(
            "docker_compose_domains",
            "Service domains",
            compose_domains,
            "redeploy",
            Extra {
                display_value: Some(summarize_text(compose_text.as_deref())),
                display_full: compose_text,
                lines: true,
                ..Extra::default()
            },
        ));
        out.push(self.plain("redirect", "Redirect", self.col("redirect"), "redeploy"));
        let labels = decode_custom_labels(self.col_str("custom_labels"));
        out.push(self.item(
            "custom_labels",
            "Container labels",
            self.col("custom_labels"),
            "redeploy",
            Extra {
                display_value: Some(summarize_text(labels.as_deref())),
                display_full: labels,
                lines: true,
                ..Extra::default()
            },
        ));
        // The model getter base64-decodes the stored value.
        let nginx = self.col_str("custom_nginx_configuration").map(|v| {
            String::from_utf8_lossy(&php_base64_decode(v, false).unwrap_or_default()).into_owned()
        });
        out.push(self.text_item(
            "custom_nginx_configuration",
            "Custom Nginx configuration",
            nginx.as_deref(),
            "build",
            false,
        ));
        for (k, l) in [
            ("is_force_https_enabled", "Force HTTPS"),
            ("is_gzip_enabled", "Gzip"),
            ("is_stripprefix_enabled", "Strip prefix"),
            ("traefik_certificate_resolver", "Certificate resolver"),
        ] {
            out.push(self.plain(k, l, self.set(k), "redeploy"));
        }
        out.push(self.plain(
            "is_http_basic_auth_enabled",
            "HTTP basic auth",
            self.col("is_http_basic_auth_enabled"),
            "redeploy",
        ));
        out.push(self.plain(
            "http_basic_auth_username",
            "HTTP basic auth username",
            self.col("http_basic_auth_username"),
            "redeploy",
        ));
        out.push(
            self.item(
                "http_basic_auth_password",
                "HTTP basic auth password",
                self.input
                    .http_basic_auth_password
                    .clone()
                    .map_or(Value::Null, Value::String),
                "redeploy",
                Extra {
                    sensitive: true,
                    ..Extra::default()
                },
            ),
        );
        for (k, l) in [
            (
                "http_basic_auth_hash_algorithm",
                "HTTP basic auth hash algorithm",
            ),
            (
                "http_basic_auth_bcrypt_cost",
                "HTTP basic auth bcrypt cost factor",
            ),
            (
                "http_basic_auth_argon2id_memory_cost",
                "HTTP basic auth Argon2id memory (KiB)",
            ),
            (
                "http_basic_auth_argon2id_time_cost",
                "HTTP basic auth Argon2id iterations",
            ),
        ] {
            out.push(self.plain(k, l, self.col(k), "redeploy"));
        }
        out
    }

    /// `$application->noindexDomains()->all()`.
    fn noindex_domains(&self) -> Value {
        let items = match php_array(self.col("noindex_domains")) {
            Value::Null => vec![],
            Value::Array(a) => a,
            Value::Object(o) => o.into_iter().map(|(_, v)| v).collect(),
            scalar => vec![scalar],
        };
        let mut out: Vec<String> = Vec::new();
        for d in items {
            let Value::String(d) = d else { continue };
            if php_trim(&d).is_empty() {
                continue;
            }
            let n = without_port(&normalize_url(&d));
            if !out.contains(&n) {
                out.push(n);
            }
        }
        Value::Array(out.into_iter().map(Value::String).collect())
    }

    /// `decodedComposeDomains()`.
    fn decoded_compose_domains(&self) -> Value {
        let Some(raw) = self.col_str("docker_compose_domains") else {
            return Value::Null;
        };
        if php_trim(raw).is_empty() {
            return Value::Null;
        }
        match serde_json::from_str::<Value>(raw).map(php_array) {
            Ok(v @ (Value::Array(_) | Value::Object(_))) => v,
            _ => Value::Null,
        }
    }

    /// `environmentItems()`.
    fn environment_items(&self) -> Vec<Value> {
        let mut vars: Vec<&EnvVar> = self.input.environment_variables.iter().collect();
        // `sortBy('key', SORT_NATURAL | SORT_FLAG_CASE)` (stable).
        vars.sort_by(|a, b| strnatcasecmp(a.key.as_bytes(), b.key.as_bytes()));
        vars.into_iter().map(|v| self.environment_item(v)).collect()
    }

    /// `environmentItem()`.
    fn environment_item(&self, v: &EnvVar) -> Value {
        let impact = if truthy(&v.is_buildtime) {
            "build"
        } else {
            "redeploy"
        };
        let value = v.value.clone().map_or(Value::Null, Value::String);
        let compare = json!({
            "value_hash": self.sensitive_hash(&value),
            "is_multiline": v.is_multiline,
            "is_literal": v.is_literal,
            "is_buildtime": v.is_buildtime,
            "is_runtime": v.is_runtime,
        });
        // Locked (is_shown_once) variables are always redacted and never store a value.
        if truthy(&v.is_shown_once) {
            let flags = environment_flags(v);
            return self.item(
                &v.key,
                &v.key,
                compare,
                impact,
                Extra {
                    sensitive: true,
                    display_value: Some(if flags.is_empty() {
                        "Hidden".to_owned()
                    } else {
                        format!("Hidden ({flags})")
                    }),
                    ..Extra::default()
                },
            );
        }
        let text = v.value.clone().unwrap_or_default();
        self.item(
            &v.key,
            &v.key,
            Value::String(self.sensitive_hash(&normalize(compare))),
            impact,
            Extra {
                display_value: Some(summarize_text(Some(&text))),
                display_full: Some(environment_lines(v)),
                lines: true,
                ..Extra::default()
            },
        )
    }

    /// `storageItems()`.
    fn storage_items(&self) -> Vec<Value> {
        let uuid = self.col_str("uuid").unwrap_or_default();
        let mut out = Vec::new();
        for v in &self.input.volumes {
            let host = v.host_path.as_deref();
            let host_blank = host.is_none_or(|h| php_trim(h).is_empty());
            if host_blank && is_preview_volume_name(&v.name, uuid) {
                continue;
            }
            // `$volume->host_path ?: $volume->name`.
            let source = match host {
                Some(h) if !h.is_empty() && h != "0" => h,
                _ => v.name.as_str(),
            };
            out.push(self.item(
                &format!("volume_{}", v.id),
                if host_blank {
                    "Volume mount"
                } else {
                    "Directory mount"
                },
                json!({"source": source, "destination": v.mount_path}),
                "redeploy",
                Extra {
                    display_value: Some(format!("{source} → {}", v.mount_path)),
                    ..Extra::default()
                },
            ));
        }
        for f in &self.input.file_mounts {
            out.push(self.item(
                &format!("file_{}", f.id),
                if f.is_directory {
                    "Directory mount"
                } else {
                    "File mount"
                },
                json!({"source": f.fs_path, "destination": f.mount_path}),
                "redeploy",
                Extra {
                    display_value: Some(format!(
                        "{} → {}",
                        f.fs_path,
                        f.mount_path.as_deref().unwrap_or_default()
                    )),
                    ..Extra::default()
                },
            ));
        }
        out
    }
}

/// `ApplicationConfigurationSnapshot::toArray()` for loaded inputs.
pub fn build(input: &SnapshotInput, enc: &Encrypter) -> Value {
    let b = Builder { enc, input };
    let section = |label: &str, items: Vec<Value>| json!({"label": label, "items": items});
    json!({
        "schema_version": SCHEMA_VERSION,
        "resource_type": apps::MORPH,
        "resource_id": input.application.get("id").cloned().unwrap_or(Value::Null),
        "sections": {
            "source": section("Source", b.source_items()),
            "build": section("Build", b.build_items()),
            "runtime": section("Runtime", b.runtime_items()),
            "domains": section("Domains & Proxy", b.domain_items()),
            "environment": section("Environment Variables", b.environment_items()),
            "storage": section("Storage", b.storage_items()),
        },
    })
}

/// `Application::deploymentConfigurationSnapshot()`.
pub async fn snapshot(db: &Db, enc: &Encrypter, a: &ApplicationRow) -> Result<Value, String> {
    Ok(build(&load_input(db, enc, a).await?, enc))
}

/// `Application::deploymentConfigurationHash()`: what `config_hash` stores.
pub async fn config_hash(db: &Db, enc: &Encrypter, a: &ApplicationRow) -> Result<String, String> {
    Ok(hash_snapshot(&snapshot(db, enc, a).await?))
}

/// `ApplicationConfigurationSnapshot::hashSnapshot()`.
pub fn hash_snapshot(snapshot: &Value) -> String {
    sha2::Sha256::digest(php_json(&comparable_snapshot(snapshot)).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `ApplicationConfigurationSnapshot::comparableSnapshot()`.
pub fn comparable_snapshot(snapshot: &Value) -> Value {
    let mut sections: Vec<(String, Value)> = Vec::new();
    if let Some(all) = snapshot.get("sections").and_then(Value::as_object) {
        for (name, section) in all {
            let mut items = Map::new();
            for item in list_values(section.get("items")) {
                let key = php_string(item.get("key").unwrap_or(&Value::Null));
                let compare = item.get("compare_value").cloned().unwrap_or(Value::Null);
                let impact = match item.get("impact") {
                    None | Some(Value::Null) => Value::String("redeploy".into()),
                    Some(v) => v.clone(),
                };
                items.insert(key, json!({"compare_value": compare, "impact": impact}));
            }
            sections.push((name.clone(), sort_keys(Value::Object(items))));
        }
    }
    sections.sort_by(|a, b| php_str_cmp(&a.0, &b.0));
    json!({
        "schema_version": snapshot.get("schema_version").cloned().unwrap_or(Value::Null),
        "sections": list_like(Value::Object(sections.into_iter().collect())),
    })
}

/// `ApplicationConfigurationSnapshot::withoutPreviewVolumes()`: drops the volumes of
/// Docker Compose preview deployments that older snapshots contain.
pub fn without_preview_volumes(mut snapshot: Value, application_uuid: &str) -> Value {
    let Some(items) = snapshot
        .pointer_mut("/sections/storage/items")
        .filter(|v| v.is_array() || v.is_object())
    else {
        return snapshot;
    };
    let kept: Vec<Value> = list_values(Some(items))
        .into_iter()
        .filter(|item| {
            let key = php_string(item.get("key").unwrap_or(&Value::Null));
            let source = php_string(
                item.pointer("/compare_value/source")
                    .unwrap_or(&Value::Null),
            );
            !(key.starts_with("volume_") && is_preview_volume_name(&source, application_uuid))
        })
        .cloned()
        .collect();
    *items = Value::Array(kept);
    snapshot
}

/// `isPreviewVolumeName()`: `/^{uuid}[_-].+-pr-\d+$/`.
fn is_preview_volume_name(name: &str, uuid: &str) -> bool {
    let Some(rest) = name.strip_prefix(uuid) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(['_', '-']) else {
        return false;
    };
    // `$` also matches before a final newline; `.` matches anything but a newline.
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    if rest.contains('\n') {
        return false;
    }
    let digits = rest.len() - rest.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return false;
    }
    rest[..rest.len() - digits]
        .strip_suffix("-pr-")
        .is_some_and(|head| !head.is_empty())
}

// ---------------------------------------------------------------------------------------
// ConfigurationDiffer / ConfigurationDiff
// ---------------------------------------------------------------------------------------

/// `ConfigurationDiff`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigurationDiff {
    changes: Vec<Value>,
    legacy_fallback: bool,
}

impl ConfigurationDiff {
    /// `ConfigurationDiff::unchanged()`.
    pub fn unchanged() -> Self {
        Self::default()
    }

    /// `ConfigurationDiff::legacy()`: the last deployment stored no snapshot, so report no
    /// changes but build on the next deployment.
    pub fn legacy() -> Self {
        Self {
            changes: Vec::new(),
            legacy_fallback: true,
        }
    }

    /// `ConfigurationDiff::fromChanges()`.
    pub fn from_changes(changes: Vec<Value>) -> Self {
        Self {
            changes,
            legacy_fallback: false,
        }
    }

    /// `isChanged()`.
    pub fn is_changed(&self) -> bool {
        !self.changes.is_empty()
    }

    /// `isLegacyFallback()`.
    pub fn is_legacy_fallback(&self) -> bool {
        self.legacy_fallback
    }

    /// `count()`.
    pub fn count(&self) -> usize {
        self.changes.len()
    }

    /// `requiresBuild()`.
    pub fn requires_build(&self) -> bool {
        self.legacy_fallback
            || self
                .changes
                .iter()
                .any(|c| c.get("impact").and_then(Value::as_str) == Some("build"))
    }

    /// `requiresRedeploy()`.
    pub fn requires_redeploy(&self) -> bool {
        self.is_changed()
    }

    /// `changes()`.
    pub fn changes(&self) -> &[Value] {
        &self.changes
    }

    /// `toArray()`.
    pub fn to_array(&self) -> Value {
        json!({
            "changed": self.is_changed(),
            "count": self.count(),
            "requires_build": self.requires_build(),
            "requires_redeploy": self.requires_redeploy(),
            "legacy_fallback": self.is_legacy_fallback(),
            "changes": self.changes,
        })
    }
}

/// `ConfigurationDiffer::flattenItems()`: `"{section}.{key}"` => item + section + label.
fn flatten_items(snapshot: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(sections) = snapshot.get("sections").and_then(Value::as_object) else {
        return out;
    };
    for (name, section) in sections {
        let label = match section.get("label") {
            Some(l) => l.clone(),
            None => Value::String(headline(name)),
        };
        for item in list_values(section.get("items")) {
            let mut merged = item.as_object().cloned().unwrap_or_default();
            let key = format!(
                "{name}.{}",
                php_string(item.get("key").unwrap_or(&Value::Null))
            );
            merged.insert("section".into(), Value::String(name.clone()));
            merged.insert("section_label".into(), label.clone());
            out.insert(key, Value::Object(merged));
        }
    }
    out
}

/// `data_get($target, $key, $default)` for one segment.
fn data_get<'a>(target: Option<&'a Value>, key: &str, default: &'a Value) -> &'a Value {
    target.and_then(|t| t.get(key)).unwrap_or(default)
}

/// `ConfigurationDiffer::diff()`.
pub fn diff(previous: &Value, current: &Value) -> ConfigurationDiff {
    let previous_items = flatten_items(previous);
    let current_items = flatten_items(current);
    let mut keys: Vec<&String> = previous_items.keys().collect();
    for k in current_items.keys() {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.sort_by(|a, b| php_str_cmp(a, b));

    let null = Value::Null;
    let dash = Value::String("-".into());
    let mut changes = Vec::new();
    for key in keys {
        if IGNORED_KEYS.contains(&key.as_str()) {
            continue;
        }
        let prev = previous_items.get(key);
        let cur = current_items.get(key);
        // Static settings missing from an older snapshot were newly tracked, not changed.
        if prev.is_none()
            && !previous_items.is_empty()
            && !data_get(cur, "section", &null)
                .as_str()
                .is_some_and(|s| DYNAMIC_SECTIONS.contains(&s))
        {
            continue;
        }
        let compare = |i: Option<&Value>| match i.and_then(|i| i.get("compare_value")) {
            Some(v) => v.clone(),
            None => Value::Null,
        };
        if php_identical(&compare(prev), &compare(cur)) {
            continue;
        }
        let item = cur.or(prev);
        let kind = match (prev, cur) {
            (None, _) => "added",
            (_, None) => "removed",
            _ => "changed",
        };
        let sensitive = truthy(data_get(prev, "sensitive", &null))
            || truthy(data_get(cur, "sensitive", &null))
            || (kind == "removed"
                && data_get(item, "section", &null).as_str() == Some("environment"));
        let display_summary = (sensitive && kind == "changed").then_some("Changed");
        let lines = data_get(item, "diff_mode", &null).as_str() == Some("lines");

        let (old_display, new_display, old_full, new_full): (Value, Value, Value, Value);
        if sensitive {
            let mask = |i: Option<&Value>| {
                if i.is_none() {
                    "-"
                } else {
                    "••••••••"
                }
            };
            old_display = Value::String(mask(prev).into());
            new_display = Value::String(mask(cur).into());
            old_full = Value::Null;
            new_full = Value::Null;
        } else if lines && kind == "changed" {
            let (mut o, mut n) = changed_lines(
                data_get(prev, "display_full", &null).as_str(),
                data_get(cur, "display_full", &null).as_str(),
            );
            // No line-level difference (e.g. only reordering): fall back to the summary.
            let (mut ov, mut nv) = (Value::String(o.clone()), Value::String(n.clone()));
            if o == "-" && n == "-" {
                ov = data_get(prev, "display_value", &dash).clone();
                nv = data_get(cur, "display_value", &dash).clone();
                o = php_string(&ov);
                n = php_string(&nv);
            }
            old_full = expandable_text(Some(&o)).map_or(Value::Null, Value::String);
            new_full = expandable_text(Some(&n)).map_or(Value::Null, Value::String);
            old_display = ov;
            new_display = nv;
        } else {
            old_display = data_get(prev, "display_value", &dash).clone();
            new_display = data_get(cur, "display_value", &dash).clone();
            old_full = data_get(prev, "display_full", &null).clone();
            new_full = data_get(cur, "display_full", &null).clone();
        }
        let expandable = !sensitive && (filled(&old_full) || filled(&new_full));
        changes.push(json!({
            "key": key,
            "section": data_get(item, "section", &null),
            "section_label": data_get(item, "section_label", &null),
            "label": data_get(item, "label", &null),
            "type": kind,
            "impact": data_get(item, "impact", &Value::String("redeploy".into())),
            "sensitive": sensitive,
            "display_summary": display_summary,
            "old_display_value": old_display,
            "new_display_value": new_display,
            "old_full_value": old_full,
            "new_full_value": new_full,
            "expandable": expandable,
        }));
    }
    ConfigurationDiff::from_changes(changes)
}

/// `changedLines()`: only the lines that differ (`array_diff` both ways).
fn changed_lines(old: Option<&str>, new: Option<&str>) -> (String, String) {
    let old_lines = text_lines(old);
    let new_lines = text_lines(new);
    let removed: Vec<&str> = old_lines
        .iter()
        .filter(|l| !new_lines.contains(l))
        .map(String::as_str)
        .collect();
    let added: Vec<&str> = new_lines
        .iter()
        .filter(|l| !old_lines.contains(l))
        .map(String::as_str)
        .collect();
    let join = |v: Vec<&str>| {
        if v.is_empty() {
            "-".to_owned()
        } else {
            v.join("\n")
        }
    };
    (join(removed), join(added))
}

/// `textLines()`: keeps leading indentation, drops trailing whitespace and blank lines.
fn text_lines(value: Option<&str>) -> Vec<String> {
    let Some(value) = value.filter(|v| !php_trim(v).is_empty()) else {
        return Vec::new();
    };
    value
        .split("\r\n")
        .flat_map(|l| l.split(['\r', '\n']))
        .map(php_rtrim)
        .filter(|l| !php_trim(l).is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------------------
// Application methods and storage
// ---------------------------------------------------------------------------------------

/// `EncryptedArrayCast::get()`: decrypts (tolerating legacy plaintext JSON) and decodes.
pub fn decode_encrypted_array(enc: &Encrypter, raw: Option<&str>) -> Option<Value> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let text = enc.decrypt_string(raw).unwrap_or_else(|_| raw.to_owned());
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(|v| v.is_array() || v.is_object())
}

/// `EncryptedArrayCast::set()`: `Crypt::encryptString(json_encode($value))`.
pub fn encode_encrypted_array(enc: &Encrypter, value: &Value) -> String {
    enc.encrypt_string(&php_json(value))
}

/// A decoded snapshot PHP treats as present (`if (! $previousSnapshot)`).
fn present(v: Option<Value>) -> Option<Value> {
    v.filter(truthy)
}

/// `Application::pendingDeploymentConfigurationDiff()` (production, pull request 0).
pub async fn pending_diff(
    db: &Db,
    enc: &Encrypter,
    a: &ApplicationRow,
) -> Result<ConfigurationDiff, String> {
    // `get_last_successful_deployment()?->configuration_snapshot`.
    let last = apps::last_finished_deployment(db, a.id, 0)
        .await
        .map_err(|e| e.to_string())?;
    let Some(previous) =
        present(last.and_then(|d| decode_encrypted_array(enc, d.str("configuration_snapshot"))))
    else {
        return Ok(ConfigurationDiff::legacy());
    };
    let current = snapshot(db, enc, a).await?;
    Ok(diff(&without_preview_volumes(previous, &a.uuid), &current))
}

/// `Application::markDeploymentConfigurationApplied($deployment)`: stores the snapshot, its
/// hash and the diff against the previous finished deployment of the same pull request on
/// the queue row, and the hash as the application's `config_hash`.
pub async fn mark_applied(
    db: &Db,
    enc: &Encrypter,
    app_id: i64,
    queue_id: i64,
    pull_request_id: i64,
) -> Result<(), String> {
    // `$this->refresh()`.
    let a = apps::find_by_id(db, app_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No query results for model [App\\Models\\Application].".to_owned())?;
    let current = snapshot(db, enc, &a).await?;
    let hash = hash_snapshot(&current);
    let previous =
        apps::last_finished_deployment_with_snapshot(db, app_id, pull_request_id, queue_id)
            .await
            .map_err(|e| e.to_string())?;
    let configuration_diff = present(
        previous.and_then(|d| decode_encrypted_array(enc, d.str("configuration_snapshot"))),
    )
    .map(|p| diff(&without_preview_volumes(p, &a.uuid), &current).to_array());
    let now = timestamp();
    apps::update_deployment(
        db,
        queue_id,
        &[
            ("configuration_hash", ColumnValue::Text(Some(hash.clone()))),
            (
                "configuration_snapshot",
                ColumnValue::Text(Some(encode_encrypted_array(enc, &current))),
            ),
            (
                "configuration_diff",
                ColumnValue::Text(configuration_diff.map(|d| encode_encrypted_array(enc, &d))),
            ),
        ],
        now,
    )
    .await
    .map_err(|e| e.to_string())?;
    apps::update_if_changed(
        db,
        app_id,
        &[("config_hash", ColumnValue::Text(Some(hash)))],
        now,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Laravel's `freshTimestamp()` as stored (`Y-m-d H:i:s`).
fn timestamp() -> NaiveDateTime {
    Utc::now().naive_utc().trunc_subsecs(0)
}

// ---------------------------------------------------------------------------------------
// Value helpers (PHP semantics)
// ---------------------------------------------------------------------------------------

fn php_json(v: &Value) -> String {
    coolify_compat::php_json::encode(v)
}

fn php_trim(s: &str) -> &str {
    coolify_remote::text::php_trim(s)
}

/// `rtrim()` with the default characters.
fn php_rtrim(s: &str) -> &str {
    s.trim_end_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// PHP truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `filled()`.
fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !php_trim(s).is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// `(string) $value`.
fn php_string(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "1".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => "Array".into(),
    }
}

/// The values of a PHP array (`collect($items)`), list or map.
fn list_values(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => Vec::new(),
    }
}

/// A decoded JSON value as PHP holds it (`json_decode($v, true)` then `json_encode`):
/// maps whose keys are `0..n` become lists, and so does an empty map.
fn php_array(v: Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(php_array).collect()),
        Value::Object(o) => list_like(Value::Object(
            o.into_iter().map(|(k, v)| (k, php_array(v))).collect(),
        )),
        other => other,
    }
}

/// `array_is_list()` maps are encoded as JSON lists.
fn list_like(v: Value) -> Value {
    match v {
        Value::Object(o) if o.keys().enumerate().all(|(i, k)| *k == i.to_string()) => {
            Value::Array(o.into_iter().map(|(_, v)| v).collect())
        }
        other => other,
    }
}

/// `normalizeValue()`: `''` is null, arrays are sorted recursively.
fn normalize(v: Value) -> Value {
    match v {
        Value::String(s) if s.is_empty() => Value::Null,
        v @ (Value::Array(_) | Value::Object(_)) => sort_recursive(php_array(v)),
        other => other,
    }
}

/// `ksort()` of a map (lists are kept).
fn sort_keys(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut entries: Vec<(String, Value)> = o.into_iter().collect();
            entries.sort_by(|a, b| php_str_cmp(&a.0, &b.0));
            list_like(Value::Object(entries.into_iter().collect()))
        }
        other => other,
    }
}

/// `Arr::sortRecursive()`: maps by key (`ksort`), lists by value (`sort`).
fn sort_recursive(v: Value) -> Value {
    match v {
        Value::Object(o) => sort_keys(Value::Object(
            o.into_iter().map(|(k, v)| (k, sort_recursive(v))).collect(),
        )),
        Value::Array(a) => {
            let mut items: Vec<Value> = a.into_iter().map(sort_recursive).collect();
            items.sort_by(php_cmp);
            Value::Array(items)
        }
        other => other,
    }
}

/// `is_numeric()` for strings (PHP 8: surrounding whitespace allowed).
fn php_numeric(s: &str) -> Option<f64> {
    let t = s.trim_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - frac_start;
    }
    if digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    if i != b.len() {
        return None;
    }
    t.parse().ok()
}

/// String comparison under `SORT_REGULAR` (`zendi_smart_strcmp`).
fn php_str_cmp(a: &str, b: &str) -> Ordering {
    match (php_numeric(a), php_numeric(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => a.as_bytes().cmp(b.as_bytes()),
    }
}

/// `zend_compare()` (what `sort($a, SORT_REGULAR)` uses).
fn php_cmp(a: &Value, b: &Value) -> Ordering {
    use Value as V;
    let num = |n: &serde_json::Number| n.as_f64().unwrap_or(0.0);
    match (a, b) {
        (V::String(x), V::String(y)) => php_str_cmp(x, y),
        (V::Number(x), V::Number(y)) => num(x).partial_cmp(&num(y)).unwrap_or(Ordering::Equal),
        (V::Number(n), V::String(s)) => match php_numeric(s) {
            Some(f) => num(n).partial_cmp(&f).unwrap_or(Ordering::Equal),
            None => php_string(a).as_bytes().cmp(s.as_bytes()),
        },
        (V::String(_), V::Number(_)) => php_cmp(b, a).reverse(),
        (V::Bool(_), _) | (_, V::Bool(_)) => truthy(a).cmp(&truthy(b)),
        (V::Null, V::Null) => Ordering::Equal,
        (V::Null, V::String(s)) => "".cmp(s.as_str()),
        (V::String(s), V::Null) => s.as_str().cmp(""),
        (V::Null, _) => false.cmp(&truthy(b)),
        (_, V::Null) => truthy(a).cmp(&false),
        (V::Array(_) | V::Object(_), V::Array(_) | V::Object(_)) => {
            let (x, y) = (list_values(Some(a)), list_values(Some(b)));
            x.len().cmp(&y.len()).then_with(|| {
                x.iter()
                    .zip(y.iter())
                    .map(|(p, q)| php_cmp(p, q))
                    .find(|o| o.is_ne())
                    .unwrap_or(Ordering::Equal)
            })
        }
        (V::Array(_) | V::Object(_), _) => Ordering::Greater,
        (_, V::Array(_) | V::Object(_)) => Ordering::Less,
    }
}

/// `===` on decoded JSON values (arrays compare keys, values and order).
fn php_identical(a: &Value, b: &Value) -> bool {
    use Value as V;
    let entries = |v: &Value| -> Vec<(String, Value)> {
        match v {
            V::Array(x) => x
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect(),
            V::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => Vec::new(),
        }
    };
    match (a, b) {
        (V::Array(_) | V::Object(_), V::Array(_) | V::Object(_)) => {
            let (x, y) = (entries(a), entries(b));
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && php_identical(v1, v2))
        }
        (V::Number(x), V::Number(y)) => {
            // An int and a float are never identical.
            x.is_f64() == y.is_f64() && x.as_f64() == y.as_f64()
        }
        _ => a == b,
    }
}

/// `strnatcasecmp()` (`strnatcmp_ex` with case folding).
fn strnatcasecmp(a: &[u8], b: &[u8]) -> Ordering {
    if a.is_empty() || b.is_empty() {
        return a.len().cmp(&b.len());
    }
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\x0B' | b'\x0C' | b'\r');
    let (mut ai, mut bi) = (0usize, 0usize);
    let mut leading = true;
    loop {
        let mut ca = at(a, ai);
        let mut cb = at(b, bi);
        while leading && ca == b'0' && ai + 1 < a.len() && a[ai + 1].is_ascii_digit() {
            ai += 1;
            ca = a[ai];
        }
        while leading && cb == b'0' && bi + 1 < b.len() && b[bi + 1].is_ascii_digit() {
            bi += 1;
            cb = b[bi];
        }
        leading = false;
        while space(ca) {
            ai += 1;
            ca = at(a, ai);
        }
        while space(cb) {
            bi += 1;
            cb = at(b, bi);
        }
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let fractional = ca == b'0' || cb == b'0';
            let result = if fractional {
                compare_left(a, &mut ai, b, &mut bi)
            } else {
                compare_right(a, &mut ai, b, &mut bi)
            };
            if result.is_ne() {
                return result;
            } else if ai >= a.len() && bi >= b.len() {
                return Ordering::Equal;
            } else if ai >= a.len() {
                return Ordering::Less;
            } else if bi >= b.len() {
                return Ordering::Greater;
            }
            ca = at(a, ai);
            cb = at(b, bi);
        }
        let (ua, ub) = (ca.to_ascii_uppercase(), cb.to_ascii_uppercase());
        if ua != ub {
            return ua.cmp(&ub);
        }
        ai += 1;
        bi += 1;
        if ai >= a.len() && bi >= b.len() {
            return Ordering::Equal;
        } else if ai >= a.len() {
            return Ordering::Less;
        } else if bi >= b.len() {
            return Ordering::Greater;
        }
    }
}

fn is_digit_at(s: &[u8], i: usize) -> bool {
    s.get(i).is_some_and(u8::is_ascii_digit)
}

/// `compare_right()`: the longest run of digits wins, then the greatest value.
fn compare_right(a: &[u8], ai: &mut usize, b: &[u8], bi: &mut usize) -> Ordering {
    let mut bias = Ordering::Equal;
    loop {
        let (da, db) = (is_digit_at(a, *ai), is_digit_at(b, *bi));
        if !da && !db {
            return bias;
        } else if !da {
            return Ordering::Less;
        } else if !db {
            return Ordering::Greater;
        } else if bias.is_eq() {
            bias = a[*ai].cmp(&b[*bi]);
        }
        *ai += 1;
        *bi += 1;
    }
}

/// `compare_left()`: left-aligned numbers, the first different digit wins.
fn compare_left(a: &[u8], ai: &mut usize, b: &[u8], bi: &mut usize) -> Ordering {
    loop {
        let (da, db) = (is_digit_at(a, *ai), is_digit_at(b, *bi));
        if !da && !db {
            return Ordering::Equal;
        } else if !da {
            return Ordering::Less;
        } else if !db {
            return Ordering::Greater;
        }
        let o = a[*ai].cmp(&b[*bi]);
        if o.is_ne() {
            return o;
        }
        *ai += 1;
        *bi += 1;
    }
}

/// `Str::headline()` for a section key (`custom_section` => `Custom Section`).
fn headline(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_uppercase().collect::<String>() + c.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `base64_decode($value, $strict)`.
fn php_base64_decode(s: &str, strict: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits, mut count, mut padding) = (0u32, 0u32, 0usize, 0usize);
    for &c in s.as_bytes() {
        if c == b'=' {
            padding += 1;
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\t' | b'\n' | b'\r' | b' ' => continue,
            _ if strict => return None,
            _ => continue,
        };
        if strict && padding > 0 {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        count += 1;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if strict && (count % 4 == 1 || (padding > 0 && (padding > 2 || (count + padding) % 4 != 0))) {
        return None;
    }
    Some(out)
}

/// `decodeCustomLabels()`: strict base64, else the raw value.
fn decode_custom_labels(value: Option<&str>) -> Option<String> {
    let value = value.filter(|v| !php_trim(v).is_empty())?;
    Some(match php_base64_decode(value, true) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => value.to_owned(),
    })
}

/// `$application->custom_network_aliases` (the getter returns a comma list).
fn network_aliases(raw: Option<&str>) -> Value {
    let Some(s) = raw else {
        return Value::Null;
    };
    match serde_json::from_str::<Value>(s) {
        Ok(v @ (Value::Array(_) | Value::Object(_))) => Value::String(
            list_values(Some(&v))
                .into_iter()
                .map(php_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => Value::String(s.to_owned()),
    }
}

/// `composeDomainsText()`: `service: domain` lines, sorted.
fn compose_domains_text(decoded: &Value) -> Option<String> {
    let entries: Vec<(String, &Value)> = match decoded {
        Value::Object(o) if !o.is_empty() => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(a) if !a.is_empty() => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => return None,
    };
    let mut lines: Vec<Value> = entries
        .into_iter()
        .map(|(service, v)| {
            let domain = v.get("domain").filter(|d| filled(d));
            Value::String(format!(
                "{service}: {}",
                domain.map_or_else(|| "-".to_owned(), php_string)
            ))
        })
        .collect();
    lines.sort_by(php_cmp);
    Some(lines.iter().map(php_string).collect::<Vec<_>>().join("\n"))
}

/// `environmentFlags()`.
fn environment_flags(v: &EnvVar) -> String {
    [
        (truthy(&v.is_buildtime), "build-time"),
        (truthy(&v.is_runtime), "runtime"),
        (truthy(&v.is_multiline), "multiline"),
        (truthy(&v.is_literal), "literal"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect::<Vec<_>>()
    .join(", ")
}

/// `environmentLines()`: one line per attribute, so the line diff shows what changed.
fn environment_lines(v: &EnvVar) -> String {
    let state = |b: &Value| if truthy(b) { "enabled" } else { "disabled" };
    let mut lines = Vec::new();
    if let Some(value) = v.value.as_deref().filter(|s| !php_trim(s).is_empty()) {
        lines.push(value.to_owned());
    }
    lines.push(format!("Available at build: {}", state(&v.is_buildtime)));
    lines.push(format!("Available at runtime: {}", state(&v.is_runtime)));
    lines.push(format!("Multiline: {}", state(&v.is_multiline)));
    lines.push(format!("Literal: {}", state(&v.is_literal)));
    lines.join("\n")
}

/// `displayValue()`.
fn display_value(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::Bool(true) => "Enabled".into(),
        Value::Bool(false) => "Disabled".into(),
        Value::Array(_) | Value::Object(_) => summarize_text(Some(&php_json(v))),
        other => summarize_text(Some(&php_string(other))),
    }
}

/// `stringifyValue()`.
fn stringify_value(v: &Value) -> Option<String> {
    match v {
        Value::Null | Value::Bool(_) => None,
        Value::Array(_) | Value::Object(_) => Some(php_json(v)),
        other => Some(php_string(other)),
    }
}

/// `SummarizesDiffText::expandableText()`: the trimmed value when it is multi-line or
/// longer than the single-line limit.
fn expandable_text(value: Option<&str>) -> Option<String> {
    let value = php_trim(value?);
    if value.is_empty() {
        return None;
    }
    (value.contains('\n') || value.chars().count() > SINGLE_LINE_LIMIT).then(|| value.to_owned())
}

/// `summarizeText()`.
fn summarize_text(value: Option<&str>) -> String {
    let Some(value) = value.map(php_trim).filter(|v| !v.is_empty()) else {
        return "-".into();
    };
    let lines = value.matches('\n').count() + 1;
    if lines > 1 {
        return format!("{} ({lines} lines)", str_limit(value, 80));
    }
    str_limit(value, SINGLE_LINE_LIMIT)
}

/// `Str::limit($value, $limit)`.
fn str_limit(value: &str, limit: usize) -> String {
    if mb_strwidth(value) <= limit {
        return value.to_owned();
    }
    // `rtrim(mb_strimwidth($value, 0, $limit, '', 'UTF-8')).'...'`
    let mut width = 0;
    let mut end = 0;
    for (i, c) in value.char_indices() {
        width += char_width(c);
        if width > limit {
            break;
        }
        end = i + c.len_utf8();
    }
    format!("{}...", php_rtrim(&value[..end]))
}

fn mb_strwidth(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// `mb_strwidth()` of one character (PHP 8.4's East Asian Width table).
fn char_width(c: char) -> usize {
    #[rustfmt::skip]
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F), (0x231A, 0x231B), (0x2329, 0x232A), (0x23E9, 0x23EC),
        (0x23F0, 0x23F0), (0x23F3, 0x23F3), (0x25FD, 0x25FE), (0x2614, 0x2615),
        (0x2630, 0x2637), (0x2648, 0x2653), (0x267F, 0x267F), (0x268A, 0x268F),
        (0x2693, 0x2693), (0x26A1, 0x26A1), (0x26AA, 0x26AB), (0x26BD, 0x26BE),
        (0x26C4, 0x26C5), (0x26CE, 0x26CE), (0x26D4, 0x26D4), (0x26EA, 0x26EA),
        (0x26F2, 0x26F3), (0x26F5, 0x26F5), (0x26FA, 0x26FA), (0x26FD, 0x26FD),
        (0x2705, 0x2705), (0x270A, 0x270B), (0x2728, 0x2728), (0x274C, 0x274C),
        (0x274E, 0x274E), (0x2753, 0x2755), (0x2757, 0x2757), (0x2795, 0x2797),
        (0x27B0, 0x27B0), (0x27BF, 0x27BF), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50),
        (0x2B55, 0x2B55), (0x2E80, 0x2E99), (0x2E9B, 0x2EF3), (0x2F00, 0x2FD5),
        (0x2FF0, 0x303E), (0x3041, 0x3096), (0x3099, 0x30FF), (0x3105, 0x312F),
        (0x3131, 0x318E), (0x3190, 0x31E5), (0x31EF, 0x321E), (0x3220, 0x3247),
        (0x3250, 0xA48C), (0xA490, 0xA4C6), (0xA960, 0xA97C), (0xAC00, 0xD7A3),
        (0xF900, 0xFAFF), (0xFE10, 0xFE19), (0xFE30, 0xFE52), (0xFE54, 0xFE66),
        (0xFE68, 0xFE6B), (0xFF01, 0xFF60), (0xFFE0, 0xFFE6), (0x16FE0, 0x16FE4),
        (0x16FF0, 0x16FF1), (0x17000, 0x187F7), (0x18800, 0x18CD5), (0x18CFF, 0x18D08),
        (0x1AFF0, 0x1AFF3), (0x1AFF5, 0x1AFFB), (0x1AFFD, 0x1AFFE), (0x1B000, 0x1B122),
        (0x1B132, 0x1B132), (0x1B150, 0x1B152), (0x1B155, 0x1B155), (0x1B164, 0x1B167),
        (0x1B170, 0x1B2FB), (0x1D300, 0x1D356), (0x1D360, 0x1D376), (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF), (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A), (0x1F200, 0x1F202),
        (0x1F210, 0x1F23B), (0x1F240, 0x1F248), (0x1F250, 0x1F251), (0x1F260, 0x1F265),
        (0x1F300, 0x1F320), (0x1F32D, 0x1F335), (0x1F337, 0x1F37C), (0x1F37E, 0x1F393),
        (0x1F3A0, 0x1F3CA), (0x1F3CF, 0x1F3D3), (0x1F3E0, 0x1F3F0), (0x1F3F4, 0x1F3F4),
        (0x1F3F8, 0x1F43E), (0x1F440, 0x1F440), (0x1F442, 0x1F4FC), (0x1F4FF, 0x1F53D),
        (0x1F54B, 0x1F54E), (0x1F550, 0x1F567), (0x1F57A, 0x1F57A), (0x1F595, 0x1F596),
        (0x1F5A4, 0x1F5A4), (0x1F5FB, 0x1F64F), (0x1F680, 0x1F6C5), (0x1F6CC, 0x1F6CC),
        (0x1F6D0, 0x1F6D2), (0x1F6D5, 0x1F6D7), (0x1F6DC, 0x1F6DF), (0x1F6EB, 0x1F6EC),
        (0x1F6F4, 0x1F6FC), (0x1F7E0, 0x1F7EB), (0x1F7F0, 0x1F7F0), (0x1F90C, 0x1F93A),
        (0x1F93C, 0x1F945), (0x1F947, 0x1F9FF), (0x1FA70, 0x1FA7C), (0x1FA80, 0x1FA89),
        (0x1FA8F, 0x1FAC6), (0x1FACE, 0x1FADC), (0x1FADF, 0x1FAE9), (0x1FAF0, 0x1FAF8),
        (0x20000, 0x2FFFD), (0x30000, 0x3FFFD),
    ];
    let c = c as u32;
    let wide = WIDE
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                Ordering::Less
            } else if lo > c {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok();
    if wide { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_and_expand() {
        assert_eq!(summarize_text(None), "-");
        assert_eq!(summarize_text(Some("  \n ")), "-");
        assert_eq!(summarize_text(Some(" short ")), "short");
        let long = "a".repeat(41);
        assert_eq!(
            summarize_text(Some(&long)),
            format!("{}...", "a".repeat(40))
        );
        assert_eq!(summarize_text(Some("a\nb\n")), "a\nb (2 lines)");
        // Wide characters count twice, and the cut is right-trimmed.
        assert_eq!(str_limit("日本語テキスト", 6), "日本語...");
        assert_eq!(str_limit("abc def", 4), "abc...");
        assert_eq!(expandable_text(Some(&long)), Some(long.clone()));
        assert_eq!(expandable_text(Some("x")), None);
        assert_eq!(expandable_text(Some(" a\nb ")), Some("a\nb".into()));
    }

    #[test]
    fn natural_case_insensitive_sort() {
        let mut keys = vec!["b", "A10", "a2", "B1", "_x", "a01"];
        keys.sort_by(|a, b| strnatcasecmp(a.as_bytes(), b.as_bytes()));
        assert_eq!(keys, ["a01", "a2", "A10", "b", "B1", "_x"]);
    }

    #[test]
    fn preview_volume_names() {
        let u = "abc";
        assert!(is_preview_volume_name("abc_data-pr-12", u));
        assert!(is_preview_volume_name("abc-data-pr-1\n", u));
        assert!(!is_preview_volume_name("abc_-pr-1", u));
        assert!(!is_preview_volume_name("abc_data-pr-", u));
        assert!(!is_preview_volume_name("abcdata-pr-1", u));
        assert!(!is_preview_volume_name("xabc_data-pr-1", u));
    }

    #[test]
    fn base64_like_php() {
        assert_eq!(php_base64_decode("aGk=", true), Some(b"hi".to_vec()));
        assert_eq!(php_base64_decode("aGk", true), Some(b"hi".to_vec()));
        assert_eq!(php_base64_decode("a Gk\n=", true), Some(b"hi".to_vec()));
        assert_eq!(php_base64_decode("aGk=x", true), None);
        assert_eq!(php_base64_decode("a", true), None);
        assert_eq!(php_base64_decode("not base64!", true), None);
        assert_eq!(php_base64_decode("aG!k", false), Some(b"hi".to_vec()));
    }

    #[test]
    fn identical_is_order_and_type_sensitive() {
        assert!(php_identical(
            &json!({"a": 1, "b": 2}),
            &json!({"a": 1, "b": 2})
        ));
        assert!(!php_identical(
            &json!({"a": 1, "b": 2}),
            &json!({"b": 2, "a": 1})
        ));
        assert!(!php_identical(&json!(1), &json!(1.0)));
        assert!(!php_identical(&json!(1), &json!("1")));
        assert!(php_identical(&json!([]), &json!({})));
    }

    #[test]
    fn diff_legacy_and_flags() {
        let d = ConfigurationDiff::legacy();
        assert!(d.requires_build() && d.is_legacy_fallback() && !d.is_changed());
        assert_eq!(
            php_json(&d.to_array()),
            r#"{"changed":false,"count":0,"requires_build":true,"requires_redeploy":false,"legacy_fallback":true,"changes":[]}"#
        );
    }
}
