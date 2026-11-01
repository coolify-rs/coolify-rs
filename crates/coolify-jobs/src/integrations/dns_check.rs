//! `CheckDomainDns`, `DnsRecordHints::forHostnames` and `toCopyText`, `queryDnsServer`.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::net::UdpSocket;

use super::cloudflare::same_address;
use super::provider_urls::fake_origin;

/// One `CheckDomainDns` result.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub status: &'static str,
    pub message: String,
    pub expected_ip: Option<String>,
    pub checked_at: String,
}

impl CheckResult {
    /// The stored `domain_dns_statuses` entry.
    pub fn stored(&self) -> Value {
        json!({
            "status": self.status,
            "message": self.message,
            "expected_ip": self.expected_ip,
            "checked_at": self.checked_at,
        })
    }
}

fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

fn result(status: &'static str, message: String, expected: Option<&str>) -> CheckResult {
    CheckResult {
        status,
        message,
        expected_ip: expected.map(str::to_owned),
        checked_at: iso_now(),
    }
}

/// `dnsRecordTypeForIp`.
fn record_type_for(expected: Option<&str>) -> &'static str {
    let candidate = expected
        .map(str::trim)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or_default()
        .trim_matches(['[', ']']);
    if matches!(candidate.parse::<IpAddr>(), Ok(IpAddr::V6(_))) {
        "AAAA"
    } else {
        "A"
    }
}

/// `dnsMismatchGuidanceMessage($expectedIp, $expectedIp)`.
fn mismatch_message(expected: Option<&str>) -> String {
    let address = expected
        .map(str::trim)
        .filter(|a| a.trim_matches(['[', ']']).parse::<IpAddr>().is_ok());
    match address {
        None => "DNS validation failed. Check your DNS records.".to_owned(),
        Some(a) => format!(
            "Required DNS record type {} pointing to {a}",
            record_type_for(Some(a))
        ),
    }
}

fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// A DNS question for `host`, type A (1) or AAAA (28).
fn build_query(host: &str, qtype: u16, id: u16) -> Option<Vec<u8>> {
    let mut packet = Vec::new();
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&qtype.to_be_bytes());
    packet.extend_from_slice(&[0, 1]);
    Some(packet)
}

fn skip_name(data: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *data.get(at)? as usize;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        at += len + 1;
    }
}

/// The addresses of the wanted type in an answer.
fn parse_answers(data: &[u8], id: u16, qtype: u16) -> Option<Vec<IpAddr>> {
    if data.len() < 12 || u16::from_be_bytes([data[0], data[1]]) != id {
        return None;
    }
    if data[3] & 0x0F != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([data[4], data[5]]);
    let answers = u16::from_be_bytes([data[6], data[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(data, at)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..answers {
        at = skip_name(data, at)?;
        let kind = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]);
        let len = u16::from_be_bytes([*data.get(at + 8)?, *data.get(at + 9)?]) as usize;
        at += 10;
        let rdata = data.get(at..at + len)?;
        if kind == qtype {
            match (qtype, rdata.len()) {
                (1, 4) => out.push(IpAddr::from([rdata[0], rdata[1], rdata[2], rdata[3]])),
                (28, 16) => {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(rdata);
                    out.push(IpAddr::from(b));
                }
                _ => {}
            }
        }
        at += len;
    }
    Some(out)
}

/// `queryDnsServer`: `None` when the server does not answer.
async fn query_server(
    server: &str,
    host: &str,
    record: &str,
    timeout: Duration,
) -> Option<Vec<IpAddr>> {
    let qtype = if record == "AAAA" { 28 } else { 1 };
    let id = (std::process::id() as u16).wrapping_mul(31);
    let packet = build_query(host, qtype, id)?;
    let target: SocketAddr = server.parse::<SocketAddr>().ok().or_else(|| {
        server
            .parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, 53))
    })?;
    let bind = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.send_to(&packet, target).await.ok()?;
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(timeout, socket.recv(&mut buf))
        .await
        .ok()?
        .ok()?;
    parse_answers(&buf[..n], id, qtype)
}

/// What `CheckDomainDns` reads of the instance and the server.
#[derive(Debug, Clone, Default)]
pub struct CheckContext {
    pub validation_enabled: bool,
    /// `custom_dns_servers`, split and trimmed.
    pub dns_servers: Vec<String>,
    pub has_server: bool,
    pub server_ip: Option<String>,
    pub skip_multiple_servers: bool,
}

fn success_message(ctx: &CheckContext, expected: Option<&str>) -> String {
    if let (Some(e), Some(ip)) = (
        expected.filter(|e| !e.trim().is_empty()),
        ctx.server_ip.as_deref(),
    ) && !ip.trim().is_empty()
        && ip != e
        && ip.parse::<IpAddr>().is_err()
    {
        return format!("DNS points to {e} ({ip}) (or Cloudflare).");
    }
    match expected {
        Some(e) if !e.is_empty() => format!("DNS points to {e} (or Cloudflare)."),
        _ => "DNS looks correct.".to_owned(),
    }
}

