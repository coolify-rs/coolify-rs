//! `AuditEventsController`, and the Eloquent shape of rows and paginators.

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::audit::{self, Filter};
use coolify_db::records::{Record, laravel_datetime};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::lenient_json;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::validation::{Field, Rule, validate};

const BASE_COLUMNS: &[&str] = &[
    "id",
    "team_id",
    "event",
    "source",
    "action",
    "level",
    "actor_type",
    "actor_id",
    "actor_name",
    "resource_type",
    "resource_uuid",
    "resource_name",
    "description",
    "created_at",
];
const SENSITIVE_COLUMNS: &[&str] = &[
    "actor_email",
    "actor_token_id",
    "actor_token_name",
    "metadata",
    "changes",
    "ip_address",
    "user_agent",
];

/// `casts()`: `created_at` as a date, `metadata` decoded, `changes` decrypted.
pub(crate) fn present_row(state: &AppState, mut r: Record) -> Value {
    if let Some(Value::String(t)) = r.get("created_at") {
        let v = laravel_datetime(t);
        r.insert("created_at".into(), Value::String(v));
    }
    if let Some(Value::String(c)) = r.get("changes").cloned() {
        let plain = state
            .encrypter
            .as_ref()
            .and_then(|e| e.decrypt_string(&c).ok())
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        r.insert("changes".into(), plain);
    }
    Value::Object(r)
}

/// `LengthAwarePaginator::toArray()` with sorted keys (`serializeApiResponse`).
pub(crate) fn paginator(
    path: &str,
    data: Vec<Value>,
    total: i64,
    per_page: i64,
    page: i64,
) -> Value {
    let last = ((total + per_page - 1) / per_page).max(1);
    let url = |p: i64| format!("{path}?page={p}");
    let count = data.len() as i64;
    let from = (count > 0).then(|| (page - 1) * per_page + 1);
    let to = from.map(|f| f + count - 1);
    let mut links = vec![json!({
        "url": (page > 1).then(|| url(page - 1)),
        "label": "&laquo; Previous",
        "page": (page > 1).then_some(page - 1),
        "active": false,
    })];
    for p in window(page, last) {
        match p {
            Some(p) => links.push(json!({
                "url": url(p),
                "label": p.to_string(),
                "page": p,
                "active": p == page,
            })),
            None => links.push(json!({"url": null, "label": "...", "page": null, "active": false})),
        }
    }
    links.push(json!({
        "url": (page < last).then(|| url(page + 1)),
        "label": "Next &raquo;",
        "page": (page < last).then_some(page + 1),
        "active": false,
    }));
    json!({
        "current_page": page,
        "data": data,
        "first_page_url": url(1),
        "from": from,
        "last_page": last,
        "last_page_url": url(last),
        "links": links,
        "next_page_url": (page < last).then(|| url(page + 1)),
        "path": path,
        "per_page": per_page,
        "prev_page_url": (page > 1).then(|| url(page - 1)),
        "to": to,
        "total": total,
    })
}

/// `UrlWindow` with `onEachSide = 3`.
fn window(page: i64, last: i64) -> Vec<Option<i64>> {
    const EACH: i64 = 3;
    const WINDOW: i64 = EACH + 4;
    let range = |a: i64, b: i64| (a..=b).map(Some).collect::<Vec<_>>();
    if last < EACH * 2 + 8 {
        return range(1, last);
    }
    let mut v = Vec::new();
    if page <= WINDOW {
        v.extend(range(1, WINDOW + EACH));
    } else if page > last - WINDOW {
        v.extend(range(1, 2));
        v.push(None);
        v.extend(range(last - (WINDOW + EACH - 1), last));
        return v;
    } else {
        v.extend(range(1, 2));
        v.push(None);
        v.extend(range(page - EACH, page + EACH));
    }
    v.push(None);
    v.extend(range(last - 1, last));
    v
}

/// The request URL as `url()->current()`: scheme and host of the request.
pub(crate) fn current_url(headers: &HeaderMap, path: &str) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}{path}")
}

pub async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "Only team admins and owners can view audit logs.",
        ));
    }
    let input = lenient_json(&HeaderMap::new(), &uri, b"");
    validate(
        &input,
        &[
            Field {
                name: "per_page",
                rules: vec![Rule::Integer, Rule::MinNum(1), Rule::MaxNum(100)],
            },
            Field {
                name: "page",
                rules: vec![Rule::Integer, Rule::MinNum(1)],
            },
            Field {
                name: "search",
                rules: vec![Rule::Nullable, Rule::String, Rule::Max(255)],
            },
            Field {
                name: "action",
                rules: vec![Rule::Nullable, Rule::String, Rule::Max(255)],
            },
            Field {
                name: "source",
                rules: vec![
                    Rule::Nullable,
                    Rule::String,
                    Rule::In(&["all", "ui", "api", "mcp", "webhook", "system", "scheduler"]),
                ],
            },
        ],
        &[],
        &input.keys().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let int = |k: &str| {
        input.get(k).and_then(|v| match v {
            Value::String(s) => s.parse::<i64>().ok(),
            Value::Number(n) => n.as_i64(),
            _ => None,
        })
    };
    let per_page = int("per_page").unwrap_or(25);
    let page = int("page").unwrap_or(1);
    let opt = |k: &str| input.str(k).filter(|s| *s != "all").map(str::to_string);
    let filter = Filter {
        search: input.str("search").unwrap_or_default().trim().to_string(),
        action: opt("action"),
        source: opt("source"),
        search_sensitive: ctx.can_read_sensitive,
    };
    let mut columns = BASE_COLUMNS.to_vec();
    if ctx.can_read_sensitive {
        columns.extend_from_slice(SENSITIVE_COLUMNS);
    }
    let (total, rows) = audit::page(
        &state.db,
        ctx.team_id,
        false,
        &filter,
        &columns,
        per_page,
        page,
    )
    .await?;
    let data = rows.into_iter().map(|r| present_row(&state, r)).collect();
    let path = current_url(&headers, uri.path());
    Ok(ok(paginator(&path, data, total, per_page, page)).into_response())
}
