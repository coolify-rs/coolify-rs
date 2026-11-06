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

use crate::git_http;

/// `sshGitUrlToHttps()`.
pub fn ssh_git_url_to_https(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = match host.strip_prefix('[') {
            Some(h) => format!("[{}", h.split_once(']').map(|x| x.0).unwrap_or(h)) + "]",
            None => host.split(':').next().unwrap_or(host).to_owned(),
        };
        let path = path.trim_start_matches('/');
        return (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"));
    }
    let re = regex::Regex::new(
        r"^(?<user>[A-Za-z0-9._-]+)@(?<host>\[[0-9A-Fa-f:.]+\]|[^:\[\]]+):(?:(?<port>\d+)/)?(?<path>.+)$",
    )
    .unwrap_or_else(|_| unreachable!());
    let c = re.captures(url)?;
    let host = c["host"].trim();
    let path = c["path"].trim_start_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

/// `gitRepositorySlug()`: the `owner/repo` that the GitHub API takes, also for SSH repository URLs.
pub fn git_repository_slug(repository: &str) -> String {
    if repository.is_empty() {
        return String::new();
    }
    let slug = match ssh_git_url_to_https(repository) {
        Some(https) => crate::applications::domains::UrlParts::parse(&https)
            .and_then(|p| p.path)
            .unwrap_or_default(),
        None if repository.starts_with("http") || repository.contains("github.com") => repository
            .replace("https://", "")
            .replace("http://", "")
            .replace("github.com/", ""),
        None => repository.to_owned(),
    };
    let slug = slug.trim_matches('/');
    slug.strip_suffix(".git").unwrap_or(slug).to_owned()
}

fn field<'a>(source: &'a Record, k: &str) -> Option<&'a str> {
    source.get(k).and_then(Value::as_str)
}

/// `githubAppPrivateKey($source)`: the App's key when it belongs to the App's team.
pub async fn app_private_key(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
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

/// `gitlabAppPrivateKey($source)` / `giteaAppPrivateKey($source)`: the decrypted key when it
/// belongs to the source's team.
pub async fn team_private_key(db: &Db, enc: &Encrypter, source: &Record) -> Option<String> {
    app_private_key(db, enc, source).await.ok()
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

fn id_text(source: &Record, k: &str) -> String {
    source
        .get(k)
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .unwrap_or_default()
}

/// `generateGithubToken($source, 'jwt')`.
pub async fn jwt(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    let key = app_private_key(db, enc, source).await?;
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(30)).await?;
    assert_clock_in_sync(&http, &api).await?;
    app_jwt(&key, &id_text(source, "app_id"))
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
    let http = git_http::client(Some(db), &api, Duration::from_secs(30)).await?;
    assert_clock_in_sync(&http, &api).await?;
    let jwt = app_jwt(&key, &id_text(source, "app_id"))?;
    let installation = id_text(source, "installation_id");
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
            error = format!(
                "GitHub App installation {installation} was not found. Install the GitHub App again or update its installation ID."
            );
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
    db: &Db,
    source: &Record,
    token: &str,
    repository: &str,
) -> Result<Option<i64>, String> {
    let api = field(source, "api_url").unwrap_or_default();
    let response = git_http::client(Some(db), api, Duration::from_secs(30))
        .await?
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
    db: Option<&Db>,
    source: &Record,
    token: &str,
    repository: &str,
) -> Result<(u16, Value), String> {
    let api = field(source, "api_url").unwrap_or_default();
    let http = git_http::client(db, api, Duration::from_secs(20)).await?;
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

fn authed(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Accept", "application/vnd.github.v3+json")
        .header("Authorization", format!("Bearer {token}"))
}

/// `GithubController::load_repositories()`: every page of `/installation/repositories`; the
/// status and error body of a failed page in `Err`-free form.
pub async fn installation_repositories(
    db: &Db,
    source: &Record,
    token: &str,
) -> Result<Result<Vec<Value>, (u16, Value)>, String> {
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(20)).await?;
    let url = format!("{api}/installation/repositories");
    let mut all = Vec::new();
    for page in 1..=100 {
        let page_text = page.to_string();
        let response = git_http::retry(|| {
            authed(http.get(&url), token)
                .query(&[("per_page", "100"), ("page", page_text.as_str())])
        })
        .await?;
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status != 200 {
            return Ok(Err((status, body)));
        }
        let repos = body
            .get("repositories")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if repos.is_empty() {
            break;
        }
        all.extend(repos);
    }
    Ok(Ok(all))
}

/// `Http::GitHub(...)->timeout(20)->retry(3, 200, throw: false)->get($endpoint)`.
pub async fn get_with_token(
    db: &Db,
    source: &Record,
    token: &str,
    endpoint: &str,
    query: &[(&str, &str)],
) -> Result<(u16, Value), String> {
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(20)).await?;
    let url = format!("{api}/{}", endpoint.trim_start_matches('/'));
    let response = git_http::retry(|| authed(http.get(&url), token).query(query)).await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

/// `loadRepositoryByPage($source, $token, $page)`.
pub async fn repository_page(
    db: &Db,
    source: &Record,
    token: &str,
    page: i64,
) -> Result<Value, String> {
    let page_text = page.to_string();
    let (status, body) = get_with_token(
        db,
        source,
        token,
        "/installation/repositories",
        &[("per_page", "100"), ("page", page_text.as_str())],
    )
    .await?;
    if status != 200 {
        let error = body
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| format!("HTTP {status}"), str::to_owned);
        return Err(format!(
            "Failed to load repositories for {} with error: {error}",
            field(source, "name").unwrap_or_default()
        ));
    }
    let total = body.get("total_count").and_then(Value::as_i64).unwrap_or(0);
    if total == 0 {
        return Ok(serde_json::json!({"total_count": 0, "repositories": []}));
    }
    Ok(serde_json::json!({"total_count": total, "repositories": body.get("repositories")}))
}

