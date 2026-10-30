//! `standalone_dockers` and `swarm_dockers` as `DestinationsController` uses them.

use chrono::NaiveDateTime;
use coolify_domain::TeamId;
use serde_json::Value;

use crate::records::laravel_datetime_from;
use crate::{Db, DbResult};

/// The morph classes stored in `destination_type`.
pub const STANDALONE_CLASS: &str = "App\\Models\\StandaloneDocker";
pub const SWARM_CLASS: &str = "App\\Models\\SwarmDocker";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Standalone,
    Swarm,
}

impl Kind {
    pub fn table(self) -> &'static str {
        match self {
            Self::Standalone => "standalone_dockers",
            Self::Swarm => "swarm_dockers",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Standalone => STANDALONE_CLASS,
            Self::Swarm => SWARM_CLASS,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Swarm => "swarm",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Destination {
    pub kind: Kind,
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub network: String,
    pub server_id: i64,
    pub server_uuid: Option<String>,
    pub team_id: Option<TeamId>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

impl Destination {
    /// `DestinationsController::transform`.
    pub fn transform(&self) -> Value {
        let time = |t: &Option<NaiveDateTime>| match t {
            Some(t) => Value::String(laravel_datetime_from(*t)),
            None => Value::Null,
        };
        let mut m = serde_json::Map::new();
        m.insert("uuid".into(), Value::String(self.uuid.clone()));
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("network".into(), Value::String(self.network.clone()));
        m.insert("type".into(), Value::String(self.kind.as_str().into()));
        m.insert(
            "server_uuid".into(),
            self.server_uuid.clone().map_or(Value::Null, Value::String),
        );
        m.insert("created_at".into(), time(&self.created_at));
        m.insert("updated_at".into(), time(&self.updated_at));
        Value::Object(m)
    }
}

type RowTuple = (
    i64,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<i64>,
    Option<NaiveDateTime>,
    Option<NaiveDateTime>,
);

fn map(kind: Kind, r: RowTuple) -> Destination {
    Destination {
        kind,
        id: r.0,
        uuid: r.1,
        name: r.2,
        network: r.3,
        server_id: r.4,
        server_uuid: r.5,
        team_id: r.6,
        created_at: r.7,
        updated_at: r.8,
    }
}

fn select(kind: Kind) -> String {
    format!(
        "SELECT d.id, d.uuid, d.name, d.network, d.server_id, s.uuid, s.team_id, d.created_at, d.updated_at \
         FROM {} d LEFT JOIN servers s ON s.id = d.server_id AND s.deleted_at IS NULL",
        kind.table()
    )
}

/// `teamScopedDockers`: standalone first, then swarm.
pub async fn list_for_team(db: &Db, team_id: TeamId) -> DbResult<Vec<Destination>> {
    let mut out = Vec::new();
    for kind in [Kind::Standalone, Kind::Swarm] {
        let rows: Vec<RowTuple> = sqlx::query_as(&format!("{} WHERE s.team_id = $1", select(kind)))
            .bind(team_id)
            .fetch_all(db.pool())
            .await?;
        out.extend(rows.into_iter().map(|r| map(kind, r)));
    }
    Ok(out)
}

/// `$server->standaloneDockers->concat($server->swarmDockers)`.
pub async fn list_for_server(db: &Db, server_id: i64) -> DbResult<Vec<Destination>> {
    let mut out = Vec::new();
    for kind in [Kind::Standalone, Kind::Swarm] {
        let rows: Vec<RowTuple> =
            sqlx::query_as(&format!("{} WHERE d.server_id = $1", select(kind)))
                .bind(server_id)
                .fetch_all(db.pool())
                .await?;
        out.extend(rows.into_iter().map(|r| map(kind, r)));
    }
    Ok(out)
}

/// `findDestinationForTeam` without the `firstOrFail`.
pub async fn find_in_team(db: &Db, team_id: TeamId, uuid: &str) -> DbResult<Option<Destination>> {
    for kind in [Kind::Standalone, Kind::Swarm] {
        let row: Option<RowTuple> = sqlx::query_as(&format!(
            "{} WHERE s.team_id = $1 AND d.uuid = $2 LIMIT 1",
            select(kind)
        ))
        .bind(team_id)
        .bind(uuid)
        .fetch_optional(db.pool())
        .await?;
        if let Some(r) = row {
            return Ok(Some(map(kind, r)));
        }
    }
    Ok(None)
}

pub async fn network_exists(db: &Db, kind: Kind, server_id: i64, network: &str) -> DbResult<bool> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE server_id = $1 AND network = $2)",
        kind.table()
    ))
    .bind(server_id)
    .bind(network)
    .fetch_one(db.pool())
    .await?)
}

/// `$class::create([...])`. `Ok(None)` on the unique constraint (`isUniqueConstraintViolation`).
pub async fn create(
    db: &Db,
    kind: Kind,
    uuid: &str,
    name: &str,
    network: &str,
    server_id: i64,
    now: NaiveDateTime,
) -> DbResult<Option<i64>> {
    db.ensure_writable()?;
    let r: Result<i64, sqlx::Error> = sqlx::query_scalar(&format!(
        "INSERT INTO {} (server_id, name, network, uuid, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
        kind.table()
    ))
    .bind(server_id)
    .bind(name)
    .bind(network)
    .bind(uuid)
    .bind(now)
    .fetch_one(db.pool())
    .await;
    match r {
        Ok(id) => Ok(Some(id)),
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `$destination->update(['name' => ...])`: `updated_at` moves only when the name changes.
pub async fn update_name(
    db: &Db,
    kind: Kind,
    id: i64,
    name: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!(
        "UPDATE {} SET name = $1, updated_at = $2 WHERE id = $3 AND name IS DISTINCT FROM $1",
        kind.table()
    ))
    .bind(name)
    .bind(now)
    .bind(id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, kind: Kind, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(&format!("DELETE FROM {} WHERE id = $1", kind.table()))
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `attachedTo()`: applications, databases or services on the destination. The
/// relations go through `SoftDeletes`, so trashed resources do not count.
pub async fn attached_to(db: &Db, kind: Kind, id: i64) -> DbResult<bool> {
    let tables: Vec<&str> = std::iter::once("applications")
        .chain(crate::servers::DATABASE_TABLES.iter().copied())
        .chain(std::iter::once("services"))
        .collect();
    let exists = tables
        .iter()
        .map(|t| {
            format!(
                "EXISTS (SELECT 1 FROM {t} WHERE destination_id = $1 AND destination_type = $2 AND deleted_at IS NULL)"
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    Ok(sqlx::query_scalar(&format!("SELECT {exists}"))
        .bind(id)
        .bind(kind.class())
        .fetch_one(db.pool())
        .await?)
}

/// `$server->standaloneDockers` or `swarmDockers`: the network names.
pub async fn networks(db: &Db, kind: Kind, server_id: i64) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT network FROM {} WHERE server_id = $1",
        kind.table()
    ))
    .bind(server_id)
    .fetch_all(db.pool())
    .await?)
}
