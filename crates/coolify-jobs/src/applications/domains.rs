//! Application domains: PHP `parse_url` for the URLs Coolify accepts,
//! `ValidationPatterns::*ApplicationDomain*`, `isValidDomainUrl`,
//! `DomainPortOverrides::normalize` and `generateUrl` / `sslip`.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

/// `parse_url` of an absolute URL: `scheme://[user[:pass]@]host[:port][path][?query][#fragment]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlParts {
    pub scheme: String,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub host: String,
    pub port: Option<String>,
    pub path: Option<String>,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

impl UrlParts {
    pub fn parse(url: &str) -> Option<UrlParts> {
        let (scheme, rest) = url.split_once("://")?;
        if scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            return None;
        }
        let mut out = UrlParts {
            scheme: scheme.to_owned(),
            ..Default::default()
        };
        let (rest, fragment) = match rest.split_once('#') {
            Some((a, f)) => (a, Some(f.to_owned())),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q.to_owned())),
            None => (rest, None),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], Some(rest[i..].to_owned())),
            None => (rest, None),
        };
        let host_port = match authority.rsplit_once('@') {
            Some((userinfo, hp)) => {
                match userinfo.split_once(':') {
                    Some((u, p)) => {
                        out.user = Some(u.to_owned());
                        out.pass = Some(p.to_owned());
                    }
                    None => out.user = Some(userinfo.to_owned()),
                }
                hp
            }
            None => authority,
        };
        if let Some(stripped) = host_port.strip_prefix('[') {
            let (h, after) = stripped.split_once(']')?;
            out.host = format!("[{h}]");
            if let Some(p) = after.strip_prefix(':') {
                out.port = Some(p.to_owned());
            }
        } else if let Some((h, p)) = host_port.rsplit_once(':') {
            out.host = h.to_owned();
            out.port = Some(p.to_owned());
        } else {
            out.host = host_port.to_owned();
        }
        if let Some(p) = &out.port {
            // parse_url fails on a non-numeric port and drops an empty one.
            if p.is_empty() {
                out.port = None;
            } else if !p.chars().all(|c| c.is_ascii_digit()) || p.len() > 5 {
                return None;
            }
        }
        out.path = path;
        out.query = query;
        out.fragment = fragment;
        Some(out)
    }

    /// Spatie `Url::getPath()`: `/` when the URL has no path.
    pub fn path_or_root(&self) -> String {
        match self.path.as_deref() {
            None | Some("") => "/".into(),
            Some(p) if !p.starts_with('/') => format!("/{p}"),
            Some(p) => p.to_owned(),
        }
    }

    /// The numeric port (`PHP_URL_PORT`).
    pub fn port_number(&self) -> Option<i64> {
        self.port.as_deref().and_then(|p| p.parse().ok())
    }
}

/// `DomainUrlParts::split` + `compose` without the port.
pub fn without_port(url: &str) -> String {
    let Some(p) = UrlParts::parse(url).filter(|p| !p.host.is_empty()) else {
        return compose("https", "", "");
    };
    let mut path = p.path.clone().unwrap_or_default();
    if let Some(q) = &p.query {
        path.push('?');
        path.push_str(q);
    }
    if let Some(f) = &p.fragment {
        path.push('#');
        path.push_str(f);
    }
    let scheme = if ["http", "https"].contains(&p.scheme.to_lowercase().as_str()) {
        p.scheme.to_lowercase()
    } else {
        "https".into()
    };
    compose(&scheme, &p.host, &path)
}

/// `DomainUrlParts::compose($scheme, $host, path: $path)`.
fn compose(scheme: &str, host: &str, path: &str) -> String {
    let scheme = if scheme.trim().eq_ignore_ascii_case("http") {
        "http"
    } else {
        "https"
    };
    let mut path = path.trim().to_owned();
    if !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('?')
        && !path.starts_with('#')
    {
        path = format!("/{path}");
    }
    format!("{scheme}://{}{path}", host.trim())
}

