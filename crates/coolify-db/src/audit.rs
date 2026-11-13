//! `audit_events`: the rows `auditLog()` and the `Auditable` trait write.
//!
//! [`record`] is the helper other parts call. It derives the same columns as
//! `AuditEvent::attributesFor`: source and action from the event name, resource
//! type, uuid and name from the context, the headline description, and the
//! redacted context as `metadata`.

use chrono::NaiveDateTime;
use serde_json::{Map, Value};

use crate::{Db, DbResult};

/// Who performed the action and from where.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub user_id: Option<i64>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub token_id: Option<i64>,
    pub token_name: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    /// The context names the actor, but nobody is signed in yet: `actor_type` stays `system`.
    pub from_context: bool,
}

/// `Str::headline` for the identifiers audit events use.
pub fn headline(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .iter()
        .map(|w| {
            let mut cs = w.chars();
            cs.next().map_or(String::new(), |f| {
                f.to_uppercase().collect::<String>() + &cs.as_str().to_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const SENSITIVE_VALUE_FIELDS: &[&str] = &[
    "git_full_url",
    "install_command",
    "build_command",
    "start_command",
    "health_check_command",
    "health_check_response_text",
    "custom_docker_run_options",
    "pre_deployment_command",
    "post_deployment_command",
    "docker_compose_custom_start_command",
    "docker_compose_custom_build_command",
    "custom_nginx_configuration",
    "postgres_conf",
    "mysql_conf",
    "mariadb_conf",
    "mongo_conf",
    "redis_conf",
    "keydb_conf",
    "internal_db_url",
    "external_db_url",
    "init_scripts",
    "dockerfile",
    "docker_compose",
    "docker_compose_raw",
    "custom_labels",
    "last_saved_proxy_configuration",
    "environment_variables",
    "environment_variables_preview",
    "validation_logs",
    "server_metadata",
    "logs",
    "configuration_snapshot",
    "configuration_diff",
    "content",
    "file_storage_content",
];

const SENSITIVE_WORDS: &[&str] = &[
    "password",
    "secret",
    "token",
    "private_key",
    "signature",
    "credential",
    "invitation_email",
    "api_key",
    "access_key",
    "authorization",
    "cookie",
    "license_key",
];

/// `AuditEvent::isSensitiveKey`.
fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    if k.ends_with("_id") || k.ends_with("_uuid") || k.ends_with("_name") {
        return false;
    }
    SENSITIVE_WORDS.iter().any(|w| k.contains(w))
}

/// `AuditEvent::isSensitiveField`.
pub fn is_sensitive_field(field: &str) -> bool {
    let k = field.to_ascii_lowercase();
    SENSITIVE_VALUE_FIELDS.contains(&field) || SENSITIVE_WORDS.iter().any(|w| k.contains(w))
}

/// `AuditEvent::stripUrlCredentials`.
fn strip_url_credentials(value: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/?#\s]+@")
            .unwrap_or_else(|_| unreachable!())
    });
    RE.replace_all(value, "$1").into_owned()
}

/// `AuditEvent::redact`.
pub fn redact(value: &Value, key: Option<&str>) -> Value {
    if let Some(k) = key
        && (SENSITIVE_VALUE_FIELDS.contains(&k) || is_sensitive_key(k))
    {
        return Value::String("[REDACTED]".into());
    }
    match value {
        Value::String(s) => Value::String(strip_url_credentials(s)),
        Value::Array(a) => Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, v)| redact(v, Some(&i.to_string())))
                .collect(),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), redact(v, Some(k))))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn filled(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => true,
    }
}

