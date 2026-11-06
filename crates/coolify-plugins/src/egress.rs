//! The `http` capability: HTTPS requests to the manifest's hosts and nowhere else (ADR 0010, section 3).
//!
//! The host name must be listed exactly. Every address it resolves to must be public, which keeps plugins
//! away from the database, Docker, cloud metadata endpoints and private networks. The connection then goes
//! to exactly the addresses that were checked, so a DNS answer that changes between the check and the
//! connection (rebinding) cannot redirect it. Redirects are not followed.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// Limits on outgoing requests.
#[derive(Debug, Clone)]
pub struct Egress {
    pub timeout: Duration,
    /// Request and response bodies, each.
    pub max_body: usize,
    /// Tests only: allow `http` and local addresses.
    pub allow_local: bool,
    /// Tests only, and only together with `allow_local`: requests to these host names go over plain HTTP to
    /// the given local address instead of resolving the name, so a test can stand in for a real service
    /// while the plugin uses the service's real URL. The host name must still be in the manifest, and the
    /// request keeps it in its `Host` header. Never read from configuration; ignored without `allow_local`.
    pub test_routes: Vec<(String, SocketAddr)>,
}

impl Default for Egress {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_body: 1024 * 1024,
            allow_local: false,
            test_routes: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct Outgoing {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct Incoming {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"];
/// Headers the plugin may not set: they belong to the connection, or would let it reach another host.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
];

impl Egress {
    pub async fn fetch(&self, allowed: &[String], req: Outgoing) -> Result<Incoming, String> {
        let mut url = reqwest::Url::parse(&req.url).map_err(|e| format!("invalid url: {e}"))?;
        let local = self.allow_local;
        match url.scheme() {
            "https" => {}
            "http" if local => {}
            s => return Err(format!("scheme {s:?} is not allowed; use https")),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("credentials in the url are not allowed".into());
        }
        let host = url.host_str().unwrap_or_default().to_owned();
        let is_address = host.starts_with('[') || host.parse::<IpAddr>().is_ok();
        if host.is_empty() || (is_address && !local) {
            return Err("the url must name a host, not an address".into());
        }
        if !allowed.contains(&host) {
            return Err(format!("host {host:?} is not in the plugin's manifest"));
        }
        let port = url.port_or_known_default().unwrap_or(443);
        if port != 443 && !local {
            return Err("only port 443 is allowed".into());
        }
        let method = req.method.to_ascii_uppercase();
        if !METHODS.contains(&method.as_str()) {
            return Err(format!("method {method:?} is not allowed"));
        }
        if req.body.len() > self.max_body {
            return Err(format!("request body over {} bytes", self.max_body));
        }
        if req.headers.len() > 32 {
            return Err("at most 32 headers".into());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &req.headers {
            let lower = name.to_ascii_lowercase();
            if RESERVED_HEADERS.contains(&lower.as_str()) {
                return Err(format!("header {name:?} cannot be set"));
            }
            let name = reqwest::header::HeaderName::from_bytes(lower.as_bytes())
                .map_err(|_| format!("invalid header name {name:?}"))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| format!("invalid value for header {name}"))?;
            headers.append(name, value);
        }

        let route = self
            .test_routes
            .iter()
            .find(|(name, _)| local && *name == host)
            .map(|(_, addr)| *addr);
        let addrs: Vec<SocketAddr> = match route {
            Some(addr) => {
                url.set_scheme("http")
                    .and_then(|()| url.set_port(Some(addr.port())))
                    .map_err(|()| "cannot route the request to its test address".to_owned())?;
                vec![addr]
            }
            None => tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| format!("cannot resolve {host}: {e}"))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(format!("{host} has no addresses"));
        }
        if !local && let Some(a) = addrs.iter().find(|a| blocked(a.ip())) {
            return Err(format!(
                "{host} resolves to {}, which is not a public address",
                a.ip()
            ));
        }
        let client = reqwest::Client::builder()
            .resolve_to_addrs(&host, &addrs)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .no_proxy()
            .https_only(!local)
            .build()
            .map_err(|e| e.to_string())?;
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut resp = client
            .request(method, url)
            .headers(headers)
            .body(req.body)
            .send()
            .await
            .map_err(|e| format!("request failed: {}", without_url(&e)))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| without_url(&e))? {
            if body.len() + chunk.len() > self.max_body {
                return Err(format!("response body over {} bytes", self.max_body));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Incoming {
            status,
            headers,
            body,
        })
    }
}

fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(url) = e.url() {
        s = s.replace(url.as_str(), "<url>");
    }
    s
}