/// `githubApi($source, $endpoint, $method, $data)` for a private App: the status, the JSON data
/// and the rate limit headers. `Err` when the token or the connection fails.
pub async fn api(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    method: reqwest::Method,
    endpoint: &str,
    data: Option<&Value>,
) -> Result<(u16, Value), String> {
    let token = installation_token(db, Some(enc), Some(source)).await?;
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(30)).await?;
    let url = format!("{api}/{}", endpoint.trim_start_matches('/'));
    let mut request = authed(http.request(method, &url), &token);
    if let Some(d) = data {
        request = request.json(d);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

/// A `github_apps` row by id.
pub async fn source_by_id(db: &Db, id: i64) -> coolify_db::DbResult<Option<Record>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(g)::text FROM github_apps g WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.and_then(|t| coolify_db::records::parse_row(&t).ok()))
}

/// `Previews::load_prs`: `githubApi($source, "/repos/{$repository}/pulls")`, sorted by number,
/// with the `X-RateLimit-Remaining` header. `Err` holds the message `githubApi()` throws.
pub async fn pull_requests(
    db: &Db,
    enc: Option<&Encrypter>,
    source: &Record,
    repository: &str,
) -> Result<(Option<String>, Vec<Value>), String> {
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(30)).await?;
    let url = format!("{api}/repos/{repository}/pulls");
    let is_public = source
        .get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request = if is_public {
        http.get(&url)
            .header("Accept", "application/vnd.github.v3+json")
    } else {
        let token = installation_token(db, enc, Some(source)).await?;
        authed(http.get(&url), &token)
    };
    let response = request.send().await.map_err(|e| e.to_string())?;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let remaining = header("X-RateLimit-Remaining");
    let reset = header("X-RateLimit-Reset");
    let success = response.status().is_success();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !success {
        let reset_time = reset
            .and_then(|r| r.parse::<i64>().ok())
            .and_then(|r| chrono::DateTime::from_timestamp(r, 0))
            .unwrap_or_default()
            .format("%Y-%m-%d %H:%M:%S");
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no error message found");
        return Err(format!(
            "GitHub API call failed:<br>Error: {message}<br>Rate Limit Status:<br>- Remaining Calls: {}<br>- Reset Time: {reset_time} UTC",
            remaining.as_deref().unwrap_or("0")
        ));
    }
    let mut rows = match body {
        Value::Array(rows) => rows,
        _ => Vec::new(),
    };
    rows.sort_by_key(|r| r.get("number").and_then(Value::as_i64).unwrap_or_default());
    Ok((remaining, rows))
}

/// `getGithubCommitRangeFiles()`: `None` when the list is unknown.
pub async fn commit_range_files(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    full_name: &str,
    before: &str,
    after: &str,
) -> Option<Vec<String>> {
    let endpoint = format!("/repos/{full_name}/compare/{before}...{after}");
    let (status, body) = api(db, enc, source, reqwest::Method::GET, &endpoint, None)
        .await
        .ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    let files = body
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.len() >= 300 {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for file in &files {
        for k in ["filename", "previous_filename"] {
            if let Some(name) = file.get(k).and_then(Value::as_str)
                && !name.is_empty()
                && !out.iter().any(|x| x == name)
            {
                out.push(name.to_owned());
            }
        }
    }
    Some(out)
}

/// `getGithubCommitMessage()`.
pub async fn commit_message(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    full_name: &str,
    sha: &str,
) -> Option<String> {
    if sha.is_empty() || sha == "HEAD" {
        return None;
    }
    let endpoint = format!("/repos/{full_name}/commits/{sha}");
    let (_, body) = api(db, enc, source, reqwest::Method::GET, &endpoint, None)
        .await
        .ok()?;
    git_http::path_str(&body, "commit.message").map(str::to_owned)
}

/// `getGithubPullRequestFiles()`: `None` when the list is unknown.
pub async fn pull_request_files(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    full_name: &str,
    pull_request_id: i64,
) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for page in 1..=30 {
        let endpoint =
            format!("/repos/{full_name}/pulls/{pull_request_id}/files?per_page=100&page={page}");
        let (status, body) = api(db, enc, source, reqwest::Method::GET, &endpoint, None)
            .await
            .ok()?;
        if !(200..300).contains(&status) {
            return None;
        }
        let files = body.as_array().cloned().unwrap_or_default();
        out.extend(
            files
                .iter()
                .filter_map(|f| f.get("filename").and_then(Value::as_str))
                .filter(|n| !n.is_empty())
                .map(str::to_owned),
        );
        if files.len() < 100 {
            return Some(out);
        }
    }
    None
}

