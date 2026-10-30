//! `App\Rules\SafeWebhookUrl` (`SafeExternalUrl`) without the custom DNS servers.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use axum::http::HeaderMap;

use crate::response::ApiError;
use crate::state::AppState;

fn in_v4(ip: Ipv4Addr, cidr: (&str, u8)) -> bool {
    let Ok(net) = cidr.0.parse::<Ipv4Addr>() else {
        return false;
    };
    let mask = if cidr.1 == 0 {
        0
    } else {
        u32::MAX << (32 - cidr.1)
    };
    u32::from(ip) & mask == u32::from(net) & mask
}

fn in_v6(ip: Ipv6Addr, cidr: (&str, u8)) -> bool {
    let Ok(net) = cidr.0.parse::<Ipv6Addr>() else {
        return false;
    };
    let mask = if cidr.1 == 0 {
        0
    } else {
        u128::MAX << (128 - cidr.1)
    };
    u128::from(ip) & mask == u128::from(net) & mask
}

fn mapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn is_link_local(ip: IpAddr) -> bool {
    match mapped(ip) {
        IpAddr::V4(v) => in_v4(v, ("169.254.0.0", 16)),
        IpAddr::V6(v) => in_v6(v, ("fe80::", 10)),
    }
}

fn is_private(ip: IpAddr) -> bool {
    match mapped(ip) {
        IpAddr::V4(v) => {
            in_v4(v, ("10.0.0.0", 8))
                || in_v4(v, ("172.16.0.0", 12))
                || in_v4(v, ("192.168.0.0", 16))
        }
        IpAddr::V6(v) => in_v6(v, ("fc00::", 7)),
    }
}

fn is_special(ip: IpAddr) -> bool {
    match mapped(ip) {
        IpAddr::V4(v) => [
            ("0.0.0.0", 8),
            ("100.64.0.0", 10),
            ("127.0.0.0", 8),
            ("169.254.0.0", 16),
            ("192.0.0.0", 24),
            ("192.0.2.0", 24),
            ("198.18.0.0", 15),
            ("198.51.100.0", 24),
            ("203.0.113.0", 24),
            ("224.0.0.0", 4),
            ("240.0.0.0", 4),
            ("255.255.255.255", 32),
        ]
        .iter()
        .any(|c| in_v4(v, *c)),
        IpAddr::V6(v) => [
            ("::", 128),
            ("::1", 128),
            ("::ffff:0:0", 96),
            ("64:ff9b::", 96),
            ("64:ff9b:1::", 48),
            ("100::", 64),
            ("2001::", 23),
            ("2001:2::", 48),
            ("2001:db8::", 32),
            ("2002::", 16),
            ("fc00::", 7),
            ("fe80::", 10),
            ("ff00::", 8),
        ]
        .iter()
        .any(|c| in_v6(v, *c)),
    }
}

fn is_localhost(ip: IpAddr) -> bool {
    match mapped(ip) {
        IpAddr::V4(v) => in_v4(v, ("127.0.0.0", 8)),
        IpAddr::V6(v) => v == Ipv6Addr::LOCALHOST,
    }
}

struct Policy {
    entries: Vec<String>,
    allow_localhost: bool,
}

impl Policy {
    fn allowed_host(&self, host: &str) -> bool {
        self.entries.iter().any(|e| !e.contains('/') && e == host)
    }

    fn allowlisted_ip(&self, ip: IpAddr) -> bool {
        self.entries.iter().any(|e| {
            if let Some((net, prefix)) = e.split_once('/') {
                let Ok(p) = prefix.parse::<u8>() else {
                    return false;
                };
                match ip {
                    IpAddr::V4(v) => in_v4(v, (net, p)),
                    IpAddr::V6(v) => in_v6(v, (net, p)),
                }
            } else {
                e.parse::<IpAddr>().is_ok_and(|x| x == ip)
            }
        })
    }

    fn allowed_ip(&self, ip: IpAddr, host: &str) -> bool {
        let ip = mapped(ip);
        if is_link_local(ip) {
            return false;
        }
        if !is_private(ip) && !is_special(ip) {
            return true;
        }
        if is_localhost(ip) {
            return self.allow_localhost && (self.allowed_host(host) || self.allowlisted_ip(ip));
        }
        if is_private(ip) {
            return self.allowed_host(host) || self.allowlisted_ip(ip);
        }
        self.allowlisted_ip(ip)
    }
}

/// The URL of `settings.advanced`, from the request host (`route()` uses the current request).
fn settings_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}/settings/advanced#endpoint-section")
}

fn split_url(value: &str) -> Option<(String, String)> {
    let (scheme, rest) = value.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        format!("[{}]", stripped.split(']').next()?)
    } else {
        authority.split(':').next()?.to_string()
    };
    Some((scheme.to_lowercase(), host))
}

/// The failure message of the rule, or `None` when the URL is allowed.
pub async fn check(
    state: &AppState,
    attribute: &str,
    value: &str,
    headers: &HeaderMap,
) -> Result<Option<String>, ApiError> {
    let name = attribute.replace('_', " ");
    if !crate::laravel_url::is_url(value) {
        return Ok(Some(format!("The {name} must be a valid URL.")));
    }
    let Some((scheme, host)) = split_url(value) else {
        return Ok(Some(format!("The {name} must be a valid URL.")));
    };
    if scheme != "http" && scheme != "https" {
        return Ok(Some(format!(
            "The {name} must use the http or https scheme."
        )));
    }
    if host.is_empty() {
        return Ok(Some(format!("The {name} must contain a valid host.")));
    }
    if host.ends_with('.') {
        return Ok(Some(format!(
            "The {name} host must not end with a trailing dot."
        )));
    }
    let host = host.to_lowercase();
    let for_ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let for_dns = for_ip.trim_end_matches('.').to_string();
    let (raw, allow_localhost) = coolify_db::server_ops::webhook_policy(&state.db).await?;
    let entries: Vec<String> = raw
        .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        .unwrap_or_default()
        .iter()
        .map(|e| e.trim().trim_end_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let policy = Policy {
        entries,
        allow_localhost,
    };
    // `privateTargetMessage` uses the attribute name unchanged.
    let private = format!(
        "The {attribute} points to a local or private address that is not allowed. Configure allowed internal targets: {}",
        settings_url(headers)
    );
    let blocked_name = for_dns == "localhost"
        || for_dns.ends_with(".local")
        || for_dns.ends_with(".internal")
        || for_dns.ends_with(".cluster.local");
    if blocked_name && !policy.allowed_host(&for_dns) {
        return Ok(Some(private));
    }
    if let Ok(ip) = for_ip.parse::<IpAddr>() {
        if !policy.allowed_ip(ip, &for_dns) {
            return Ok(Some(if is_link_local(ip) {
                format!("The {name} must not point to link-local addresses.")
            } else {
                private
            }));
        }
        return Ok(None);
    }
    let resolved: Vec<IpAddr> = if for_dns == "localhost" {
        vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
    } else {
        tokio::net::lookup_host((for_dns.as_str(), 80))
            .await
            .map(|i| i.map(|a| a.ip()).collect())
            .unwrap_or_default()
    };
    if resolved.is_empty() {
        return Ok(Some(format!("The {name} host could not be resolved.")));
    }
    for ip in resolved {
        if !policy.allowed_ip(ip, &for_dns) {
            return Ok(Some(if is_link_local(ip) {
                format!("The {name} must not resolve to a link-local address.")
            } else {
                private
            }));
        }
    }
    Ok(None)
}
