//! GitHub App access for private repositories: `generateGithubToken()`,
//! `generateGithubInstallationToken()` and `githubApi()` (bootstrap/helpers/github.php).

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use chrono::Utc;
use coolify_compat::encrypter::Encrypter;
use coolify_db::Db;
use coolify_db::records::Record;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::Value;
use sha2::Sha256;

fn field<'a>(source: &'a Record, k: &str) -> Option<&'a str> {
    source.get(k).and_then(Value::as_str)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("coolify-rs")
        .build()
        .map_err(|e| e.to_string())
}

/// `githubAppPrivateKey($source)`: the App's key when it belongs to the App's team.
async fn app_private_key(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    const MISSING: &str = "Private key not found for this GitHub App.";
    let id = source
        .get("private_key_id")
        .and_then(Value::as_i64)
        .ok_or(MISSING)?;
    let team = source.get("team_id").and_then(Value::as_i64);
    let row = coolify_db::git_sources::private_key_row(db, id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or(MISSING)?;
    if row.0 != team {
        return Err(MISSING.into());
    }
    enc.decrypt_maybe_serialized(&row.1)
        .map_err(|_| "The MAC is invalid.".to_owned())
}

/// The RS256 JWT `generateGithubAppJwt()` builds: issued a minute ago, valid for eight minutes.
pub fn app_jwt(private_key_pem: &str, app_id: &str) -> Result<String, String> {
    let key = RsaPrivateKey::from_pkcs1_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(private_key_pem))
        .map_err(|_| "The GitHub App private key is not a valid RSA key.".to_owned())?;
    let now = Utc::now().timestamp();
    let header = B64URL.encode(r#"{"typ":"JWT","alg":"RS256"}"#);
    let claims = B64URL
        .encode(serde_json::json!({"iss": app_id, "iat": now - 60, "exp": now + 480}).to_string());
    let input = format!("{header}.{claims}");
    let signature = SigningKey::<Sha256>::new(key).sign(input.as_bytes());
    Ok(format!("{input}.{}", B64URL.encode(signature.to_bytes())))
}

/// `assertGithubClockInSync($apiUrl)`.
async fn assert_clock_in_sync(http: &reqwest::Client, api_url: &str) -> Result<(), String> {
    let response = http
        .get(format!("{api_url}/zen"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let Some(date) = response
        .headers()
        .get("date")
        .and_then(|d| d.to_str().ok())
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
    else {
        return Ok(());
    };
    let server = Utc::now();
    let diff = (server.timestamp() - date.timestamp()).abs();
    if diff > 50 {
        return Err(format!(
            "System time is out of sync with GitHub API time:<br>- System time: {} UTC<br>- GitHub time: {} UTC<br>- Difference: {diff} seconds<br>Please synchronize your system clock.",
            server.format("%Y-%m-%d %H:%M:%S"),
            date.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S")
        ));
    }
    Ok(())
}

/// `generateGithubToken($source, 'installation')`.
pub async fn installation_token(
    db: &Db,
    enc: Option<&Encrypter>,
    source: Option<&Record>,
) -> Result<String, String> {
    let source = source.ok_or("Source is required for API calls")?;
    let enc = enc.ok_or("No application encryption key has been specified.")?;
    let key = app_private_key(db, enc, source).await?;
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = client()?;
    assert_clock_in_sync(&http, &api).await?;
    let app_id = source
        .get("app_id")
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .unwrap_or_default();
    let jwt = app_jwt(&key, &app_id)?;
    let installation = source
        .get("installation_id")
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .unwrap_or_default();
    let url = format!("{api}/app/installations/{installation}/access_tokens");
    let name = field(source, "name").unwrap_or_default();
    // Up to three tries on connection errors and 5xx answers, one second apart per attempt.
    let mut last: Option<Result<reqwest::Response, reqwest::Error>> = None;
    for attempt in 1..=3u64 {
        let r = http
            .post(&url)
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github.machine-man-preview+json")
            .send()
            .await;
        let retry = match &r {
            Err(e) => e.is_connect() || e.is_timeout(),
            Ok(resp) => resp.status().is_server_error(),
        };
        last = Some(r);
        if !retry || attempt == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(attempt * 1000)).await;
    }
    let response = match last {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            return Err(format!(
                "Failed to get installation token for {name} with error: {e}"
            ));
        }
        None => {
            return Err(format!(
                "Failed to get installation token for {name} with error: no answer"
            ));
        }
    };
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let mut error = body
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| format!("HTTP {}", status.as_u16()), str::to_owned);
        if error == "Not Found" {
            error = "Repository not found. Is it moved or deleted?".into();
        }
        return Err(format!(
            "Failed to get installation token for {name} with error: {error}"
        ));
    }
    body.get("token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Failed to get installation token for {name} with error: no token"))
}

