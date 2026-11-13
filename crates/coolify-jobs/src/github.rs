//! GitHub App access for private repositories: `generateGithubToken()`,
//! `generateGithubInstallationToken()` and `githubApi()` (bootstrap/helpers/github.php).

use crate::git_http::SendRetry as _;
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

/// The `Date` header of an answer, for [`assert_clock_in_sync`].
pub fn date_header(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("date")
        .and_then(|d| d.to_str().ok())
        .map(str::to_owned)
}

/// `assertGithubClockInSync($response)`: GitHub rejects an App JWT when the clocks differ. Called
/// with the `Date` header of the failed answer of a JWT request, so no extra request is made.
pub fn assert_clock_in_sync(date: Option<&str>) -> Result<(), String> {
    clock_skew(date, Utc::now())
}

fn clock_skew(date: Option<&str>, server: chrono::DateTime<Utc>) -> Result<(), String> {
    let Some(date) = date
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
    else {
        return Ok(());
    };
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
    app_jwt(&key, &id_text(source, "app_id"))
}

/// The time between the tries of [`send_transient_retry`]: `$attempt * 1000` milliseconds.
const TRANSIENT_RETRY_STEP: Duration = Duration::from_millis(1000);

/// `isTransientGithubFailure($exception)`: a dropped connection or a 5xx answer.
fn is_transient_failure(answer: &reqwest::Result<reqwest::Response>) -> bool {
    match answer {
        Err(e) => e.is_connect() || e.is_timeout(),
        Ok(response) => response.status().is_server_error(),
    }
}

/// `->retry(3, fn (int $attempt) => $attempt * 1000, isTransientGithubFailure(...), throw: false)`:
/// GitHub sometimes answers with a 5xx page or drops the connection, and a repeated read or
/// token request is harmless. The answer or connection error of the last try.
pub(crate) async fn send_transient_retry(
    build: impl Fn() -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    send_transient_retry_every(build, TRANSIENT_RETRY_STEP).await
}

async fn send_transient_retry_every(
    build: impl Fn() -> reqwest::RequestBuilder,
    step: Duration,
) -> reqwest::Result<reqwest::Response> {
    let mut attempt: u32 = 1;
    loop {
        let answer = build().send_retry().await;
        if attempt == 3 || !is_transient_failure(&answer) {
            return answer;
        }
        tokio::time::sleep(step * attempt).await;
        attempt += 1;
    }
}

/// The HTTP timeout of a GitHub request: `githubRequestTimeout($request, $timeout)` when the
/// caller limits it, else the 30 seconds of the HTTP client.
fn request_timeout(timeout: Option<Duration>) -> Duration {
    timeout.unwrap_or(Duration::from_secs(30))
}

/// `generateGithubToken($source, 'installation')`.
pub async fn installation_token(
    db: &Db,
    enc: Option<&Encrypter>,
    source: Option<&Record>,
) -> Result<String, String> {
    installation_token_within(db, enc, source, None).await
}

/// `generateGithubToken($source, 'installation', $timeout)`: `timeout` limits each request.
pub async fn installation_token_within(
    db: &Db,
    enc: Option<&Encrypter>,
    source: Option<&Record>,
    timeout: Option<Duration>,
) -> Result<String, String> {
    let source = source.ok_or("Source is required for API calls")?;
    let enc = enc.ok_or("No application encryption key has been specified.")?;
    let key = app_private_key(db, enc, source).await?;
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        request_timeout(timeout),
    )
    .await?;
    let jwt = app_jwt(&key, &id_text(source, "app_id"))?;
    let installation = id_text(source, "installation_id");
    let url = format!("{api}/app/installations/{installation}/access_tokens");
    let name = field(source, "name").unwrap_or_default();
    // Up to three tries on connection errors and 5xx answers, one second apart per attempt.
    let answer = send_transient_retry(|| {
        http.post(&url)
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github.machine-man-preview+json")
    })
    .await;
    let response = match answer {
        Ok(r) => r,
        Err(e) => {
            return Err(format!(
                "Failed to get installation token for {name} with error: {e}"
            ));
        }
    };
    let status = response.status();
    let date = date_header(&response);
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        assert_clock_in_sync(date.as_deref())?;
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
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        api,
        Duration::from_secs(30),
    )
    .await?;
    let url = format!("{api}/repos/{repository}");
    // A `githubApi()` read: tried again on connection errors and 5xx answers.
    let response = send_transient_retry(|| authed(http.get(&url), token))
        .await
        .map_err(|e| e.to_string())?;
    let answer = ApiAnswer::read(response).await;
    answer.successful()?;
    Ok(answer.data.get("id").and_then(Value::as_i64))
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
    let http = git_http::client(
        db,
        git_http::may_use_private_networks(source),
        api,
        Duration::from_secs(20),
    )
    .await?;
    let mut attempt = 1;
    loop {
        let r = http
            .get(format!("{api}/repos/{repository}"))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Accept", "application/vnd.github.v3+json")
            .header("Authorization", format!("Bearer {token}"))
            .send_retry()
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
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        Duration::from_secs(20),
    )
    .await?;
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
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        Duration::from_secs(20),
    )
    .await?;
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

