//! Queries of image updates (`UpdateResourceImage`): the writes that have no function in
//! `applications`, `services`, `databases` or `environment_variables`.
//!
//! The other writes of an image update use those modules: the tag of a Docker image application
//! (`applications::update_if_changed`), the image of a standalone database
//! (`databases::update_if_changed`), and a new environment variable
//! (`environment_variables::create_on_resource`).

use chrono::NaiveDateTime;

use super::{SERVICE_APPLICATION_MORPH, SERVICE_DATABASE_MORPH};
use crate::{Db, DbError, DbResult};

/// `$application->docker_compose_image_overrides = $overrides; $application->save()`: the image
/// override per Compose service, as the JSON text of Laravel's `array` cast, or null without
/// overrides. The caller does Eloquent's dirty check.
pub async fn set_compose_image_overrides(
    db: &Db,
    application_id: i64,
    overrides_json: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE applications SET docker_compose_image_overrides = $2::json, updated_at = $3 WHERE id = $1",
    )
    .bind(application_id)
    .bind(overrides_json)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// The new image of one application or database of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubImage<'a> {
    /// `App\Models\ServiceApplication` or `App\Models\ServiceDatabase`.
    pub morph: &'a str,
    pub id: i64,
    pub image: &'a str,
}

/// The `DB::transaction()` of `UpdateResourceImage::updateService()`: the compose file of the
/// service and the image of each application and database that changes, all or nothing. Used
/// for the update and to put the old values back. A value that did not change is not written
/// (Eloquent's dirty check).
pub async fn write_service_images(
    db: &Db,
    service_id: i64,
    docker_compose_raw: Option<&str>,
    images: &[SubImage<'_>],
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tables = Vec::with_capacity(images.len());
    for image in images {
        tables.push(sub_table(image.morph)?);
    }
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "UPDATE services SET docker_compose_raw = $2, updated_at = $3 \
         WHERE id = $1 AND docker_compose_raw IS DISTINCT FROM $2",
    )
    .bind(service_id)
    .bind(docker_compose_raw)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    for (image, table) in images.iter().zip(tables) {
        sqlx::query(&format!(
            "UPDATE {table} SET image = $2, updated_at = $3 WHERE id = $1 AND image IS DISTINCT FROM $2"
        ))
        .bind(image.id)
        .bind(image.image)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The table of an application or a database of a service.
fn sub_table(morph: &str) -> DbResult<&'static str> {
    match morph {
        SERVICE_APPLICATION_MORPH => Ok("service_applications"),
        SERVICE_DATABASE_MORPH => Ok("service_databases"),
        other => Err(DbError::Schema(format!(
            "{other} is not an application or a database of a service"
        ))),
    }
}

/// `$variable->value = $value; $variable->save()` for a value that is not a reference to a
/// shared variable, with the `saving` hook of the model: `is_shared` is false, and a value that
/// is not null ends `preserve_compose_null`. `value_encrypted` is `encrypt($value)`.
pub async fn set_variable_value(
    db: &Db,
    id: i64,
    value_encrypted: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE environment_variables SET value = $2, is_shared = false, \
         preserve_compose_null = CASE WHEN $2::text IS NOT NULL THEN false ELSE preserve_compose_null END, \
         updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(value_encrypted)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_parts_of_a_service_have_an_image_to_write() {
        assert_eq!(
            sub_table(SERVICE_APPLICATION_MORPH).ok(),
            Some("service_applications")
        );
        assert_eq!(
            sub_table(SERVICE_DATABASE_MORPH).ok(),
            Some("service_databases")
        );
        assert!(sub_table(r"App\Models\Application").is_err());
        assert!(sub_table("services; DROP TABLE services").is_err());
    }
}
