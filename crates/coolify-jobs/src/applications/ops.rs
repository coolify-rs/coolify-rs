//! Remote work of the application routes outside the deployment job: `logs_by_uuid`,
//! `GetRollbackImages`, `CleanupPreviewDeployment`, `DeleteResourceJob` for a preview and
//! `VolumeCloneJob`; and the domains `ApplicationPreview` generates.

use std::collections::BTreeMap;

use chrono::Utc;
use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::application_ops as ops;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

use crate::Jobs;
use crate::applications::domains::{self, UrlParts};
use crate::compose_parser::{domains as compose_domains, php, service, validate};
use crate::server::ServerInfo;

/// `ApplicationPreview::generatedPreviewDomain($sourceDomain)`: the URL and its port.
pub fn generated_preview_domain(
    template: &str,
    pull_request_id: i64,
    overrides: &BTreeMap<String, i64>,
    source: &str,
) -> (String, Option<i64>) {
    let url = UrlParts::parse(source).unwrap_or_default();
    let path = match url.path.as_deref() {
        None | Some("" | "/") => String::new(),
        Some(p) => p.to_owned(),
    };
    let random = coolify_domain::new_public_id();
    let mut preview = template
        .replace("{{random}}", &random)
        .replace("{{domain}}", &url.host)
        .replace("{{pr_id}}", &pull_request_id.to_string());
    let mut schema = url.scheme.clone();
    let lower = preview.to_ascii_lowercase();
    for s in ["https", "http"] {
        if lower.starts_with(&format!("{s}://")) {
            schema = s.to_owned();
            preview = preview[s.len() + 3..].to_owned();
            break;
        }
    }
    let port = url
        .port_number()
        .or_else(|| overrides.get(&domains::without_port(source)).copied());
    (format!("{schema}://{preview}{path}"), port)
}

/// What a preview gets from `generate_preview_fqdn()` or `generate_preview_fqdn_compose()`.
#[derive(Debug, Clone, Default)]
pub struct PreviewDomains {
    pub fqdn: Option<String>,
    /// `domain_port_overrides` as JSON.
    pub overrides: Option<String>,
    /// `docker_compose_domains` as JSON (Compose only).
    pub compose: Option<String>,
}

/// `saving`: `DomainPortOverrides::normalize($fqdn, $overrides)`, as columns.
fn normalized(
    fqdn: Option<&str>,
    overrides: &BTreeMap<String, i64>,
) -> (Option<String>, Option<String>) {
    let (fqdn, overrides) = domains::normalize_port_overrides(fqdn, overrides);
    let json = overrides.map(|o| domains::overrides_json(fqdn.as_deref(), &o));
    (fqdn, json)
}

/// `DomainPortOverrides::normalize($fqdn, $overrides)` for a saved preview.
pub fn normalized_domains(
    fqdn: Option<&str>,
    overrides: &BTreeMap<String, i64>,
) -> (Option<String>, Option<String>) {
    normalized(fqdn, overrides)
}

/// `generate_preview_fqdn()`: `None` when the application has no domain (nothing is saved then).
pub fn preview_fqdn(
    application_fqdn: Option<&str>,
    template: &str,
    pull_request_id: i64,
    overrides: &BTreeMap<String, i64>,
) -> Option<PreviewDomains> {
    let fqdn = application_fqdn.filter(|f| !f.is_empty() && *f != "0")?;
    let source = fqdn.split(',').next().unwrap_or(fqdn);
    let (url, port) = generated_preview_domain(template, pull_request_id, overrides, source);
    let own: BTreeMap<String, i64> = port.map(|p| (url.clone(), p)).into_iter().collect();
    let (fqdn, overrides) = normalized(Some(&url), &own);
    Some(PreviewDomains {
        fqdn,
        overrides,
        compose: None,
    })
}

/// `composeServiceNamesForPreview()`. Upstream reads the names from
/// `$application->parse(pull_request_id: ...)`; the preview branch of the parser is not ported, so
/// they come from the raw Compose file: the same services, in the same order.
pub fn compose_service_names(raw: &str) -> Vec<String> {
    let Ok(compose) = validate::parse_docker_compose_yaml(raw) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(services)) = php::get(&compose, "services") else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for (name, s) in services {
        let image = php::get(s, "image").map(php::to_string).unwrap_or_default();
        if service::is_database_image(&image, s) {
            continue;
        }
        let name = name.as_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The named volumes `$application->parse(pull_request_id: ...)` stores on the application for a
/// preview: `(name, mount path)`. Bind mounts (`LocalFileVolume` rows) are not derived here; they
/// belong to the preview branch of the parser.
pub fn compose_preview_volumes(
    raw: &str,
    uuid: &str,
    pull_request_id: i64,
) -> Vec<(String, String)> {
    let Ok(compose) = validate::parse_docker_compose_yaml(raw) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(services)) = php::get(&compose, "services") else {
        return Vec::new();
    };
    let declarations = php::get(&compose, "volumes");
    let mut out: Vec<(String, String)> = Vec::new();
    for (_, s) in services {
        let Some(PhpValue::Array(volumes)) = php::get(s, "volumes") else {
            continue;
        };
        for (_, volume) in volumes {
            let (source, target) = match volume {
                PhpValue::Str(text) => {
                    let Ok(parsed) = validate::parse_docker_volume_string(text) else {
                        continue;
                    };
                    (parsed.source, parsed.target)
                }
                PhpValue::Array(_) => {
                    if php::get(volume, "type").map(php::to_string).as_deref() != Some("volume") {
                        continue;
                    }
                    (
                        php::get(volume, "source").map(php::to_string),
                        php::get(volume, "target").map(php::to_string),
                    )
                }
                _ => continue,
            };
            let (Some(source), Some(target)) = (source, target) else {
                continue;
            };
            if source.starts_with(['.', '/', '~', '$']) {
                continue;
            }
            let network = declarations
                .and_then(|d| php::get(d, &source))
                .and_then(|d| php::data_get(d, "driver_opts.type"));
            if matches!(network, Some(PhpValue::Str(t)) if t == "cifs" || t == "nfs") {
                continue;
            }
            let name = format!("{uuid}_{}-pr-{pull_request_id}", php::slug(&source));
            if !out.iter().any(|(n, _)| *n == name) {
                out.push((name, target));
            }
        }
    }
    out
}