/// `githubApi($source, "repos/{repo}")['data']['id']` with an installation token.
pub async fn repository_id(
    source: &Record,
    token: &str,
    repository: &str,
) -> Result<Option<i64>, String> {
    let api = field(source, "api_url").unwrap_or_default();
    let response = client()?
        .get(format!("{api}/repos/{repository}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Accept", "application/vnd.github.v3+json")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let remaining = response
        .headers()
        .get("X-RateLimit-Remaining")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("0")
        .to_owned();
    let reset = response
        .headers()
        .get("X-RateLimit-Reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no error message found");
        let reset = chrono::DateTime::from_timestamp(reset, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        return Err(format!(
            "GitHub API call failed:<br>Error: {message}<br>Rate Limit Status:<br>- Remaining Calls: {remaining}<br>- Reset Time: {reset} UTC"
        ));
    }
    Ok(body.get("id").and_then(Value::as_i64))
}

/// `Http::GitHub($source->api_url, $token)->timeout(20)->retry(3, 200, throw: false)->get("/repos/{repo}")`
/// (the repository check of `POST /applications/private-github-app`): the status and JSON body
/// of the last attempt. A failed answer or connection error is tried three times, 200 ms apart;
/// `Err` is the connection error of the last try (upstream throws, so the API answers 500).
pub async fn repository_access(
    source: &Record,
    token: &str,
    repository: &str,
) -> Result<(u16, Value), String> {
    let api = field(source, "api_url").unwrap_or_default();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("coolify-rs")
        .build()
        .map_err(|e| e.to_string())?;
    let mut attempt = 1;
    loop {
        let r = http
            .get(format!("{api}/repos/{repository}"))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Accept", "application/vnd.github.v3+json")
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() || attempt == 3 => {
                let status = resp.status().as_u16();
                return Ok((status, resp.json().await.unwrap_or(Value::Null)));
            }
            Err(e) if attempt == 3 => return Err(e.to_string()),
            _ => {}
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1::EncodeRsaPrivateKey;

    /// A one-route GitHub API stand-in: `/repos/o/r` answers 200, everything else 404.
    async fn fake_api() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let authorized = req.contains("authorization: Bearer tok")
                    || req.contains("Authorization: Bearer tok");
                let (status, body) = if req.starts_with("GET /repos/o/r ") && authorized {
                    ("200 OK", r#"{"id":42,"full_name":"o/r"}"#)
                } else {
                    ("404 Not Found", r#"{"message":"Not Found"}"#)
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn repository_access_answers_and_retries() {
        let (api, hits) = fake_api().await;
        let mut source = Record::new();
        source.insert("api_url".into(), Value::String(api));
        let (status, body) = repository_access(&source, "tok", "o/r").await.unwrap();
        assert_eq!(
            (status, body.get("id").and_then(Value::as_i64)),
            (200, Some(42))
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A failed answer is tried three times and the last one is returned.
        let (status, body) = repository_access(&source, "tok", "o/missing")
            .await
            .unwrap();
        assert_eq!(status, 404);
        assert_eq!(body["message"], "Not Found");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[test]
    fn jwt_shape() {
        let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap();
        let pem = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        let jwt = app_jwt(&pem, "123").unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&B64URL.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "123");
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            540
        );
    }
}
