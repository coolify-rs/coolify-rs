//! Container labels of Compose services (bootstrap/helpers/docker.php): `defaultLabels()`,
//! `fqdnLabelsForTraefik()` and `fqdnLabelsForCaddy()` with every argument the compose parsers
//! pass (service labels, service name, image, only port, noindex domains, redirects, port
//! overrides), `addTraefikDockerNetworkLabel()` and `escapeDollarSign()`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use coolify_compat::yaml::PhpValue;
use md5::Digest as _;

use super::php::{self, SpatieUrl};
use crate::applications::domains::{normalize_url, without_port};

/// `defaultLabels($uuid, $name, $projectName, $resourceName, $environment, $pull_request_id, $type)`
/// (no service sub-labels).
#[allow(clippy::too_many_arguments)]
pub fn default_labels(
    uuid: &str,
    name: &str,
    project_name: &str,
    resource_name: &str,
    environment: &str,
    pull_request_id: i64,
    kind: &str,
    version: &str,
) -> Vec<String> {
    vec![
        "coolify.managed=true".into(),
        format!("coolify.version={version}"),
        format!("coolify.{kind}Uuid={uuid}"),
        format!("coolify.type={kind}"),
        format!("coolify.name={}", php::slug(name)),
        format!("coolify.resourceName={}", php::slug(resource_name)),
        format!("coolify.projectName={}", php::slug(project_name)),
        format!("coolify.serviceName={}", php::slug(resource_name)),
        format!("coolify.environmentName={}", php::slug(environment)),
        format!("coolify.pullRequestId={pull_request_id}"),
    ]
}

/// `escapeDollarSign($value)` (`str_replace` converts scalars to strings; arrays are mapped).
pub fn escape_dollar_sign(v: &PhpValue) -> PhpValue {
    match v {
        PhpValue::Array(items) => PhpValue::Array(
            items
                .iter()
                .map(|(k, v)| (k.clone(), escape_dollar_sign(v)))
                .collect(),
        ),
        other => PhpValue::Str(php::to_string(other).replace('$', "$$")),
    }
}

/// `addTraefikDockerNetworkLabel($labels, $network)`.
pub fn add_traefik_docker_network_label(labels: &mut Vec<PhpValue>, network: &str) {
    let has = labels.iter().any(|l| match l {
        PhpValue::Str(s) => php::before(s, "=") == "traefik.docker.network",
        _ => false,
    });
    if !has {
        labels.push(PhpValue::Str(format!("traefik.docker.network={network}")));
    }
}

/// `traefikServiceNameHash($serviceName)`.
fn traefik_service_name_hash(service_name: &str) -> String {
    let digest = md5::Md5::digest(service_name.as_bytes());
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..4]
        .to_owned()
}

static NON_LABEL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^a-zA-Z0-9-]+").unwrap_or_else(|_| unreachable!()));
static DASHES: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"-+").unwrap_or_else(|_| unreachable!()));

/// `traefikSafeServiceNameSegment($serviceName)`.
pub fn traefik_safe_service_name_segment(service_name: &str) -> String {
    let n = service_name.replace(['.', '_'], "-");
    let n = NON_LABEL.replace_all(&n, "-");
    let n = DASHES.replace_all(&n, "-");
    let mut n = n.trim_matches('-').to_owned();
    if n.is_empty() {
        n = "service".into();
    }
    format!("{n}-{}", traefik_service_name_hash(service_name))
}

/// `caddyTrafficAppKey($uuid, $serviceName)`.
fn caddy_traffic_app_key(uuid: &str, service_name: Option<&str>) -> String {
    let key = match service_name.filter(|s| php::truthy(&PhpValue::Str((*s).to_owned()))) {
        Some(s) => format!("{uuid}-{}", traefik_safe_service_name_segment(s)),
        None => uuid.to_owned(),
    };
    static NON_KEY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[^A-Za-z0-9-]+").unwrap_or_else(|_| unreachable!()));
    NON_KEY.replace_all(&key, "-").into_owned()
}

/// `isNoindexDomain($domain, $noindex_domains)`.
fn is_noindex_domain(domain: &str, noindex: &[String]) -> bool {
    if noindex.is_empty() {
        return false;
    }
    let norm = |u: &str| without_port(&normalize_url(u));
    let d = norm(domain);
    noindex.iter().any(|n| norm(n) == d)
}