/// `AuditEvent::firstContextValue`.
fn first_context_value(
    context: &Map<String, Value>,
    preferred: Option<&str>,
    suffix: &str,
) -> Value {
    if let Some(p) = preferred
        && filled(context.get(p))
    {
        return context.get(p).cloned().unwrap_or(Value::Null);
    }
    context
        .iter()
        .find(|(k, _)| k.ends_with(suffix))
        .map_or(Value::Null, |(_, v)| v.clone())
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// The columns `AuditEvent::attributesFor` computes.
#[derive(Debug, Clone, PartialEq)]
pub struct Attributes {
    pub source: String,
    pub resource_type: Option<String>,
    pub action: String,
    pub actor_type: String,
    pub resource_uuid: Option<String>,
    pub resource_name: Option<String>,
    pub description: String,
    pub metadata: Value,
}

/// `AuditEvent::attributesFor` without the database.
pub fn attributes_for(event: &str, context: &Map<String, Value>, actor: &Actor) -> Attributes {
    let parts: Vec<&str> = event.split('.').collect();
    let source = parts.first().copied().unwrap_or("system").to_string();
    let resource_type = context
        .get("resource")
        .and_then(as_text)
        .or_else(|| parts.get(1).map(|s| (*s).to_string()));
    let action = context
        .get("action")
        .and_then(as_text)
        .or_else(|| {
            parts
                .last()
                .filter(|s| !s.is_empty())
                .map(|s| (*s).to_string())
        })
        .unwrap_or_else(|| "event".into());
    let uuid_key = resource_type.as_ref().map(|t| format!("{t}_uuid"));
    let name_key = resource_type.as_ref().map(|t| format!("{t}_name"));
    let resource_uuid = as_text(&first_context_value(context, uuid_key.as_deref(), "_uuid"));
    let resource_name = as_text(&first_context_value(context, name_key.as_deref(), "_name"));
    let actor_type = if ["mcp", "webhook", "system", "scheduler"].contains(&source.as_str()) {
        source.clone()
    } else if actor.token_id.is_some() {
        "api_token".to_string()
    } else if actor.user_id.is_some() && !actor.from_context {
        "user".to_string()
    } else {
        "system".to_string()
    };
    let description = context
        .get("audit_description")
        .and_then(as_text)
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                resource_name
                    .clone()
                    .unwrap_or_else(|| headline(resource_type.as_deref().unwrap_or(""))),
                headline(&action)
            )
            .trim()
            .to_string()
        });
    let mut meta = context.clone();
    // 4.7.5 (3379fa448): the actor's email, address and user agent have their own columns and stay
    // out of the metadata.
    for key in [
        "audit_changes",
        "actor_email",
        "actor_ip",
        "actor_user_agent",
    ] {
        meta.shift_remove(key);
    }
    // API tokens store `team_id` as a string. Keep it an integer, so JSON filters on metadata work.
    let numeric_team = match meta.get("team_id") {
        Some(Value::String(team))
            if !team.is_empty() && team.bytes().all(|b| b.is_ascii_digit()) =>
        {
            team.parse::<i64>().ok()
        }
        _ => None,
    };
    if let Some(id) = numeric_team {
        meta.insert("team_id".into(), Value::from(id));
    }
    Attributes {
        source,
        resource_type,
        action,
        actor_type,
        resource_uuid,
        resource_name,
        description,
        metadata: redact(&Value::Object(meta), None),
    }
}

