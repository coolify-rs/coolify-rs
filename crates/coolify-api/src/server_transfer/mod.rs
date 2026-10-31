//! `App\Services\ServerTransfer`: moves a server and its records to another instance.

pub mod bundle;
pub mod checklist;
pub mod exporter;
pub mod importer;
pub mod migrator;

use coolify_db::Db;
use serde_json::{Map, Value};

pub use coolify_jobs::server_transfer::now_iso;

/// `data_get($value, 'a.b')`; `None` when a key is missing.
pub fn dg<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut at = value;
    for part in path.split('.') {
        at = match at {
            Value::Object(o) => o.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(at)
}

/// `blank()`.
pub fn blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(_) => false,
    }
}

/// `(int) $value`.
pub fn as_int(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::Bool(b) => i64::from(*b),
        Value::String(s) => {
            let t = s.trim_start();
            let end = t
                .char_indices()
                .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+'))))
                .map_or(t.len(), |(i, _)| i);
            t[..end].parse().unwrap_or(0)
        }
        _ => 0,
    }
}

/// `(string) $value`.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(true) => "1".to_owned(),
        Value::Bool(false) => String::new(),
        other => other.to_string(),
    }
}

/// `(bool) $value`.
pub fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(crate::serialize::php_truthy)
}

/// `instanceSettings()->fqdn ?: config('app.url')`, without a trailing slash.
pub async fn instance_url(db: &Db) -> String {
    let (fqdn, _, _) = coolify_db::servers::instance_urls(db)
        .await
        .unwrap_or((None, None, None));
    let url = fqdn
        .filter(|f| !f.is_empty() && f != "0")
        .or_else(|| std::env::var("APP_URL").ok().filter(|u| !u.is_empty()))
        .unwrap_or_else(|| "http://localhost".to_owned());
    url.trim_end_matches('/').to_owned()
}

/// `ServerTransferClaimer::deleteArchive` in its own transaction.
pub async fn delete_archive_now(
    state: &crate::state::AppState,
    server_id: i64,
) -> Result<(), TransferError> {
    let mut tx = state.db.pool().begin().await?;
    importer::delete_archive(&mut tx, server_id).await?;
    tx.commit().await?;
    Ok(())
}

/// `isDev()`.
pub fn is_dev() -> bool {
    std::env::var("APP_ENV").is_ok_and(|e| e == "local")
}

/// A failed import, as `ServerTransferController::import` answers it.
#[derive(Debug)]
pub enum TransferError {
    /// `ValidationException::withMessages`.
    Validation(Vec<(String, Vec<String>)>),
    /// Any other exception.
    Message(String),
}

impl From<sqlx::Error> for TransferError {
    fn from(e: sqlx::Error) -> Self {
        tracing::warn!(error = %e, "server transfer query failed");
        Self::Message(format!("SQLSTATE: {e}"))
    }
}

impl From<String> for TransferError {
    fn from(e: String) -> Self {
        Self::Message(e)
    }
}

impl TransferError {
    /// `$e->getMessage()`: the first message, and how many more there are.
    pub fn message(&self) -> String {
        match self {
            Self::Message(m) => m.clone(),
            Self::Validation(errors) => {
                let mut all = errors.iter().flat_map(|(_, m)| m.iter());
                let first = all.next().cloned().unwrap_or_default();
                let more = all.count();
                match more {
                    0 => first,
                    1 => format!("{first} (and 1 more error)"),
                    n => format!("{first} (and {n} more errors)"),
                }
            }
        }
    }

    /// `$e->errors()`.
    pub fn errors(&self) -> Option<Map<String, Value>> {
        match self {
            Self::Message(_) => None,
            Self::Validation(errors) => Some(
                errors
                    .iter()
                    .map(|(k, m)| (k.clone(), serde_json::json!(m)))
                    .collect(),
            ),
        }
    }
}

/// `ServerTransferMigrator::validateInstanceUrl` without the SafeWebhookUrl check, which needs
/// the request state and runs in [`migrator::validate_instance_url`].
pub fn is_local_development_target(url: &str) -> bool {
    if !is_dev() {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    matches!(scheme, "http" | "https")
        && matches!(host, "localhost" | "127.0.0.1" | "host.docker.internal")
}