/// `preferredComposeServiceNamesFromDomainKeys($domainKeys)`.
fn preferred_names(keys: &[String]) -> Vec<String> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for key in keys {
        let normalized = compose_domains::normalize_compose_service_name(key);
        match groups.iter_mut().find(|(n, _)| *n == normalized) {
            Some((_, list)) => {
                if !list.contains(key) {
                    list.push(key.clone());
                }
            }
            None => groups.push((normalized, vec![key.clone()])),
        }
    }
    groups
        .into_iter()
        .map(|(n, list)| {
            list.iter()
                .find(|k| **k != n)
                .or(list.first())
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// `generate_preview_fqdn_compose()` with `$keepExistingDomains = false`.
pub fn preview_fqdn_compose(
    application_domains: Option<&str>,
    preview_domains: Option<&str>,
    compose_names: &[String],
    template: &str,
    pull_request_id: i64,
    overrides: &BTreeMap<String, i64>,
) -> PreviewDomains {
    preview_fqdn_compose_keeping(
        application_domains,
        preview_domains,
        compose_names,
        template,
        pull_request_id,
        overrides,
        None,
    )
}

/// `generate_preview_fqdn_compose($keepExistingDomains)`: `keep` holds the preview's
/// `domain_port_overrides` when the preview keeps its domains.
pub fn preview_fqdn_compose_keeping(
    application_domains: Option<&str>,
    preview_domains: Option<&str>,
    compose_names: &[String],
    template: &str,
    pull_request_id: i64,
    overrides: &BTreeMap<String, i64>,
    keep: Option<&BTreeMap<String, i64>>,
) -> PreviewDomains {
    let app_domains = php::json_array(application_domains);
    let own_domains = php::json_array(preview_domains);
    let domain_keys: Vec<String> = app_domains
        .iter()
        .chain(own_domains.iter())
        .map(|(k, _)| k.as_string())
        .collect();
    let unmapped: Vec<String> = domain_keys
        .iter()
        .filter(|k| compose_domains::find_compose_service_name(k, compose_names).is_none())
        .cloned()
        .collect();
    let mut known: Vec<String> = compose_names.to_vec();
    let preferred = preferred_names(if compose_names.is_empty() {
        &domain_keys
    } else {
        &unmapped
    });
    for name in preferred {
        if !known.contains(&name) {
            known.push(name);
        }
    }
    let mut app_domains =
        compose_domains::rekey_compose_domains_to_service_names(&app_domains, &known);
    for name in compose_names {
        let key = Key::from_php_string(name);
        if !app_domains.iter().any(|(k, _)| *k == key) {
            app_domains.push((
                key,
                PhpValue::Array(vec![(
                    Key::Str("domain".into()),
                    PhpValue::Str(String::new()),
                )]),
            ));
        }
    }
    let mut names: Vec<String> = Vec::new();
    for name in app_domains
        .iter()
        .map(|(k, _)| k.as_string())
        .chain(compose_names.iter().cloned())
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let own_domains = compose_domains::rekey_compose_domains_to_service_names(&own_domains, &known);
    let mut result: php::Arr = Vec::new();
    let mut ports: BTreeMap<String, i64> = BTreeMap::new();
    for name in &names {
        if let Some(existing) = keep {
            let current = compose_domains::get_compose_service_domain_string(&own_domains, name);
            let (fqdn, kept) = domains::normalize_port_overrides(current.as_deref(), existing);
            if let Some(fqdn) = fqdn.filter(|f| !f.trim().is_empty()) {
                result =
                    compose_domains::put_compose_service_domain(&result, name, Some(&fqdn), &names);
                ports.extend(kept.unwrap_or_default());
                continue;
            }
        }
        let domain = compose_domains::get_compose_service_domain_string(&app_domains, name)
            .filter(|d| !d.is_empty() && d != "0");
        let Some(domain) = domain else {
            result = compose_domains::put_compose_service_domain(&result, name, Some(""), &names);
            continue;
        };
        let mut generated: Vec<String> = Vec::new();
        for d in domain.split(',').map(str::trim) {
            if d.is_empty() || d == "0" {
                continue;
            }
            let (url, port) = generated_preview_domain(template, pull_request_id, overrides, d);
            if let Some(p) = port {
                ports.insert(url.clone(), p);
            }
            generated.push(url);
        }
        result = compose_domains::put_compose_service_domain(
            &result,
            name,
            Some(&generated.join(",")),
            &names,
        );
    }
    let result = compose_domains::rekey_compose_domains_to_service_names(&result, &names);
    let all: Vec<String> = result
        .iter()
        .filter_map(|(_, entry)| compose_domains::compose_domain_entry_string(entry))
        .filter(|d| !d.is_empty() && d != "0")
        .flat_map(|d| d.split(',').map(str::to_owned).collect::<Vec<_>>())
        .collect();
    let joined = all.join(",");
    let (fqdn, overrides) = normalized((!joined.is_empty()).then_some(joined.as_str()), &ports);
    PreviewDomains {
        fqdn,
        overrides,
        compose: Some(php::json_encode(&PhpValue::Array(result))),
    }
}

// ---------------------------------------------------------------------------

/// `removeAnsiColors($text)`.
static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("\x1b[\\[][A-Za-z0-9];?[0-9]*m?").unwrap_or_else(|_| unreachable!())
});
/// The `coolify.pullRequestId` label.
static PR_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"coolify\.pullRequestId=(\d+)").unwrap_or_else(|_| unreachable!())
});

