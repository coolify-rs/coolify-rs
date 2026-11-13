//! The parts of `App\Rules\SafeWebhookUrl` for connections that are not HTTP:
//! `checkConnectionHost` (SMTP hosts), `pinnedAddress` (SFTP and FTPS endpoints) and the
//! `schemes` option of the rule.
//!
//! Upstream's `preferSystemDns` and `customDnsTimeout` choose between the system resolver and the
//! custom DNS servers of the instance. This port only asks the system resolver (see
//! `safe_url_pin`), so both options have nothing to change here.

use std::net::IpAddr;

use coolify_db::Db;

use super::{UrlPolicy, link_local, mapped_v4, parse_url, resolve};

/// `SafeWebhookUrl::DEFAULT_PORTS`.
pub const DEFAULT_PORTS: [(&str, u16); 4] =
    [("https", 443), ("http", 80), ("sftp", 22), ("ftps", 21)];

/// `DEFAULT_PORTS[$scheme]`.
pub fn default_port(scheme: &str) -> Option<u16> {
    DEFAULT_PORTS
        .iter()
        .find(|(s, _)| *s == scheme)
        .map(|(_, p)| *p)
}

/// The scheme message of `SafeWebhookUrl::validate` for a rule with `schemes`:
/// `'The :attribute must use the '.implode(' or ', $this->schemes).' scheme.'`.
pub fn scheme_error(attribute: &str, schemes: &[&str]) -> String {
    format!(
        "The {attribute} must use the {} scheme.",
        schemes.join(" or ")
    )
}

/// The `status` of `checkConnectionHost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStatus {
    Allowed,
    Blocked,
    LinkLocal,
    Unresolved,
}

/// What `checkConnectionHost` returns. `ips` has the checked addresses, or is empty when the
/// host is an IP address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHost {
    pub status: HostStatus,
    pub ips: Vec<IpAddr>,
}

impl ConnectionHost {
    fn of(status: HostStatus) -> Self {
        Self {
            status,
            ips: Vec::new(),
        }
    }
}

/// `isCloud()`, as `notifications::Ctx` reads it.
fn is_cloud() -> bool {
    std::env::var("SELF_HOSTED").is_ok_and(|v| v == "false" || v == "0")
}

/// `strtolower`, brackets off (`normalizeHostForIpCheck`), trailing dots off.
fn host_for_check(host: &str) -> String {
    let lower = host.to_lowercase();
    let unbracketed = lower
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&lower);
    unbracketed.trim_end_matches('.').to_owned()
}

impl UrlPolicy {
    /// `SafeWebhookUrl::forSmtpHost()`: SMTP hosts get the targets of git sources, so private
    /// networks are allowed on a self-hosted instance.
    pub async fn for_smtp_host(db: &Db) -> Self {
        let mut policy = Self::load(db).await;
        policy.allow_private = !is_cloud();
        policy
    }

    /// `SafeWebhookUrl::checkConnectionHost($host)`: resolves and checks a host for a connection
    /// that is not HTTP. The caller checks the host syntax first.
    pub async fn check_connection_host(&self, host: &str) -> ConnectionHost {
        let host = host_for_check(host);
        if self.blocked_host(&host) && !self.allowed_host(&host) {
            return ConnectionHost::of(HostStatus::Blocked);
        }
        let resolved = if host.parse::<IpAddr>().is_ok() {
            None
        } else {
            Some(resolve(&host, 25).await)
        };
        self.check_resolved_host(&host, resolved)
    }

