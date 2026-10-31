//! `Http::GitSource($url)` (`AppServiceProvider`): requests to a git provider are pinned to the
//! addresses `SafeExternalUrl` allows, and follow redirects on the same origin only.

use std::time::Duration;

use coolify_db::Db;
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
        match build().send().await {
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
