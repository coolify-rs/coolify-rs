//! `tags` and the `taggables` pivot, for any taggable resource (morph type + id).

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::records::{Record, parse_row};
use crate::{Db, DbError, DbResult};

fn decode(t: &str) -> DbResult<Record> {
    parse_row(t).map_err(|e| DbError::Schema(format!("decoding tag row: {e}")))
}

/// `$resource->tags` (`morphToMany`, the query Eloquent runs, so the order is the same).
pub async fn for_resource(db: &Db, morph: &str, resource_id: i64) -> DbResult<Vec<Record>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM (SELECT tags.* FROM tags INNER JOIN taggables ON tags.id = taggables.tag_id \
         WHERE taggables.taggable_id = $1 AND taggables.taggable_type = $2) t",
    )
    .bind(resource_id)
    .bind(morph)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(|t| decode(t)).collect()
}

/// `Tag::query()->createOrFirst(['team_id' => $teamId, 'name' => $name])`: the tag id.
pub async fn create_or_first(
    db: &Db,
    team_id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<i64> {
    db.ensure_writable()?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO tags (uuid, name, team_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) \
         ON CONFLICT (team_id, name) DO NOTHING RETURNING id",
    )
    .bind(new_public_id())
    .bind(name)
    .bind(team_id)
    .bind(now)
    .fetch_optional(db.pool())
    .await?;
    if let Some(id) = id {
        return Ok(id);
    }
    Ok(
        sqlx::query_scalar("SELECT id FROM tags WHERE team_id = $1 AND name = $2 LIMIT 1")
            .bind(team_id)
            .bind(name)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `$resource->tags()->syncWithoutDetaching([$tagId])`.
pub async fn attach(db: &Db, morph: &str, resource_id: i64, tag_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO taggables (tag_id, taggable_id, taggable_type) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(tag_id)
    .bind(resource_id)
    .bind(morph)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `Tag::where('team_id', $teamId)->where('uuid', $uuid)->first()`: the tag id.
pub async fn id_in_team(db: &Db, team_id: i64, uuid: &str) -> DbResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM tags WHERE team_id = $1 AND uuid = $2 LIMIT 1")
            .bind(team_id)
            .bind(uuid)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// `$resource->tags()->detach($tagId)`: whether the tag was attached. The tag stays.
pub async fn detach(db: &Db, morph: &str, resource_id: i64, tag_id: i64) -> DbResult<bool> {
    db.ensure_writable()?;
    let done = sqlx::query(
        "DELETE FROM taggables WHERE tag_id = $1 AND taggable_id = $2 AND taggable_type = $3",
    )
    .bind(tag_id)
    .bind(resource_id)
    .bind(morph)
    .execute(db.pool())
    .await?;
    Ok(done.rows_affected() > 0)
}

/// `->whereHas('tags', fn ($q) => $q->where('name', $name))`: the ids of the tagged resources.
pub async fn resource_ids_with_tag(db: &Db, morph: &str, name: &str) -> DbResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT taggables.taggable_id FROM taggables INNER JOIN tags ON tags.id = taggables.tag_id \
         WHERE taggables.taggable_type = $1 AND tags.name = $2",
    )
    .bind(morph)
    .bind(name)
    .fetch_all(db.pool())
    .await?)
}