/// The arguments the compose parsers pass to `fqdnLabelsForTraefik()` / `fqdnLabelsForCaddy()`.
#[derive(Debug, Clone)]
pub struct ProxyLabelArgs<'a> {
    pub uuid: &'a str,
    /// `fqdnLabelsForCaddy($network)`; unused when service labels are passed.
    pub network: &'a str,
    pub domains: &'a [String],
    pub force_https: bool,
    pub permanent_redirect: bool,
    /// `$serviceLabels` (the parsers always pass a collection).
    pub service_labels: Option<&'a [PhpValue]>,
    pub gzip: bool,
    pub stripprefix: bool,
    pub service_name: Option<&'a str>,
    pub image: &'a str,
    pub only_port: Option<i64>,
    pub noindex_domains: &'a [String],
    pub redirect_direction: &'a str,
    pub port_overrides: &'a BTreeMap<String, i64>,
    pub certificate_resolver: Option<&'a str>,
    pub traffic_analytics: bool,
    pub supports_log_append: bool,
}

fn port_of(args: &ProxyLabelArgs<'_>, domain: &str, url: &SpatieUrl) -> Option<i64> {
    url.port
        .map(i64::from)
        .or_else(|| args.port_overrides.get(&without_port(domain)).copied())
        .or(args.only_port)
}

static TRAEFIK_MIDDLEWARE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"traefik\.http\.middlewares\.(.*?)(\.|\n?\z)")
        .unwrap_or_else(|_| unreachable!())
});
static COOLIFY_MIDDLEWARES: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"coolify\.traefik\.middlewares=(.*)").unwrap_or_else(|_| unreachable!())
});