    /// The decision of `checkConnectionHost` after DNS: `resolved` is `None` for an IP address.
    pub fn check_resolved_host(&self, host: &str, resolved: Option<Vec<IpAddr>>) -> ConnectionHost {
        let host = host_for_check(host);
        if self.blocked_host(&host) && !self.allowed_host(&host) {
            return ConnectionHost::of(HostStatus::Blocked);
        }
        let literal = host.parse::<IpAddr>().ok();
        let ips: Vec<IpAddr> = match (literal, resolved) {
            (Some(ip), _) => vec![ip],
            (None, Some(ips)) => ips,
            (None, None) => Vec::new(),
        };
        if ips.is_empty() {
            return ConnectionHost::of(HostStatus::Unresolved);
        }
        for ip in &ips {
            if !self.allowed_ip(*ip, &host) {
                return ConnectionHost::of(if link_local(mapped_v4(*ip)) {
                    HostStatus::LinkLocal
                } else {
                    HostStatus::Blocked
                });
            }
        }
        ConnectionHost {
            status: HostStatus::Allowed,
            ips: if literal.is_some() { Vec::new() } else { ips },
        }
    }

    /// `SafeWebhookUrl::pinnedAddress($url, $schemes)`: the address to connect to for an endpoint
    /// that is not HTTP (SFTP, FTPS): the validated IP of its host, so a DNS change after the
    /// check cannot send the connection to another address. `Err` is the exception message of
    /// `resolveUrlForRequest`.
    pub async fn pinned_address(&self, url: &str, schemes: &[&str]) -> Result<String, String> {
        let parsed = parse_url(url).ok_or("Webhook URL is invalid.")?;
        if parsed.host.ends_with('.') {
            return Err("Webhook URL host must not end with a trailing dot.".to_owned());
        }
        if !schemes.contains(&parsed.scheme.as_str()) {
            return Err("Webhook URL scheme is unsafe.".to_owned());
        }
        let host = host_for_check(&parsed.host);
        if let Ok(ip) = host.parse::<IpAddr>() {
            if !self.allowed_ip(ip, &host) {
                return Err("Webhook URL resolved to an unsafe IP address.".to_owned());
            }
            return Ok(host);
        }
        let port = default_port(&parsed.scheme).unwrap_or(parsed.port);
        let ips = resolve(&host, port).await;
        if ips.is_empty() {
            return Err("Webhook URL host could not be resolved.".to_owned());
        }
        if ips.iter().any(|ip| !self.allowed_ip(*ip, &host)) {
            return Err("Webhook URL resolved to an unsafe IP address.".to_owned());
        }
        if self.blocked_host(&host) && !self.allowed_host(&host) {
            return Err("Webhook URL host is unsafe.".to_owned());
        }
        Ok(preferred_ip(&ips).to_string())
    }
}