/// What `githubApi()` returns, with the status of the answer.
#[derive(Debug, Clone)]
pub struct ApiAnswer {
    pub status: u16,
    /// `$response->header('X-RateLimit-Remaining')`.
    pub rate_limit_remaining: Option<String>,
    /// `$response->header('X-RateLimit-Reset')`.
    pub rate_limit_reset: Option<String>,
    /// `$response->json()`.
    pub data: Value,
}

impl ApiAnswer {
    async fn read(response: reqwest::Response) -> Self {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let rate_limit_remaining = header("X-RateLimit-Remaining");
        let rate_limit_reset = header("X-RateLimit-Reset");
        let status = response.status().as_u16();
        Self {
            status,
            rate_limit_remaining,
            rate_limit_reset,
            data: response.json().await.unwrap_or(Value::Null),
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The exception `githubApi()` throws for a failed answer when `$throwError` is true.
    pub fn successful(&self) -> Result<(), String> {
        if self.is_success() {
            return Ok(());
        }
        let reset = self
            .rate_limit_reset
            .as_deref()
            .and_then(|r| r.parse::<i64>().ok())
            .and_then(|r| chrono::DateTime::from_timestamp(r, 0))
            .unwrap_or_default()
            .format("%Y-%m-%d %H:%M:%S");
        let message = self
            .data
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no error message found");
        Err(format!(
            "GitHub API call failed:<br>Error: {message}<br>Rate Limit Status:<br>- Remaining Calls: {}<br>- Reset Time: {reset} UTC",
            self.rate_limit_remaining.as_deref().unwrap_or("0")
        ))
    }
}

/// `githubApi($source, $endpoint, $method, $data, throwError: false, timeout: $timeout)`: a
/// public source is asked without a token. Only reads are retried on connection errors and 5xx
/// answers (a repeated POST could create a second pull request comment). `Err` when the token
/// or the connection fails.
pub async fn api_request(
    db: &Db,
    enc: Option<&Encrypter>,
    source: &Record,
    method: reqwest::Method,
    endpoint: &str,
    data: Option<&Value>,
    timeout: Option<Duration>,
) -> Result<ApiAnswer, String> {
    let is_public = source
        .get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let token = if is_public {
        None
    } else {
        Some(installation_token_within(db, enc, Some(source), timeout).await?)
    };
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        request_timeout(timeout),
    )
    .await?;
    let url = format!("{api}/{}", endpoint.trim_start_matches('/'));
    let sends_data = data.is_some_and(|d| !is_empty_data(d))
        && [
            reqwest::Method::POST,
            reqwest::Method::PATCH,
            reqwest::Method::PUT,
        ]
        .contains(&method);
    let build = || {
        let request = http.request(method.clone(), &url);
        let request = match token.as_deref() {
            Some(token) => authed(request, token),
            None => request.header("Accept", "application/vnd.github.v3+json"),
        };
        match data {
            Some(d) if sends_data => request.json(d),
            _ => request,
        }
    };
    let response = if method == reqwest::Method::GET {
        send_transient_retry(build).await
    } else {
        build().send_retry().await
    }
    .map_err(|e| e.to_string())?;
    Ok(ApiAnswer::read(response).await)
}

/// PHP's `if ($data)`: an empty array is no data.
fn is_empty_data(data: &Value) -> bool {
    match data {
        Value::Null => true,
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// `githubApi($source, $endpoint, $method, $data, throwError: false)`: the status and the JSON
/// data. `Err` when the token or the connection fails.
pub async fn api(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    method: reqwest::Method,
    endpoint: &str,
    data: Option<&Value>,
) -> Result<(u16, Value), String> {
    let answer = api_request(db, Some(enc), source, method, endpoint, data, None).await?;
    Ok((answer.status, answer.data))
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
    let endpoint = format!("/repos/{repository}/pulls");
    let answer = api_request(db, enc, source, reqwest::Method::GET, &endpoint, None, None).await?;
    answer.successful()?;
    let remaining = answer.rate_limit_remaining;
    let mut rows = match answer.data {
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

/// `GET {api_url}/app` with the App's JWT: the status and JSON. A failed answer is first checked
/// for clock skew (`assertGithubClockInSync($response)`), whose message is the `Err`.
pub async fn app_info(db: &Db, source: &Record, jwt: &str) -> Result<(u16, Value), String> {
    jwt_get(db, source, jwt, "app", true).await
}

/// A GET with the App's JWT (`Http::GitSource($api_url)->withHeaders([...])->get(...)`).
async fn jwt_get(
    db: &Db,
    source: &Record,
    jwt: &str,
    endpoint: &str,
    check_clock: bool,
) -> Result<(u16, Value), String> {
    let api = field(source, "api_url").unwrap_or_default().to_owned();
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        Duration::from_secs(10),
    )
    .await?;
    let response = http
        .get(format!("{api}/{endpoint}"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .send_retry()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if check_clock && !status.is_success() {
        assert_clock_in_sync(date_header(&response).as_deref())?;
    }
    Ok((
        status.as_u16(),
        response.json().await.unwrap_or(Value::Null),
    ))
}

/// `GithubAppPermissionJob`: the answer of `GET /app` (the permissions and events to store on the
/// row), with the permissions the installation approved under `installation_permissions`.
pub async fn fetch_permissions(db: &Db, enc: &Encrypter, source: &Record) -> Result<Value, String> {
    let jwt = jwt(db, enc, source).await?;
    let (status, mut body) = app_info(db, source, &jwt).await?;
    if !(200..300).contains(&status) {
        return Err(format!("Failed to fetch GitHub app permissions: {}", body));
    }
    // `approvedInstallationPermission()`: GET /app returns the permissions the App requests. A
    // new permission counts only after the installation approves it.
    let installation = id_text(source, "installation_id");
    let approved = if source.get("installation_id").is_none_or(Value::is_null)
        || installation.is_empty()
        || installation == "0"
    {
        Value::Null
    } else {
        let endpoint = format!("app/installations/{installation}");
        let (status, answer) = jwt_get(db, source, &jwt, &endpoint, false).await?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "Failed to fetch GitHub app installation permissions: {}",
                answer
            ));
        }
        answer.get("permissions").cloned().unwrap_or(Value::Null)
    };
    if let Some(map) = body.as_object_mut() {
        map.insert("installation_permissions".into(), approved);
    }
    Ok(body)
}

/// The columns `GithubAppPermissionJob::handle()` stores, from [`fetch_permissions`]' answer:
/// `(column, value)` in upstream's order, and the webhook events.
pub fn permission_columns(info: &Value) -> (Vec<(&'static str, Option<String>)>, Value) {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let requested = |k: &str| text(info.get("permissions").and_then(|p| p.get(k)));
    let columns = vec![
        ("contents", requested("contents")),
        ("metadata", requested("metadata")),
        ("pull_requests", requested("pull_requests")),
        (
            "deployments",
            text(
                info.get("installation_permissions")
                    .and_then(|p| p.get("deployments")),
            ),
        ),
        ("administration", requested("administration")),
        (
            "organization_self_hosted_runners",
            requested("organization_self_hosted_runners"),
        ),
        ("actions", requested("actions")),
    ];
    let events = info
        .get("events")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    (columns, events)
}

/// `GithubAppPermissionJob::handle()`: fetches the permissions and stores them on the App.
pub async fn sync_permissions(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    now: chrono::NaiveDateTime,
) -> Result<(), String> {
    let info = fetch_permissions(db, enc, source).await?;
    let (columns, events) = permission_columns(&info);
    let value = |name: &str| {
        columns
            .iter()
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| v.clone())
    };
    sqlx::query(
        "UPDATE github_apps SET contents = $1, metadata = $2, pull_requests = $3, deployments = $4, administration = $5, \
         organization_self_hosted_runners = $6, actions = $7, webhook_events = $8::json, updated_at = $9 WHERE id = $10",
    )
    .bind(value("contents"))
    .bind(value("metadata"))
    .bind(value("pull_requests"))
    .bind(value("deployments"))
    .bind(value("administration"))
    .bind(value("organization_self_hosted_runners"))
    .bind(value("actions"))
    .bind(events.to_string())
    .bind(now)
    .bind(source.get("id").and_then(Value::as_i64))
    .execute(db.pool())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// `GithubApp::missingDeploymentRequirements()`: permissions that GitHub deployments need but the
/// App does not have.
pub fn missing_deployment_requirements(source: &Record) -> Vec<&'static str> {
    if field(source, "deployments") == Some("write") {
        Vec::new()
    } else {
        vec!["Repository permission \"Deployments\": write"]
    }
}

/// `GithubApp::canSyncDeployments()`: whether Coolify can create GitHub Deployments and their
/// statuses with this App.
pub fn can_sync_deployments(source: &Record) -> bool {
    let is_public = source
        .get("is_public")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    !is_public && missing_deployment_requirements(source).is_empty()
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
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &api,
        Duration::from_secs(30),
    )
    .await?;
    let jwt = app_jwt(&key, &id_text(source, "app_id"))?;
    let response = http
        .get(format!("{api}/app"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Authorization", format!("Bearer {jwt}"))
        .send_retry()
        .await
        .map_err(|e| e.to_string())?;
    let ok = response.status().is_success();
    let date = date_header(&response);
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !ok {
        assert_clock_in_sync(date.as_deref())?;
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
pub async fn manifest_conversion(
    db: &Db,
    allow_private: bool,
    api_url: &str,
    code: &str,
) -> Result<Value, String> {
    let http = git_http::client(Some(db), allow_private, api_url, Duration::from_secs(10)).await?;
    let response = http
        .post(format!("{api_url}/app-manifests/{code}/conversions"))
        .header("Accept", "application/vnd.github+json")
        .send_retry()
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
        let http = git_http::client(
            Some(db),
            git_http::may_use_private_networks(source),
            &api,
            Duration::from_secs(10),
        )
        .await?;
        let response = http
            .get(format!("{api}/app/installations/{installation_id}"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .send_retry()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            assert_clock_in_sync(date_header(&response).as_deref())?;
        }
        Ok::<_, String>(response)
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

    /// Answers 503 to the first `fail` requests, then 200.
    async fn flaky_api(fail: usize) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
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
                let seen = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let (status, body) = if seen < fail {
                    ("503 Service Unavailable", "<html>unicorn</html>")
                } else {
                    ("200 OK", r#"{"ok":true}"#)
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn reads_are_retried_on_server_errors() {
        let step = Duration::from_millis(1);
        let client = reqwest::Client::new();
        // Two 5xx answers, then the real one.
        let (api, hits) = flaky_api(2).await;
        let answer = send_transient_retry_every(|| client.get(format!("{api}/x")), step)
            .await
            .unwrap();
        assert_eq!(answer.status().as_u16(), 200);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
        // Three tries at most: the last 5xx answer is returned, not thrown.
        let (api, hits) = flaky_api(5).await;
        let answer = send_transient_retry_every(|| client.get(format!("{api}/x")), step)
            .await
            .unwrap();
        assert_eq!(answer.status().as_u16(), 503);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
        // A 4xx answer is not transient.
        let (api, hits) = fake_api().await;
        let answer = send_transient_retry_every(|| client.get(format!("{api}/nope")), step)
            .await
            .unwrap();
        assert_eq!(answer.status().as_u16(), 404);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn clock_skew_is_read_from_the_date_header() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // No header or an unreadable one: nothing to compare.
        assert_eq!(clock_skew(None, now), Ok(()));
        assert_eq!(clock_skew(Some(""), now), Ok(()));
        assert_eq!(clock_skew(Some("not a date"), now), Ok(()));
        // Within 50 seconds is in sync.
        assert_eq!(
            clock_skew(Some("Sun, 01 Nov 2026 12:00:50 GMT"), now),
            Ok(())
        );
        let error = clock_skew(Some("Sun, 01 Nov 2026 12:05:00 GMT"), now).unwrap_err();
        assert_eq!(
            error,
            "System time is out of sync with GitHub API time:<br>- System time: 2026-11-01 12:00:00 UTC<br>- GitHub time: 2026-11-01 12:05:00 UTC<br>- Difference: 300 seconds<br>Please synchronize your system clock."
        );
    }

    #[test]
    fn deployments_need_the_write_permission_of_a_private_app() {
        let source = |v: Value| -> Record { serde_json::from_value(v).unwrap() };
        let ready = source(serde_json::json!({"is_public": false, "deployments": "write"}));
        assert!(missing_deployment_requirements(&ready).is_empty());
        assert!(can_sync_deployments(&ready));
        for other in [
            serde_json::json!({"is_public": false, "deployments": "read"}),
            serde_json::json!({"is_public": false, "deployments": null}),
            serde_json::json!({"is_public": false}),
        ] {
            let row = source(other);
            assert_eq!(
                missing_deployment_requirements(&row),
                vec!["Repository permission \"Deployments\": write"]
            );
            assert!(!can_sync_deployments(&row));
        }
        let public = source(serde_json::json!({"is_public": true, "deployments": "write"}));
        assert!(!can_sync_deployments(&public));
    }

    #[test]
    fn the_deployments_permission_is_the_one_the_installation_approved() {
        // GET /app requests deployments: write, the installation has not approved it yet.
        let info = serde_json::json!({
            "permissions": {"contents": "read", "metadata": "read", "pull_requests": "write", "deployments": "write"},
            "events": ["push", "pull_request"],
            "installation_permissions": {"contents": "read"},
        });
        let (columns, events) = permission_columns(&info);
        let get = |k: &str| columns.iter().find(|(c, _)| *c == k).unwrap().1.clone();
        assert_eq!(get("contents").as_deref(), Some("read"));
        assert_eq!(get("pull_requests").as_deref(), Some("write"));
        assert_eq!(get("deployments"), None);
        assert_eq!(get("administration"), None);
        assert_eq!(events, serde_json::json!(["push", "pull_request"]));
        let approved = serde_json::json!({"permissions": {}, "installation_permissions": {"deployments": "write"}});
        let (columns, events) = permission_columns(&approved);
        let deployments = columns.iter().find(|(c, _)| *c == "deployments").unwrap();
        assert_eq!(deployments.1.as_deref(), Some("write"));
        assert_eq!(events, serde_json::json!([]));
    }

    #[test]
    fn failed_answers_carry_the_rate_limit() {
        let answer = ApiAnswer {
            status: 403,
            rate_limit_remaining: Some("0".into()),
            rate_limit_reset: Some("1700000000".into()),
            data: serde_json::json!({"message": "API rate limit exceeded"}),
        };
        assert_eq!(
            answer.successful().unwrap_err(),
            "GitHub API call failed:<br>Error: API rate limit exceeded<br>Rate Limit Status:<br>- Remaining Calls: 0<br>- Reset Time: 2023-11-14 22:13:20 UTC"
        );
        let ok = ApiAnswer {
            status: 201,
            rate_limit_remaining: None,
            rate_limit_reset: None,
            data: Value::Null,
        };
        assert!(ok.successful().is_ok());
        assert!(is_empty_data(&serde_json::json!({})));
        assert!(!is_empty_data(&serde_json::json!({"state": "success"})));
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
