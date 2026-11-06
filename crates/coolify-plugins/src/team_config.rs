//! Each team's values of a plugin's `[[settings]]` (ADR 0014).
//!
//! A secret is encrypted with the same `Encrypter` (Laravel's, under `APP_KEY`) that coolify-rs uses for
//! Coolify's own encrypted columns, so it is as safe at rest as a private key or a database password. coolify-api
//! hands the host its encrypter ([`crate::Host::use_encrypter`]); without one, secrets can't be saved and
//! `config.get` answers none for them. Values are read back in clear only for the plugin's own calls: the API
//! answers whether a secret is set, never its value, and nothing here logs a value or puts one in an error.

use std::collections::BTreeMap;

use coolify_compat::Encrypter;
use coolify_db::Db;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::Row;

use crate::PluginError;
use crate::manifest::{Manifest, Setting};

/// A declared setting and the team's state of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingState {
    pub setting: Setting,
    /// The team set a value.
    pub set: bool,
    /// The team's value of a setting that is not secret; always `None` for a secret.
    pub value: Option<String>,
}

impl SettingState {
    /// The API's shape: the declaration, `set`, and `value` only for a setting that is not secret.
    pub fn to_json(&self) -> Value {
        let s = &self.setting;
        let mut v = json!({
            "key": s.key,
            "label": s.label,
            "help": s.help,
            "kind": s.kind().as_str(),
            "secret": s.secret,
            "required": s.required,
            "options": s.options,
            "default": s.default,
            "set": self.set,
        });
        if !s.secret {
            v["value"] = self.value.clone().into();
        }
        v
    }
}

/// A plugin's settings for one team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamConfig {
    pub settings: Vec<SettingState>,
}

impl TeamConfig {
    /// A required setting is unset.
    pub fn needs_configuration(&self) -> bool {
        self.settings.iter().any(|s| s.setting.required && !s.set)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "settings": self.settings.iter().map(SettingState::to_json).collect::<Vec<_>>(),
            "needs_configuration": self.needs_configuration(),
        })
    }
}

/// What an update changed, by key. Never values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub set: Vec<String>,
    pub cleared: Vec<String>,
}