/// Addresses a plugin may never reach: anything that is not a public unicast address.
pub(crate) fn blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return blocked_v4(v4);
            }
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation 2001:db8::/32
                || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // discard 100::/64
                || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64 64:ff9b::/96 and 64:ff9b:1::/48
                || s[..6] == [0; 6] // IPv4-compatible ::a.b.c.d
                || s[0] == 0x2002 // 6to4 2002::/16
        }
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0 // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared address space 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || o[0] >= 240 // reserved 240.0.0.0/4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_non_public_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "198.18.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "::127.0.0.1",
        ] {
            assert!(blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
        for ip in ["1.1.1.1", "140.82.112.3", "2606:4700:4700::1111"] {
            assert!(!blocked(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn refuses_requests_outside_the_policy() {
        let egress = Egress::default();
        let allowed = vec!["hooks.example.com".to_owned()];
        let req = |url: &str| Outgoing {
            method: "POST".into(),
            url: url.into(),
            headers: vec![],
            body: vec![],
        };
        let cases = [
            ("https://evil.example.com/x", "not in the plugin's manifest"),
            ("http://hooks.example.com/x", "use https"),
            ("https://hooks.example.com:8443/x", "only port 443"),
            ("https://127.0.0.1/x", "not an address"),
            ("https://user:pw@hooks.example.com/x", "credentials"),
            ("file:///etc/passwd", "use https"),
        ];
        for (url, want) in cases {
            let err = egress.fetch(&allowed, req(url)).await.err().unwrap();
            assert!(err.contains(want), "{url}: {err}");
        }
        let mut with_host = req("https://hooks.example.com/x");
        with_host
            .headers
            .push(("Host".into(), "169.254.169.254".into()));
        let err = egress.fetch(&allowed, with_host).await.err().unwrap();
        assert!(err.contains("cannot be set"), "{err}");
    }

    /// A local server that answers every connection with `200 hello` and reports the request's head.
    async fn local_server() -> (SocketAddr, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let _ = tx
                    .send(String::from_utf8_lossy(&buf[..n]).into_owned())
                    .await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
                    )
                    .await;
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn test_routes_reach_a_local_stand_in_only_with_allow_local() {
        let (addr, mut seen) = local_server().await;
        let allowed = vec!["hooks.example.invalid".to_owned()];
        let req = || Outgoing {
            method: "POST".into(),
            url: "https://hooks.example.invalid/v2/enqueue?x=1".into(),
            headers: vec![("x-key".into(), "k".into())],
            body: b"{}".to_vec(),
        };
        let routed = Egress {
            allow_local: true,
            test_routes: vec![("hooks.example.invalid".into(), addr)],
            ..Egress::default()
        };
        let r = routed.fetch(&allowed, req()).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"hello"[..]));
        let head = seen.recv().await.unwrap();
        assert!(
            head.starts_with("POST /v2/enqueue?x=1 HTTP/1.1\r\n"),
            "{head}"
        );
        let host_line = format!("host: hooks.example.invalid:{}\r\n", addr.port());
        assert!(head.to_ascii_lowercase().contains(&host_line), "{head}");

        // The manifest still decides: a routed name that isn't listed is refused.
        let err = routed.fetch(&[], req()).await.err().unwrap();
        assert!(err.contains("not in the plugin's manifest"), "{err}");

        // Without allow_local the routes are ignored: the name is resolved as usual (and `.invalid` never
        // resolves), so nothing reaches the local server.
        let ignored = Egress {
            test_routes: vec![("hooks.example.invalid".into(), addr)],
            ..Egress::default()
        };
        let err = ignored.fetch(&allowed, req()).await.err().unwrap();
        assert!(err.contains("cannot resolve"), "{err}");
        assert!(seen.try_recv().is_err());
    }
}