/// `GET {api_url}/app` with the App's JWT: the status and JSON.
pub async fn app_info(db: &Db, source: &Record, jwt: &str) -> Result<(u16, Value), String> {
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(10)).await?;
    let response = http
        .get(format!("{api}/app"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

/// `GithubAppPermissionJob`: the permissions and events to store on the row, as columns.
pub async fn fetch_permissions(db: &Db, enc: &Encrypter, source: &Record) -> Result<Value, String> {
    let jwt = jwt(db, enc, source).await?;
    let (status, body) = app_info(db, source, &jwt).await?;
    if !(200..300).contains(&status) {
        return Err(format!("Failed to fetch GitHub app permissions: {}", body));
    }
    Ok(body)
}

/// `syncGithubAppName()`: the App slug, `None` when the App has no id or key or GitHub sends no
/// slug.
pub async fn app_slug(db: &Db, enc: &Encrypter, source: &Record) -> Result<Option<String>, String> {
    if source.get("app_id").is_none_or(Value::is_null)
        || source.get("private_key_id").is_none_or(Value::is_null)
    {
        return Ok(None);
    }
    let Ok(key) = app_private_key(db, enc, source).await else {
        return Ok(None);
    };
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(Some(db), &api, Duration::from_secs(30)).await?;
    assert_clock_in_sync(&http, &api).await?;
    let jwt = app_jwt(&key, &id_text(source, "app_id"))?;
    let response = http
        .get(format!("{api}/app"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Authorization", format!("Bearer {jwt}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ok = response.status().is_success();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !ok {
        return Err(body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Failed to fetch GitHub App information.")
            .to_owned());
    }
    Ok(body
        .get("slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

/// `Github::redirect()`'s manifest conversion.
pub async fn manifest_conversion(db: &Db, api_url: &str, code: &str) -> Result<Value, String> {
    let http = git_http::client(Some(db), api_url, Duration::from_secs(10)).await?;
    let response = http
        .post(format!("{api_url}/app-manifests/{code}/conversions"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "HTTP request returned status code {}",
            response.status().as_u16()
        ));
    }
    response.json().await.map_err(|e| e.to_string())
}

/// `githubInstallationVerificationError()`: why the installation does not belong to this App.
pub async fn installation_verification_error(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    installation_id: &str,
) -> Option<String> {
    let has_key = app_private_key(db, enc, source)
        .await
        .is_ok_and(|k| !k.is_empty());
    let app_id = id_text(source, "app_id");
    if app_id.is_empty() || app_id == "null" || !has_key {
        return Some("The GitHub App ID or private key is missing.".into());
    }
    let answer = async {
        let jwt = jwt(db, enc, source).await?;
        let api = field(source, "api_url").unwrap_or_default().to_owned();
        let http = git_http::client(Some(db), &api, Duration::from_secs(10)).await?;
        http.get(format!("{api}/app/installations/{installation_id}"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let response = match answer {
        Ok(r) => r,
        Err(e) => return Some(e.replace("<br>", " ")),
    };
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Some(format!(
            "GitHub API returned HTTP {}: {}",
            status.as_u16(),
            body.get("message")
                .and_then(Value::as_str)
                .unwrap_or("no error message found")
        ));
    }
    let answered = body
        .get("app_id")
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .unwrap_or_default();
    (answered != app_id).then(|| "The installation belongs to a different GitHub App.".to_owned())
}

/// `githubApiUrlFromHtmlUrl($htmlUrl)`.
pub fn api_url_from_html_url(html: &str) -> String {
    let (scheme, rest) = html.split_once("://").unwrap_or(("", html));
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let (host, port) = authority.split_once(':').unwrap_or((authority, ""));
    let host = host.to_lowercase();
    if host == "github.com" {
        return "https://api.github.com".into();
    }
    if host.ends_with(".ghe.com") && !host.starts_with("api.") {
        return format!("https://api.{host}");
    }
    let port = if port.is_empty() {
        String::new()
    } else {
        format!(":{port}")
    };
    format!("{scheme}://{host}{port}/api/v3")
}

/// `normalizeGithubOrganization()`.
pub fn normalize_organization(v: Option<&str>) -> Option<String> {
    let v = v.filter(|o| !o.trim().is_empty())?;
    Some(
        v.trim_matches(|c: char| matches!(c, '/' | ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B'))
            .to_owned(),
    )
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
        let (status, body) = repository_access(None, &source, "tok", "o/r")
            .await
            .unwrap();
        assert_eq!(
            (status, body.get("id").and_then(Value::as_i64)),
            (200, Some(42))
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A failed answer is tried three times and the last one is returned.
        let (status, body) = repository_access(None, &source, "tok", "o/missing")
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