/// `CheckDomainDns::handle` for one URL. `is_cloudflare` is `isCloudflareIp()`.
pub async fn check(
    url: &str,
    ctx: &CheckContext,
    expected: Option<&str>,
    is_cloudflare: fn(IpAddr) -> bool,
) -> CheckResult {
    if !ctx.validation_enabled {
        return result(
            "skipped",
            "DNS validation is disabled in instance settings.".into(),
            expected,
        );
    }
    if !ctx.has_server {
        return result(
            "skipped",
            "No server available for DNS validation.".into(),
            None,
        );
    }
    if ctx.skip_multiple_servers {
        return result(
            "skipped",
            "DNS check skipped for multi-server applications.".into(),
            expected,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let Some(host) = host_of(url) else {
        return result(
            "failed",
            "Could not validate DNS for this domain.".into(),
            expected,
        );
    };
    if host.contains("sslip.io") {
        return result("ok", "DNS looks correct.".into(), expected);
    }
    let record = record_type_for(expected);
    let matches = |ip: IpAddr| {
        is_cloudflare(ip) || expected.is_some_and(|e| same_address(Some(&ip.to_string()), Some(e)))
    };
    let mut received = false;
    for server in &ctx.dns_servers {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_secs(1) {
            return result(
                "failed",
                "Could not validate DNS for this domain.".into(),
                expected,
            );
        }
        let timeout = remaining.min(Duration::from_secs(5));
        let Some(records) = query_server(server, &host, record, timeout).await else {
            continue;
        };
        for ip in records {
            received = true;
            if matches(ip) {
                return result("ok", success_message(ctx, expected), expected);
            }
        }
    }
    // resolveWithSystemDns: a test instance only asks the DNS servers it was given.
    if !received && Instant::now() < deadline && fake_origin().is_none() {
        let wanted_v6 = record == "AAAA";
        if let Ok(found) = tokio::net::lookup_host((host.as_str(), 0)).await {
            for addr in found.filter(|a| a.is_ipv6() == wanted_v6) {
                if matches(addr.ip()) {
                    return result("ok", success_message(ctx, expected), expected);
                }
            }
        }
    }
    result("failed", mismatch_message(expected), expected)
}

/// A row of the manual records table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hint {
    pub name: String,
    pub kind: &'static str,
    pub value: String,
}

/// `DnsRecordHints::normalizeHostname`, for a host from a domain row.
fn normalize_host(h: &str) -> Option<String> {
    let h = h.trim().trim_end_matches('.').to_lowercase();
    (!h.is_empty()).then_some(h)
}

/// `DnsRecordHints::forHostnames`.
pub fn hints(hostnames: &[String], ipv4: Option<&str>, ipv6: Option<&str>) -> Vec<Hint> {
    let mut out = Vec::new();
    for h in hostnames.iter().filter_map(|h| normalize_host(h)) {
        if let Some(v4) = ipv4.filter(|v| matches!(v.parse::<IpAddr>(), Ok(IpAddr::V4(_)))) {
            out.push(Hint {
                name: h.clone(),
                kind: "A",
                value: v4.to_owned(),
            });
        }
        if let Some(v6) = ipv6.filter(|v| matches!(v.parse::<IpAddr>(), Ok(IpAddr::V6(_)))) {
            out.push(Hint {
                name: h.clone(),
                kind: "AAAA",
                value: v6.to_owned(),
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: IpAddr) -> bool {
        false
    }

    #[tokio::test]
    async fn skips() {
        let mut ctx = CheckContext::default();
        let r = check("http://a.test", &ctx, Some("1.1.1.1"), none).await;
        assert_eq!(r.status, "skipped");
        assert_eq!(
            r.message,
            "DNS validation is disabled in instance settings."
        );
        ctx.validation_enabled = true;
        let r = check("http://a.test", &ctx, Some("1.1.1.1"), none).await;
        assert_eq!(r.message, "No server available for DNS validation.");
        assert_eq!(r.expected_ip, None);
        ctx.has_server = true;
        let r = check("http://x.1.2.3.4.sslip.io", &ctx, Some("1.1.1.1"), none).await;
        assert_eq!(r.status, "ok");
        assert_eq!(r.message, "DNS looks correct.");
        ctx.skip_multiple_servers = true;
        let r = check("http://a.test", &ctx, Some("1.1.1.1"), none).await;
        assert_eq!(
            r.message,
            "DNS check skipped for multi-server applications."
        );
    }

    #[test]
    fn messages() {
        assert_eq!(
            mismatch_message(Some("1.2.3.4")),
            "Required DNS record type A pointing to 1.2.3.4"
        );
        assert_eq!(
            mismatch_message(Some("2001:db8::1")),
            "Required DNS record type AAAA pointing to 2001:db8::1"
        );
        assert_eq!(
            mismatch_message(None),
            "DNS validation failed. Check your DNS records."
        );
    }

    #[test]
    fn answers() {
        let id = 7;
        let mut data = build_query("a.test", 1, id).unwrap();
        data[2] = 0x81;
        data[3] = 0x80;
        data[7] = 1;
        data.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
        assert_eq!(
            parse_answers(&data, id, 1).unwrap(),
            [IpAddr::from([1, 2, 3, 4])]
        );
        assert!(parse_answers(&data, 9, 1).is_none());
    }

    #[test]
    fn hint_rows() {
        let h = hints(
            &["B.test".into(), "a.test".into(), "a.test".into()],
            Some("1.2.3.4"),
            None,
        );
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].name, "a.test");
    }
}
