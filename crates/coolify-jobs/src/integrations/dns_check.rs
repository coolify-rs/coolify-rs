//! `CheckDomainDns`, `DnsRecordHints::forHostnames` and `toCopyText`, `queryDnsServer` and the
//! resolver it uses (`App\Services\Dns\DnsResolver`: a question over UDP, asked again over TCP
//! when the answer is truncated).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpStream, UdpSocket};

use super::cloudflare::{Dns, same_address};
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

/// What `DnsResolver::query()` makes of the raw answer to the question with this id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// The address records of the wanted type.
    Records(Vec<IpAddr>),
    /// The TC bit is set: "Response too big for UDP, retry with TCP".
    Truncated,
    /// Too small, or the answer of another question.
    Invalid,
}

/// The checks of `DnsResolver::query()` on a raw answer: its size, the transaction id and, over
/// UDP, the truncation bit.
fn read_answer(data: &[u8], id: u16, qtype: u16, udp: bool) -> Answer {
    if data.len() < 12 || u16::from_be_bytes([data[0], data[1]]) != id {
        return Answer::Invalid;
    }
    let flags = u16::from_be_bytes([data[2], data[3]]);
    if udp && flags & 0x0200 != 0 {
        return Answer::Truncated;
    }
    match parse_answers(data, id, qtype) {
        Some(records) => Answer::Records(records),
        None => Answer::Invalid,
    }
}

/// `exchangeUdp()`: one datagram of at most 4096 bytes.
async fn exchange_udp(target: SocketAddr, packet: &[u8]) -> Option<Vec<u8>> {
    let bind = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.send_to(packet, target).await.ok()?;
    let mut buf = [0u8; 4096];
    let n = socket.recv(&mut buf).await.ok()?;
    (n > 0).then(|| buf[..n].to_vec())
}

/// `exchangeTcp()`: the question and the answer each follow their two-byte length.
async fn exchange_tcp(target: SocketAddr, packet: &[u8]) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(target).await.ok()?;
    let length = u16::try_from(packet.len()).ok()?;
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend_from_slice(packet);
    stream.write_all(&framed).await.ok()?;
    let mut size = [0u8; 2];
    stream.read_exact(&mut size).await.ok()?;
    let mut answer = vec![0u8; usize::from(u16::from_be_bytes(size))];
    stream.read_exact(&mut answer).await.ok()?;
    Some(answer)
}

/// `queryDnsServer`: `None` when the server does not answer. A truncated UDP answer is asked
/// again over TCP.
async fn query_server(
    server: &str,
    host: &str,
    record: &str,
    timeout: Duration,
) -> Option<Vec<IpAddr>> {
    let qtype = if record == "AAAA" { 28 } else { 1 };
    let target: SocketAddr = server.parse::<SocketAddr>().ok().or_else(|| {
        server
            .parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, 53))
    })?;
    // `random_int(0, 0xFFFF)`.
    let id: u16 = rand::random();
    let packet = build_query(host, qtype, id)?;
    let raw = tokio::time::timeout(timeout, exchange_udp(target, &packet))
        .await
        .ok()??;
    match read_answer(&raw, id, qtype, true) {
        Answer::Records(records) => Some(records),
        Answer::Invalid => None,
        Answer::Truncated => {
            let id: u16 = rand::random();
            let packet = build_query(host, qtype, id)?;
            let raw = tokio::time::timeout(timeout, exchange_tcp(target, &packet))
                .await
                .ok()??;
            match read_answer(&raw, id, qtype, false) {
                Answer::Records(records) => Some(records),
                _ => None,
            }
        }
    }
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

/// What `CheckDomainDns::resultForAddresses()` needs to ask Cloudflare where a proxied hostname
/// goes (`CloudflareDnsProvider::sendsHostnameToAnotherServer()`).
#[derive(Debug)]
pub struct OriginCheck<'a> {
    pub dns: &'a Dns<'a>,
    /// `$server->team_id`.
    pub team_id: Option<i64>,
    /// `ServerDnsAddresses::forServer($server)->addresses()`.
    pub addresses: Vec<String>,
}

/// What the addresses that DNS returned for the host mean, before Cloudflare is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressMatch {
    /// One of them is the expected address.
    Expected,
    /// None is the expected address, but one is a Cloudflare IP: the origin is hidden.
    Cloudflare,
    None,
}

