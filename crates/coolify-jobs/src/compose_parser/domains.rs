//! Domains of Compose services (bootstrap/helpers/domains.php and shared.php): the
//! `docker_compose_domains` map helpers, `generateUrl()` / `generateFqdn()` and the
//! `getFqdnWithoutPort()` / `getHostWithoutPort()` value builders.

use coolify_compat::yaml::{Key, PhpValue};

use super::php::{self, Arr, SpatieUrl};

/// `normalizeComposeServiceName($serviceName)`.
pub fn normalize_compose_service_name(name: &str) -> String {
    name.replace(['-', '.'], "_")
}

/// `findComposeServiceName($candidate, $serviceNames)`.
pub fn find_compose_service_name(candidate: &str, names: &[String]) -> Option<String> {
    if names.iter().any(|n| n == candidate) {
        return Some(candidate.to_owned());
    }
    let normalized = normalize_compose_service_name(candidate);
    let matches: Vec<&String> = names
        .iter()
        .filter(|n| normalize_compose_service_name(n) == normalized)
        .collect();
    (matches.len() == 1).then(|| matches[0].clone())
}

/// `composeDomainEntryString($entry)`.
pub fn compose_domain_entry_string(entry: &PhpValue) -> Option<String> {
    match entry {
        PhpValue::Str(s) => Some(s.clone()),
        PhpValue::Array(_) => match php::get(entry, "domain") {
            Some(PhpValue::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `getComposeServiceDomainString($domains, $serviceName)`.
pub fn get_compose_service_domain_string(
    domains: &[(Key, PhpValue)],
    service_name: &str,
) -> Option<String> {
    let normalized = normalize_compose_service_name(service_name);
    let mut matches: Vec<(Option<String>, bool)> = Vec::new();
    for (key, entry) in domains {
        let key = key.as_string();
        if key != service_name
            && key != normalized
            && normalize_compose_service_name(&key) != normalized
        {
            continue;
        }
        matches.push((compose_domain_entry_string(entry), key == service_name));
    }
    if matches.is_empty() {
        return None;
    }
    let filled: Vec<&(Option<String>, bool)> = matches
        .iter()
        .filter(|(d, _)| php::filled_str(d.as_deref()))
        .collect();
    if !filled.is_empty() {
        if let Some((d, _)) = filled.iter().find(|(_, requested)| *requested) {
            return d.clone();
        }
        return filled[0].0.clone();
    }
    if let Some((d, _)) = matches.iter().find(|(_, requested)| *requested) {
        return d.clone();
    }
    matches[0].0.clone()
}

/// `hasComposeServiceDomainEntry($domains, $serviceName)`.
pub fn has_compose_service_domain_entry(domains: &[(Key, PhpValue)], service_name: &str) -> bool {
    let normalized = normalize_compose_service_name(service_name);
    domains.iter().any(|(k, _)| {
        let k = k.as_string();
        k == service_name || normalize_compose_service_name(&k) == normalized
    })
}

/// `putComposeServiceDomain($domains, $serviceName, $domainString, $serviceNames)`.
pub fn put_compose_service_domain(
    domains: &[(Key, PhpValue)],
    service_name: &str,
    domain: Option<&str>,
    names: &[String],
) -> Arr {
    let mut domains: Arr = domains.to_vec();
    let storage_key =
        find_compose_service_name(service_name, names).unwrap_or_else(|| service_name.to_owned());
    let mut merged: Arr = vec![(
        Key::Str("domain".into()),
        domain.map_or(PhpValue::Null, |d| PhpValue::Str(d.to_owned())),
    )];
    let keys: Vec<Key> = domains.iter().map(|(k, _)| k.clone()).collect();
    for key in keys {
        let ks = key.as_string();
        if ks != storage_key
            && find_compose_service_name(&ks, names).as_deref() != Some(storage_key.as_str())
        {
            continue;
        }
        if let Some(PhpValue::Array(existing)) = php::get_key(&domains, &key).cloned() {
            merged = php::merge(&existing, &merged);
        }
        if ks != storage_key {
            domains.retain(|(k, _)| *k != key);
        }
    }
    php::put_str(&mut domains, &storage_key, PhpValue::Array(merged));
    domains
}

/// `preferComposeDomainValue()`.
fn prefer_compose_domain_value(
    existing: &PhpValue,
    existing_is_canonical: bool,
    incoming: &PhpValue,
    incoming_is_canonical: bool,
) -> PhpValue {
    let ef = php::filled(existing);
    let inf = php::filled(incoming);
    if ef && !inf {
        return existing.clone();
    }
    if inf && !ef {
        return incoming.clone();
    }
    if ef && inf {
        if incoming_is_canonical && !existing_is_canonical {
            return incoming.clone();
        }
        return existing.clone();
    }
    if incoming_is_canonical {
        incoming.clone()
    } else {
        existing.clone()
    }
}

/// `rekeyComposeDomainsToServiceNames($domains, $serviceNames)`.
pub fn rekey_compose_domains_to_service_names(
    domains: &[(Key, PhpValue)],
    names: &[String],
) -> Arr {
    let mut rekeyed: Arr = Vec::new();
    let mut canonical: Vec<(String, bool)> = Vec::new();
    for (key, value) in domains {
        let key = key.as_string();
        let original = find_compose_service_name(&key, names).unwrap_or_else(|| key.clone());
        let is_canonical = key == original;
        let value: Arr = match value {
            PhpValue::Array(items) => items.clone(),
            other => vec![(Key::Str("domain".into()), other.clone())],
        };
        let okey = Key::from_php_string(&original);
        let Some(PhpValue::Array(current)) = php::get_key(&rekeyed, &okey).cloned() else {
            php::put(&mut rekeyed, okey, PhpValue::Array(value));
            canonical.retain(|(k, _)| *k != original);
            canonical.push((original, is_canonical));
            continue;
        };
        let existing_domain = php::get_key(&current, &Key::Str("domain".into()))
            .cloned()
            .unwrap_or(PhpValue::Null);
        let incoming_domain = php::get_key(&value, &Key::Str("domain".into()))
            .cloned()
            .unwrap_or(PhpValue::Null);
        let existing_canonical = canonical
            .iter()
            .find(|(k, _)| *k == original)
            .is_some_and(|(_, c)| *c);
        let mut merged = php::merge(&current, &value);
        php::put_str(
            &mut merged,
            "domain",
            prefer_compose_domain_value(
                &existing_domain,
                existing_canonical,
                &incoming_domain,
                is_canonical,
            ),
        );
        php::put(&mut rekeyed, okey, PhpValue::Array(merged));
        for (k, c) in &mut canonical {
            if *k == original {
                *c = *c || is_canonical;
            }
        }
    }
    rekeyed
}

/// `parseDomainUrlParts($fqdn)`: `(scheme, host, path)` with a bare `/` path dropped.
pub fn parse_domain_url_parts(fqdn: &str) -> Option<(String, String, String)> {
    let url = SpatieUrl::from_string(fqdn).ok()?;
    if url.host.is_empty() {
        return None;
    }
    let path = if url.path.is_empty() || url.path == "/" {
        String::new()
    } else {
        url.path.clone()
    };
    Some((url.scheme, url.host, path))
}

/// `getFqdnWithoutPort($fqdn)`.
pub fn get_fqdn_without_port(fqdn: &str) -> String {
    match parse_domain_url_parts(fqdn) {
        Some((scheme, host, path)) if !scheme.is_empty() => format!("{scheme}://{host}{path}"),
        _ => fqdn.to_owned(),
    }
}

/// `getHostWithoutPort($fqdn)`.
pub fn get_host_without_port(fqdn: &str) -> String {
    match parse_domain_url_parts(fqdn) {
        Some((_, host, path)) => format!("{host}{path}"),
        None => fqdn.to_owned(),
    }
}

/// What `generateUrl()` / `generateFqdn()` read from the server: its wildcard domain, or the
/// sslip.io name of its IP (`sslip($server)`, resolved once).
#[derive(Debug, Clone)]
pub struct UrlBase {
    /// `settings.wildcard_domain`, or `http://{ip}.sslip.io`.
    pub wildcard: String,
}

impl UrlBase {
    /// `sslip($server)` unless the server has a wildcard domain.
    pub async fn for_server(
        wildcard_domain: Option<&str>,
        server_ip: &str,
        public_ip: Option<&str>,
    ) -> Self {
        let wildcard = match wildcard_domain.filter(|w| !w.is_empty()) {
            Some(w) => w.to_owned(),
            None => {
                let ip = if server_ip == "host.docker.internal" {
                    public_ip.unwrap_or("localhost")
                } else {
                    server_ip
                };
                let resolved = crate::applications::domains::resolve_host(ip).await;
                format!(
                    "http://{}.sslip.io",
                    crate::applications::domains::sslip_label(&resolved)
                )
            }
        };
        Self { wildcard }
    }

    fn parts(&self) -> (String, String, String) {
        match SpatieUrl::from_string(&self.wildcard) {
            Ok(u) => {
                let path = if u.path == "/" { String::new() } else { u.path };
                (u.scheme, u.host, path)
            }
            Err(_) => (String::new(), String::new(), String::new()),
        }
    }

    /// `generateUrl($server, $random)`.
    pub fn generate_url(&self, random: &str) -> String {
        let (scheme, host, path) = self.parts();
        format!("{scheme}://{random}.{host}{path}")
    }

    /// `generateFqdn($server, $random, parserVersion: $version)` with the instance version.
    pub fn generate_fqdn(
        &self,
        random: &str,
        parser_version: i64,
        coolify_version: &str,
    ) -> String {
        let (scheme, host, path) = self.parts();
        if parser_version >= 5 && php::version_compare(coolify_version, "4.0.0-beta.420.7") >= 0 {
            return format!("{random}.{host}{path}");
        }
        format!("{scheme}://{random}.{host}{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_map() {
        let names = vec!["my-app".to_owned(), "web".to_owned()];
        assert_eq!(
            find_compose_service_name("my_app", &names).as_deref(),
            Some("my-app")
        );
        let domains = put_compose_service_domain(&[], "my_app", Some("http://x"), &names);
        assert_eq!(
            php::json_encode(&PhpValue::Array(domains)),
            r#"{"my-app":{"domain":"http:\/\/x"}}"#
        );
        assert_eq!(get_fqdn_without_port("http://a.b:3000/x"), "http://a.b/x");
        assert_eq!(get_host_without_port("http://a.b:3000"), "a.b");
    }
}