/// `auditLog($event, $context, $level)`: one row. `changes_encrypted` is the
/// `encrypted:array` value of `AuditEvent::recordModelMutation`.
#[allow(clippy::too_many_arguments)]
pub async fn record(
    db: &Db,
    team_id: Option<i64>,
    event: &str,
    context: &Value,
    level: &str,
    actor: &Actor,
    changes_encrypted: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let empty = Map::new();
    let context = context.as_object().unwrap_or(&empty);
    let a = attributes_for(event, context, actor);
    let level = if ["info", "warning", "error"].contains(&level) {
        level
    } else {
        "info"
    };
    // 4.7.5: `actor_ip` and `actor_user_agent` of the context come before the request's, and an
    // empty user agent is stored as NULL (`Str::limit(...) ?: null`).
    let ip_address = context
        .get("actor_ip")
        .and_then(as_text)
        .or_else(|| actor.ip.clone());
    let user_agent = stored_user_agent(
        context.get("actor_user_agent").and_then(as_text),
        actor.user_agent.as_deref(),
    );
    sqlx::query(
        "INSERT INTO audit_events (team_id, event, source, action, level, actor_type, actor_id, actor_name, \
         actor_email, actor_token_id, actor_token_name, resource_type, resource_uuid, resource_name, description, \
         metadata, changes, ip_address, user_agent, created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16::json,$17,$18,$19,$20)",
    )
    .bind(team_id)
    .bind(event)
    .bind(&a.source)
    .bind(&a.action)
    .bind(level)
    .bind(&a.actor_type)
    .bind(actor.user_id)
    .bind(&actor.name)
    .bind(&actor.email)
    .bind(actor.token_id)
    .bind(&actor.token_name)
    .bind(&a.resource_type)
    .bind(&a.resource_uuid)
    .bind(&a.resource_name)
    .bind(&a.description)
    .bind(a.metadata.to_string())
    .bind(changes_encrypted)
    .bind(&ip_address)
    .bind(&user_agent)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Str::limit((string) ($context['actor_user_agent'] ?? request()->userAgent()), 200, '') ?: null`.
pub fn stored_user_agent(
    from_context: Option<String>,
    from_request: Option<&str>,
) -> Option<String> {
    let agent = from_context.or_else(|| from_request.map(str::to_owned))?;
    let limited: String = agent.chars().take(200).collect();
    (!limited.is_empty() && limited != "0").then_some(limited)
}

/// `auditUrl($url)` (4.7.5): scheme, host, port and path of a URL, without user info, query
/// string or fragment. `None` for a value that is not a non-empty string.
pub fn audit_url(url: Option<&str>) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    let invalid = || Some("[invalid-url]".to_owned());
    let Some((scheme, rest)) = url.split_once("://") else {
        return invalid();
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return invalid();
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let rest = rest.split('?').next().unwrap_or_default();
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    // An IPv6 literal keeps its brackets; the port is what follows the last colon after them.
    let (host, port) = match host_port.rfind(':') {
        Some(i) if !host_port[i..].contains(']') => (&host_port[..i], Some(&host_port[i + 1..])),
        _ => (host_port, None),
    };
    if host.is_empty() {
        return invalid();
    }
    let port = match port {
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => format!(":{p}"),
        Some(p) if !p.is_empty() => return invalid(),
        _ => String::new(),
    };
    Some(format!("{scheme}://{host}{port}{path}"))
}

/// `id, actor_type, actor_id, resource_name, description, metadata` of an event to anonymize.
type AnonymizeRow = (
    i64,
    String,
    Option<i64>,
    Option<String>,
    String,
    Option<String>,
);

/// `AuditEvent::anonymizeUser($user)` (4.7.5, 3379fa448), run when a user is deleted: removes the
/// personal data from the events that the user did, and from the events about the user (team
/// member, impersonation, deletion). Returns the number of rows changed.
pub async fn anonymize_user(db: &Db, user_id: i64, name: &str, email: &str) -> DbResult<u64> {
    db.ensure_writable()?;
    const REPLACEMENT: &str = "[deleted user]";
    let personal: Vec<String> = [email, name]
        .iter()
        .map(|v| v.to_lowercase())
        .filter(|v| !v.is_empty() && v != "0")
        .collect();
    let rows: Vec<AnonymizeRow> = sqlx::query_as(
        "SELECT id, actor_type, actor_id, resource_name, description, metadata::text FROM audit_events \
         WHERE (actor_type = 'user' AND actor_id = $1) \
            OR (metadata::jsonb -> 'member_id') @> to_jsonb($1::bigint) \
            OR (metadata::jsonb -> 'target_user_id') @> to_jsonb($1::bigint) \
            OR (metadata::jsonb -> 'deleted_user_id') @> to_jsonb($1::bigint) \
         ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?;
    let mut changed = 0;
    for (id, actor_type, actor_id, resource_name, description, metadata) in rows {
        let own = actor_type == "user" && actor_id == Some(user_id);
        let (resource_name, description) =
            anonymized_resource(resource_name, description, &personal, REPLACEMENT);
        let metadata = metadata
            .as_deref()
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .map(|m| replace_personal_values(&m, &personal, REPLACEMENT).to_string());
        changed += sqlx::query(
            "UPDATE audit_events SET \
               actor_name = CASE WHEN $2 THEN $3 ELSE actor_name END, \
               actor_email = CASE WHEN $2 THEN NULL ELSE actor_email END, \
               ip_address = CASE WHEN $2 THEN NULL ELSE ip_address END, \
               user_agent = CASE WHEN $2 THEN NULL ELSE user_agent END, \
               resource_name = $4, description = $5, metadata = $6::json \
             WHERE id = $1",
        )
        .bind(id)
        .bind(own)
        .bind(REPLACEMENT)
        .bind(resource_name)
        .bind(description)
        .bind(metadata)
        .execute(db.pool())
        .await?
        .rows_affected();
    }
    Ok(changed)
}

/// The `resource_name` and `description` of `anonymizeUser`: a resource named after the user is
/// renamed, and the description, which starts with that name, with it.
pub fn anonymized_resource(
    resource_name: Option<String>,
    description: String,
    personal: &[String],
    replacement: &str,
) -> (Option<String>, String) {
    match resource_name {
        Some(name) if personal.contains(&name.to_lowercase()) => {
            let tail = description.get(name.len()..).unwrap_or_default();
            (Some(replacement.to_owned()), format!("{replacement}{tail}"))
        }
        other => (other, description),
    }
}

/// `AuditEvent::replacePersonalValues`: every string of the metadata that is the user's email or
/// name (compared in lower case) is replaced.
pub fn replace_personal_values(value: &Value, personal: &[String], replacement: &str) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| replace_personal_values(v, personal, replacement))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), replace_personal_values(v, personal, replacement)))
                .collect(),
        ),
        Value::String(s) if personal.contains(&s.to_lowercase()) => {
            Value::String(replacement.to_owned())
        }
        other => other.clone(),
    }
}

/// The name and email columns of an actor.
pub async fn actor_user(db: &Db, user_id: i64) -> DbResult<Option<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT name, email FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `personal_access_tokens.name`.
pub async fn token_name(db: &Db, token_id: i64) -> DbResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM personal_access_tokens WHERE id = $1")
            .bind(token_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// The filters of `AuditEvent::scopeFiltered`.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub search: String,
    pub action: Option<String>,
    pub source: Option<String>,
    pub search_sensitive: bool,
}

/// `latestFirst()->paginate()`: total and one page, as JSON rows.
pub async fn page(
    db: &Db,
    team_id: i64,
    include_instance: bool,
    filter: &Filter,
    columns: &[&str],
    per_page: i64,
    page: i64,
) -> DbResult<(i64, Vec<crate::records::Record>)> {
    page_sorted(
        db,
        team_id,
        include_instance,
        filter,
        columns,
        per_page,
        page,
        None,
    )
    .await
}

/// `Team\AuditLog::setSort`: `(column, direction)` before `latestFirst()`.
#[allow(clippy::too_many_arguments)]
pub async fn page_sorted(
    db: &Db,
    team_id: i64,
    include_instance: bool,
    filter: &Filter,
    columns: &[&str],
    per_page: i64,
    page: i64,
    sort: Option<(&'static str, &'static str)>,
) -> DbResult<(i64, Vec<crate::records::Record>)> {
    let team = if include_instance {
        "(team_id = $1 OR team_id IS NULL)"
    } else {
        "team_id = $1"
    };
    let sens = if filter.search_sensitive {
        " OR actor_email LIKE $4"
    } else {
        ""
    };
    let wh = format!(
        "{team} AND ($2::text IS NULL OR action = $2) AND ($3::text IS NULL OR source = $3) \
         AND ($5 = '' OR event LIKE $4 OR description LIKE $4 OR resource_name LIKE $4 OR actor_name LIKE $4{sens})"
    );
    let like = format!("%{}%", filter.search);
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM audit_events WHERE {wh}"))
        .bind(team_id)
        .bind(&filter.action)
        .bind(&filter.source)
        .bind(&like)
        .bind(&filter.search)
        .fetch_one(db.pool())
        .await?;
    let order = sort.map_or(String::new(), |(c, d)| format!("{c} {d}, "));
    let sql = format!(
        "SELECT row_to_json(r)::text FROM (SELECT {} FROM audit_events WHERE {wh} \
         ORDER BY {order}created_at DESC, id DESC LIMIT $6 OFFSET $7) r",
        columns.join(", ")
    );
    let rows: Vec<String> = sqlx::query_scalar(&sql)
        .bind(team_id)
        .bind(&filter.action)
        .bind(&filter.source)
        .bind(&like)
        .bind(&filter.search)
        .bind(per_page)
        .bind((page - 1).saturating_mul(per_page))
        .fetch_all(db.pool())
        .await?;
    let rows = rows
        .iter()
        .map(|t| {
            crate::records::parse_row(t)
                .map_err(|e| crate::DbError::Schema(format!("decoding audit row: {e}")))
        })
        .collect::<DbResult<Vec<_>>>()?;
    Ok((total, rows))
}

/// `AuditEvent::query()->where('action', ...)`: the distinct actions and sources of a team.
pub async fn distinct_values(
    db: &Db,
    team_id: i64,
    include_instance: bool,
    column: &'static str,
) -> DbResult<Vec<String>> {
    debug_assert!(column == "action" || column == "source");
    let team = if include_instance {
        "(team_id = $1 OR team_id IS NULL)"
    } else {
        "team_id = $1"
    };
    Ok(sqlx::query_scalar(&format!(
        "SELECT DISTINCT {column} FROM audit_events WHERE {team} ORDER BY {column}"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn headlines() {
        assert_eq!(headline("storage_created"), "Storage Created");
        assert_eq!(headline("created"), "Created");
        assert_eq!(headline("login_succeeded"), "Login Succeeded");
    }

    #[test]
    fn attributes_like_upstream() {
        let ctx = json!({"team_id": 4, "tag_uuid": "u1", "tag_name": "x", "secret_value": "s"});
        let a = attributes_for(
            "api.tag.created",
            ctx.as_object().unwrap(),
            &Actor {
                token_id: Some(1),
                user_id: Some(2),
                ..Actor::default()
            },
        );
        assert_eq!(a.source, "api");
        assert_eq!(a.action, "created");
        assert_eq!(a.actor_type, "api_token");
        assert_eq!(a.resource_type.as_deref(), Some("tag"));
        assert_eq!(a.resource_uuid.as_deref(), Some("u1"));
        assert_eq!(a.description, "x Created");
        assert_eq!(a.metadata["secret_value"], json!("[REDACTED]"));
    }

    /// `AuditEvent::attributesFor` (4.7.5): the actor's personal data stays out of the metadata
    /// and a token's string `team_id` becomes an integer.
    #[test]
    fn metadata_has_no_actor_data_and_an_integer_team() {
        let ctx = json!({
            "team_id": "4",
            "member_id": 7,
            "member_name": "Bob",
            "actor_email": "alice@example.test",
            "actor_ip": "203.0.113.9",
            "actor_user_agent": "curl/8",
            "audit_changes": {"a": 1},
        });
        let a = attributes_for(
            "ui.team_member.removed",
            ctx.as_object().unwrap(),
            &Actor::default(),
        );
        assert_eq!(
            a.metadata,
            json!({"team_id": 4, "member_id": 7, "member_name": "Bob"})
        );
        // Only digits are converted.
        let ctx = json!({"team_id": "4a"});
        let a = attributes_for("ui.x.y", ctx.as_object().unwrap(), &Actor::default());
        assert_eq!(a.metadata["team_id"], json!("4a"));
    }

    #[test]
    fn user_agents_are_limited_and_empty_ones_are_null() {
        assert_eq!(stored_user_agent(None, Some("")), None);
        assert_eq!(stored_user_agent(None, None), None);
        assert_eq!(
            stored_user_agent(None, Some("curl/8")).as_deref(),
            Some("curl/8")
        );
        // The context wins over the request.
        assert_eq!(
            stored_user_agent(Some("worker".into()), Some("curl/8")).as_deref(),
            Some("worker")
        );
        assert_eq!(
            stored_user_agent(None, Some(&"a".repeat(300))).map(|s| s.len()),
            Some(200)
        );
    }

    /// `auditUrl()` (4.7.5).
    #[test]
    fn audit_urls_keep_scheme_host_port_and_path_only() {
        assert_eq!(audit_url(None), None);
        assert_eq!(audit_url(Some("")), None);
        assert_eq!(
            audit_url(Some(
                "https://user:pw@example.test:8443/hook/x?token=1#frag"
            ))
            .as_deref(),
            Some("https://example.test:8443/hook/x")
        );
        assert_eq!(
            audit_url(Some("http://example.test")).as_deref(),
            Some("http://example.test")
        );
        assert_eq!(
            audit_url(Some("http://[2001:db8::1]:8080/a")).as_deref(),
            Some("http://[2001:db8::1]:8080/a")
        );
        assert_eq!(
            audit_url(Some("not a url")).as_deref(),
            Some("[invalid-url]")
        );
        assert_eq!(
            audit_url(Some("/relative/path")).as_deref(),
            Some("[invalid-url]")
        );
    }

    /// `AuditEvent::anonymizeUser()` (4.7.5): the pure parts.
    #[test]
    fn personal_values_are_replaced() {
        let personal = vec!["bob@example.test".to_string(), "bob member".to_string()];
        let metadata = json!({
            "member_id": 2,
            "member_name": "Bob Member",
            "invitation_email": "BOB@example.test",
            "list": ["bob member", "other"],
            "team_id": 1,
        });
        assert_eq!(
            replace_personal_values(&metadata, &personal, "[deleted user]"),
            json!({
                "member_id": 2,
                "member_name": "[deleted user]",
                "invitation_email": "[deleted user]",
                "list": ["[deleted user]", "other"],
                "team_id": 1,
            })
        );
        assert_eq!(
            anonymized_resource(
                Some("Bob Member".into()),
                "Bob Member Removed".into(),
                &personal,
                "[deleted user]"
            ),
            (
                Some("[deleted user]".to_string()),
                "[deleted user] Removed".to_string()
            )
        );
        assert_eq!(
            anonymized_resource(Some("Shop".into()), "Shop Updated".into(), &personal, "x"),
            (Some("Shop".to_string()), "Shop Updated".to_string())
        );
    }

    #[test]
    fn strips_credentials() {
        assert_eq!(
            redact(&json!("https://user:pw@example.com/x"), None),
            json!("https://example.com/x")
        );
    }
}