fn address_match(
    addresses: &[IpAddr],
    expected: Option<&str>,
    is_cloudflare: fn(IpAddr) -> bool,
) -> AddressMatch {
    if addresses
        .iter()
        .any(|ip| expected.is_some_and(|e| same_address(Some(&ip.to_string()), Some(e))))
    {
        AddressMatch::Expected
    } else if addresses.iter().any(|ip| is_cloudflare(*ip)) {
        AddressMatch::Cloudflare
    } else {
        AddressMatch::None
    }
}

/// `CheckDomainDns::resultForAddresses()`: the result for the addresses that DNS returned for the
/// host, or `None` when none of them matches. A Cloudflare IP matches unless the Cloudflare
/// records of the team's zone send the host to another server.
async fn result_for_addresses(
    addresses: &[IpAddr],
    host: &str,
    ctx: &CheckContext,
    expected: Option<&str>,
    is_cloudflare: fn(IpAddr) -> bool,
    origin: Option<&OriginCheck<'_>>,
    deadline: Instant,
) -> Option<CheckResult> {
    match address_match(addresses, expected, is_cloudflare) {
        AddressMatch::None => return None,
        AddressMatch::Expected => {}
        AddressMatch::Cloudflare => {
            if let Some(origin) = origin {
                // `max(1, intdiv($deadline - hrtime(true), 1_000_000_000))`.
                let seconds = deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1);
                let mut server_addresses: Vec<String> =
                    expected.map(str::to_owned).into_iter().collect();
                server_addresses.extend(origin.addresses.iter().cloned());
                if origin
                    .dns
                    .sends_hostname_to_another_server(
                        origin.team_id,
                        host,
                        &server_addresses,
                        Duration::from_secs(seconds),
                    )
                    .await
                {
                    return Some(result(
                        "failed",
                        format!(
                            "Cloudflare sends {host} to another server. {}",
                            mismatch_message(expected)
                        ),
                        expected,
                    ));
                }
            }
        }
    }
    Some(result("ok", success_message(ctx, expected), expected))
}

/// `CheckDomainDns::handle` for one URL. `is_cloudflare` is `isCloudflareIp()`. Without the
/// Cloudflare zones of the team, a Cloudflare IP always matches.
pub async fn check(
    url: &str,
    ctx: &CheckContext,
    expected: Option<&str>,
    is_cloudflare: fn(IpAddr) -> bool,
) -> CheckResult {
    check_with_origin(url, ctx, expected, is_cloudflare, None).await
}

