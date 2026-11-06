//! `Http::GitSource($url)` (`AppServiceProvider`): requests to a git provider are pinned to the
//! addresses `SafeExternalUrl` allows, and follow redirects on the same origin only.

use std::time::Duration;

use coolify_db::Db;
pub use reqwest::Method;
use reqwest::{Client, RequestBuilder, Response};
use serde_json::Value;

use crate::databases::s3::UrlPolicy;

/// The client of `Http::GitSource($url)`. `db` is `None` only in tests: no target check.
pub async fn client(db: Option<&Db>, url: &str, timeout: Duration) -> Result<Client, String> {
    let same_origin = reqwest::redirect::Policy::custom(|attempt| {
        let same = attempt.previous().last().is_some_and(|prev| {
            prev.scheme() == attempt.url().scheme()
                && prev.host_str() == attempt.url().host_str()
                && prev.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if attempt.previous().len() >= 5 {
            attempt.error("Too many redirects.")
        } else if same {
            attempt.follow()
        } else {
            let target = format!(
                "{}://{}{}",
                attempt.url().scheme(),
                attempt.url().host_str().unwrap_or_default(),
                attempt
                    .url()
                    .port()
                    .map(|p| format!(":{p}"))
                    .unwrap_or_default()
            );
            attempt.error(format!("Refused a redirect to another host ({target})."))
        }
    });
    let mut builder = Client::builder()
        .timeout(timeout)
        .redirect(same_origin)
        .user_agent("coolify-rs");
    if let Some(db) = db {
        let mut policy = UrlPolicy::load(db).await;
        policy.allow_private = true;
        let (host, port, ips) = policy.request_target(url).await?;
        if !ips.is_empty() {
            let addrs: Vec<std::net::SocketAddr> = ips
                .iter()
                .map(|ip| std::net::SocketAddr::new(*ip, port))
                .collect();
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
    }
    builder.build().map_err(|e| e.to_string())
}

/// `->retry(3, 200, throw: false)`: the last answer, three tries 200 ms apart; `Err` is the
/// connection error of the last try.
pub async fn retry(build: impl Fn() -> RequestBuilder) -> Result<Response, String> {
    let mut attempt = 1;
    loop {
        match build().send_retry().await {
            Ok(r) if r.status().is_success() || attempt == 3 => return Ok(r),
            Err(e) if attempt == 3 => return Err(e.to_string()),
            _ => {}
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `data_get($response->json(), $key, $response->body())`: the JSON field or the body.
pub fn error_text(body: &str, key: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get(key).cloned())
        .and_then(|v| v.as_str().map(str::to_owned).or(Some(v.to_string())))
        .unwrap_or_else(|| body.to_owned())
}

/// `data_get($item, 'a.b')` on a JSON value, as a string.
pub fn path_str<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    path_value(v, path).and_then(Value::as_str)
}

/// `data_get($item, 'a.b')` on a JSON value.
pub fn path_value<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, key| cur.get(key))
}

/// `$url['scheme']`, `urlHostWithPort($url)` and `rtrim($url['path'] ?? '', '/')`.
pub fn oauth_url_parts(html: &str) -> (String, String, String) {
    let (scheme, rest) = html.split_once("://").unwrap_or(("https", html));
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("/{}", path.trim_end_matches('/'))
    };
    let prefix = if prefix == "/" { String::new() } else { prefix };
    (scheme.to_owned(), authority.to_owned(), prefix)
}

/// The methods `RetryHttpConnectionFailures` sends again: a POST or PATCH can reach the server
/// before the failure.
const RETRIED_METHODS: [reqwest::Method; 5] = [
    reqwest::Method::GET,
    reqwest::Method::HEAD,
    reqwest::Method::OPTIONS,
    reqwest::Method::PUT,
    reqwest::Method::DELETE,
];

/// `RetryHttpConnectionFailures`, the middleware upstream puts on every HTTP call: an idempotent
/// request is sent once more, half a second later, after a connection failure (DNS included). A
/// request that timed out after it was sent is not a connection failure and is not sent again.
pub trait SendRetry {
    fn send_retry(self) -> impl std::future::Future<Output = reqwest::Result<Response>> + Send;
}

impl SendRetry for RequestBuilder {
    fn send_retry(self) -> impl std::future::Future<Output = reqwest::Result<Response>> + Send {
        send_with_connect_retry(self)
    }
}

async fn send_with_connect_retry(request: RequestBuilder) -> reqwest::Result<Response> {
    let again = request.try_clone();
    let method = again
        .as_ref()
        .and_then(RequestBuilder::try_clone)
        .and_then(|r| r.build().ok())
        .map(|r| r.method().clone());
    match request.send().await {
        Err(e) if e.is_connect() && method.is_some_and(|m| RETRIED_METHODS.contains(&m)) => {
            match again {
                Some(again) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    again.send().await
                }
                None => Err(e),
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port with nothing listening on it.
    fn closed_port() -> u16 {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| unreachable!());
        listener.local_addr().map(|a| a.port()).unwrap_or_default()
    }

    #[tokio::test]
    async fn connection_failures_of_idempotent_requests_are_sent_again() {
        let url = format!("http://127.0.0.1:{}/", closed_port());
        let client = Client::new();
        let started = std::time::Instant::now();
        assert!(client.get(&url).send_retry().await.is_err());
        assert!(started.elapsed() >= Duration::from_millis(500));
        let started = std::time::Instant::now();
        assert!(client.post(&url).send_retry().await.is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn requests_that_timed_out_after_sending_are_not_sent_again() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|_| unreachable!());
        let port = listener.local_addr().map(|a| a.port()).unwrap_or_default();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = accepted.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(stream);
            }
        });
        let client = Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap_or_else(|_| unreachable!());
        let result = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send_retry()
            .await;
        assert!(result.is_err_and(|e| e.is_timeout()));
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
