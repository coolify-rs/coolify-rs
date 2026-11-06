//! `coolify-rs check-portable`: can this database be handed back to Coolify (ADR 0012)?
//!
//! Without provider plugins it always can: plugins keep their data in `coolify_rs_plugins`, which Coolify
//! never reads. A provider plugin writes values Coolify does not know into Coolify's own tables, and marks
//! those rows in `coolify_rs_plugins.owned_resources`. This check lists both the marked rows and every
//! value Coolify would not accept, marked or not, so nothing a plugin forgot to mark slips through.

use coolify_db::Db;
use serde::Serialize;
use sqlx::Row;

use crate::PluginError;
use crate::store::{self, Owned};

/// The `build_pack` values Coolify stores: its `BuildPackTypes` enum (`app/Enums/BuildPackTypes.php`) and
/// `dockerimage`, which the Docker image endpoints write.
pub const COOLIFY_BUILD_PACKS: &[&str] = &[
    "nixpacks",
    "static",
    "dockerfile",
    "dockercompose",
    "railpack",
    "dockerimage",
];

/// Columns of Coolify's tables with a closed set of values, checked row by row.
const CLOSED_COLUMNS: &[(&str, &str, &[&str])] =
    &[("applications", "build_pack", COOLIFY_BUILD_PACKS)];

/// A value in one of Coolify's tables that Coolify does not accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnknownValue {
    pub table: String,
    pub column: String,
    pub row_id: i64,
    pub value: String,
    /// The plugin that marked the row as its own, if one did.
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Whether Coolify can take the database back as it is.
    pub portable: bool,
    pub owned_resources: Vec<Owned>,
    pub unknown_values: Vec<UnknownValue>,
    /// Whether the `coolify_rs_plugins` schema exists. Coolify ignores it either way.
    pub plugin_schema: bool,
}

pub async fn check(db: &Db) -> Result<Report, PluginError> {
    let owned = store::owned(db).await?;
    let mut unknown = Vec::new();
    for (table, column, allowed) in CLOSED_COLUMNS {
        let allowed: Vec<String> = allowed.iter().map(|s| (*s).to_owned()).collect();
        // Names come from the constant above, never from input.
        let rows = sqlx::query(&format!(
            "SELECT id, {column}::text AS value FROM public.{table} \
             WHERE {column} IS NOT NULL AND NOT ({column}::text = ANY($1)) ORDER BY id"
        ))
        .bind(&allowed)
        .fetch_all(db.pool())
        .await?;
        for r in rows {
            let row_id: i64 = r.try_get("id")?;
            unknown.push(UnknownValue {
                table: (*table).to_owned(),
                column: (*column).to_owned(),
                row_id,
                value: r.try_get("value")?,
                owner: owned
                    .iter()
                    .find(|o| o.table == *table && o.row_id == row_id)
                    .map(|o| o.plugin_id.clone()),
            });
        }
    }
    let plugin_schema: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'coolify_rs_plugins')",
    )
    .fetch_one(db.pool())
    .await?;
    Ok(Report {
        portable: owned.is_empty() && unknown.is_empty(),
        owned_resources: owned,
        unknown_values: unknown,
        plugin_schema,
    })
}

impl Report {
    /// The report for a person.
    pub fn human(&self) -> String {
        let mut out = String::new();
        if self.portable {
            out.push_str("Portable: Coolify can take this database back as it is.\n");
        } else {
            out.push_str("Not portable: Coolify cannot take this database back yet.\n");
        }
        if !self.owned_resources.is_empty() {
            out.push_str(
                "\nRows owned by provider plugins (hand each back, or remove it, first):\n",
            );
            for o in &self.owned_resources {
                out.push_str(&format!("  {} #{} by {}", o.table, o.row_id, o.plugin_id));
                if !o.note.is_empty() {
                    out.push_str(&format!(": {}", o.note));
                }
                out.push('\n');
            }
        }
        if !self.unknown_values.is_empty() {
            out.push_str("\nValues Coolify does not accept:\n");
            for u in &self.unknown_values {
                out.push_str(&format!(
                    "  {}.{} = {:?} in row #{}{}\n",
                    u.table,
                    u.column,
                    u.value,
                    u.row_id,
                    u.owner
                        .as_ref()
                        .map(|p| format!(" (owned by {p})"))
                        .unwrap_or_else(|| " (not marked by any plugin)".into())
                ));
            }
        }
        if self.plugin_schema {
            out.push_str(
                "\nThe coolify_rs_plugins schema holds coolify-rs's plugin data. Coolify ignores it, so it can \
                 stay; `DROP SCHEMA coolify_rs_plugins CASCADE` removes every trace of plugins.\n",
            );
        }
        out
    }
}