/// The middleware names declared in the service labels (`$middlewares_from_labels`).
fn middlewares_from_labels(labels: &[PhpValue]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in labels {
        let text = match item {
            PhpValue::Array(items) => {
                let k = items
                    .first()
                    .map(|(k, _)| k.as_string())
                    .unwrap_or_default();
                let v = items
                    .first()
                    .map(|(_, v)| php::to_string(v))
                    .unwrap_or_default();
                format!("{k}={v}")
            }
            PhpValue::Str(s) => s.clone(),
            _ => continue,
        };
        let found: Vec<String> = if let Some(c) = TRAEFIK_MIDDLEWARE.captures(&text) {
            vec![c[1].to_owned()]
        } else if let Some(c) = COOLIFY_MIDDLEWARES.captures(&text) {
            c[1].split(',').map(str::to_owned).collect()
        } else {
            Vec::new()
        };
        for f in found {
            if php::truthy(&PhpValue::Str(f.clone())) && !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// PHP `$labels->sort()` of strings (byte order, stable).
fn sorted(mut labels: Vec<String>) -> Vec<String> {
    labels.sort();
    labels
}

/// `fqdnLabelsForTraefik(...)` without basic auth, retries or unique uuids.
#[allow(clippy::too_many_lines)]
pub fn fqdn_labels_for_traefik(args: &ProxyLabelArgs<'_>) -> Vec<String> {
    let uuid = args.uuid;
    let mut labels: Vec<String> = vec!["traefik.enable=true".into()];
    if args.gzip {
        labels.push(
            "traefik.http.middlewares.gzip-sse.compress.excludedcontenttypes=text/event-stream"
                .into(),
        );
    }
    labels.push("traefik.http.middlewares.redirect-to-https.redirectscheme.scheme=https".into());
    let mut redirect_to_https = "redirect-to-https";
    if args.permanent_redirect {
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
    let redirect_permanent = if args.permanent_redirect {
        "true"
    } else {
        "false"
    };
    let from_labels = args
        .service_labels
        .map(middlewares_from_labels)
        .unwrap_or_default();
    let ghost = args.image.contains("ghost");

    for (i, domain) in args.domains.iter().enumerate() {
        let Ok(url) = SpatieUrl::from_string(domain) else {
            continue;
        };
        let host = url.host.as_str();
        let traefik_host = host.trim_matches(|c| c == '[' || c == ']');
        let path = url.path.as_str();
        let port = port_of(args, domain, &url).filter(|p| *p != 0);
        let (mut http_label, mut https_label) =
            (format!("http-{i}-{uuid}"), format!("https-{i}-{uuid}"));
        if let Some(s) = args
            .service_name
            .filter(|s| php::truthy(&PhpValue::Str((*s).to_owned())))
        {
            let safe = traefik_safe_service_name_segment(s);
            http_label = format!("http-{i}-{uuid}-{safe}");
            https_label = format!("https-{i}-{uuid}-{safe}");
        }
        if ghost {
            labels.push(format!(
                "traefik.http.middlewares.redir-ghost-{uuid}.redirectregex.regex=^{path}/(.*)"
            ));
            labels.push(format!(
                "traefik.http.middlewares.redir-ghost-{uuid}.redirectregex.replacement=/$1"
            ));
            labels.push(format!(
                "caddy_{i}.handle_path.{i}_redir-ghost-{uuid}.handler=rewrite"
            ));
            labels.push(format!(
                "caddy_{i}.handle_path.{i}_redir-ghost-{uuid}.rewrite.regexp=^{path}/(.*)"
            ));
            labels.push(format!(
                "caddy_{i}.handle_path.{i}_redir-ghost-{uuid}.rewrite.replacement=/$1"
            ));
        }
        let noindex_name = format!("{i}-{uuid}-noindex");
        let noindex = is_noindex_domain(domain, args.noindex_domains);
        if noindex {
            labels.push(format!(
                "traefik.http.middlewares.{noindex_name}.headers.customresponseheaders.X-Robots-Tag=noindex, nofollow"
            ));
        }
        let to_www = format!("{i}-{uuid}-to-www");
        let to_non_www = format!("{i}-{uuid}-to-non-www");
        let redirect_to_non_www = [
            format!(
                "traefik.http.middlewares.{to_non_www}.redirectregex.regex=^(http|https)://www\\.(.+)"
            ),
            format!(
                "traefik.http.middlewares.{to_non_www}.redirectregex.replacement=$${{1}}://$${{2}}"
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
                "traefik.http.middlewares.{to_www}.redirectregex.replacement=$${{1}}://www.$${{2}}"
            ),
            format!(
                "traefik.http.middlewares.{to_www}.redirectregex.permanent={redirect_permanent}"
            ),
        ];
        let middlewares_for = |labels: &mut Vec<String>, label: &str| -> Vec<String> {
            let mut m: Vec<String> = Vec::new();
            if path != "/" && args.stripprefix && !ghost {
                labels.push(format!(
                    "traefik.http.middlewares.{label}-stripprefix.stripprefix.prefixes={path}"
                ));
                m.push(format!("{label}-stripprefix"));
            }
            if args.gzip {
                m.push("gzip-sse".into());
            }
            if ghost {
                m.push(format!("redir-ghost-{uuid}"));
            }
            if args.redirect_direction == "non-www" && host.starts_with("www.") {
                labels.extend(redirect_to_non_www.iter().cloned());
                m.push(to_non_www.clone());
            }
            if args.redirect_direction == "www" && !host.starts_with("www.") {
                labels.extend(redirect_to_www.iter().cloned());
                m.push(to_www.clone());
            }
            if noindex {
                m.push(noindex_name.clone());
            }
            m.extend(from_labels.iter().cloned());
            m
        };
        let rule = format!("Host(`{traefik_host}`) && PathPrefix(`{path}`)");
        if url.scheme == "https" {
            labels.push(format!("traefik.http.routers.{https_label}.rule={rule}"));
            labels.push(format!(
                "traefik.http.routers.{https_label}.entryPoints=https"
            ));
            if let Some(p) = port {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.service={https_label}"
                ));
                labels.push(format!(
                    "traefik.http.services.{https_label}.loadbalancer.server.port={p}"
                ));
            }
            let middlewares = middlewares_for(&mut labels, &https_label);
            if !middlewares.is_empty() {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.middlewares={}",
                    middlewares.join(",")
                ));
            }
            labels.push(format!("traefik.http.routers.{https_label}.tls=true"));
            if let Some(r) = args.certificate_resolver.filter(|r| !r.trim().is_empty()) {
                labels.push(format!(
                    "traefik.http.routers.{https_label}.tls.certresolver={r}"
                ));
            }
            labels.push(format!("traefik.http.routers.{http_label}.rule={rule}"));
            labels.push(format!(
                "traefik.http.routers.{http_label}.entryPoints=http"
            ));
            if let Some(p) = port {
                labels.push(format!(
                    "traefik.http.services.{http_label}.loadbalancer.server.port={p}"
                ));
                labels.push(format!(
                    "traefik.http.routers.{http_label}.service={http_label}"
                ));
            }
            let http_middlewares = if args.force_https {
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
            labels.push(format!("traefik.http.routers.{http_label}.rule={rule}"));
            labels.push(format!(
                "traefik.http.routers.{http_label}.entryPoints=http"
            ));
            if let Some(p) = port {
                labels.push(format!(
                    "traefik.http.services.{http_label}.loadbalancer.server.port={p}"
                ));
                labels.push(format!(
                    "traefik.http.routers.{http_label}.service={http_label}"
                ));
            }
            let middlewares = middlewares_for(&mut labels, &http_label);
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

/// `caddySiteAddress()`.
fn caddy_site_address(scheme: &str, host: &str, force_https: bool) -> String {
    if scheme == "https" && !force_https {
        format!("http://{host}, https://{host}")
    } else {
        format!("{scheme}://{host}")
    }
}

/// `fqdnLabelsForCaddy(...)` without basic auth or retries.
pub fn fqdn_labels_for_caddy(args: &ProxyLabelArgs<'_>) -> Vec<String> {
    let mut labels: Vec<String> = vec![if args.service_labels.is_some() {
        format!("caddy_ingress_network={}", args.uuid)
    } else {
        format!("caddy_ingress_network={}", args.network)
    }];
    let app_key = caddy_traffic_app_key(args.uuid, args.service_name);
    for (i, domain) in args.domains.iter().enumerate() {
        let Ok(url) = SpatieUrl::from_string(domain) else {
            continue;
        };
        let host = url.host.as_str();
        let path = url.path.as_str();
        let host_without_www = host.replace("www.", "");
        let site = caddy_site_address(&url.scheme, host, args.force_https);
        let port = port_of(args, domain, &url);
        let handle = if args.stripprefix {
            "handle_path"
        } else {
            "handle"
        };
        labels.push(format!("caddy_{i}={site}"));
        if is_noindex_domain(domain, args.noindex_domains) {
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
        match port.filter(|p| *p != 0) {
            Some(p) => labels.push(format!(
                "caddy_{i}.{handle}.{i}_reverse_proxy={{{{upstreams {p}}}}}"
            )),
            None => labels.push(format!(
                "caddy_{i}.{handle}.{i}_reverse_proxy={{{{upstreams}}}}"
            )),
        }
        labels.push(format!("caddy_{i}.{handle}={path}*"));
        if args.gzip {
            labels.push(format!("caddy_{i}.encode=zstd gzip"));
        }
        let redirect_scheme = if args.force_https {
            url.scheme.as_str()
        } else {
            "{scheme}"
        };
        let code = if args.permanent_redirect {
            " permanent"
        } else {
            ""
        };
        if args.redirect_direction == "www" && !host.starts_with("www.") {
            labels.push(format!(
                "caddy_{i}.redir={redirect_scheme}://www.{host}{{uri}}{code}"
            ));
        }
        if args.redirect_direction == "non-www" && host.starts_with("www.") {
            labels.push(format!(
                "caddy_{i}.redir={redirect_scheme}://{host_without_www}{{uri}}{code}"
            ));
        }
        if args.traffic_analytics {
            labels.push(format!("caddy_{i}.log.output=file /traffic/access.log"));
            labels.push(format!("caddy_{i}.log.output.roll_size=20MiB"));
            labels.push(format!("caddy_{i}.log.output.roll_keep=5"));
            labels.push(format!("caddy_{i}.log.output.roll_keep_for=168h"));
            labels.push(format!("caddy_{i}.log.format=json"));
            if args.supports_log_append {
                labels.push(format!("caddy_{i}.log_append=coolify_app_id {app_key}"));
            }
        }
    }
    sorted(labels)
}

/// `Server::caddySupportsLogAppend()` from the server's `proxy` column (JSON).
pub fn caddy_supports_log_append(proxy: &serde_json::Value) -> bool {
    use serde_json::Value;
    let get = |k: &str| proxy.get(k);
    // hasPendingProxyConfiguration()
    if get("status").and_then(Value::as_str) == Some("running") {
        let truthy = |v: Option<&Value>| match v {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => !s.is_empty() && s != "0",
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
            Some(_) => true,
        };
        if truthy(get("certificates_restart_required")) {
            return false;
        }
        let saved = get("last_saved_settings")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let applied = get("last_applied_settings")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        if let (Some(s), Some(a)) = (saved, applied)
            && s != a
        {
            return false;
        }
    }
    if get("type").and_then(Value::as_str) != Some("CADDY") {
        return false;
    }
    let config = get("last_saved_proxy_configuration")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Ok(parsed) = coolify_compat::yaml_parse::parse(config, 128) else {
        return false;
    };
    let Some(PhpValue::Str(image)) = php::data_get(&parsed, "services.caddy.image") else {
        return false;
    };
    static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?:^|/)caddy-docker-proxy:(\d+)\.(\d+)")
            .unwrap_or_else(|_| unreachable!())
    });
    VERSION.captures(image).is_some_and(|c| {
        let major: i64 = c[1].parse().unwrap_or(0);
        let minor: i64 = c[2].parse().unwrap_or(0);
        (major, minor) >= (2, 9)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(traefik_safe_service_name_segment("web"), "web-2567");
        assert_eq!(traefik_safe_service_name_segment("my-app"), "my-app-4dcb");
    }
}