/// `ValidationPatterns::normalizeApplicationDomainUrl`.
pub fn normalize_url(url: &str) -> String {
    let Some(p) = UrlParts::parse(url) else {
        return url.to_owned();
    };
    let mut out = format!("{}://", p.scheme.to_lowercase());
    if let Some(u) = &p.user {
        out.push_str(u);
        if let Some(pw) = &p.pass {
            out.push(':');
            out.push_str(pw);
        }
        out.push('@');
    }
    out.push_str(&p.host.to_lowercase());
    if let Some(port) = &p.port {
        out.push(':');
        // parse_url returns the port as an int.
        out.push_str(
            &port
                .parse::<u32>()
                .map_or_else(|_| port.clone(), |n| n.to_string()),
        );
    }
    if let Some(path) = &p.path {
        out.push_str(path);
    }
    if let Some(q) = &p.query {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = &p.fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// `ValidationPatterns::applicationDomainList`.
pub fn domain_list(value: Option<&str>) -> Vec<String> {
    let Some(v) = value.filter(|v| !v.trim().is_empty()) else {
        return Vec::new();
    };
    let v = v.strip_prefix(',').unwrap_or(v);
    let v = v.strip_suffix(',').unwrap_or(v);
    v.trim()
        .split(',')
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .collect()
}

/// `ValidationPatterns::normalizeApplicationDomains`.
pub fn normalize_domains(value: Option<&str>) -> Option<String> {
    let list = domain_list(value);
    if list.is_empty() {
        return None;
    }
    Some(
        list.iter()
            .map(|u| normalize_url(u))
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.trim_end_matches('.').split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

/// `isValidDomainUrl`: http(s) with a host that `FILTER_VALIDATE_URL` accepts (`_` allowed).
pub fn is_valid_domain_url(url: &str) -> bool {
    let Some(p) = UrlParts::parse(url) else {
        return false;
    };
    if !["http", "https"].contains(&p.scheme.to_lowercase().as_str()) || p.host.is_empty() {
        return false;
    }
    let host = p.host.replace('_', "-");
    let host_ok = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        inner.parse::<Ipv6Addr>().is_ok()
    } else {
        valid_hostname(&host)
    };
    let path_ok = p
        .path
        .as_deref()
        .is_none_or(|path| !path.chars().any(|c| c.is_whitespace() || c.is_control()));
    host_ok && path_ok
}

/// `ValidationPatterns::validateApplicationDomains`.
pub fn validate_domains(value: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for url in domain_list(Some(value)) {
        if url.chars().any(|c| "`$;&|<>()\\\r\n".contains(c)) || !is_valid_domain_url(&url) {
            errors.push(format!("Invalid URL: {url}"));
            continue;
        }
        let Some(p) = UrlParts::parse(&url) else {
            errors.push(format!("Invalid URL: {url}"));
            continue;
        };
        if let Some(port) = p.port_number()
            && !(1..=65535).contains(&port)
        {
            errors.push(format!(
                "Invalid port for URL: {url}. The port must be between 1 and 65535."
            ));
            continue;
        }
        let unwrapped = p.host.trim_matches(|c| c == '[' || c == ']');
        if !unwrapped.contains('.') && unwrapped.parse::<IpAddr>().is_err() {
            errors.push(format!(
                "Invalid URL: {url}. The hostname must be a fully qualified domain name."
            ));
        }
    }
    errors
}

/// `DomainPortOverrides::normalize`: ports move from the domains to the overrides.
pub fn normalize_port_overrides(
    fqdn: Option<&str>,
    existing: &BTreeMap<String, i64>,
) -> (Option<String>, Option<BTreeMap<String, i64>>) {
    let Some(fqdn) = fqdn.filter(|f| !f.trim().is_empty()) else {
        return (None, None);
    };
    // keyBy('domain')->values(): the last one wins, at the position of the first.
    let mut domains: Vec<(String, Option<i64>)> = Vec::new();
    for d in fqdn
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .filter(|d| is_valid_domain_url(d))
    {
        let portless = without_port(d);
        let port = UrlParts::parse(d)
            .and_then(|p| p.port_number())
            .or_else(|| existing.get(&portless).copied());
        match domains.iter_mut().find(|(k, _)| *k == portless) {
            Some(slot) => slot.1 = port,
            None => domains.push((portless, port)),
        }
    }
    let effective: BTreeMap<String, i64> = domains
        .iter()
        .filter_map(|(d, p)| p.map(|p| (d.clone(), p)))
        .collect();
    let mut overrides: Vec<(String, i64)> = Vec::new();
    for (d, p) in &domains {
        let port = p.or_else(|| www_counterpart(d).and_then(|c| effective.get(&c).copied()));
        if let Some(port) = port {
            overrides.push((d.clone(), port));
        }
    }
    let joined = domains
        .iter()
        .map(|(d, _)| d.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let overrides: BTreeMap<String, i64> = overrides.into_iter().collect();
    (
        (!joined.is_empty()).then_some(joined),
        (!overrides.is_empty()).then_some(overrides),
    )
}

/// The JSON object of `domain_port_overrides`, in insertion (domain) order.
pub fn overrides_json(fqdn: Option<&str>, overrides: &BTreeMap<String, i64>) -> String {
    let mut m = serde_json::Map::new();
    for d in fqdn.unwrap_or_default().split(',') {
        if let Some(p) = overrides.get(d) {
            m.insert(d.to_owned(), serde_json::Value::from(*p));
        }
    }
    coolify_compat::php_json::encode(&serde_json::Value::Object(m))
}

fn www_counterpart(url: &str) -> Option<String> {
    let p = UrlParts::parse(url)?;
    if p.host.is_empty() {
        return None;
    }
    let host = if p.host.to_lowercase().starts_with("www.") {
        p.host[4..].to_owned()
    } else {
        format!("www.{}", p.host)
    };
    let mut path = p.path.unwrap_or_default();
    if let Some(q) = p.query {
        path.push('?');
        path.push_str(&q);
    }
    if let Some(f) = p.fragment {
        path.push('#');
        path.push_str(&f);
    }
    Some(compose(&p.scheme, &host, &path))
}

/// `sslipHostLabel`.
pub fn sslip_label(ip: &str) -> String {
    let trimmed = ip.trim().trim_matches(|c| c == '[' || c == ']');
    let Ok(v6) = trimmed.parse::<Ipv6Addr>() else {
        return ip.trim().to_owned();
    };
    let mut s = v6.to_string();
    if s.starts_with("::") {
        s = format!("0{s}");
    }
    if s.ends_with("::") {
        s.push('0');
    }
    s.replace(':', "-")
}

/// `resolveHostToIp`: IPv4 first, else IPv6, else the host unchanged.
pub async fn resolve_host(host: &str) -> String {
    let bare = host.trim().trim_matches(|c| c == '[' || c == ']');
    if bare.parse::<IpAddr>().is_ok() {
        return host.to_owned();
    }
    let Ok(addrs) = tokio::net::lookup_host((bare, 0)).await else {
        return host.to_owned();
    };
    let addrs: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map_or_else(|| host.to_owned(), IpAddr::to_string)
}

/// `generateUrl($server, $uuid)`: `{scheme}://{uuid}.{wildcard host}{path}`, or the sslip.io name.
pub async fn generate_url(
    wildcard_domain: Option<&str>,
    server_ip: &str,
    public_ip: Option<&str>,
    uuid: &str,
) -> String {
    let wildcard = match wildcard_domain.filter(|w| !w.is_empty()) {
        Some(w) => w.to_owned(),
        None => {
            let ip = if server_ip == "host.docker.internal" {
                public_ip.unwrap_or("localhost")
            } else {
                server_ip
            };
            format!("http://{}.sslip.io", sslip_label(&resolve_host(ip).await))
        }
    };
    let Some(p) = UrlParts::parse(&wildcard) else {
        return format!("http://{uuid}.{wildcard}");
    };
    let path = p.path_or_root();
    let path = if path == "/" { String::new() } else { path };
    format!("{}://{uuid}.{}{path}", p.scheme, p.host)
}

/// The last step of `domainConflictKey($domain)` (v4.7): the URL port is the container port, and
/// the public address does not include it. `domain` has no scheme anymore.
pub fn conflict_key_without_port(domain: &str) -> String {
    static PORT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^([^/:\[]+|\[[^\]]+\]):\d+").unwrap_or_else(|_| unreachable!())
    });
    PORT.replace(domain, "${1}").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `domainConflictKey()`: `https://app.example.com:3000` and `https://app.example.com` are
    /// the same public address.
    #[test]
    fn conflict_keys_ignore_the_container_port() {
        assert_eq!(
            conflict_key_without_port("app.example.com:3000"),
            "app.example.com"
        );
        assert_eq!(
            conflict_key_without_port("app.example.com:3000/api"),
            "app.example.com/api"
        );
        assert_eq!(
            conflict_key_without_port("[2001:db8::1]:8080/x"),
            "[2001:db8::1]/x"
        );
        assert_eq!(
            conflict_key_without_port("app.example.com/a:1"),
            "app.example.com/a:1"
        );
        assert_eq!(
            conflict_key_without_port("app.example.com"),
            "app.example.com"
        );
    }

    #[test]
    fn overrides() {
        let (fqdn, ov) = normalize_port_overrides(
            Some("https://app.example.com:3000, http://b.example.com"),
            &BTreeMap::new(),
        );
        assert_eq!(
            fqdn.as_deref(),
            Some("https://app.example.com,http://b.example.com")
        );
        assert_eq!(ov.unwrap().get("https://app.example.com"), Some(&3000));
        assert_eq!(
            without_port("HTTPS://A.example.com:80/x?y"),
            "https://A.example.com/x?y"
        );
        assert_eq!(
            normalize_domains(Some(",HTTP://A.Example.com/Path , ")).as_deref(),
            Some("http://a.example.com/Path")
        );
        assert_eq!(sslip_label("2001:db8::"), "2001-db8--0");
        assert!(validate_domains("http://localhost")[0].contains("fully qualified"));
        assert_eq!(
            validate_domains("ftp://a.b"),
            vec!["Invalid URL: ftp://a.b".to_owned()]
        );
    }
}
