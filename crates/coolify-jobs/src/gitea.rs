//! `bootstrap/helpers/gitea.php`: Gitea OAuth tokens, API calls, repositories and branches, plus
//! the token refresh flow it shares with GitLab.

use crate::git_http::SendRetry as _;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use coolify_compat::encrypter::Encrypter;
use coolify_db::Db;
use coolify_db::records::Record;
use serde_json::{Value, json};

use crate::git_http::{self, path_str};

/// `GITEA_PAGE_SIZE`.
const PAGE_SIZE: usize = 50;

pub(crate) fn field<'a>(source: &'a Record, k: &str) -> Option<&'a str> {
    source
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// `GiteaApp::apiUrlBase()`.
pub fn api_url_base(source: &Record) -> String {
    let mut url = field(source, "api_url")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    if !url.contains("/api/v1") {
        url.push_str("/api/v1");
    }
    url
}

/// `GiteaApp::allowsRepositoryOwner($owner)`: the organization field limits the source to
/// repositories of these owners. Empty allows all owners.
pub fn allows_repository_owner(source: &Record, owner: Option<&str>) -> bool {
    let owner = owner.unwrap_or_default().to_lowercase();
    let mut organizations = field(source, "organization")
        .unwrap_or_default()
        .split(',')
        .map(|o| o.trim().to_lowercase())
        .filter(|o| !o.is_empty() && o != "0")
        .peekable();
    organizations.peek().is_none() || organizations.any(|o| o == owner)
}

/// The differences between `refreshGitlabToken()` and `refreshGiteaToken()`.
pub(crate) struct TokenFlow {
    pub table: &'static str,
    pub provider: &'static str,
    pub token_path: &'static str,
    pub send_redirect_uri: bool,
    pub default_expires_in: i64,
    /// Gitea may answer without a new refresh token (`?? $source->refresh_token`).
    pub keep_refresh_token: bool,
}

type LockMap = HashMap<(&'static str, i64), Arc<tokio::sync::Mutex<()>>>;

/// `Cache::lock("gitea_token_refresh_{$source->id}", 20)`, per process.
static LOCKS: LazyLock<Mutex<LockMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_of(table: &'static str, id: i64) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.entry((table, id)).or_default().clone()
}

async fn reload(db: &Db, table: &str, id: i64) -> Result<Record, String> {
    coolify_db::git_sources::source_row(db, table, id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No query results for model.".to_owned())
}

/// The shared body of `refreshGitlabToken()` / `refreshGiteaToken()`; answers with the access
/// token to use.
pub(crate) async fn refresh(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    flow: &TokenFlow,
) -> Result<String, String> {
    const MARGIN: i64 = 60;
    let provider = flow.provider;
    let decrypt = |row: &Record, k: &str| -> Option<String> {
        field(row, k).and_then(|c| enc.decrypt_maybe_serialized(c).ok())
    };
    if decrypt(source, "refresh_token").is_none() {
        // `refreshGiteaToken()` names both products the source can be.
        if flow.table == "gitea_apps" {
            return Err("Gitea / Forgejo source has no refresh token. Please reconnect the Gitea / Forgejo App.".to_owned());
        }
        return Err(format!(
            "{provider} source has no refresh token. Please reconnect the {provider} app."
        ));
    }
    let fresh = |row: &Record| {
        let now = chrono::Utc::now().timestamp();
        row.get("expires_at")
            .and_then(Value::as_i64)
            .is_some_and(|e| e > now + MARGIN)
    };
    let current = |row: &Record| decrypt(row, "access_token").unwrap_or_default();
    if fresh(source) {
        return Ok(current(source));
    }
    let id = source.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let lock = lock_of(flow.table, id);
    let Ok(_guard) = tokio::time::timeout(Duration::from_secs(20), lock.lock()).await else {
        let row = reload(db, flow.table, id).await?;
        if fresh(&row) {
            return Ok(current(&row));
        }
        return Err(format!(
            "{provider} token refresh timed out. Please try again."
        ));
    };
    let row = reload(db, flow.table, id).await?;
    if fresh(&row) {
        return Ok(current(&row));
    }
    let base = field(&row, "html_url")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(&row),
        &base,
        Duration::from_secs(30),
    )
    .await?;
    let client_id = field(&row, "client_id").unwrap_or_default().to_owned();
    let client_secret = decrypt(&row, "client_secret").unwrap_or_default();
    let refresh_token = decrypt(&row, "refresh_token").unwrap_or_default();
    let redirect_uri = field(&row, "redirect_uri").unwrap_or_default().to_owned();
    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if flow.send_redirect_uri {
        form.push(("redirect_uri", redirect_uri.as_str()));
    }
    let response = http
        .post(format!("{base}{}", flow.token_path))
        .form(&form)
        .send_retry()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Failed to refresh {provider} token: {}",
            git_http::error_text(status.as_u16(), &body, "error_description")
        ));
    }
    let data: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let access = data
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "Failed to refresh {provider} token: {}",
                git_http::unexpected_response(status.as_u16(), &body)
            )
        })?
        .to_owned();
    let new_refresh = match data.get("refresh_token").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None if flow.keep_refresh_token => refresh_token.clone(),
        None => {
            return Err(format!(
                "Failed to refresh {provider} token: {}",
                git_http::unexpected_response(status.as_u16(), &body)
            ));
        }
    };
    let expires_in = data
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(flow.default_expires_in);
    coolify_db::git_sources::store_oauth_tokens(
        db,
        flow.table,
        id,
        &enc.encrypt_string(&access),
        &enc.encrypt_string(&new_refresh),
        chrono::Utc::now().timestamp() + expires_in,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(access)
}