/// `SafeWebhookUrl::preferredIp`: the first IPv4 address, else the first address.
pub fn preferred_ip(ips: &[IpAddr]) -> IpAddr {
    ips.iter()
        .copied()
        .find(IpAddr::is_ipv4)
        .unwrap_or_else(|| ips[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn self_hosted() -> UrlPolicy {
        UrlPolicy {
            allowed: Vec::new(),
            allow_localhost: false,
            allow_private: true,
        }
    }

    #[test]
    fn public_hosts_are_allowed_with_their_addresses() {
        let p = UrlPolicy::default();
        let r = p.check_resolved_host("SMTP.Example.com.", Some(vec![ip("93.184.216.34")]));
        assert_eq!(r.status, HostStatus::Allowed);
        assert_eq!(r.ips, vec![ip("93.184.216.34")]);
    }

    #[test]
    fn an_ip_host_is_checked_and_returns_no_addresses() {
        let p = UrlPolicy::default();
        let r = p.check_resolved_host("[2606:4700::1111]", None);
        assert_eq!(r.status, HostStatus::Allowed);
        assert!(r.ips.is_empty());
        assert_eq!(
            p.check_resolved_host("10.0.0.5", None).status,
            HostStatus::Blocked
        );
    }

    #[test]
    fn private_addresses_follow_the_git_source_policy() {
        // Coolify Cloud: blocked. Self-hosted: allowed.
        let cloud = UrlPolicy::default();
        assert_eq!(
            cloud
                .check_resolved_host("mail.corp.example", Some(vec![ip("10.0.0.5")]))
                .status,
            HostStatus::Blocked
        );
        assert_eq!(
            cloud
                .check_resolved_host("mail.internal", Some(vec![ip("93.184.216.34")]))
                .status,
            HostStatus::Blocked
        );
        let own = self_hosted();
        assert_eq!(
            own.check_resolved_host("mail.corp.example", Some(vec![ip("10.0.0.5")]))
                .status,
            HostStatus::Allowed
        );
        assert_eq!(
            own.check_resolved_host("mail_server", Some(vec![ip("172.18.0.4")]))
                .status,
            HostStatus::Allowed
        );
    }

    #[test]
    fn loopback_and_link_local_stay_blocked() {
        let own = self_hosted();
        assert_eq!(
            own.check_resolved_host("localhost", Some(vec![ip("127.0.0.1")]))
                .status,
            HostStatus::Blocked
        );
        assert_eq!(
            own.check_resolved_host("127.0.0.1", None).status,
            HostStatus::Blocked
        );
        assert_eq!(
            own.check_resolved_host("metadata.example", Some(vec![ip("169.254.169.254")]))
                .status,
            HostStatus::LinkLocal
        );
        assert_eq!(
            own.check_resolved_host("169.254.169.254", None).status,
            HostStatus::LinkLocal
        );
    }

    #[test]
    fn one_blocked_address_blocks_the_host() {
        let p = UrlPolicy::default();
        let r = p.check_resolved_host(
            "mixed.example",
            Some(vec![ip("93.184.216.34"), ip("192.168.1.9")]),
        );
        assert_eq!(r.status, HostStatus::Blocked);
        assert!(r.ips.is_empty());
    }

    #[test]
    fn a_host_without_addresses_is_unresolved() {
        let p = UrlPolicy::default();
        assert_eq!(
            p.check_resolved_host("nowhere.example", Some(Vec::new()))
                .status,
            HostStatus::Unresolved
        );
    }

    #[test]
    fn allowlisted_loopback_host() {
        let p = UrlPolicy {
            allowed: vec!["localhost".into()],
            allow_localhost: true,
            allow_private: false,
        };
        assert_eq!(
            p.check_resolved_host("localhost", Some(vec![ip("127.0.0.1")]))
                .status,
            HostStatus::Allowed
        );
    }

    #[test]
    fn scheme_messages_and_ports() {
        assert_eq!(
            scheme_error("endpoint", &["sftp"]),
            "The endpoint must use the sftp scheme."
        );
        assert_eq!(
            scheme_error("webhook url", &["http", "https"]),
            "The webhook url must use the http or https scheme."
        );
        assert_eq!(default_port("sftp"), Some(22));
        assert_eq!(default_port("ftps"), Some(21));
        assert_eq!(default_port("gopher"), None);
    }

    #[test]
    fn the_preferred_address_is_ipv4() {
        assert_eq!(
            preferred_ip(&[ip("2606:4700::1111"), ip("1.1.1.1")]),
            ip("1.1.1.1")
        );
        assert_eq!(
            preferred_ip(&[ip("2606:4700::1111")]),
            ip("2606:4700::1111")
        );
    }

    #[tokio::test]
    async fn pinned_address_checks_the_scheme_and_ip_hosts() {
        let p = UrlPolicy::default();
        assert_eq!(
            p.pinned_address("sftp://203.0.114.7:2222/backups", &["sftp"])
                .await
                .as_deref(),
            Ok("203.0.114.7")
        );
        assert_eq!(
            p.pinned_address("http://203.0.114.7/", &["sftp", "ftps"])
                .await
                .unwrap_err(),
            "Webhook URL scheme is unsafe."
        );
        assert_eq!(
            p.pinned_address("sftp://10.0.0.9/backups", &["sftp"])
                .await
                .unwrap_err(),
            "Webhook URL resolved to an unsafe IP address."
        );
    }
}
