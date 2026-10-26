//! Client IP resolution and the instance API allowlist.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

/// Proxies whose `X-Forwarded-For` is trusted.
///
/// Upstream trusts every proxy (`TrustProxies::$proxies = '*'`), which lets
/// any client spoof the IP used by the API allowlist when the app is reachable
/// without a proxy. coolify-rs trusts nothing unless configured
/// (`COOLIFY_RS_TRUSTED_PROXIES`), and then walks the header right-to-left.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl TrustedProxies {
    /// Comma-separated IPs/CIDRs; `*` trusts all (upstream behaviour).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut v = Vec::new();
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if item == "*" {
                v.push(("0.0.0.0".parse().map_err(|_| "unreachable")?, 0));
                v.push(("::".parse().map_err(|_| "unreachable")?, 0));
                continue;
            }
            let (ip, mask) = match item.split_once('/') {
                Some((ip, m)) => (
                    ip,
                    m.parse::<u8>().map_err(|_| format!("bad mask in {item}"))?,
                ),
                None => (item, if item.contains(':') { 128 } else { 32 }),
            };
            let ip: IpAddr = ip.parse().map_err(|_| format!("bad address {item}"))?;
            v.push((ip, mask));
        }
        Ok(Self(v))
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|(net, mask)| in_cidr(ip, *net, *mask))
    }

    pub fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let peer_ip = peer.ip().to_canonical();
        if !self.trusts(peer_ip) {
            return peer_ip;
        }
        let chain: Vec<IpAddr> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|s| s.trim().parse::<IpAddr>().ok())
            .collect();
        chain
            .iter()
            .rev()
            .copied()
            .find(|ip| !self.trusts(*ip))
            .or_else(|| chain.first().copied())
            .unwrap_or(peer_ip)
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, mask: u8) -> bool {
    match (ip.to_canonical(), net) {
        (IpAddr::V4(a), IpAddr::V4(b)) if mask <= 32 => {
            let m = if mask == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(mask))
            };
            u32::from(a) & m == u32::from(b) & m
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if mask <= 128 => {
            let m = if mask == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(mask))
            };
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    }
}

/// Upstream `ApiIpAllowed` + `checkIPAgainstAllowlist` (`bootstrap/helpers/shared.php:2562`).
pub fn api_ip_allowed(allowed_ips: Option<&str>, client: IpAddr) -> bool {
    let raw = allowed_ips.unwrap_or("").trim();
    if raw.is_empty() || raw == "0.0.0.0" {
        return true;
    }
    let client = client.to_canonical();
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((subnet, mask)) = entry.split_once('/') {
            if subnet == "0.0.0.0" {
                return true;
            }
            // PHP `(int)` cast: non-numeric masks become 0.
            let mask: i64 = mask.trim().parse().unwrap_or(0);
            let Ok(net) = subnet.parse::<IpAddr>() else {
                continue;
            };
            let max = if net.is_ipv6() { 128 } else { 32 };
            if !(0..=max).contains(&mask) {
                continue;
            }
            if in_cidr(client, net, mask as u8) {
                return true;
            }
        } else if entry == "0.0.0.0" {
            return true;
        } else if entry
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical() == client)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn allowlist_matches_upstream_rules() {
        assert!(api_ip_allowed(None, ip("1.2.3.4")));
        assert!(api_ip_allowed(Some("  "), ip("1.2.3.4")));
        assert!(api_ip_allowed(Some("0.0.0.0"), ip("1.2.3.4")));
        assert!(api_ip_allowed(
            Some("10.0.0.0/8, 192.168.1.5"),
            ip("10.20.30.40")
        ));
        assert!(api_ip_allowed(
            Some("10.0.0.0/8, 192.168.1.5"),
            ip("192.168.1.5")
        ));
        assert!(!api_ip_allowed(
            Some("10.0.0.0/8, 192.168.1.5"),
            ip("192.168.1.6")
        ));
        assert!(api_ip_allowed(Some("0.0.0.0/32"), ip("8.8.8.8")));
        assert!(api_ip_allowed(Some("2001:db8::/32"), ip("2001:db8:1::1")));
        assert!(api_ip_allowed(Some("2001:DB8::1"), ip("2001:db8:0:0::1")));
        assert!(!api_ip_allowed(Some("10.0.0.0/33"), ip("10.0.0.1")));
        // IPv4-mapped IPv6 peers compare as IPv4.
        assert!(api_ip_allowed(Some("127.0.0.1"), ip("::ffff:127.0.0.1")));
    }

    #[test]
    fn forwarded_for_only_from_trusted_proxies() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            "6.6.6.6, 203.0.113.9"
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        );
        let peer: SocketAddr = "10.0.0.2:5555".parse().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            TrustedProxies::default().client_ip(peer, &h),
            ip("10.0.0.2")
        );
        let tp = TrustedProxies::parse("10.0.0.0/8").unwrap_or_default();
        // Rightmost untrusted hop, not the spoofable leftmost entry.
        assert_eq!(tp.client_ip(peer, &h), ip("203.0.113.9"));
        let all = TrustedProxies::parse("*").unwrap_or_default();
        assert_eq!(all.client_ip(peer, &h), ip("6.6.6.6"));
    }
}