impl Changed {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.cleared.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Messages by key; none of them repeats a value.
    #[error("invalid settings")]
    Invalid(BTreeMap<String, String>),
    #[error(transparent)]
    Store(#[from] PluginError),
}

struct Stored {
    team_id: i64,
    key: String,
    value: String,
    secret: bool,
}

fn row(r: &sqlx::postgres::PgRow) -> Result<Stored, sqlx::Error> {
    Ok(Stored {
        team_id: r.try_get("team_id")?,
        key: r.try_get("key")?,
        value: r.try_get("value")?,
        secret: r.try_get("secret")?,
    })
}

async fn rows(db: &Db, plugin: &str, team: Option<i64>) -> Result<Vec<Stored>, PluginError> {
    let found = sqlx::query(
        "SELECT team_id, key, value, secret FROM coolify_rs_plugins.team_settings \
         WHERE plugin_id = $1 AND ($2::bigint IS NULL OR team_id = $2) ORDER BY team_id, key",
    )
    .bind(plugin)
    .bind(team)
    .fetch_all(db.pool())
    .await?;
    Ok(found.iter().map(row).collect::<Result<_, _>>()?)
}

/// The team's settings of the plugin, as the API shows them.
pub async fn read(db: &Db, manifest: &Manifest, team_id: i64) -> Result<TeamConfig, PluginError> {
    let stored = rows(db, &manifest.id, Some(team_id)).await?;
    let settings = manifest
        .settings
        .iter()
        .map(|s| {
            // A row whose secrecy disagrees with the manifest is left over from an update that could not
            // fix it; it counts as unset, as `config.get` treats it.
            let r = stored
                .iter()
                .find(|r| r.key == s.key && r.secret == s.secret);
            SettingState {
                setting: s.clone(),
                set: r.is_some(),
                value: r.filter(|_| !s.secret).map(|r| r.value.clone()),
            }
        })
        .collect();
    Ok(TeamConfig { settings })
}

/// What `config.get` answers for `setting`: the team's value, decrypted for a secret, or the default.
pub async fn value(
    db: &Db,
    encrypter: Option<&Encrypter>,
    plugin: &str,
    team_id: i64,
    setting: &Setting,
) -> Result<Option<String>, PluginError> {
    let found: Option<(String, bool)> = sqlx::query_as(
        "SELECT value, secret FROM coolify_rs_plugins.team_settings \
         WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
    )
    .bind(plugin)
    .bind(team_id)
    .bind(&setting.key)
    .fetch_optional(db.pool())
    .await?;
    let Some((value, secret)) = found.filter(|(_, secret)| *secret == setting.secret) else {
        return Ok(setting.default.clone());
    };
    if !secret {
        return Ok(Some(value));
    }
    let Some(encrypter) = encrypter else {
        tracing::warn!(plugin, team = team_id, key = %setting.key, "a plugin secret can't be read: APP_KEY is not configured");
        return Ok(None);
    };
    match encrypter.decrypt_string(&value) {
        Ok(plain) => Ok(Some(plain)),
        Err(_) => {
            tracing::warn!(plugin, team = team_id, key = %setting.key, "a plugin secret can't be decrypted with APP_KEY");
            Ok(None)
        }
    }
}

/// A partial update from the API: each key of `changes` is a declared setting, and its value a string (set
/// it; an empty or blank string clears it) or null (clear it). Everything is checked before anything is
/// written; then the changes are written and audited (keys only) in one transaction.
pub async fn update(
    db: &Db,
    encrypter: Option<&Encrypter>,
    manifest: &Manifest,
    team_id: i64,
    user_id: Option<i64>,
    changes: &Map<String, Value>,
) -> Result<Changed, UpdateError> {
    let mut errors = BTreeMap::new();
    let mut writes: Vec<(&Setting, Option<String>)> = Vec::new();
    for (key, v) in changes {
        let Some(s) = manifest.setting(key) else {
            errors.insert(key.clone(), "The plugin has no such setting.".to_owned());
            continue;
        };
        let value = match v {
            Value::Null => None,
            Value::String(text) => {
                let text = Setting::normalize(text);
                (!text.is_empty()).then(|| text.to_owned())
            }
            _ => {
                errors.insert(
                    key.clone(),
                    format!("{} must be a string or null.", s.label),
                );
                continue;
            }
        };
        if let Some(text) = &value {
            if let Err(e) = s.check(text) {
                errors.insert(key.clone(), e);
                continue;
            }
            if s.secret && encrypter.is_none() {
                errors.insert(
                    key.clone(),
                    format!(
                        "{} is a secret, and secrets can't be saved: APP_KEY is not configured.",
                        s.label
                    ),
                );
                continue;
            }
        }
        writes.push((s, value));
    }
    if !errors.is_empty() {
        return Err(UpdateError::Invalid(errors));
    }
    let mut changed = Changed::default();
    let mut tx = db.pool().begin().await.map_err(PluginError::from)?;
    for (s, value) in writes {
        match value {
            Some(text) => {
                let stored = match (s.secret, encrypter) {
                    (true, Some(e)) => e.encrypt_string(&text),
                    // Refused above; never store a secret in clear.
                    (true, None) => continue,
                    (false, _) => text,
                };
                sqlx::query(
                    "INSERT INTO coolify_rs_plugins.team_settings (plugin_id, team_id, key, value, secret, updated_by) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (plugin_id, team_id, key) DO UPDATE SET value = $4, secret = $5, \
                     updated_by = $6, updated_at = now()",
                )
                .bind(&manifest.id)
                .bind(team_id)
                .bind(&s.key)
                .bind(stored)
                .bind(s.secret)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(PluginError::from)?;
                changed.set.push(s.key.clone());
            }
            None => {
                let n = sqlx::query(
                    "DELETE FROM coolify_rs_plugins.team_settings WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
                )
                .bind(&manifest.id)
                .bind(team_id)
                .bind(&s.key)
                .execute(&mut *tx)
                .await
                .map_err(PluginError::from)?
                .rows_affected();
                if n > 0 {
                    changed.cleared.push(s.key.clone());
                }
            }
        }
    }
    if !changed.is_empty() {
        sqlx::query(
            "INSERT INTO coolify_rs_plugins.audit (plugin_id, team_id, user_id, action, detail) \
             VALUES ($1, $2, $3, 'team_settings_updated', $4)",
        )
        .bind(&manifest.id)
        .bind(team_id)
        .bind(user_id)
        .bind(json!({ "set": changed.set, "cleared": changed.cleared }))
        .execute(&mut *tx)
        .await
        .map_err(PluginError::from)?;
    }
    tx.commit().await.map_err(PluginError::from)?;
    Ok(changed)
}

/// Brings every team's values in line with a new version's `[[settings]]`:
///
/// - a key the new version no longer declares is deleted;
/// - a value that turned secret is encrypted in place (deleted when there is no `APP_KEY`);
/// - a secret that is no longer secret is deleted, because it was entered as a secret and members would now
///   see it;
/// - a value the new declaration refuses (another kind, other options) is deleted.
///
/// A secret that can't be decrypted is kept: an `APP_KEY` problem must not destroy it. What happened is
/// audited by team and key.
pub async fn reconcile(
    db: &Db,
    encrypter: Option<&Encrypter>,
    manifest: &Manifest,
) -> Result<(), PluginError> {
    let mut dropped = Vec::new();
    let mut encrypted = Vec::new();
    let keys: Vec<String> = manifest.settings.iter().map(|s| s.key.clone()).collect();
    let mut tx = db.pool().begin().await?;
    let removed: Vec<(i64, String)> = sqlx::query_as(
        "DELETE FROM coolify_rs_plugins.team_settings WHERE plugin_id = $1 AND NOT (key = ANY($2)) \
         RETURNING team_id, key",
    )
    .bind(&manifest.id)
    .bind(&keys)
    .fetch_all(&mut *tx)
    .await?;
    dropped.extend(removed);
    tx.commit().await?;

    for r in rows(db, &manifest.id, None).await? {
        let Some(s) = manifest.setting(&r.key) else {
            continue;
        };
        let delete = match (r.secret, s.secret) {
            // Was a secret, is not any more: never show it.
            (true, false) => true,
            (true, true) => match encrypter.map(|e| e.decrypt_string(&r.value)) {
                Some(Ok(plain)) => s.check(&plain).is_err(),
                // No key, or not ours: keep it rather than lose it.
                _ => false,
            },
            (false, _) => s.check(&r.value).is_err() || (s.secret && encrypter.is_none()),
        };
        if delete {
            sqlx::query(
                "DELETE FROM coolify_rs_plugins.team_settings WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
            )
            .bind(&manifest.id)
            .bind(r.team_id)
            .bind(&r.key)
            .execute(db.pool())
            .await?;
            dropped.push((r.team_id, r.key));
        } else if s.secret
            && !r.secret
            && let Some(e) = encrypter
        {
            sqlx::query(
                "UPDATE coolify_rs_plugins.team_settings SET value = $4, secret = true, updated_at = now() \
                 WHERE plugin_id = $1 AND team_id = $2 AND key = $3",
            )
            .bind(&manifest.id)
            .bind(r.team_id)
            .bind(&r.key)
            .bind(e.encrypt_string(&r.value))
            .execute(db.pool())
            .await?;
            encrypted.push((r.team_id, r.key));
        }
    }
    if !dropped.is_empty() || !encrypted.is_empty() {
        let list = |v: &[(i64, String)]| {
            v.iter()
                .map(|(t, k)| json!({ "team_id": t, "key": k }))
                .collect::<Vec<_>>()
        };
        crate::store::audit(
            db,
            &manifest.id,
            None,
            None,
            "settings_reconciled",
            json!({ "dropped": list(&dropped), "encrypted": list(&encrypted) }),
        )
        .await?;
    }
    Ok(())
}
