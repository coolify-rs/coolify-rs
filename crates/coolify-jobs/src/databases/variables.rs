//! `EnvironmentVariable` accessors the API shows: `real_value` and `displayRealValue()`.

use std::sync::LazyLock;

use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::databases::DatabaseRow;
use coolify_db::environment_variables as vars;
use regex::Regex;

use super::env::{Owner, escape_env_variables, resolve_references};

static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{(.*?)\}\}").unwrap_or_else(|_| unreachable!()));

/// `resolveRealValue(revealLockedSharedVariables: false)`: references to shown-once shared
/// variables keep their text.
#[allow(clippy::too_many_arguments)]
pub async fn display_real_value(
    db: &Db,
    enc: &Encrypter,
    d: &DatabaseRow,
    server_id: Option<i64>,
    value: Option<&str>,
    legacy: bool,
    is_literal: bool,
    is_multiline: bool,
) -> Result<Option<String>, String> {
    display_real_value_of(
        db,
        enc,
        &Owner::database(d),
        server_id,
        value,
        legacy,
        is_literal,
        is_multiline,
    )
    .await
}

/// `resolveRealValue(revealLockedSharedVariables: false)` for any resource.
#[allow(clippy::too_many_arguments)]
pub async fn display_real_value_of(
    db: &Db,
    enc: &Encrypter,
    owner: &Owner<'_>,
    server_id: Option<i64>,
    value: Option<&str>,
    legacy: bool,
    is_literal: bool,
    is_multiline: bool,
) -> Result<Option<String>, String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(value.map(str::to_owned));
    };
    let mut out = value.trim().to_owned();
    let found: Vec<String> = REFERENCE
        .captures_iter(&out.clone())
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_owned()))
        .collect();
    for inner in found {
        let trimmed = inner.trim();
        let Some((kind, key)) = trimmed.split_once('.') else {
            continue;
        };
        let owner_id = match kind {
            "environment" => owner.environment_id,
            "project" => owner.project_id,
            "team" => Some(owner.team_id),
            "server" => server_id,
            _ => continue,
        };
        let Some(owner_id) = owner_id else { continue };
        let shared = vars::shared(db, owner.team_id, kind, key, owner_id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some((Some(cipher), false)) = shared {
            let plain = enc
                .decrypt_string(&cipher)
                .map_err(|_| "The MAC is invalid.".to_owned())?;
            out = out.replace(&format!("{{{{{inner}}}}}"), &plain);
        }
    }
    if !legacy {
        return Ok(Some(out));
    }
    // `legacyEscapedValue`.
    let json_container = (out.starts_with('{') || out.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(&out).is_ok();
    Ok(Some(if json_container {
        out
    } else if is_literal || is_multiline {
        format!("'{out}'")
    } else {
        escape_env_variables(&out)
    }))
}

/// The resolved value (`real_value`).
pub async fn real_value(
    db: &Db,
    enc: &Encrypter,
    d: &DatabaseRow,
    server_id: Option<i64>,
    value: Option<&str>,
) -> Result<Option<String>, String> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => Ok(Some(
            resolve_references(db, enc, &Owner::database(d), server_id, v).await?,
        )),
        None => Ok(value.map(str::to_owned)),
    }
}