/// The filter of `getCurrentApplicationContainerStatus($server, $application, $pullRequestId)`.
pub(crate) fn matches_pull_request(labels: &str, pull_request_id: Option<i64>) -> bool {
    let value: Option<i64> = PR_LABEL
        .captures(labels)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok());
    let is_base = !labels.contains("coolify.pullRequestId=") || value.unwrap_or(0) == 0;
    let wanted = pull_request_id.filter(|p| *p != 0);
    match wanted {
        Some(_) if is_base => false,
        _ if is_base => true,
        Some(p) => value == Some(p),
        None => false,
    }
}

/// `format_docker_labels_to_json($labels)->get($key)`.
fn label_value<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    labels
        .split(',')
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// `normalizeLogLines($lines)`: the value for `docker logs -n`.
pub fn normalize_log_lines(lines: Option<&str>) -> String {
    let Some(lines) = lines else {
        return "100".into();
    };
    if lines == "all" {
        return "all".into();
    }
    // filter_var(FILTER_VALIDATE_INT) allows surrounding whitespace and a sign, not leading zeros.
    let t = lines.trim_matches([' ', '\t', '\n', '\r', '\x0b', '\0']);
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    let valid = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    match t.parse::<i64>().ok().filter(|_| valid) {
        Some(-1) => "all".into(),
        Some(n) if n >= 0 => n.min(10000).to_string(),
        _ => "100".into(),
    }
}

/// The result of `logs_by_uuid` after the checks on the application and the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogsResult {
    Logs(String),
    /// 400 `Application is not running.`
    NotRunning,
    /// 404: no container (running or stopped) for the `service_name`.
    NoService,
}

impl Jobs {
    /// The containers of the application for a pull request (`getCurrentApplicationContainerStatus`).
    pub(crate) async fn containers_for_pull_request(
        &self,
        t: &coolify_remote::Target,
        server: &ServerInfo,
        uuid: &str,
        pull_request_id: Option<i64>,
    ) -> Result<Vec<Value>, String> {
        if server.is_swarm() {
            return Ok(Vec::new());
        }
        Ok(self
            .application_containers(t, uuid, true)
            .await?
            .into_iter()
            .filter(|c| {
                matches_pull_request(
                    c.get("Labels").and_then(Value::as_str).unwrap_or_default(),
                    pull_request_id,
                )
            })
            .collect())
    }

