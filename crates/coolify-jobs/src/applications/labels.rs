//! Proxy labels of an application: `generateLabelsApplication`, `fqdnLabelsForTraefik`,
//! `fqdnLabelsForCaddy` and `defaultLabels` (bootstrap/helpers/docker.php).

use std::collections::BTreeMap;

use super::domains::{UrlParts, without_port};
use crate::databases::slug;

/// What the label generators read from the application, its settings and its server.
#[derive(Debug, Clone)]
pub struct LabelInput {
    pub uuid: String,
    pub fqdn: Option<String>,
    /// `settings.is_static ? [80] : ports_exposes_array`.
    pub ports: Vec<String>,
    pub force_https: bool,
    pub permanent_redirect: bool,
    pub gzip: bool,
    pub stripprefix: bool,
    /// `applications.redirect`: `both`, `www` or `non-www`.
    pub redirect: String,
    pub noindex_domains: Vec<String>,
    pub port_overrides: BTreeMap<String, i64>,
    /// `traefikCertificateResolver()`: `None` when unset or `none`.
    pub certificate_resolver: Option<String>,
    pub network: String,
    /// `server.settings.generate_exact_labels` with the proxy type (`TRAEFIK` / `CADDY`).
    pub exact_labels: Option<String>,
    pub traffic_analytics: bool,
    /// The `is_http_basic_auth_*` arguments, when enabled with a username and a password.
    pub basic_auth: Option<BasicAuth>,
    /// The preview branch of `generateLabelsApplication`: no redirect direction.
    pub preview: bool,
}

/// HTTP basic auth of the label generators. Each generator hashes the password once.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub username: String,
    /// Decrypted.
    pub password: String,
    /// `http_basic_auth_bcrypt_cost`.
    pub bcrypt_cost: u32,
    /// `(memory_cost, time_cost)` when `usesArgon2idBasicAuth()` (Caddy labels only).
    pub argon2id: Option<(u32, u32)>,
    /// `Server::caddySupportsBasicAuthDirective()`.
    pub supports_directive: bool,
}

impl BasicAuth {
    fn bcrypt(&self) -> String {
        coolify_compat::password::hash_bcrypt(&self.password, self.bcrypt_cost).unwrap_or_default()
    }

    fn caddy_hash(&self) -> String {
        match self.argon2id {
            Some((memory, time)) => {
                coolify_compat::password::hash_argon2id(&self.password, memory, time)
                    .unwrap_or_default()
            }
            None => self.bcrypt(),
        }
    }
}

/// `Server::caddyDockerProxyImageVersion($server->configuredCaddyProxyImage())`.
fn configured_caddy_version(server: &coolify_db::servers::ServerRow) -> Option<(u32, u32)> {
    use serde_json::Value;
    let proxy = server.record.get("proxy")?;
    if proxy.get("type").and_then(Value::as_str) != Some("CADDY") {
        return None;
    }
    let saved = proxy
        .get("last_saved_proxy_configuration")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parsed = coolify_compat::yaml_parse::parse(saved, 128).ok()?;
    let services = php_get(&parsed, "services")?;
    let caddy = php_get(services, "caddy")?;
    let coolify_compat::yaml::PhpValue::Str(image) = php_get(caddy, "image")? else {
        return None;
    };
    let re = regex::Regex::new(r"(?:^|/)caddy-docker-proxy:(\d+)\.(\d+)").ok()?;
    let found = re.captures(image)?;
    Some((found[1].parse().ok()?, found[2].parse().ok()?))
}

fn php_get<'a>(
    value: &'a coolify_compat::yaml::PhpValue,
    key: &str,
) -> Option<&'a coolify_compat::yaml::PhpValue> {
    let coolify_compat::yaml::PhpValue::Array(items) = value else {
        return None;
    };
    items
        .iter()
        .find(|(k, _)| k.as_string() == key)
        .map(|(_, v)| v)
}

