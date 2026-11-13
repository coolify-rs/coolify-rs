//! `bootstrap/helpers/gitlab.php`: GitLab OAuth tokens, API calls, repositories and branches.

use std::time::Duration;

use coolify_compat::encrypter::Encrypter;
use coolify_db::Db;
use coolify_db::records::Record;
use serde_json::{Value, json};

use crate::git_http::{self, path_str, path_value};
use crate::gitea::{TokenFlow, field, refresh};

/// `GitlabApp::apiUrlBase()`.
pub fn api_url_base(source: &Record) -> String {
    let mut url = field(source, "api_url")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    if !url.contains("/api/v4") {
        url.push_str("/api/v4");
    }
    url
}

/// `refreshGitlabToken($source)`: the current access token, refreshed when it is about to expire.
pub async fn access_token(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    refresh(
        db,
        enc,
        source,
        &TokenFlow {
            table: "gitlab_apps",
            provider: "GitLab",
            token_path: "/oauth/token",
            send_redirect_uri: true,
            default_expires_in: 7200,
            keep_refresh_token: false,
        },
    )
    .await
}

/// `generateGitlabCloneToken($source)`.
pub async fn clone_token(db: &Db, enc: &Encrypter, source: &Record) -> Result<String, String> {
    access_token(db, enc, source).await
}

/// `gitlabApi($source, $endpoint, $method, $data)`: the JSON data and `x-total`.
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
        .get("x-total")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "GitLab API call failed: {}",
            git_http::error_text(status.as_u16(), &body, "message")
        ));
    }
    Ok((serde_json::from_str(&body).unwrap_or(Value::Null), total))
}

/// `loadGitlabRepositories($source, $page)`.
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
    let url = format!("{base}/projects");
    let page_text = page.to_string();
    let response = git_http::retry(|| {
        http.get(&url)
            .header("Accept", "application/json")
            .bearer_auth(&token)
            .query(&[
                ("membership", "true"),
                ("per_page", "100"),
                ("page", page_text.as_str()),
                ("order_by", "name"),
                ("sort", "asc"),
            ])
    })
    .await?;
    if !response.status().is_success() {
        return Ok(json!({"total_count": 0, "has_more": false, "repositories": []}));
    }
    let projects: Vec<Value> = response.json().await.unwrap_or_default();
    let has_more = projects.len() == 100;
    let groups: Vec<String> = field(source, "group_name")
        .unwrap_or_default()
        .split(',')
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty())
        .collect();
    let repositories: Vec<Value> = projects
        .iter()
        .filter(|p| {
            if groups.is_empty() {
                return true;
            }
            let namespace = path_str(p, "namespace.full_path")
                .unwrap_or_default()
                .to_lowercase();
            groups
                .iter()
                .any(|g| namespace == *g || namespace.starts_with(&format!("{g}/")))
        })
        .map(|p| {
            json!({
                "id": p.get("id"),
                "name": p.get("name"),
                "path_with_namespace": p.get("path_with_namespace"),
                "default_branch": p.get("default_branch").filter(|v| !v.is_null()).cloned().unwrap_or(json!("main")),
                "web_url": p.get("web_url"),
                "namespace": {
                    "full_path": path_value(p, "namespace.full_path"),
                    "kind": path_value(p, "namespace.kind"),
                },
            })
        })
        .collect();
    Ok(json!({
        "total_count": repositories.len(),
        "has_more": has_more,
        "repositories": repositories,
    }))
}

/// `loadGitlabBranches($source, $projectId, $page)`.
pub async fn load_branches(
    db: &Db,
    enc: &Encrypter,
    source: &Record,
    project_id: i64,
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
    let url = format!("{base}/projects/{project_id}/repository/branches");
    let page_text = page.to_string();
    let response = git_http::retry(|| {
        http.get(&url)
            .header("Accept", "application/json")
            .bearer_auth(&token)
            .query(&[("per_page", "100"), ("page", page_text.as_str())])
    })
    .await?;
    if !response.status().is_success() {
        return Ok(json!([]));
    }
    Ok(response.json().await.unwrap_or_else(|_| json!([])))
}