/// `refreshGiteaToken($source)`.
pub async fn access_token(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    refresh(
        db,
        enc,
        source,
        &TokenFlow {
            table: "gitea_apps",
            provider: "Gitea",
            token_path: "/login/oauth/access_token",
            send_redirect_uri: false,
            default_expires_in: 3600,
            keep_refresh_token: true,
        },
    )
    .await
}

/// `generateGiteaCloneToken($source)`.
pub async fn clone_token(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    access_token(db, enc, source).await
}

/// `giteaApi($source, $endpoint, $method, $data)`: the JSON data and `x-total-count`.
pub async fn api(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    method: reqwest::Method,
    endpoint: &str,
    data: Option<&Value>,
) -> Result<(Value, i64), String> {
    let token = access_token(db, enc, source).await?;
    let base = api_url_base(source);
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &base,
        Duration::from_secs(20),
    )
    .await?;
    let url = format!("{base}/{}", endpoint.trim_start_matches('/'));
    let response = git_http::retry(|| {
        let r = http
            .request(method.clone(), &url)
            .header("Accept", "application/json")
            .bearer_auth(&token);
        match data {
            Some(d) => r.json(d),
            None => r,
        }
    })
    .await?;
    let status = response.status();
    let total = response
        .headers()
        .get("x-total-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Gitea API call failed: {}",
            git_http::error_text(status.as_u16(), &body, "message")
        ));
    }
    Ok((serde_json::from_str(&body).unwrap_or(Value::Null), total))
}

/// `Http::Gitea($source->apiUrlBase(), $token)->timeout(20)->retry(3, 200, throw: false)->get("/repos/{owner}/{name}")`
/// (the repository check of `ApplicationsController::giteaAppRepository()`): the status and JSON
/// body of the last attempt. `Err` is the connection error of the last try.
pub async fn repository_access(
    db: Option<&Db>,
    source: &Record,
    token: &str,
    repository_path: &str,
) -> Result<(u16, Value), String> {
    let base = api_url_base(source);
    let http = git_http::client(
        db,
        git_http::may_use_private_networks(source),
        &base,
        Duration::from_secs(20),
    )
    .await?;
    let path = repository_path
        .split('/')
        .map(raw_url_encode)
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("{base}/repos/{path}");
    let response = git_http::retry(|| {
        http.get(&url)
            .header("Accept", "application/json")
            .bearer_auth(token)
    })
    .await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

/// `loadGiteaRepositories($source, $page)`.
pub async fn load_repositories(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    page: i64,
) -> Result<Value, String> {
    let token = access_token(db, enc, source).await?;
    let base = api_url_base(source);
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &base,
        Duration::from_secs(20),
    )
    .await?;
    let url = format!("{base}/user/repos");
    let page_text = page.to_string();
    let limit = PAGE_SIZE.to_string();
    let response = git_http::retry(|| {
        http.get(&url)
            .header("Accept", "application/json")
            .bearer_auth(&token)
            .query(&[("limit", limit.as_str()), ("page", page_text.as_str())])
    })
    .await?;
    if !response.status().is_success() {
        return Ok(json!({"total_count": 0, "has_more": false, "repositories": []}));
    }
    let list: Vec<Value> = response.json().await.unwrap_or_default();
    let has_more = list.len() == PAGE_SIZE;
    let repositories: Vec<Value> = list
        .iter()
        .filter(|r| allows_repository_owner(source, path_str(r, "owner.login")))
        .map(|r| {
            json!({
                "id": r.get("id"),
                "name": r.get("name"),
                "full_name": r.get("full_name"),
                "default_branch": r.get("default_branch").filter(|v| !v.is_null()).cloned().unwrap_or(json!("main")),
                "html_url": r.get("html_url"),
                "owner": path_str(r, "owner.login"),
            })
        })
        .collect();
    Ok(json!({
        "total_count": repositories.len(),
        "has_more": has_more,
        "repositories": repositories,
    }))
}

/// `loadGiteaBranches($source, $fullName, $page)`.
pub async fn load_branches(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    full_name: &str,
    page: i64,
) -> Result<Value, String> {
    let token = access_token(db, enc, source).await?;
    let base = api_url_base(source);
    let http = git_http::client(
        Some(db),
        git_http::may_use_private_networks(source),
        &base,
        Duration::from_secs(20),
    )
    .await?;
    let path = full_name
        .splitn(2, '/')
        .map(raw_url_encode)
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("{base}/repos/{path}/branches");
    let page_text = page.to_string();
    let limit = PAGE_SIZE.to_string();
    let response = git_http::retry(|| {
        http.get(&url)
            .header("Accept", "application/json")
            .bearer_auth(&token)
            .query(&[("limit", limit.as_str()), ("page", page_text.as_str())])
    })
    .await?;
    if !response.status().is_success() {
        return Ok(json!([]));
    }
    Ok(response.json().await.unwrap_or_else(|_| json!([])))
}

/// PHP `rawurlencode()`.
pub fn raw_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(organization: Value) -> Record {
        let mut row = Record::new();
        row.insert("organization".into(), organization);
        row
    }

    #[test]
    fn the_organization_field_limits_repository_owners() {
        // Empty allows all owners.
        for empty in [Value::Null, json!(""), json!(" , ")] {
            let row = source(empty);
            assert!(allows_repository_owner(&row, Some("anyone")));
            assert!(allows_repository_owner(&row, None));
        }
        let row = source(json!("Acme, coollabs"));
        assert!(allows_repository_owner(&row, Some("acme")));
        assert!(allows_repository_owner(&row, Some("CoolLabs")));
        assert!(!allows_repository_owner(&row, Some("someone")));
        assert!(!allows_repository_owner(&row, None));
    }
}
