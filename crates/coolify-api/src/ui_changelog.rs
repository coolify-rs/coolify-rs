//! `App\Livewire\SettingsDropdown` and `App\Services\ChangelogService` (livewire/settings-dropdown.blade.php).
//! Entries come from `{COOLIFY_RS_DATA_DIR}/changelogs/*.json`, upstream's monthly files.
//! `PullChangelog` is not ported, because it fetches them from upstream's CDN.

use std::sync::LazyLock;

use axum::extract::State;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Value, json};

use crate::response::{ApiResult, ok};
use crate::session::SessionUser;
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/changelog", get(show))
        .route("/ui/changelog/read", post(mark_read))
        .route("/ui/changelog/read-all", post(mark_all_read))
        .route_layer(from_fn_with_state(state, crate::session::user_guard))
}

fn changelog_dir() -> std::path::PathBuf {
    std::env::var_os("COOLIFY_RS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("changelogs")
}

/// `ChangelogService::applyCustomStyling`.
fn apply_styling(html: &str) -> String {
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"<h1[^>]*>", r#"<h1 class="text-xl font-bold dark:text-white mb-2">"#),
            (r"<h2[^>]*>", r#"<h2 class="text-lg font-semibold dark:text-white mb-2">"#),
            (r"<h3[^>]*>", r#"<h3 class="text-md font-semibold dark:text-white mb-1">"#),
            (r"<p[^>]*>", r#"<p class="mb-2 dark:text-neutral-300">"#),
            (r"<ul[^>]*>", r#"<ul class="mb-2 ml-4 list-disc">"#),
            (r"<ol[^>]*>", r#"<ol class="mb-2 ml-4 list-decimal">"#),
            (r"<li[^>]*>", r#"<li class="dark:text-neutral-300">"#),
            (
                r"<pre[^>]*>",
                r#"<pre class="bg-gray-100 dark:bg-coolgray-300 p-2 rounded text-sm overflow-x-auto my-2">"#,
            ),
            (
                r"<code[^>]*>",
                r#"<code class="bg-gray-100 dark:bg-coolgray-300 px-1 py-0.5 rounded text-sm">"#,
            ),
            (
                r"<a([^>]*)>",
                r#"<a$1 class="text-blue-500 hover:text-blue-600 underline" target="_blank" rel="noopener">"#,
            ),
            (r"<strong[^>]*>", r#"<strong class="font-semibold dark:text-white">"#),
            (r"<em[^>]*>", r#"<em class="italic dark:text-neutral-300">"#),
        ]
        .into_iter()
        .filter_map(|(p, r)| Regex::new(p).ok().map(|re| (re, r)))
        .collect()
    });
    let mut out = html.to_string();
    for (re, replacement) in RULES.iter() {
        out = re.replace_all(&out, *replacement).into_owned();
    }
    autolink(&out)
}

/// The last `preg_replace` of `applyCustomStyling`: plain URLs outside tags and `<a>` elements.
fn autolink(html: &str) -> String {
    static RE: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#"(?s)<a\b[^>]*>.*?</a>|<[^>]+>|https?://[^\s<>"]+"#).ok());
    let Some(re) = RE.as_ref() else {
        return html.to_string();
    };
    re.replace_all(html, |c: &regex::Captures<'_>| {
        let m = &c[0];
        if m.starts_with('<') {
            m.to_string()
        } else {
            format!(
                r#"<a href="{m}" class="text-blue-500 hover:text-blue-600 underline" target="_blank" rel="noopener">{m}</a>"#
            )
        }
    })
    .into_owned()
}

/// `ChangelogService::parseMarkdown`.
fn parse_markdown(content: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(content, pulldown_cmark::Options::ENABLE_TABLES);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    apply_styling(&html)
}

/// `ChangelogService::getEntries`: the three newest months, without future entries.
async fn entries() -> Vec<Value> {
    let mut months: Vec<String> = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(changelog_dir()).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            if let Some(n) = e.file_name().to_str().and_then(|n| n.strip_suffix(".json")) {
                months.push(n.to_string());
            }
        }
    }
    months.sort();
    months.reverse();
    months.truncate(3);
    let now = Utc::now();
    let mut out = Vec::new();
    for month in months {
        let Ok(text) =
            tokio::fs::read_to_string(changelog_dir().join(format!("{month}.json"))).await
        else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for entry in data
            .get("entries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |k: &str| {
                entry
                    .get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            };
            let (Some(tag), Some(title), Some(content), Some(published)) = (
                field("tag_name"),
                field("title"),
                field("content"),
                field("published_at"),
            ) else {
                continue;
            };
            let at = DateTime::parse_from_rfc3339(published)
                .map(|d| d.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(published, "%Y-%m-%d %H:%M:%S")
                        .map(|d| d.and_utc())
                });
            let Ok(at) = at else { continue };
            if at > now {
                continue;
            }
            out.push(json!({
                "tag_name": tag,
                "title": title,
                "content": content,
                "content_html": parse_markdown(content),
                "published_at": at.to_rfc3339(),
            }));
        }
    }
    out
}

async fn read_tags(
    state: &AppState,
    user_id: i64,
) -> Result<Vec<String>, crate::response::ApiError> {
    Ok(
        sqlx::query_scalar("SELECT release_tag FROM user_changelog_reads WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(state.db.pool())
            .await
            .map_err(coolify_db::DbError::from)?,
    )
}

/// `getEntriesForUser` and `getUnreadCountForUser`.
async fn show(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    let read = read_tags(&state, session.user_id).await?;
    let mut list = entries().await;
    for e in &mut list {
        let is_read = e
            .get("tag_name")
            .and_then(Value::as_str)
            .is_some_and(|t| read.iter().any(|r| r == t));
        e["is_read"] = json!(is_read);
    }
    let unread = list.iter().filter(|e| e["is_read"] == json!(false)).count();
    Ok(ok(json!({
        "entries": list,
        "unread": unread,
        "current_version": format!("v{}", state.version),
    }))
    .into_response())
}

/// `UserChangelogRead::markAsRead`.
async fn mark(state: &AppState, user_id: i64, tag: &str) -> Result<(), crate::response::ApiError> {
    state.db.ensure_writable()?;
    let now = state.now();
    let updated = sqlx::query(
        "UPDATE user_changelog_reads SET read_at = $3, updated_at = $3 WHERE user_id = $1 AND release_tag = $2",
    )
    .bind(user_id)
    .bind(tag)
    .bind(now)
    .execute(state.db.pool())
    .await
    .map_err(coolify_db::DbError::from)?;
    if updated.rows_affected() == 0 {
        sqlx::query(
            "INSERT INTO user_changelog_reads (user_id, release_tag, read_at, created_at, updated_at) VALUES ($1, $2, $3, $3, $3)",
        )
        .bind(user_id)
        .bind(tag)
        .bind(now)
        .execute(state.db.pool())
        .await
        .map_err(coolify_db::DbError::from)?;
    }
    Ok(())
}

async fn mark_read(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Json(body): Json<Value>,
) -> ApiResult {
    let tag = body.get("tag").and_then(Value::as_str).unwrap_or_default();
    mark(&state, session.user_id, tag).await?;
    Ok(ok(json!({"message": "ok"})).into_response())
}

async fn mark_all_read(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    for e in entries().await {
        if let Some(tag) = e.get("tag_name").and_then(Value::as_str) {
            mark(&state, session.user_id, tag).await?;
        }
    }
    Ok(ok(json!({"message": "ok"})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_gets_upstream_classes() {
        let html = parse_markdown("## Fixes\n\n- One `x`\n\n[a](https://x.test)");
        assert!(html.contains(r#"<h2 class="text-lg font-semibold dark:text-white mb-2">Fixes"#));
        assert!(html.contains(r#"<ul class="mb-2 ml-4 list-disc">"#));
        assert!(html.contains(r#"target="_blank""#));
        let plain = parse_markdown("see https://x.test/1 and [b](https://x.test/2)");
        assert!(plain.contains(r#"<a href="https://x.test/1" class="#));
        assert_eq!(plain.matches("<a ").count(), 2);
    }
}
