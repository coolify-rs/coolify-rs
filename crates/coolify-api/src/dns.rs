//! `queryDnsServer`: one A or AAAA question to one DNS server over UDP, and over TCP when the
//! answer is truncated, as upstream's `createDnsQuery` does.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

const TIMEOUT: Duration = Duration::from_secs(5);

fn question(host: &str, v6: bool) -> Option<Vec<u8>> {
    let mut q = vec![0x43, 0x4f, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(u8::try_from(label.len()).ok()?);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, if v6 { 28 } else { 1 }, 0, 1]);
    Some(q)
}

fn skip_name(m: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *m.get(at)?;
        if len & 0xc0 == 0xc0 {
            return Some(at + 2);
        }
        if len == 0 {
            return Some(at + 1);
        }
        at += 1 + usize::from(len);
    }
}

/// The addresses in a response, and whether it was truncated.
fn answers(m: &[u8], v6: bool) -> Option<(Vec<IpAddr>, bool)> {
    if m.len() < 12 || m[0] != 0x43 || m[1] != 0x4f {
        return None;
    }
    let truncated = m[2] & 0x02 != 0;
    let qd = u16::from_be_bytes([m[4], m[5]]);
    let an = u16::from_be_bytes([m[6], m[7]]);
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(m, at)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        at = skip_name(m, at)?;
        let kind = u16::from_be_bytes([*m.get(at)?, *m.get(at + 1)?]);
        let len = usize::from(u16::from_be_bytes([*m.get(at + 8)?, *m.get(at + 9)?]));
        let data = m.get(at + 10..at + 10 + len)?;
        match (kind, v6) {
            (1, false) if len == 4 => {
                out.push(IpAddr::V4(Ipv4Addr::new(
                    data[0], data[1], data[2], data[3],
                )));
            }
            (28, true) if len == 16 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(data);
                out.push(IpAddr::V6(Ipv6Addr::from(b)));
            }
            _ => {}
        }
        at += 10 + len;
    }
    Some((out, truncated))
}

async fn over_udp(addr: SocketAddr, q: &[u8]) -> Option<Vec<u8>> {
    let bind: SocketAddr = if addr.is_ipv6() {
        "[::]:0".parse().ok()?
    } else {
        "0.0.0.0:0".parse().ok()?
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.send_to(q, addr).await.ok()?;
    let mut buf = vec![0u8; 4096];
    let (n, _) = tokio::time::timeout(TIMEOUT, socket.recv_from(&mut buf))
        .await
        .ok()?
        .ok()?;
    buf.truncate(n);
    Some(buf)
}

async fn over_tcp(addr: SocketAddr, q: &[u8]) -> Option<Vec<u8>> {
    tokio::time::timeout(TIMEOUT, async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        let len = u16::try_from(q.len()).ok()?;
        stream.write_all(&len.to_be_bytes()).await.ok()?;
        stream.write_all(q).await.ok()?;
        let n = stream.read_u16().await.ok()?;
        let mut buf = vec![0u8; usize::from(n)];
        stream.read_exact(&mut buf).await.ok()?;
        Some(buf)
    })
    .await
    .ok()?
}

/// The addresses of `host` according to `server`; empty when it does not answer.
pub async fn query(server: &str, host: &str, v6: bool) -> Vec<IpAddr> {
    query_port(server, 53, host, v6).await
}

async fn query_port(server: &str, port: u16, host: &str, v6: bool) -> Vec<IpAddr> {
    let Ok(ip) = server.trim_matches(['[', ']']).parse::<IpAddr>() else {
        return Vec::new();
    };
    let Some(q) = question(host, v6) else {
        return Vec::new();
    };
    let addr = SocketAddr::new(ip, port);
    let Some(m) = over_udp(addr, &q).await else {
        return Vec::new();
    };
    match answers(&m, v6) {
        Some((_, true)) => over_tcp(addr, &q)
            .await
            .and_then(|m| answers(&m, v6))
            .map_or_else(Vec::new, |r| r.0),
        Some((list, false)) => list,
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn asks_a_local_server() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (n, from) = server.recv_from(&mut buf).await.unwrap();
            let mut reply = buf[..n].to_vec();
            reply[2] = 0x81;
            reply[3] = 0x80;
            reply[7] = 1;
            reply.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
            server.send_to(&reply, from).await.unwrap();
        });
        let got = query_port("127.0.0.1", port, "app.example.test", false).await;
        task.await.unwrap();
        assert_eq!(got, vec![IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))]);
    }

    #[test]
    fn rejects_foreign_ids() {
        assert!(answers(&[0; 12], false).is_none());
    }
}