/// `CheckDomainDns::handle` for one URL, with the Cloudflare zones of the server's team.
pub async fn check_with_origin(
    url: &str,
    ctx: &CheckContext,
    expected: Option<&str>,
    is_cloudflare: fn(IpAddr) -> bool,
    origin: Option<&OriginCheck<'_>>,
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
        if records.is_empty() {
            continue;
        }
        received = true;
        if let Some(found) = result_for_addresses(
            &records,
            &host,
            ctx,
            expected,
            is_cloudflare,
            origin,
            deadline,
        )
        .await
        {
            return found;
        }
    }
    // resolveWithSystemDns: a test instance only asks the DNS servers it was given.
    if !received && Instant::now() < deadline && fake_origin().is_none() {
        let wanted_v6 = record == "AAAA";
        if let Ok(found) = tokio::net::lookup_host((host.as_str(), 0)).await {
            let addresses: Vec<IpAddr> = found
                .filter(|a| a.is_ipv6() == wanted_v6)
                .map(|a| a.ip())
                .collect();
            if let Some(found) = result_for_addresses(
                &addresses,
                &host,
                ctx,
                expected,
                is_cloudflare,
                origin,
                deadline,
            )
            .await
            {
                return found;
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

    fn cloudflare(ip: IpAddr) -> bool {
        ip.to_string().starts_with("104.16.")
    }

    #[test]
    fn a_cloudflare_address_hides_the_origin() {
        let ips =
            |list: &[&str]| -> Vec<IpAddr> { list.iter().map(|i| i.parse().unwrap()).collect() };
        // The expected address wins, also next to a Cloudflare IP.
        assert_eq!(
            address_match(
                &ips(&["104.16.0.1", "1.2.3.4"]),
                Some("1.2.3.4"),
                cloudflare
            ),
            AddressMatch::Expected
        );
        assert_eq!(
            address_match(&ips(&["104.16.0.1"]), Some("1.2.3.4"), cloudflare),
            AddressMatch::Cloudflare
        );
        assert_eq!(
            address_match(&ips(&["5.6.7.8"]), Some("1.2.3.4"), cloudflare),
            AddressMatch::None
        );
        assert_eq!(
            address_match(&[], Some("1.2.3.4"), cloudflare),
            AddressMatch::None
        );
        assert_eq!(
            address_match(&ips(&["2001:db8::1"]), Some("2001:DB8:0::1"), none),
            AddressMatch::Expected
        );
    }

    #[tokio::test]
    async fn without_the_zones_a_cloudflare_address_matches() {
        let ctx = CheckContext {
            validation_enabled: true,
            has_server: true,
            ..CheckContext::default()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let cf_ip: Vec<IpAddr> = vec!["104.16.0.1".parse().unwrap()];
        let found = result_for_addresses(
            &cf_ip,
            "app.test",
            &ctx,
            Some("1.2.3.4"),
            cloudflare,
            None,
            deadline,
        )
        .await
        .unwrap();
        assert_eq!(found.status, "ok");
        assert_eq!(found.message, "DNS points to 1.2.3.4 (or Cloudflare).");
        let other: Vec<IpAddr> = vec!["5.6.7.8".parse().unwrap()];
        assert_eq!(
            result_for_addresses(
                &other,
                "app.test",
                &ctx,
                Some("1.2.3.4"),
                cloudflare,
                None,
                deadline
            )
            .await,
            None
        );
    }

    /// A DNS answer with one A record for the question `a.test`.
    fn answer(id: u16, flags: u16, address: [u8; 4]) -> Vec<u8> {
        let mut packet = build_query("a.test", 1, id).unwrap();
        packet[2..4].copy_from_slice(&flags.to_be_bytes());
        packet[6..8].copy_from_slice(&1u16.to_be_bytes());
        // A compressed name pointing at the question, type A, class IN, a TTL, four bytes.
        packet.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        packet.extend_from_slice(&address);
        packet
    }

    #[test]
    fn answers_are_checked_like_the_resolver_does() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert_eq!(
            read_answer(&answer(7, 0x8180, [1, 2, 3, 4]), 7, 1, true),
            Answer::Records(vec![ip])
        );
        // Another transaction id.
        assert_eq!(
            read_answer(&answer(8, 0x8180, [1, 2, 3, 4]), 7, 1, true),
            Answer::Invalid
        );
        assert_eq!(read_answer(&[0, 7, 0x81], 7, 1, true), Answer::Invalid);
        // The TC bit over UDP asks for TCP; over TCP the answer is read.
        assert_eq!(
            read_answer(&answer(7, 0x8380, [1, 2, 3, 4]), 7, 1, true),
            Answer::Truncated
        );
        assert_eq!(
            read_answer(&answer(7, 0x8380, [1, 2, 3, 4]), 7, 1, false),
            Answer::Records(vec![ip])
        );
        // An A record is no answer to an AAAA question.
        assert_eq!(
            read_answer(&answer(7, 0x8180, [1, 2, 3, 4]), 7, 28, true),
            Answer::Records(vec![])
        );
        // NXDOMAIN without records: an empty list, which the check skips.
        let mut nxdomain = build_query("a.test", 1, 7).unwrap();
        nxdomain[2..4].copy_from_slice(&0x8183u16.to_be_bytes());
        assert_eq!(read_answer(&nxdomain, 7, 1, true), Answer::Records(vec![]));
    }

    #[tokio::test]
    async fn a_truncated_udp_answer_is_asked_again_over_tcp() {
        // One server that answers truncated over UDP and in full over TCP, on the same port.
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind(addr).await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (n, from) = udp.recv_from(&mut buf).await.unwrap();
            let id = u16::from_be_bytes([buf[0], buf[1]]);
            let mut reply = buf[..n].to_vec();
            reply[2..4].copy_from_slice(&0x8380u16.to_be_bytes());
            let _ = id;
            udp.send_to(&reply, from).await.unwrap();
        });
        tokio::spawn(async move {
            let (mut stream, _) = tcp.accept().await.unwrap();
            let mut size = [0u8; 2];
            stream.read_exact(&mut size).await.unwrap();
            let mut question = vec![0u8; usize::from(u16::from_be_bytes(size))];
            stream.read_exact(&mut question).await.unwrap();
            let id = u16::from_be_bytes([question[0], question[1]]);
            let reply = answer(id, 0x8180, [9, 8, 7, 6]);
            let mut framed = (reply.len() as u16).to_be_bytes().to_vec();
            framed.extend_from_slice(&reply);
            stream.write_all(&framed).await.unwrap();
        });
        let found = query_server(&addr.to_string(), "a.test", "A", Duration::from_secs(2)).await;
        assert_eq!(found, Some(vec!["9.8.7.6".parse().unwrap()]));
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