    /// The middle of `logs_by_uuid`. An `Err` is a failed remote command: upstream's
    /// `instant_remote_process` throws and the request ends with a 500.
    pub async fn application_logs(
        &self,
        a: &ApplicationRow,
        pull_request_id: Option<i64>,
        service_name: Option<&str>,
        lines: Option<&str>,
        show_timestamps: bool,
    ) -> Result<LogsResult, String> {
        let db = &self.0.db;
        let server_id = a.server_id().ok_or("The application has no server.")?;
        let row = servers::find_by_id(db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let server = ServerInfo::from_row(&row);
        let t = server.target(self).await?;
        let containers = self
            .containers_for_pull_request(&t, &server, &a.uuid, pull_request_id)
            .await?;
        if containers.is_empty() {
            return Ok(LogsResult::NotRunning);
        }
        let container = match service_name.filter(|s| !s.trim().is_empty()) {
            Some(s) => {
                let found = containers.iter().find(|c| {
                    label_value(
                        c.get("Labels").and_then(Value::as_str).unwrap_or_default(),
                        "com.docker.compose.service",
                    ) == Some(s)
                });
                let Some(found) = found else {
                    return Ok(LogsResult::NoService);
                };
                found
            }
            // A stopped container still has logs, like in the UI; a running one is preferred.
            None => containers
                .iter()
                .find(|c| c.get("State").and_then(Value::as_str) == Some("running"))
                .unwrap_or(&containers[0]),
        };
        let id = container
            .get("ID")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lines = normalize_log_lines(lines);
        let mut command = format!("docker logs -n {lines}");
        if show_timestamps {
            command.push_str(" --timestamps");
        }
        command.push_str(&format!(" {} 2>&1", quote(id)));
        let output = self
            .remote()
            .run(&t, &[&command], RunOptions::op("application.logs"))
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let output = ANSI.replace_all(&output, "").into_owned();
        let (known, locked) = self
            .log_redaction_secrets(a, server_id, pull_request_id.is_some())
            .await;
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        let locked: Vec<&str> = locked.iter().map(String::as_str).collect();
        Ok(LogsResult::Logs(coolify_compat::redact::redact_text(
            &output, &known, &locked,
        )))
    }

    /// `GetRollbackImages::handle($application)`; an `Err` makes the route answer with an empty list.
    pub async fn rollback_images(&self, a: &ApplicationRow) -> Result<Value, String> {
        let db = &self.0.db;
        let mut current: Option<String> = None;
        let mut images: Vec<(String, Value)> = Vec::new();
        let row = match a.server_id() {
            Some(id) => servers::find_by_id(db, id)
                .await
                .map_err(|e| e.to_string())?,
            None => None,
        };
        if let Some(row) = row
            .as_ref()
            .filter(|r| ServerInfo::from_row(r).is_functional())
        {
            let server = ServerInfo::from_row(row);
            let t = server.target(self).await?;
            let containers = self
                .containers_for_pull_request(&t, &server, &a.uuid, Some(0))
                .await?;
            // currentTag(): the one tag of the running containers.
            let mut tags: Vec<Option<String>> = Vec::new();
            for c in containers
                .iter()
                .filter(|c| c.get("State").and_then(Value::as_str) == Some("running"))
            {
                let tag = image_tag(c.get("Image").and_then(Value::as_str).unwrap_or_default());
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            if tags.len() == 1 {
                current = tags.remove(0);
            }
            let repository = a
                .str("docker_registry_image_name")
                .filter(|n| !n.is_empty() && *n != "0")
                .unwrap_or(&a.uuid)
                .to_owned();
            let output = self
                .remote()
                .run(
                    &t,
                    &["docker images --format '{{.Repository}}#{{.Tag}}#{{.CreatedAt}}'"],
                    RunOptions::op("application.rollback_images"),
                )
                .await
                .map_err(|e| e.message)?
                .unwrap_or_default();
            for line in output.trim().split('\n') {
                let parts: Vec<&str> = line.split('#').collect();
                if parts[0] != repository {
                    continue;
                }
                let Some(tag) = parts.get(1).filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let image = json!({
                    "tag": tag,
                    "created_at": parts.get(2),
                    "is_current": Some(*tag) == current.as_deref(),
                    "source": "local",
                });
                match images.iter_mut().find(|(k, _)| k == tag) {
                    Some(slot) => slot.1 = image,
                    None => images.push(((*tag).to_owned(), image)),
                }
            }
        }
        // registryImages()
        let has_image_name = a
            .str("docker_registry_image_name")
            .is_some_and(|n| !n.trim().is_empty());
        let excluded = matches!(a.str("build_pack"), Some("dockerimage" | "dockercompose"))
            || a.str("dockerfile").is_some_and(|d| !d.trim().is_empty());
        if has_image_name && !excluded {
            for (commit, deployed_at) in ops::finished_commits(db, a.id, 10)
                .await
                .map_err(|e| e.to_string())?
            {
                let tag: String = commit.chars().take(128).collect();
                if images.iter().any(|(k, _)| *k == tag) {
                    continue;
                }
                let image = json!({
                    "tag": tag,
                    "created_at": deployed_at,
                    "is_current": Some(tag.as_str()) == current.as_deref(),
                    "source": "registry",
                });
                images.push((tag, image));
            }
        }
        let images: Vec<Value> = images.into_iter().map(|(_, i)| i).collect();
        Ok(json!({"current": current, "images": images}))
    }

    /// `Previews::stop`: `docker stack rm` on Swarm, otherwise `stopContainers()`.
    pub async fn stop_preview_deployment(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
    ) -> Result<(), String> {
        let server_id = a.server_id().ok_or("Server not found.")?;
        let row = servers::find_by_id(&self.0.db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let server = ServerInfo::from_row(&row);
        let t = server.target(self).await?;
        if server.is_swarm() {
            let stack = format!(
                "docker stack rm {}",
                quote(&format!("{}-{pull_request_id}", a.uuid))
            );
            self.remote()
                .run(&t, &[&stack], RunOptions::op("preview.stop"))
                .await
                .map_err(|e| e.message)?;
            return Ok(());
        }
        let containers = self
            .containers_for_pull_request(&t, &server, &a.uuid, Some(pull_request_id))
            .await?;
        let grace = a
            .setting_int("stop_grace_period")
            .filter(|g| (1..=3600).contains(g))
            .unwrap_or(30);
        for c in containers {
            let Some(name) = c.get("Names").and_then(Value::as_str) else {
                continue;
            };
            let stop = crate::databases::docker_stop_command(
                u32::try_from(grace).unwrap_or(30),
                &quote(name),
                server.docker_version(),
            );
            // `throwError: false`.
            let _ = self
                .remote()
                .run_quiet(
                    &t,
                    &[&stop, &format!("docker rm -f {}", quote(name))],
                    RunOptions::op("preview.stop"),
                )
                .await;
        }
        Ok(())
    }

    /// `CleanupPreviewDeployment::run($application, $pullRequestId, $preview)`, after the preview
    /// was soft deleted; then `DeleteResourceJob::dispatch($preview)`. A server that is not
    /// functional stops it right there: the row stays soft deleted, as upstream.
    pub async fn cleanup_preview_deployment(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
        preview_id: i64,
    ) {
        let db = &self.0.db;
        let Some(server_id) = a.server_id() else {
            return;
        };
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let cancelled = self
            .cancel_preview_deployments(a.id, pull_request_id, &t)
            .await;
        if cancelled > 0 {
            self.queue_next_deployment(a.id, server_id).await;
        }
        // stopRunningContainers()
        if server.is_swarm() {
            let stack = quote(&format!("{}-{pull_request_id}", a.uuid));
            let _ = self
                .remote()
                .run_quiet(
                    &t,
                    &[&format!("docker stack rm {stack}")],
                    RunOptions::op("preview.cleanup"),
                )
                .await;
        } else if let Ok(containers) = self
            .containers_for_pull_request(&t, &server, &a.uuid, Some(pull_request_id))
            .await
        {
            for c in containers {
                if let Some(name) = c.get("Names").and_then(Value::as_str) {
                    let _ = self
                        .remote()
                        .run_quiet(
                            &t,
                            &[&format!("docker rm -f {}", quote(name))],
                            RunOptions::op("preview.cleanup"),
                        )
                        .await;
                }
            }
        }
        let jobs = self.clone();
        let a = a.clone();
        tokio::spawn(async move {
            jobs.delete_preview_resource(&a, pull_request_id, preview_id, &t, &server)
                .await;
        });
    }

    /// `cancelActiveDeployments()`: `(rows cancelled)`.
    async fn cancel_preview_deployments(
        &self,
        application_id: i64,
        pull_request_id: i64,
        t: &coolify_remote::Target,
    ) -> usize {
        let db = &self.0.db;
        let now = Utc::now().naive_utc();
        let active = apps::deployments_with_status(db, application_id, &["queued", "in_progress"])
            .await
            .unwrap_or_default();
        let mut cancelled = 0;
        for d in active
            .iter()
            .filter(|d| d.int("pull_request_id").unwrap_or(0) == pull_request_id)
        {
            let _ = apps::update_deployment(
                db,
                d.id,
                &[(
                    "status",
                    ColumnValue::Text(Some("cancelled-by-user".into())),
                )],
                now,
            )
            .await;
            cancelled += 1;
            let entry = apps::LogEntry {
                command: None,
                output: "Deployment cancelled: Pull request closed.".into(),
                kind: "stderr",
                timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
                hidden: false,
                batch: 1,
            };
            let _ = apps::append_log(db, d.id, &[entry], now).await;
            let helper = quote(&d.uuid);
            let out = self
                .remote()
                .run_quiet(
                    t,
                    &[&format!(
                        "docker ps -a --filter name={helper} --format '{{{{.Names}}}}'"
                    )],
                    RunOptions::op("preview.cleanup"),
                )
                .await
                .unwrap_or_default();
            if !out.trim().is_empty() {
                let _ = self
                    .remote()
                    .run_quiet(
                        t,
                        &[&format!("docker rm -f {helper}")],
                        RunOptions::op("preview.cleanup"),
                    )
                    .await;
            }
        }
        cancelled
    }

    /// `DeleteResourceJob::deleteApplicationPreview()` and `forceDeleting`. The Compose volumes
    /// come from the rows the parser stored on the application, not from a parse with the pull
    /// request id: that branch of the parser is not ported.
    async fn delete_preview_resource(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
        preview_id: i64,
        t: &coolify_remote::Target,
        server: &ServerInfo,
    ) {
        let db = &self.0.db;
        let cancelled = self
            .cancel_preview_deployments(a.id, pull_request_id, t)
            .await;
        if cancelled > 0 {
            self.queue_next_deployment(a.id, server.id).await;
        }
        if server.is_swarm() {
            let stack = quote(&format!("{}-{pull_request_id}", a.uuid));
            let _ = self
                .remote()
                .run_quiet(
                    t,
                    &[&format!("docker stack rm {stack}")],
                    RunOptions::op("preview.delete"),
                )
                .await;
        } else if let Ok(containers) = self
            .containers_for_pull_request(t, server, &a.uuid, Some(pull_request_id))
            .await
            && !containers.is_empty()
        {
            let names: Vec<String> = containers
                .iter()
                .filter_map(|c| c.get("Names").and_then(Value::as_str))
                .map(|n| quote(&n.replace('/', "")))
                .collect();
            let list = names.join(" ");
            let stop = crate::databases::docker_stop_command(30, &list, server.docker_version());
            let _ = self
                .remote()
                .run_quiet(
                    t,
                    &[&stop, &format!("docker rm -f {list}")],
                    RunOptions::op("preview.delete"),
                )
                .await;
        }
        let mut compose_volumes: Vec<String> = Vec::new();
        let suffix = format!("-pr-{pull_request_id}");
        if a.str("build_pack") == Some("dockercompose") {
            let prefix = format!("{}_", a.uuid);
            for (name, _, _) in apps::persistent_volumes(db, a.id).await.unwrap_or_default() {
                if name.starts_with(&prefix) && name.ends_with(&suffix) {
                    let _ = self
                        .remote()
                        .run_quiet(
                            t,
                            &[&format!("docker volume rm -f {}", quote(&name))],
                            RunOptions::op("preview.delete"),
                        )
                        .await;
                    compose_volumes.push(name);
                }
            }
            self.remove_compose_preview_network(a, pull_request_id, t)
                .await;
        } else {
            for name in ops::preview_suffixed_volumes(db, a.id)
                .await
                .unwrap_or_default()
            {
                let volume = quote(&format!("{name}{suffix}"));
                let _ = self
                    .remote()
                    .run_quiet(
                        t,
                        &[&format!("docker volume rm -f {volume}")],
                        RunOptions::op("preview.delete"),
                    )
                    .await;
            }
        }
        if let Err(e) = ops::force_delete_preview(db, preview_id, a.id, &compose_volumes).await {
            tracing::warn!(error = %e, preview = preview_id, "preview could not be deleted");
        }
    }

    /// `removeComposePreviewNetwork()`.
    async fn remove_compose_preview_network(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
        t: &coolify_remote::Target,
    ) {
        let network = format!("{}-{pull_request_id}", a.uuid);
        if network == a.network() {
            return;
        }
        let quoted = quote(&network);
        let Ok(Some(output)) = self
            .remote()
            .run(
                t,
                &[&format!(
                    "docker network inspect --format '{{{{json .Containers}}}}' {quoted}"
                )],
                RunOptions::op("preview.delete"),
            )
            .await
        else {
            return;
        };
        let Ok(Value::Object(containers)) = serde_json::from_str::<Value>(output.trim()) else {
            return;
        };
        let others = containers
            .values()
            .any(|c| c.get("Name").and_then(Value::as_str) != Some("coolify-proxy"));
        if others {
            return;
        }
        for cmd in [
            format!("docker network disconnect {quoted} coolify-proxy"),
            format!("docker network rm {quoted}"),
        ] {
            let _ = self
                .remote()
                .run_quiet(t, &[&cmd], RunOptions::op("preview.delete"))
                .await;
        }
    }

    /// The asynchronous part of `clone_application(..., cloneVolumeData: true)`, per volume:
    /// `StopApplication`, `VolumeCloneJob`, then a redeployment of the source. Upstream dispatches
    /// them to queues; here they run in that order.
    pub fn spawn_clone_volumes(
        &self,
        source: ApplicationRow,
        target_server_id: i64,
        volumes: Vec<(String, i64, String)>,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let source_server = source.server_id().unwrap_or(target_server_id);
            for (source_volume, volume_id, target_volume) in volumes {
                let _ = jobs.stop_application(&source, false, false, true).await;
                if let Err(e) = jobs
                    .clone_volume_data(
                        source_server,
                        target_server_id,
                        &source_volume,
                        Some(volume_id),
                        &target_volume,
                    )
                    .await
                {
                    tracing::error!(volume = %source_volume, error = %e, "Failed to copy volume data");
                }
                let deployment = coolify_domain::new_public_id();
                if let Err(e) = jobs
                    .queue_application_deployment(
                        &source,
                        &deployment,
                        crate::applications::queue::QueueOptions {
                            no_questions_asked: true,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    tracing::error!(error = %e, "source could not be redeployed after a volume copy");
                }
            }
        });
    }

    /// `VolumeCloneJob::cloneRemoteVolume()`: the archive goes through this host.
    async fn clone_remote_volume(
        &self,
        source: &ServerInfo,
        target: &ServerInfo,
        source_volume: &str,
        volume_id: Option<i64>,
        target_volume: &str,
    ) -> Result<(), String> {
        let (ts, tt) = (source.target(self).await?, target.target(self).await?);
        let opts = || {
            RunOptions::op("application.clone_volume").timeout(std::time::Duration::from_secs(3600))
        };
        let inspected = self
            .remote()
            .run(
                &ts,
                &[&format!(
                    "docker volume inspect {} 2>/dev/null || echo '[]'",
                    quote(source_volume)
                )],
                opts(),
            )
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let (driver, options) = volume_configuration(&inspected);
        let bind = driver == "local"
            && options.iter().any(|(k, v)| k == "type" && v == "none")
            && options
                .iter()
                .find(|(k, _)| k == "o")
                .is_some_and(|(_, v)| v.split(',').any(|o| o.trim() == "bind"));
        let create = if bind {
            create_volume_command("local", &[], target_volume)
        } else {
            create_volume_command(&driver, &options, target_volume)
        };
        let plain = bind || (driver == "local" && options.is_empty());
        if !bind {
            // `createCloneArchiveDirectory()`.
            let make_dir = async |server: &ServerInfo, t: &coolify_remote::Target, name: &str| {
                if !server.is_non_root() {
                    let dir = format!("/data/coolify/clone/{name}");
                    let q = quote(&dir);
                    self.remote()
                        .run(
                            t,
                            &[&format!("mkdir -p {q}"), &format!("chmod 777 {q}")],
                            opts(),
                        )
                        .await
                        .map_err(|e| e.message)?;
                    return Ok::<String, String>(dir);
                }
                let dir = self
                    .remote()
                    .run(t, &["mktemp -d /var/tmp/coolify-clone.XXXXXXXXXX"], opts())
                    .await
                    .map_err(|e| e.message)?
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let q = quote(&dir);
                self.remote()
                    .run(
                        t,
                        &[
                            &format!("chown {} {q}", quote(&server.user)),
                            &format!("chmod 700 {q}"),
                        ],
                        opts(),
                    )
                    .await
                    .map_err(|e| e.message)?;
                Ok(dir)
            };
            let source_dir = make_dir(source, &ts, source_volume).await?;
            let target_dir = make_dir(target, &tt, target_volume).await?;
            let local = std::env::temp_dir()
                .join("coolify-rs-volume-clones")
                .join(coolify_domain::new_public_id());
            let result: Result<(), String> = async {
                tokio::fs::create_dir_all(&local)
                    .await
                    .map_err(|e| e.to_string())?;
                let archive = local.join("volume-data.tar.gz");
                self.remote()
                    .run(
                        &ts,
                        &[&format!(
                            "docker run --rm -v {}:/source -v {}:/clone alpine sh -c 'cd /source && tar czf /clone/volume-data.tar.gz .'",
                            quote(source_volume),
                            quote(&source_dir)
                        )],
                        opts(),
                    )
                    .await
                    .map_err(|e| e.message)?;
                let mut read = self
                    .remote()
                    .open_command(&ts, &format!("cat {}/volume-data.tar.gz", quote(&source_dir)))
                    .await
                    .map_err(|e| e.message)?;
                let mut file = tokio::fs::File::create(&archive)
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::io::copy(&mut read.stdout, &mut file)
                    .await
                    .map_err(|e| e.to_string())?;
                drop(file);
                self.remote()
                    .copy_file(&tt, &archive, &format!("{target_dir}/volume-data.tar.gz"))
                    .await
                    .map_err(|e| e.message)?;
                self.remote()
                    .run(
                        &tt,
                        &[
                            create.as_str(),
                            &format!(
                                "docker run --rm -v {}:/target -v {}:/clone alpine sh -c 'cd /target && tar xzf /clone/volume-data.tar.gz'",
                                quote(target_volume),
                                quote(&target_dir)
                            ),
                        ],
                        opts(),
                    )
                    .await
                    .map_err(|e| e.message)?;
                Ok(())
            }
            .await;
            let _ = tokio::fs::remove_dir_all(&local).await;
            for (t, dir) in [(&ts, &source_dir), (&tt, &target_dir)] {
                let _ = self
                    .remote()
                    .run_quiet(t, &[&format!("rm -rf {}", quote(dir))], opts())
                    .await;
            }
            result?;
        } else {
            self.remote()
                .run(&tt, &[create.as_str()], opts())
                .await
                .map_err(|e| e.message)?;
        }
        if plain && let Some(id) = volume_id {
            ops::set_ignores_driver_options(&self.0.db, id)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// `VolumeCloneJob::handle()`.
    pub(crate) async fn clone_volume_data(
        &self,
        source_server: i64,
        target_server_id: i64,
        source_volume: &str,
        volume_id: Option<i64>,
        target_volume: &str,
    ) -> Result<(), String> {
        let db = &self.0.db;
        if source_server != target_server_id {
            let load = async |id: i64| {
                servers::find_by_id(db, id)
                    .await
                    .map_err(|e| e.to_string())?
                    .map(|r| ServerInfo::from_row(&r))
                    .ok_or_else(|| "Server not found.".to_owned())
            };
            let (source, target) = (load(source_server).await?, load(target_server_id).await?);
            return self
                .clone_remote_volume(&source, &target, source_volume, volume_id, target_volume)
                .await;
        }
        if source_volume == target_volume {
            return Ok(());
        }
        let row = servers::find_by_id(db, source_server)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let t = ServerInfo::from_row(&row).target(self).await?;
        let inspected = self
            .remote()
            .run(
                &t,
                &[&format!(
                    "docker volume inspect {} 2>/dev/null || echo '[]'",
                    quote(source_volume)
                )],
                RunOptions::op("application.clone_volume"),
            )
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        let (driver, options) = volume_configuration(&inspected);
        let bind = driver == "local"
            && options.iter().any(|(k, v)| k == "type" && v == "none")
            && options
                .iter()
                .find(|(k, _)| k == "o")
                .is_some_and(|(_, v)| v.split(',').any(|o| o.trim() == "bind"));
        let (create, remembered_plain) = if bind {
            (create_volume_command("local", &[], target_volume), true)
        } else {
            (
                create_volume_command(&driver, &options, target_volume),
                driver == "local" && options.is_empty(),
            )
        };
        let mut commands = vec![create];
        if !bind {
            commands.push(format!(
                "docker run --rm -v {}:/source -v {}:/target alpine sh -c 'cp -a /source/. /target/'",
                quote(source_volume),
                quote(target_volume)
            ));
        }
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.remote()
            .run(&t, &refs, RunOptions::op("application.clone_volume"))
            .await
            .map_err(|e| e.message)?;
        if remembered_plain && let Some(volume_id) = volume_id {
            ops::set_ignores_driver_options(db, volume_id)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// `sourceVolumeConfiguration()`: driver and options, for the `docker volume inspect` output.
fn volume_configuration(inspected: &str) -> (String, Vec<(String, String)>) {
    let parsed: Value = serde_json::from_str(inspected.trim()).unwrap_or(Value::Null);
    let volume = parsed.get(0).cloned().unwrap_or(Value::Null);
    let driver = volume
        .get("Driver")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .unwrap_or("local")
        .to_owned();
    let mut options = Vec::new();
    if let Some(Value::Object(o)) = volume.get("Options") {
        for (k, v) in o {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(true) => "1".into(),
                Value::Bool(false) => String::new(),
                _ => continue,
            };
            if !k.is_empty() {
                options.push((k.clone(), text));
            }
        }
    }
    (driver, options)
}

/// `createTargetVolumeCommand()`.
fn create_volume_command(driver: &str, options: &[(String, String)], target: &str) -> String {
    let mut command = "docker volume create".to_owned();
    if driver != "local" {
        command.push_str(&format!(" --driver {}", quote(driver)));
    }
    for (k, v) in options {
        command.push_str(&format!(" --opt {}", quote(&format!("{k}={v}"))));
    }
    command.push_str(&format!(" {}", quote(target)));
    format!("sh -c {}", quote(&command))
}

/// `GetRollbackImages::imageTag($imageReference)`.
pub fn image_tag(reference: &str) -> Option<String> {
    if reference.contains('@') {
        return None;
    }
    let colon = reference.rfind(':')?;
    if reference.rfind('/').is_some_and(|slash| colon < slash) {
        return None;
    }
    let tag = &reference[colon + 1..];
    (!tag.is_empty() && tag != "0").then(|| tag.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines() {
        assert_eq!(normalize_log_lines(None), "100");
        assert_eq!(normalize_log_lines(Some("all")), "all");
        assert_eq!(normalize_log_lines(Some("-1")), "all");
        assert_eq!(normalize_log_lines(Some("-2")), "100");
        assert_eq!(normalize_log_lines(Some("x")), "100");
        assert_eq!(normalize_log_lines(Some("05")), "100");
        assert_eq!(normalize_log_lines(Some("0")), "0");
        assert_eq!(normalize_log_lines(Some("20000")), "10000");
    }

    #[test]
    fn image_tags() {
        assert_eq!(image_tag("nginx:alpine").as_deref(), Some("alpine"));
        assert_eq!(image_tag("registry.example.com:5000/team/app"), None);
        assert_eq!(image_tag("a@sha256:abc"), None);
    }

    #[test]
    fn pull_request_filter() {
        assert!(matches_pull_request("a=b", None));
        assert!(matches_pull_request("coolify.pullRequestId=0", Some(0)));
        assert!(!matches_pull_request("coolify.pullRequestId=3", None));
        assert!(matches_pull_request("coolify.pullRequestId=3", Some(3)));
        assert!(!matches_pull_request("coolify.pullRequestId=3", Some(4)));
        assert!(!matches_pull_request("a=b", Some(4)));
    }

    #[test]
    fn preview_domains() {
        let mut overrides = BTreeMap::new();
        overrides.insert("https://app.example.com".to_owned(), 3000);
        let d = preview_fqdn(
            Some("https://app.example.com/x,https://b.example.com"),
            "{{pr_id}}.{{domain}}",
            7,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(d.fqdn.as_deref(), Some("https://7.app.example.com/x"));
        let d = preview_fqdn(
            Some("https://app.example.com"),
            "{{pr_id}}.{{domain}}",
            7,
            &overrides,
        )
        .unwrap();
        assert_eq!(d.fqdn.as_deref(), Some("https://7.app.example.com"));
        assert!(d.overrides.unwrap().contains("3000"));
        assert!(preview_fqdn(None, "x", 1, &overrides).is_none());
    }
}
