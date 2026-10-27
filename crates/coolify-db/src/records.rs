//! Model records: rows serialized the way Laravel's `Model::toArray()` does.
//!
//! Rows are fetched with `row_to_json`, which keeps database column order
//! (Laravel keeps it too), native booleans/integers and parsed JSON. Each
//! model spec then applies upstream's `$hidden` list and datetime casts.
//!
//! Unlike upstream, columns not listed in `known` are **dropped** (and
//! logged once): a newer upstream release adding a secret column must not
//! leak it through coolify-rs before the port is reviewed.

use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value};

pub type Record = Map<String, Value>;

#[derive(Debug)]
pub struct ModelSpec {
    pub table: &'static str,
    /// Columns this build knows how to present (baseline + reviewed newer ones).
    pub known: &'static [&'static str],
    /// Model `$hidden` attributes.
    pub hidden: &'static [&'static str],
    /// Columns of type `timestamp`.
    pub timestamps: &'static [&'static str],
    /// Timestamp attributes cast to `datetime` (Carbon). Others render raw.
    pub datetimes: &'static [&'static str],
}

pub static TEAM: ModelSpec = ModelSpec {
    table: "teams",
    known: &[
        "id",
        "name",
        "description",
        "personal_team",
        "created_at",
        "updated_at",
        "show_boarding",
        "custom_server_limit",
        "is_mcp_server_enabled",
        "is_build_server_fallback_enabled",
        "avatar_path",
        "avatar_storage_type",
        "avatar_s3_storage_id",
        // main (2026_10_25_115811), reviewed: boolean flag, not secret.
        "is_two_factor_required",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static USER: ModelSpec = ModelSpec {
    table: "users",
    known: &[
        "id",
        "name",
        "email",
        "email_verified_at",
        "password",
        "remember_token",
        "created_at",
        "updated_at",
        "two_factor_secret",
        "two_factor_recovery_codes",
        "two_factor_confirmed_at",
        "force_password_reset",
        "marketing_emails",
        "pending_email",
        "email_change_code",
        "email_change_code_expires_at",
        "avatar_path",
        "avatar_storage_type",
        "avatar_s3_storage_id",
        "current_team_id",
        "created_before_oauth_identities",
    ],
    hidden: &[
        "password",
        "remember_token",
        "two_factor_recovery_codes",
        "two_factor_secret",
        "created_before_oauth_identities",
    ],
    timestamps: &[
        "created_at",
        "updated_at",
        "email_verified_at",
        "two_factor_confirmed_at",
        "email_change_code_expires_at",
    ],
    datetimes: &[
        "created_at",
        "updated_at",
        "email_verified_at",
        "email_change_code_expires_at",
    ],
};

pub static PROJECT: ModelSpec = ModelSpec {
    table: "projects",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "team_id",
        "created_at",
        "updated_at",
        "icon_path",
        "icon_storage_type",
        "icon_s3_storage_id",
        // main (2026_10_23_182201), reviewed: layout JSON, not secret.
        "canvas_position",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static ENVIRONMENT: ModelSpec = ModelSpec {
    table: "environments",
    known: &[
        "id",
        "name",
        "project_id",
        "created_at",
        "updated_at",
        "description",
        "uuid",
        "canvas_positions",
    ],
    hidden: &[],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

pub static PRIVATE_KEY: ModelSpec = ModelSpec {
    table: "private_keys",
    known: &[
        "id",
        "uuid",
        "name",
        "description",
        "private_key",
        "is_git_related",
        "team_id",
        "created_at",
        "updated_at",
        "fingerprint",
    ],
    // `private_key` is hidden by the model; handlers re-add it (decrypted)
    // only for tokens with `can_read_sensitive`.
    hidden: &["private_key"],
    timestamps: &["created_at", "updated_at"],
    datetimes: &["created_at", "updated_at"],
};

static WARNED: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

impl ModelSpec {
    /// Applies hidden fields, unknown-column filtering and datetime casts.
    pub fn present(&self, raw: Record) -> Record {
        let mut out = Record::new();
        for (k, v) in raw {
            if !self.known.contains(&k.as_str()) {
                warn_unknown(self.table, &k);
                continue;
            }
            if self.hidden.contains(&k.as_str()) {
                continue;
            }
            let v = match v {
                Value::String(s)
                    if self.timestamps.contains(&k.as_str()) && looks_like_pg_timestamp(&s) =>
                {
                    Value::String(if self.datetimes.contains(&k.as_str()) {
                        laravel_datetime(&s)
                    } else {
                        s.replacen('T', " ", 1)
                    })
                }
                other => other,
            };
            out.insert(k, v);
        }
        out
    }

    /// Same as [`present`](Self::present) with extra attributes hidden (controller `makeHidden`).
    pub fn present_without(&self, raw: Record, extra_hidden: &[&str]) -> Record {
        let mut r = self.present(raw);
        for h in extra_hidden {
            r.shift_remove(*h);
        }
        r
    }
}

fn warn_unknown(table: &str, column: &str) {
    let key = format!("{table}.{column}");
    if let Ok(mut w) = WARNED.lock()
        && w.insert(key.clone())
    {
        tracing::warn!(column = %key, "dropping unreviewed upstream column from API output");
    }
}

/// `row_to_json` renders `timestamp(0)` as `YYYY-MM-DDTHH:MM:SS`.
fn looks_like_pg_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 19
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, c)| [4, 7, 10, 13, 16].contains(&i) || c.is_ascii_digit())
}

/// Carbon's default JSON serialization: microseconds and `Z` (UTC).
pub fn laravel_datetime(pg: &str) -> String {
    format!("{pg}.000000Z")
}

pub fn laravel_datetime_from(dt: chrono::NaiveDateTime) -> String {
    format!("{}.000000Z", dt.format("%Y-%m-%dT%H:%M:%S"))
}

/// Parses `row_to_json(...)::text` output into an order-preserving record.
pub fn parse_row(text: &str) -> Result<Record, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presents_like_laravel() {
        let raw = parse_row(
            r#"{"id":1,"name":"A","password":"$2y$","email_verified_at":"2026-10-26T20:26:28",
                "two_factor_confirmed_at":"2026-10-26T20:26:29","created_at":"2026-10-26T20:26:28",
                "secret_new_column":"x","current_team_id":null}"#,
        )
        .unwrap();
        let r = USER.present(raw);
        let keys: Vec<&str> = r.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "name",
                "email_verified_at",
                "two_factor_confirmed_at",
                "created_at",
                "current_team_id"
            ]
        );
        assert_eq!(r["email_verified_at"], "2026-10-26T20:26:28.000000Z");
        // Not cast: Laravel returns the raw database string.
        assert_eq!(r["two_factor_confirmed_at"], "2026-10-26 20:26:29");
    }

    #[test]
    fn non_timestamps_untouched() {
        let raw = parse_row(r#"{"id":1,"name":"2026-10-26T20:26:28","description":null}"#).unwrap();
        let r = PROJECT.present(raw);
        assert_eq!(r["name"], "2026-10-26T20:26:28");
    }
}
