//! Resources a user starred for quick access on the dashboard: upstream `App\Models\Star`,
//! `App\Traits\HasStars`, `App\Livewire\StarToggle` and the star methods of `App\Livewire\Dashboard`
//! (table `stars`, migration `2026_11_04_000000_create_stars_table`).
//!
//! A database that does not have the table yet (an older schema) has no stars: reads answer empty
//! and the delete hook does nothing.

use chrono::NaiveDateTime;
use coolify_domain::TeamId;

use crate::databases::Kind;
use crate::{Db, DbError, DbResult};

/// `Server::class` as `getMorphClass()` stores it (upstream has no morph map).
pub const SERVER_MORPH: &str = r"App\Models\Server";

/// `Star::STARRABLE_TYPES`, each with the table of its model.
pub fn starrable_types() -> Vec<(&'static str, &'static str)> {
    let mut types = vec![
        (SERVER_MORPH, "servers"),
        (crate::applications::MORPH, "applications"),
        (crate::services::MORPH, "services"),
    ];
    types.extend(Kind::ALL.into_iter().map(|k| (k.morph(), k.table())));
    types
}

/// Whether the schema has the `stars` table.
pub fn has_table(db: &Db) -> bool {
    db.schema().columns.contains_key("stars")
}

/// The kind of resource a star button is shown for. A database page does not say which of the
/// nine standalone models it shows, so `Database` looks the uuid up in all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKind {
    Server,
    Application,
    Service,
    Database,
}

impl StarKind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "server" => Some(Self::Server),
            "application" => Some(Self::Application),
            "service" => Some(Self::Service),
            "database" => Some(Self::Database),
            _ => None,
        }
    }

    /// `(morph class, table)` of the models of this kind.
    fn models(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Server => vec![(SERVER_MORPH, "servers")],
            Self::Application => vec![(crate::applications::MORPH, "applications")],
            Self::Service => vec![(crate::services::MORPH, "services")],
            Self::Database => Kind::ALL
                .into_iter()
                .map(|k| (k.morph(), k.table()))
                .collect(),
        }
    }
}

/// A model a user can star: `$resource->getMorphClass()` and `$resource->getKey()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Starrable {
    pub morph: &'static str,
    pub id: i64,
    /// `None` for a server.
    pub environment_id: Option<i64>,
    pub uuid: String,
}

/// The undeleted resource of the team with this uuid: a server of the team, or a resource in one
/// of the team's projects. Access grants are the caller's to check (`authorize('view', $resource)`).
pub async fn find_starrable(
    db: &Db,
    team_id: TeamId,
    kind: StarKind,
    uuid: &str,
) -> DbResult<Option<Starrable>> {
    for (morph, table) in kind.models() {
        let found: Option<(i64, Option<i64>)> = if kind == StarKind::Server {
            sqlx::query_as(
                "SELECT id, NULL::bigint FROM servers WHERE uuid = $1 AND team_id = $2 AND deleted_at IS NULL",
            )
            .bind(uuid)
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?
        } else {
            sqlx::query_as(&format!(
                "SELECT r.id, r.environment_id FROM {table} r \
                 JOIN environments e ON e.id = r.environment_id JOIN projects p ON p.id = e.project_id \
                 WHERE r.uuid = $1 AND p.team_id = $2 AND r.deleted_at IS NULL ORDER BY r.id LIMIT 1"
            ))
            .bind(uuid)
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?
        };
        if let Some((id, environment_id)) = found {
            return Ok(Some(Starrable {
                morph,
                id,
                environment_id,
                uuid: uuid.to_owned(),
            }));
        }
    }
    Ok(None)
}

/// `StarToggle::starQuery()->exists()`.
pub async fn is_starred(db: &Db, user_id: i64, morph: &str, id: i64) -> DbResult<bool> {
    if !has_table(db) {
        return Ok(false);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM stars WHERE user_id = $1 AND starrable_type = $2 AND starrable_id = $3)",
    )
    .bind(user_id)
    .bind(morph)
    .bind(id)
    .fetch_one(db.pool())
    .await?)
}