/// `Server::hasPendingProxyConfiguration()`.
fn has_pending_proxy_configuration(server: &coolify_db::servers::ServerRow) -> bool {
    use serde_json::Value;
    let Some(proxy) = server.record.get("proxy") else {
        return false;
    };
    if proxy.get("status").and_then(Value::as_str) != Some("running") {
        return false;
    }
    if proxy
        .get("certificates_restart_required")
        .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
    {
        return true;
    }
    let filled = |k: &str| {
        proxy
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    matches!((filled("last_saved_settings"), filled("last_applied_settings")), (Some(a), Some(b)) if a != b)
}

/// `Server::caddySupportsBasicAuthDirective()` (`caddyRunsCurrentVersion()`).
pub fn caddy_supports_basic_auth_directive(server: &coolify_db::servers::ServerRow) -> bool {
    !has_pending_proxy_configuration(server)
        && configured_caddy_version(server).is_some_and(|v| v >= (2, 9))
}

/// `Server::caddySupportsArgon2idBasicAuth()`.
pub fn caddy_supports_argon2id(server: &coolify_db::servers::ServerRow) -> bool {
    caddy_supports_basic_auth_directive(server)
        && configured_caddy_version(server).is_some_and(|v| v >= (2, 11))
}

impl LabelInput {
    /// The label inputs of an application on its server.
    pub fn new(
        a: &coolify_db::applications::ApplicationRow,
        server: &coolify_db::servers::ServerRow,
    ) -> Self {
        use serde_json::Value;
        let settings = server.settings.clone().unwrap_or_default();
        let ports: Vec<String> = if a.setting_bool("is_static") {
            vec!["80".into()]
        } else {
            a.str("ports_exposes")
                .map(|p| p.split(',').map(str::to_owned).collect())
                .unwrap_or_default()
        };
        let json_col = |k: &str| match a.raw.get(k) {
            Some(Value::String(t)) => serde_json::from_str::<Value>(t).ok(),
            Some(v) => Some(v.clone()),
            None => None,
        };
        let noindex_domains = match json_col("noindex_domains") {
            Some(Value::Array(v)) => v
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let port_overrides = match json_col("domain_port_overrides") {
            Some(Value::Object(o)) => o
                .iter()
                .filter_map(|(k, v)| {
                    v.as_i64()
                        .or_else(|| v.as_str()?.parse().ok())
                        .map(|p| (k.clone(), p))
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        // Server::traefikCertificateResolver(): the application's, else the server's; `none` is none.
        let resolver = a
            .setting_str("traefik_certificate_resolver")
            .filter(|r| !r.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                settings
                    .get("traefik_certificate_resolver")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|r| !r.is_empty() && r != "none");
        let proxy_type = server
            .record
            .get("proxy")
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let exact = settings
            .get("generate_exact_labels")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        LabelInput {
            uuid: a.uuid.clone(),
            fqdn: a.str("fqdn").map(str::to_owned),
            ports,
            force_https: a
                .settings
                .get("is_force_https_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            permanent_redirect: a.setting_bool("is_permanent_redirect_enabled"),
            gzip: a
                .settings
                .get("is_gzip_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            stripprefix: a
                .settings
                .get("is_stripprefix_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            redirect: a.str("redirect").unwrap_or("both").to_owned(),
            noindex_domains,
            port_overrides,
            certificate_resolver: resolver,
            network: a.network().to_owned(),
            exact_labels: exact.then(|| proxy_type.unwrap_or_default()),
            traffic_analytics: settings
                .get("is_traffic_analytics_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            basic_auth: None,
            preview: false,
        }
    }

    /// The preview branch of `generateLabelsApplication($application, $preview)`: the preview's
    /// domains, every one of them noindex.
    #[must_use]
    pub fn for_preview(
        mut self,
        pull_request_id: i64,
        fqdn: Option<&str>,
        port_overrides: BTreeMap<String, i64>,
    ) -> Self {
        self.uuid = format!("{}-pr-{pull_request_id}", self.uuid);
        self.fqdn = fqdn.map(str::to_owned);
        self.noindex_domains = fqdn
            .unwrap_or_default()
            .split(',')
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .collect();
        self.port_overrides = port_overrides;
        self.redirect = "both".to_owned();
        self.preview = true;
        self
    }

    /// `generateLabelsApplication`'s basic auth arguments. A password that cannot be decrypted
    /// gives an error, as the `encrypted` cast throws.
    pub fn with_basic_auth(
        mut self,
        a: &coolify_db::applications::ApplicationRow,
        server: &coolify_db::servers::ServerRow,
        enc: &coolify_compat::Encrypter,
    ) -> Result<Self, String> {
        let (Some(username), Some(cipher), true) = (
            a.str("http_basic_auth_username"),
            a.str("http_basic_auth_password"),
            a.bool("is_http_basic_auth_enabled"),
        ) else {
            return Ok(self);
        };
        let password = enc.decrypt_string(cipher).map_err(|e| e.to_string())?;
        let argon2id = (a.str("http_basic_auth_hash_algorithm") == Some("argon2id")
            && caddy_supports_argon2id(server))
        .then(|| {
            (
                a.int("http_basic_auth_argon2id_memory_cost")
                    .unwrap_or(65536) as u32,
                a.int("http_basic_auth_argon2id_time_cost").unwrap_or(4) as u32,
            )
        });
        self.basic_auth = Some(BasicAuth {
            username: username.to_owned(),
            password,
            bcrypt_cost: a.int("http_basic_auth_bcrypt_cost").unwrap_or(10) as u32,
            argon2id,
            supports_directive: caddy_supports_basic_auth_directive(server),
        });
        Ok(self)
    }
}

/// `isNoindexDomain`: compares portless, normalised URLs.
fn is_noindex(domain: &str, noindex: &[String]) -> bool {
    if noindex.is_empty() {
        return false;
    }
    let norm = |u: &str| without_port(&super::domains::normalize_url(u));
    let d = norm(domain);
    noindex.iter().any(|n| norm(n) == d)
}

/// The port of a domain: its own, the override of its portless form, or the first exposed port.
fn port_for(domain: &str, url: &UrlParts, input: &LabelInput) -> Option<String> {
    url.port
        .clone()
        .or_else(|| {
            input
                .port_overrides
                .get(&without_port(domain))
                .map(i64::to_string)
        })
        .or_else(|| input.ports.first().cloned())
}

/// PHP `sort()` of the label list (byte order).
fn sorted(mut labels: Vec<String>) -> Vec<String> {
    labels.sort();
    labels
}

/// `fqdnLabelsForTraefik` for an application (no service labels, no unique uuids, no ghost image),
/// with `escape_redirect_replacement_for_compose` as given.
pub fn traefik(input: &LabelInput, domains: &[String], escape_redirect: bool) -> Vec<String> {
    let u = &input.uuid;
    let mut labels: Vec<String> = vec!["traefik.enable=true".into()];
    let basic_auth_label = format!("http-basic-auth-{u}");
    if let Some(auth) = &input.basic_auth {
        labels.push(format!(
            "traefik.http.middlewares.{basic_auth_label}.basicauth.users={}:{}",
            auth.username,
            auth.bcrypt()
        ));
    }
    if input.gzip {
        labels.push(
            "traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream"
                .into(),
        );
    }
    labels.push("traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https".into());
    let mut redirect_to_https = "redirect-to-https";
    if input.permanent_redirect {
        redirect_to_https = "redirect-to-https-permanent";
        labels.push(
            "traefik.http.middlewares.redirect-to-https-permanent.redirectscheme.scheme=https"
                .into(),
        );
        labels.push(
            "traefik.http.middlewares.redirect-to-https-permanent.redirectscheme.permanent=true"
                .into(),
        );
    }
    let redirect_permanent = if input.permanent_redirect {
        "true"
    } else {
        "false"
    };
    for (i, domain) in domains.iter().enumerate() {
        let Some(url) = UrlParts::parse(domain) else {
            continue;
        };
        let host = url.host.as_str();
        let traefik_host = host.trim_matches(|c| c == '[' || c == ']');
        let path = url.path_or_root();
        let port = port_for(domain, &url, input);
        let http_label = format!("http-{i}-{u}");
        let https_label = format!("https-{i}-{u}");
        let noindex_name = format!("{i}-{u}-noindex");
        let noindex = is_noindex(domain, &input.noindex_domains);
        if noindex {
            labels.push(format!(
                "traefik.http.middlewares.{noindex_name}.headers.customresponseheaders.X-Robots-Tag=noindex, nofollow"
            ));
        }
        let to_www = format!("{i}-{u}-to-www");
        let to_non_www = format!("{i}-{u}-to-non-www");
        let cap = if escape_redirect { "$$" } else { "$" };
        let redirect_to_non_www = [
            format!(
                "traefik.http.middlewares.{to_non_www}.redirectregex.regex=^(http|https)://www\\.(.+)"
            ),
            format!(
                "traefik.http.middlewares.{to_non_www}.redirectregex.replacement={cap}{{1}}://{cap}{{2}}"
            ),
            format!(
                "traefik.http.middlewares.{to_non_www}.redirectregex.permanent={redirect_permanent}"
            ),
        ];
        let redirect_to_www = [
            format!(
                "traefik.http.middlewares.{to_www}.redirectregex.regex=^(http|https)://(?:www\\.)?(.+)"
            ),
            format!(
                "traefik.http.middlewares.{to_www}.redirectregex.replacement={cap}{{1}}://www.{cap}{{2}}"
            ),
            format!(
                "traefik.http.middlewares.{to_www}.redirectregex.permanent={redirect_permanent}"
            ),
        ];
        // The middleware list of a router (stripprefix label pushed when the path is not `/`).
        let router_middlewares = |labels: &mut Vec<String>, label: &str| -> Vec<String> {
            let mut m: Vec<String> = Vec::new();
            if path != "/" && input.stripprefix {
                labels.push(format!(
                    "traefik.http.middlewares.{label}-stripprefix.stripprefix.prefixes={path}"
                ));
                m.push(format!("{label}-stripprefix"));
            }
            if input.gzip {
                m.push("gzip-sse".into());
            }
            if input.redirect == "non-www" && host.starts_with("www.") {
                labels.extend(redirect_to_non_www.iter().cloned());
                m.push(to_non_www.clone());
            }
            if input.redirect == "www" && !host.starts_with("www.") {
                labels.extend(redirect_to_www.iter().cloned());
                m.push(to_www.clone());
            }
            if input.basic_auth.is_some() {
                m.push(basic_auth_label.clone());
            }
            if noindex {
                m.push(noindex_name.clone());
            }
            m
        };
        if url.scheme == "https" {
            labels.push(format!("traefik.http.routers.{https_label}.rule=Host(`{traefik_host}`) && PathPrefix(`{path}`)"));
            labels.push(format!(
                "traefik.http.routers.{https_label}.entryPoints=https"
            ));
            if let Some(p) = &port {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.service={https_label}"
                ));
                labels.push(format!(
                    "traefik.http.services.{https_label}.loadbalancer.server.port={p}"
                ));
            }
            let middlewares = router_middlewares(&mut labels, &https_label);
            if !middlewares.is_empty() {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.middlewares={}",
                    middlewares.join(",")
                ));
            }
            labels.push(format!("traefik.http.routers.{https_label}.tls=true"));
            if let Some(r) = input
                .certificate_resolver
                .as_deref()
                .filter(|r| !r.is_empty())
            {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.tls.certresolver={r}"
                ));
            }
            labels.push(format!("traefik.http.routers.{http_label}.rule=Host(`{traefik_host}`) && PathPrefix(`{path}`)"));
            labels.push(format!(
                "traefik.http.routers.{http_label}.entryPoints=http"
            ));
            if let Some(p) = &port {
                labels.push(format!(
                    "traefik.http.services.{http_label}.loadbalancer.server.port={p}"
                ));
                labels.push(format!(
                    "traefik.http.routers.{http_label}.service={http_label}"
                ));
            }
            let http_middlewares = if input.force_https {
                let mut m = Vec::new();
                if noindex {
                    m.push(noindex_name.clone());
                }
                m.push(redirect_to_https.to_owned());
                m
            } else {
                middlewares
            };
            if !http_middlewares.is_empty() {
                labels.push(format!(
                    "traefik.http.routers.{http_label}.middlewares={}",
                    http_middlewares.join(",")
                ));
            }
        } else {
            labels.push(format!("traefik.http.routers.{http_label}.rule=Host(`{traefik_host}`) && PathPrefix(`{path}`)"));
            labels.push(format!(
                "traefik.http.routers.{http_label}.entryPoints=http"
            ));
            if let Some(p) = &port {
                labels.push(format!(
                    "traefik.http.services.{http_label}.loadbalancer.server.port={p}"
                ));
                labels.push(format!(
                    "traefik.http.routers.{http_label}.service={http_label}"
                ));
            }
            let middlewares = router_middlewares(&mut labels, &http_label);
            if !middlewares.is_empty() {
                labels.push(format!(
                    "traefik.http.routers.{http_label}.middlewares={}",
                    middlewares.join(",")
                ));
            }
        }
    }
    sorted(labels)
}

/// `caddySiteAddress`.
fn caddy_site_address(scheme: &str, host: &str, force_https: bool) -> String {
    if scheme == "https" && !force_https {
        format!("http://{host}, https://{host}")
    } else {
        format!("{scheme}://{host}")
    }
}

/// `fqdnLabelsForCaddy` for an application (`log_append` is not detected).
pub fn caddy(input: &LabelInput, domains: &[String]) -> Vec<String> {
    let mut labels: Vec<String> = vec![format!("caddy_ingress_network={}", input.network)];
    let app_key = input.uuid.clone();
    let caddy_hash = input.basic_auth.as_ref().map(BasicAuth::caddy_hash);
    for (i, domain) in domains.iter().enumerate() {
        let Some(url) = UrlParts::parse(domain) else {
            continue;
        };
        let host = url.host.as_str();
        let path = url.path_or_root();
        let host_without_www = host.replace("www.", "");
        let site = caddy_site_address(&url.scheme, host, input.force_https);
        let port = port_for(domain, &url, input);
        let handle = if input.stripprefix {
            "handle_path"
        } else {
            "handle"
        };
        labels.push(format!("caddy_{i}={site}"));
        if is_noindex(domain, &input.noindex_domains) {
            labels.push(format!("caddy_{i}.header.0_-Server="));
            labels.push(format!(
                "caddy_{i}.header.1_X-Robots-Tag=\"noindex, nofollow\""
            ));
        } else {
            labels.push(format!("caddy_{i}.header=-Server"));
        }
        labels.push(format!(
            "caddy_{i}.try_files={{path}} /index.html /index.php"
        ));
        match port.as_deref().filter(|p| !p.is_empty() && *p != "0") {
            Some(p) => labels.push(format!(
                "caddy_{i}.{handle}.{i}_reverse_proxy={{{{upstreams {p}}}}}"
            )),
            None => labels.push(format!(
                "caddy_{i}.{handle}.{i}_reverse_proxy={{{{upstreams}}}}"
            )),
        }
        labels.push(format!("caddy_{i}.{handle}={path}*"));
        if input.gzip {
            labels.push(format!("caddy_{i}.encode=zstd gzip"));
        }
        let redirect_scheme = if input.force_https {
            url.scheme.as_str()
        } else {
            "{scheme}"
        };
        let code = if input.permanent_redirect {
            " permanent"
        } else {
            ""
        };
        if input.redirect == "www" && !host.starts_with("www.") {
            labels.push(format!(
                "caddy_{i}.redir={redirect_scheme}://www.{host}{{uri}}{code}"
            ));
        }
        if input.redirect == "non-www" && host.starts_with("www.") {
            labels.push(format!(
                "caddy_{i}.redir={redirect_scheme}://{host_without_www}{{uri}}{code}"
            ));
        }
        if let (Some(auth), Some(hash)) = (&input.basic_auth, &caddy_hash) {
            let directive = if auth.supports_directive {
                "basic_auth"
            } else {
                "basicauth"
            };
            if auth.argon2id.is_some() {
                labels.push(format!("caddy_{i}.{directive}=argon2id"));
            }
            labels.push(format!(
                "caddy_{i}.{directive}.{}=\"{hash}\"",
                auth.username
            ));
        }
        if input.traffic_analytics {
            labels.push(format!("caddy_{i}.log.output=file /traffic/access.log"));
            labels.push(format!("caddy_{i}.log.output.roll_size=20MiB"));
            labels.push(format!("caddy_{i}.log.output.roll_keep=5"));
            labels.push(format!("caddy_{i}.log.output.roll_keep_for=168h"));
            labels.push(format!("caddy_{i}.log.format=json"));
            let _ = &app_key;
        }
    }
    sorted(labels)
}

/// `generateLabelsApplication($application)` for the production deployment.
pub fn application(input: &LabelInput) -> Vec<String> {
    let Some(fqdn) = input.fqdn.as_deref().filter(|f| !f.is_empty()) else {
        return Vec::new();
    };
    let domains: Vec<String> = fqdn.split(',').map(str::to_owned).collect();
    match input.exact_labels.as_deref() {
        Some("TRAEFIK") => traefik(input, &domains, !input.preview),
        Some("CADDY") => caddy(input, &domains),
        Some(_) => Vec::new(),
        None => {
            let mut labels = traefik(input, &domains, false);
            labels.extend(caddy(input, &domains));
            labels
        }
    }
}

/// `defaultLabels($uuid, $uuid, $project, $name, $environment, 0)`.
pub fn default_labels(
    uuid: &str,
    project: &str,
    name: &str,
    environment: &str,
    version: &str,
    pull_request_id: i64,
) -> Vec<String> {
    vec![
        "coolify.managed=true".into(),
        format!("coolify.version={version}"),
        format!("coolify.applicationUuid={uuid}"),
        "coolify.type=application".into(),
        format!("coolify.name={}", slug(uuid)),
        format!("coolify.resourceName={}", slug(name)),
        format!("coolify.projectName={}", slug(project)),
        format!("coolify.serviceName={}", slug(name)),
        format!("coolify.environmentName={}", slug(environment)),
        format!("coolify.pullRequestId={pull_request_id}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(fqdn: &str) -> LabelInput {
        LabelInput {
            uuid: "vwk8s4gc0cw8kkw4ow8sgcc0".into(),
            fqdn: Some(fqdn.into()),
            ports: vec!["80".into()],
            force_https: true,
            permanent_redirect: false,
            gzip: true,
            stripprefix: true,
            redirect: "both".into(),
            noindex_domains: vec![],
            port_overrides: BTreeMap::new(),
            certificate_resolver: Some("letsencrypt".into()),
            network: "coolify".into(),
            exact_labels: None,
            traffic_analytics: false,
            basic_auth: None,
            preview: false,
        }
    }

    #[test]
    fn worked_example() {
        let labels = application(&input("https://app.example.com,http://api.example.com"));
        let expected = "traefik.enable=true
traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream
traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=redirect-to-https
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)
traefik.http.routers.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-0-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=http
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`api.example.com`) && PathPrefix(`/`)
traefik.http.routers.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.service=http-1-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.entryPoints=https
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.middlewares=gzip-sse
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.rule=Host(`app.example.com`) && PathPrefix(`/`)
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.service=https-0-vwk8s4gc0cw8kkw4ow8sgcc0
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls.certresolver=letsencrypt
traefik.http.routers.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.tls=true
traefik.http.services.http-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
traefik.http.services.http-1-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
traefik.http.services.https-0-vwk8s4gc0cw8kkw4ow8sgcc0.loadbalancer.server.port=80
caddy_0.encode=zstd gzip
caddy_0.handle_path.0_reverse_proxy={{upstreams 80}}
caddy_0.handle_path=/*
caddy_0.header=-Server
caddy_0.try_files={path} /index.html /index.php
caddy_0=https://app.example.com
caddy_1.encode=zstd gzip
caddy_1.handle_path.1_reverse_proxy={{upstreams 80}}
caddy_1.handle_path=/*
caddy_1.header=-Server
caddy_1.try_files={path} /index.html /index.php
caddy_1=http://api.example.com
caddy_ingress_network=coolify";
        assert_eq!(labels.join("\n"), expected);
    }
}
