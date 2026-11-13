//! `service_applications` and `service_databases` columns added by Coolify v4.7:
//! `is_removed_from_compose` (`markServicePartsRemovedFromCompose()`).

use chrono::NaiveDateTime;

use crate::{Db, DbResult};

const PART_TABLES: [&str; 2] = ["service_applications", "service_databases"];

/// `markServicePartsRemovedFromCompose($resource, $services)`: marks the applications and
/// databases of a service whose names are no longer in its compose file. They keep their
/// settings, so a part that comes back under the same name is used again. The service status
/// ignores marked parts, and the user can delete them. Nothing changes without service names.
pub async fn mark_removed_from_compose(
    db: &Db,
    service_id: i64,
    service_names: &[String],
    now: NaiveDateTime,
) -> DbResult<()> {
    if service_names.is_empty() {
        return Ok(());
    }
    db.ensure_writable()?;
    for table in PART_TABLES {
        sqlx::query(&format!(
            "UPDATE {table} SET is_removed_from_compose = false, updated_at = $3 \
             WHERE service_id = $1 AND name = ANY($2) AND is_removed_from_compose = true \
             AND deleted_at IS NULL"
        ))
        .bind(service_id)
        .bind(service_names)
        .bind(now)
        .execute(db.pool())
        .await?;
        sqlx::query(&format!(
            "UPDATE {table} SET is_removed_from_compose = true, updated_at = $3 \
             WHERE service_id = $1 AND NOT (name = ANY($2)) AND is_removed_from_compose = false \
             AND deleted_at IS NULL"
        ))
        .bind(service_id)
        .bind(service_names)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// The names of the storage rows (`local_persistent_volumes`) that the applications and databases
/// of a service still have (`removeContainersWithRemovedVolumesCommands()`).
pub async fn storage_names(db: &Db, service_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT v.name FROM local_persistent_volumes v WHERE \
           (v.resource_type = $2 AND v.resource_id IN (SELECT id FROM service_applications WHERE service_id = $1 AND deleted_at IS NULL)) \
           OR (v.resource_type = $3 AND v.resource_id IN (SELECT id FROM service_databases WHERE service_id = $1 AND deleted_at IS NULL)) \
         ORDER BY v.id",
    )
    .bind(service_id)
    .bind(crate::services::APPLICATION_MORPH)
    .bind(crate::services::DATABASE_MORPH)
    .fetch_all(db.pool())
    .await?)
}

/// The names of the parts of a service that are marked as removed from its compose file.
pub async fn removed_from_compose_names(db: &Db, service_id: i64) -> DbResult<Vec<String>> {
    let mut names = Vec::new();
    for table in PART_TABLES {
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT name FROM {table} WHERE service_id = $1 AND is_removed_from_compose AND deleted_at IS NULL ORDER BY id"
        ))
        .bind(service_id)
        .fetch_all(db.pool())
        .await?;
        names.extend(rows);
    }
    Ok(names)
}