/// `StarToggle::toggle()`: removes the user's star of the resource, or adds one after the user's
/// last star (`max('position') + 1`, 0 for the first). Returns whether the resource is starred now.
pub async fn toggle(
    db: &Db,
    user_id: i64,
    morph: &str,
    id: i64,
    now: NaiveDateTime,
) -> DbResult<bool> {
    db.ensure_writable()?;
    if !has_table(db) {
        return Err(DbError::Schema("the stars table is missing".into()));
    }
    let removed = sqlx::query(
        "DELETE FROM stars WHERE user_id = $1 AND starrable_type = $2 AND starrable_id = $3",
    )
    .bind(user_id)
    .bind(morph)
    .bind(id)
    .execute(db.pool())
    .await?
    .rows_affected();
    if removed > 0 {
        return Ok(false);
    }
    // `firstOrCreate`: a star added by a second click in between is kept as it is.
    sqlx::query(
        "INSERT INTO stars (user_id, starrable_type, starrable_id, position, created_at, updated_at) \
         SELECT $1, $2, $3, COALESCE((SELECT MAX(position) FROM stars WHERE user_id = $1), -1) + 1, $4, $4 \
         ON CONFLICT (user_id, starrable_type, starrable_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(morph)
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(true)
}

/// `Dashboard::unstar()`: `auth()->user()->stars()->whereKey($starId)->delete()`.
pub async fn unstar(db: &Db, user_id: i64, star_id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    if !has_table(db) {
        return Ok(());
    }
    sqlx::query("DELETE FROM stars WHERE id = $2 AND user_id = $1")
        .bind(user_id)
        .bind(star_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `Dashboard::reorderStars()`: the position of each of the user's stars is its index in the list.
/// Stars that are not in the list (other teams) and stars of other users keep their positions.
pub async fn reorder(db: &Db, user_id: i64, star_ids: &[i64], now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    if !has_table(db) {
        return Ok(());
    }
    let mut tx = db.pool().begin().await?;
    for (position, star_id) in star_ids.iter().enumerate() {
        let Ok(position) = i32::try_from(position) else {
            break;
        };
        // Eloquent writes (and touches `updated_at`) only when the position changed.
        sqlx::query(
            "UPDATE stars SET position = $3, updated_at = $4 \
             WHERE id = $2 AND user_id = $1 AND position IS DISTINCT FROM $3",
        )
        .bind(user_id)
        .bind(*star_id)
        .bind(position)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `HasStars::bootHasStars()`: the stars of a model go when the model is deleted (soft deletes
/// included). Does nothing on a schema without the table.
pub async fn delete_for(db: &Db, morph: &str, id: i64) -> DbResult<()> {
    if !has_table(db) {
        return Ok(());
    }
    sqlx::query("DELETE FROM stars WHERE starrable_type = $1 AND starrable_id = $2")
        .bind(morph)
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// A star with the resource it points at, as `Dashboard::starredResources()` loads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarredResource {
    pub star_id: i64,
    pub position: i32,
    /// The morph class, one of [`starrable_types`].
    pub starrable_type: &'static str,
    pub starrable_id: i64,
    pub uuid: String,
    pub name: String,
    /// The team of the server, or of the project of the resource.
    pub team_id: Option<TeamId>,
    /// `None` for a server.
    pub environment_id: Option<i64>,
    pub environment_uuid: Option<String>,
    pub environment_name: Option<String>,
    pub project_uuid: Option<String>,
    pub project_name: Option<String>,
}

impl StarredResource {
    pub fn is_server(&self) -> bool {
        self.starrable_type == SERVER_MORPH
    }

    /// `$database->type()` when the star is on a standalone database.
    pub fn database_type(&self) -> Option<&'static str> {
        Kind::ALL
            .into_iter()
            .find(|k| k.morph() == self.starrable_type)
            .map(Kind::type_name)
    }
}

type ServerStar = (i64, i32, i64, String, String, Option<i64>);
type ResourceStar = (
    i64,
    i32,
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    String,
    String,
    String,
    String,
);

/// The user's stars on resources that still exist, `orderBy('position')->orderBy('id')`. Stars on
/// trashed models and on resources without an environment and project are left out (`$star->starrable`
/// is null, or `environment?->project?->team_id` is). The team and access filters are the caller's.
pub async fn starred(db: &Db, user_id: i64) -> DbResult<Vec<StarredResource>> {
    if !has_table(db) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (morph, table) in starrable_types() {
        if morph == SERVER_MORPH {
            let rows: Vec<ServerStar> = sqlx::query_as(
                "SELECT st.id, st.position, st.starrable_id, s.uuid::text, s.name::text, s.team_id \
                 FROM stars st JOIN servers s ON s.id = st.starrable_id \
                 WHERE st.user_id = $1 AND st.starrable_type = $2 AND s.deleted_at IS NULL",
            )
            .bind(user_id)
            .bind(morph)
            .fetch_all(db.pool())
            .await?;
            out.extend(rows.into_iter().map(|r| StarredResource {
                star_id: r.0,
                position: r.1,
                starrable_type: morph,
                starrable_id: r.2,
                uuid: r.3,
                name: r.4,
                team_id: r.5,
                environment_id: None,
                environment_uuid: None,
                environment_name: None,
                project_uuid: None,
                project_name: None,
            }));
            continue;
        }
        let rows: Vec<ResourceStar> = sqlx::query_as(&format!(
            "SELECT st.id, st.position, st.starrable_id, r.uuid::text, r.name::text, p.team_id, \
                    r.environment_id, e.uuid::text, e.name::text, p.uuid::text, p.name::text \
             FROM stars st JOIN {table} r ON r.id = st.starrable_id \
             JOIN environments e ON e.id = r.environment_id JOIN projects p ON p.id = e.project_id \
             WHERE st.user_id = $1 AND st.starrable_type = $2 AND r.deleted_at IS NULL"
        ))
        .bind(user_id)
        .bind(morph)
        .fetch_all(db.pool())
        .await?;
        out.extend(rows.into_iter().map(|r| StarredResource {
            star_id: r.0,
            position: r.1,
            starrable_type: morph,
            starrable_id: r.2,
            uuid: r.3,
            name: r.4,
            team_id: r.5,
            environment_id: r.6,
            environment_uuid: Some(r.7),
            environment_name: Some(r.8),
            project_uuid: Some(r.9),
            project_name: Some(r.10),
        }));
    }
    out.sort_by_key(|s| (s.position, s.star_id));
    Ok(out)
}

/// What `resource_logo()` reads of an application: `(build_pack, source_type, fqdn,
/// is_http_basic_auth_enabled)`.
pub type ApplicationLogo = (Option<String>, Option<String>, Option<String>, bool);

pub async fn application_logo(db: &Db, application_id: i64) -> DbResult<Option<ApplicationLogo>> {
    Ok(sqlx::query_as(
        "SELECT build_pack::text, source_type::text, fqdn::text, COALESCE(is_http_basic_auth_enabled, false) \
         FROM applications WHERE id = $1",
    )
    .bind(application_id)
    .fetch_optional(db.pool())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::{SERVER_MORPH, StarKind, starrable_types};

    /// `Star::STARRABLE_TYPES`: the server, the application, the service and the nine databases.
    #[test]
    fn starrable_types_are_the_twelve_upstream_models() {
        let types = starrable_types();
        let morphs: Vec<&str> = types.iter().map(|t| t.0).collect();
        assert_eq!(types.len(), 12);
        for model in [
            "Server",
            "Application",
            "Service",
            "StandalonePostgresql",
            "StandaloneMysql",
            "StandaloneMariadb",
            "StandaloneMongodb",
            "StandaloneRedis",
            "StandaloneKeydb",
            "StandaloneDragonfly",
            "StandaloneClickhouse",
            "StandaloneSqlite",
        ] {
            assert!(
                morphs.contains(&format!("App\\Models\\{model}").as_str()),
                "{model} is starrable"
            );
        }
        assert!(types.contains(&(SERVER_MORPH, "servers")));
        assert!(types.contains(&("App\\Models\\StandaloneRedis", "standalone_redis")));
    }

    #[test]
    fn star_kinds_parse_the_page_names_only() {
        assert_eq!(StarKind::parse("server"), Some(StarKind::Server));
        assert_eq!(StarKind::parse("application"), Some(StarKind::Application));
        assert_eq!(StarKind::parse("service"), Some(StarKind::Service));
        assert_eq!(StarKind::parse("database"), Some(StarKind::Database));
        assert_eq!(StarKind::parse("project"), None);
        assert_eq!(StarKind::parse("App\\Models\\Server"), None);
        assert_eq!(StarKind::Database.models().len(), 9);
    }
}
