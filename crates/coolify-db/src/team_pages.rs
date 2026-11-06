//! Rows behind the team screens: `Team/Create`, `Team/DangerZone` (`DeleteTeam`),
//! `Team/InviteLink`, `Team/Invitations`, `Team/AdminView`, and the deletion of users
//! (`User::boot` deleting hook, `DeleteUserAccount`, `DeleteTeamlessUser`).

use chrono::NaiveDateTime;
use coolify_domain::TeamId;
use sqlx::{Postgres, Transaction};

use crate::{Db, DbError, DbResult};

const USER_TOKENABLE: &str = "App\\Models\\User";

/// `Team::create([...])` and `$user->teams()->attach($team, ['role' => 'owner'])`.
pub async fn create_team(
    db: &Db,
    user_id: i64,
    name: &str,
    description: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<TeamId> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let team: TeamId = sqlx::query_scalar(
        "INSERT INTO teams (name, description, personal_team, is_mcp_server_enabled, \
           is_build_server_fallback_enabled, is_two_factor_required, created_at, updated_at) \
         VALUES ($1, $2, false, true, true, false, $3, $3) RETURNING id",
    )
    .bind(coolify_domain::naming::strip_tags(Some(name)))
    .bind(coolify_domain::naming::strip_tags(description))
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    attach(&mut tx, team, user_id, "owner").await?;
    tx.commit().await?;
    Ok(team)
}

/// `User::recreate_personal_team` and the `created` hook of `User`.
pub async fn recreate_personal_team(
    db: &Db,
    user_id: i64,
    user_name: &str,
    now: NaiveDateTime,
) -> DbResult<TeamId> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let team = personal_team(&mut tx, user_id, user_name, now).await?;
    tx.commit().await?;
    Ok(team)
}

async fn personal_team(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    user_name: &str,
    now: NaiveDateTime,
) -> DbResult<TeamId> {
    let team: TeamId = sqlx::query_scalar(
        "INSERT INTO teams (name, personal_team, show_boarding, is_mcp_server_enabled, \
           is_build_server_fallback_enabled, is_two_factor_required, created_at, updated_at) \
         VALUES ($1, true, true, true, true, false, $2, $2) RETURNING id",
    )
    .bind(coolify_domain::naming::strip_tags(Some(&format!(
        "{user_name}'s Team"
    ))))
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    attach(tx, team, user_id, "owner").await?;
    Ok(team)
}

async fn attach(
    tx: &mut Transaction<'_, Postgres>,
    team_id: TeamId,
    user_id: i64,
    role: &str,
) -> DbResult<()> {
    sqlx::query("INSERT INTO team_user (team_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(team_id)
        .bind(user_id)
        .bind(role)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `User::create([... 'force_password_reset' => true])` for an invitee without an account. The
/// bool of the result is the id of the personal team, whose settings rows the caller creates.
pub async fn create_invited_user(
    db: &Db,
    name: &str,
    email: &str,
    password_hash: &str,
    now: NaiveDateTime,
) -> DbResult<(i64, TeamId)> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (name, email, password, force_password_reset, created_at, updated_at) \
         VALUES ($1, $2, $3, true, $4, $4) RETURNING id",
    )
    .bind(name)
    .bind(email)
    .bind(password_hash)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    let team = personal_team(&mut tx, id, name, now).await?;
    tx.commit().await?;
    Ok((id, team))
}

/// What the team pages need to know of a team beyond its settings.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TeamFacts {
    pub name: String,
    pub personal_team: bool,
    pub avatar_path: Option<String>,
    pub avatar_storage_type: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

/// The row as Eloquent holds it, for `Auditable`.
pub async fn team_row(db: &Db, team_id: TeamId) -> DbResult<Option<crate::records::Record>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT row_to_json(t)::text FROM teams t WHERE t.id = $1")
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?;
    row.map(|r| crate::records::parse_row(&r))
        .transpose()
        .map_err(|e| DbError::Schema(format!("decoding team row: {e}")))
}

pub async fn facts(db: &Db, team_id: TeamId) -> DbResult<Option<TeamFacts>> {
    Ok(sqlx::query_as(
        "SELECT name, personal_team, avatar_path, avatar_storage_type, updated_at FROM teams WHERE id = $1",
    )
    .bind(team_id)
    .fetch_optional(db.pool())
    .await?)
}

/// `currentTeam()->subscription`.
pub async fn has_subscription(db: &Db, team_id: TeamId) -> DbResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM subscriptions WHERE team_id = $1)")
            .bind(team_id)
            .fetch_one(db.pool())
            .await?,
    )
}

/// `Team::deletionBlockers()`: projects, servers and Git sources of the team.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Blockers {
    pub projects: i64,
    pub servers: i64,
    pub sources: i64,
}

impl Blockers {
    pub fn is_empty(&self) -> bool {
        self.projects == 0 && self.servers == 0 && self.sources == 0
    }
}

pub async fn deletion_blockers(db: &Db, team_id: TeamId) -> DbResult<Blockers> {
    let (projects, servers, sources): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM projects WHERE team_id = $1), \
                (SELECT count(*) FROM servers WHERE team_id = $1 AND deleted_at IS NULL), \
                (SELECT count(*) FROM github_apps WHERE team_id = $1 AND is_system_wide = false) \
              + (SELECT count(*) FROM gitlab_apps WHERE team_id = $1 AND is_system_wide = false) \
              + (SELECT count(*) FROM gitea_apps WHERE team_id = $1 AND is_system_wide = false)",
    )
    .bind(team_id)
    .fetch_one(db.pool())
    .await?;
    Ok(Blockers {
        projects,
        servers,
        sources,
    })
}

/// `currentTeam()->projects` and `currentTeam()->servers`: uuid and name.
pub async fn resources(
    db: &Db,
    team_id: TeamId,
) -> DbResult<(Vec<(String, String)>, Vec<(String, String)>)> {
    let projects = sqlx::query_as("SELECT uuid, name FROM projects WHERE team_id = $1")
        .bind(team_id)
        .fetch_all(db.pool())
        .await?;
    let servers =
        sqlx::query_as("SELECT uuid, name FROM servers WHERE team_id = $1 AND deleted_at IS NULL")
            .bind(team_id)
            .fetch_all(db.pool())
            .await?;
    Ok((projects, servers))
}

/// `Application::isRunning` for any application of the team.
pub async fn has_running_applications(db: &Db, team_id: TeamId) -> DbResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM applications a JOIN environments e ON e.id = a.environment_id \
           JOIN projects p ON p.id = e.project_id \
         WHERE p.team_id = $1 AND a.deleted_at IS NULL AND a.status LIKE '%running%')",
    )
    .bind(team_id)
    .fetch_one(db.pool())
    .await?)
}

/// `Team::deleteOwnedRecords()` and the delete, inside the caller's transaction.
async fn delete_team_row(tx: &mut Transaction<'_, Postgres>, team_id: TeamId) -> DbResult<()> {
    let team = team_id.to_string();
    sqlx::query("DELETE FROM personal_access_tokens WHERE tokenable_type = $1 AND team_id = $2")
        .bind(USER_TOKENABLE)
        .bind(&team)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM private_keys WHERE team_id = $1")
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
    for table in ["github_apps", "gitlab_apps", "gitea_apps"] {
        sqlx::query(&format!(
            "UPDATE {table} SET team_id = 0 WHERE team_id = $1 AND is_system_wide = true"
        ))
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(&format!("DELETE FROM {table} WHERE team_id = $1"))
            .bind(team_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("DELETE FROM taggables WHERE tag_id IN (SELECT id FROM tags WHERE team_id = $1)")
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
    for table in ["tags", "shared_environment_variables", "s3_storages"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE team_id = $1"))
            .bind(team_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("DELETE FROM teams WHERE id = $1")
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The members `DeleteTeam` signs out, and the transaction's result.
#[derive(Debug)]
pub enum DeleteTeamError {
    NotOwner,
    RunningApplications,
    Servers,
    Resources,
    Db(DbError),
}

impl From<sqlx::Error> for DeleteTeamError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.into())
    }
}

impl From<DbError> for DeleteTeamError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}

/// `App\Actions\Team\DeleteTeam::handle`, up to resolving the next team. Returns the other
/// members, whose sessions end. The owner keeps a dangling pivot row, as upstream: `team_user`
/// has no foreign key and `DeleteTeam` only detaches the other members.
pub async fn delete_team(
    db: &Db,
    team_id: TeamId,
    user_id: i64,
) -> Result<Vec<i64>, DeleteTeamError> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("SELECT id FROM teams WHERE id = $1 FOR UPDATE")
        .bind(team_id)
        .execute(&mut *tx)
        .await?;
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM team_user WHERE team_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(team_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if role.as_deref() != Some("owner") {
        return Err(DeleteTeamError::NotOwner);
    }
    if has_running_applications(db, team_id).await? {
        return Err(DeleteTeamError::RunningApplications);
    }
    let blockers = deletion_blockers(db, team_id).await?;
    if blockers.servers > 0 {
        return Err(DeleteTeamError::Servers);
    }
    if !blockers.is_empty() {
        return Err(DeleteTeamError::Resources);
    }
    let members: Vec<i64> =
        sqlx::query_scalar("SELECT user_id FROM team_user WHERE team_id = $1 AND user_id <> $2")
            .bind(team_id)
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM team_user WHERE team_id = $1 AND user_id <> $2")
        .bind(team_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    // clearStoredTeamIfMatches for every member and the owner.
    sqlx::query("UPDATE users SET current_team_id = NULL WHERE current_team_id = $1")
        .bind(team_id)
        .execute(&mut *tx)
        .await?;
    delete_team_row(&mut tx, team_id).await?;
    tx.commit().await?;
    Ok(members)
}

/// `$team->forceFill(['avatar_path' => ...])->save()`.
pub async fn set_team_avatar(
    db: &Db,
    team_id: TeamId,
    path: Option<&str>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE teams SET avatar_path = $2, avatar_storage_type = $3, avatar_s3_storage_id = NULL, updated_at = $4 \
         WHERE id = $1 AND (avatar_path IS DISTINCT FROM $2 OR avatar_storage_type IS DISTINCT FROM $3 OR avatar_s3_storage_id IS NOT NULL)",
    )
    .bind(team_id)
    .bind(path)
    .bind(path.map(|_| "local"))
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

// ---- Invitations ----

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Invitation {
    pub id: i64,
    pub uuid: String,
    pub team_id: TeamId,
    pub email: String,
    pub role: String,
    /// `encrypted` cast: decrypt before use.
    pub link: String,
    pub via: String,
    pub created_at: Option<NaiveDateTime>,
}

const INVITATION_COLUMNS: &str = "id, uuid, team_id, email, role, link, via, created_at";

/// `TeamInvitation::ownedByCurrentTeam()->get()`.
pub async fn invitations(db: &Db, team_id: TeamId) -> DbResult<Vec<Invitation>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM team_invitations WHERE team_id = $1"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn invitation_in_team(db: &Db, team_id: TeamId, id: i64) -> DbResult<Option<Invitation>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM team_invitations WHERE team_id = $1 AND id = $2"
    ))
    .bind(team_id)
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn invitation_by_email(
    db: &Db,
    team_id: TeamId,
    email: &str,
) -> DbResult<Option<Invitation>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM team_invitations WHERE team_id = $1 AND email = $2"
    ))
    .bind(team_id)
    .bind(email)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn invitation_by_uuid(db: &Db, uuid: &str) -> DbResult<Option<Invitation>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM team_invitations WHERE uuid = $1"
    ))
    .bind(uuid)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn invitations_by_email(db: &Db, email: &str) -> DbResult<Vec<Invitation>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM team_invitations WHERE email = $1"
    ))
    .bind(email)
    .fetch_all(db.pool())
    .await?)
}

/// `TeamInvitation::firstOrCreate([...])`; the link is already encrypted.
#[allow(clippy::too_many_arguments)]
pub async fn create_invitation(
    db: &Db,
    team_id: TeamId,
    uuid: &str,
    email: &str,
    role: &str,
    encrypted_link: &str,
    via: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO team_invitations (team_id, uuid, email, role, link, via, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
    )
    .bind(team_id)
    .bind(uuid)
    .bind(email)
    .bind(role)
    .bind(encrypted_link)
    .bind(via)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_invitation(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM team_invitations WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `$user->teams()->attach($team, ['role' => $invitation->role])`.
pub async fn attach_member(db: &Db, team_id: TeamId, user_id: i64, role: &str) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    attach(&mut tx, team_id, user_id, role).await?;
    tx.commit().await?;
    Ok(())
}

/// `$user->forceFill(['password' => ...])->save()` on accepting a magic link. Also drops the
/// tokens, as `DeletesUserSessions` does for a changed password.
pub async fn set_password(db: &Db, user_id: i64, hash: &str, now: NaiveDateTime) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("UPDATE users SET password = $2, updated_at = $3 WHERE id = $1")
        .bind(user_id)
        .bind(hash)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    revoke_user_tokens(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(())
}

async fn revoke_user_tokens(tx: &mut Transaction<'_, Postgres>, user_id: i64) -> DbResult<()> {
    sqlx::query(
        "DELETE FROM personal_access_tokens WHERE tokenable_type = $1 AND tokenable_id = $2",
    )
    .bind(USER_TOKENABLE)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `User::deleteIfNotVerifiedAndForcePasswordReset` up to the decision: a provisional invitee
/// with no invitation left and no team but their own.
pub async fn is_disposable_invitee(db: &Db, email: &str) -> DbResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT u.id FROM users u WHERE u.email = $1 AND u.email_verified_at IS NULL AND u.force_password_reset = true \
           AND NOT EXISTS (SELECT 1 FROM team_invitations i WHERE i.email = u.email) \
           AND NOT EXISTS (SELECT 1 FROM team_user tu JOIN teams t ON t.id = tu.team_id \
             WHERE tu.user_id = u.id AND (t.personal_team = false OR EXISTS ( \
               SELECT 1 FROM team_user o WHERE o.team_id = t.id AND o.user_id <> u.id)))",
    )
    .bind(email)
    .fetch_optional(db.pool())
    .await?)
}

// ---- Instance users (`Team/AdminView`) ----

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminUser {
    pub id: i64,
    pub name: String,
    pub email: String,
}

/// The query of `Team\AdminView::render`. `team_filter` is `all`, `current` or `outside`.
#[allow(clippy::too_many_arguments)]
pub async fn admin_users(
    db: &Db,
    me: i64,
    team_id: TeamId,
    search: &str,
    team_filter: &str,
    sort: &str,
    per_page: i64,
    page: i64,
) -> DbResult<(i64, Vec<AdminUser>)> {
    let team_clause = match team_filter {
        "current" => {
            " AND EXISTS (SELECT 1 FROM team_user tu JOIN teams t ON t.id = tu.team_id WHERE tu.user_id = users.id AND t.id = $2)"
        }
        "outside" => {
            " AND NOT EXISTS (SELECT 1 FROM team_user tu JOIN teams t ON t.id = tu.team_id WHERE tu.user_id = users.id AND t.id = $2)"
        }
        _ => " AND $2::bigint IS NOT NULL",
    };
    let order = match sort {
        "name_desc" => "name DESC, id",
        "email_asc" => "email, id",
        "email_desc" => "email DESC, id",
        "name_asc" => "name, id",
        _ => "id",
    };
    let wh = format!("id <> $1{team_clause} AND ($3 = '' OR name LIKE $4 OR email LIKE $4)");
    let like = format!("%{search}%");
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM users WHERE {wh}"))
        .bind(me)
        .bind(team_id)
        .bind(search)
        .bind(&like)
        .fetch_one(db.pool())
        .await?;
    let rows = sqlx::query_as(&format!(
        "SELECT id, name, email FROM users WHERE {wh} ORDER BY {order} LIMIT $5 OFFSET $6"
    ))
    .bind(me)
    .bind(team_id)
    .bind(search)
    .bind(&like)
    .bind(per_page)
    .bind((page - 1).saturating_mul(per_page))
    .fetch_all(db.pool())
    .await?;
    Ok((total, rows))
}

/// One team of a user, with the facts account deletion looks at.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserTeam {
    pub id: TeamId,
    pub name: String,
    pub role: String,
    pub members: i64,
}

/// `$user->teams()->with('members')`.
pub async fn user_teams(db: &Db, user_id: i64) -> DbResult<Vec<UserTeam>> {
    Ok(sqlx::query_as(
        "SELECT t.id, t.name, tu.role, (SELECT count(*) FROM team_user m JOIN users u ON u.id = m.user_id WHERE m.team_id = t.id) AS members \
         FROM team_user tu JOIN teams t ON t.id = tu.team_id WHERE tu.user_id = $1 ORDER BY tu.id",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?)
}

/// `team_user.role` of the root team.
pub async fn root_team_role(db: &Db, user_id: i64) -> DbResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT role FROM team_user WHERE team_id = 0 AND user_id = $1")
            .bind(user_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

pub async fn user_email(db: &Db, user_id: i64) -> DbResult<Option<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT name, email FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// Why [`delete_user`] cannot finish.
#[derive(Debug)]
pub enum DeleteUserError {
    AloneInRootTeam,
    /// A team only this user belongs to still owns servers or projects, whose removal has side
    /// effects that are not ported.
    SoloTeamWithResources(String),
    Db(DbError),
}

impl From<sqlx::Error> for DeleteUserError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.into())
    }
}

impl From<DbError> for DeleteUserError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}

/// `$user->delete()` with the deleting hook of `User` (`Team/AdminView`, and invitees removed by
/// `deleteIfNotVerifiedAndForcePasswordReset`). Returns the rows of the teams that went with the
/// user, for their audit events.
pub async fn delete_user(
    db: &Db,
    user_id: i64,
) -> Result<Vec<crate::records::Record>, DeleteUserError> {
    db.ensure_writable()?;
    let teams = user_teams(db, user_id).await?;
    let email = user_email(db, user_id).await?.map(|u| u.1);
    let mut deleted = Vec::new();
    let mut tx = db.pool().begin().await?;
    revoke_user_tokens(&mut tx, user_id).await?;
    for team in &teams {
        let alone = team.members == 1;
        if team.id == 0 && alone {
            return Err(DeleteUserError::AloneInRootTeam);
        }
        if alone {
            let b = deletion_blockers(db, team.id).await?;
            if b.projects > 0 || b.servers > 0 {
                return Err(DeleteUserError::SoloTeamWithResources(team.name.clone()));
            }
            if let Some(row) = team_row(db, team.id).await? {
                deleted.push(row);
            }
            detach(&mut tx, team.id, user_id).await?;
            delete_team_row(&mut tx, team.id).await?;
            if let Some(email) = &email {
                sqlx::query("DELETE FROM team_invitations WHERE email = $1")
                    .bind(email)
                    .execute(&mut *tx)
                    .await?;
            }
            continue;
        }
        if team.role == "owner" {
            let others: Vec<(i64, String)> = sqlx::query_as(
                "SELECT user_id, role FROM team_user WHERE team_id = $1 AND user_id <> $2 ORDER BY id",
            )
            .bind(team.id)
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
            if !others.iter().any(|(_, r)| r == "owner" || r == "admin")
                && let Some((other, _)) = others.iter().find(|(_, r)| r == "member")
            {
                sqlx::query(
                    "UPDATE team_user SET role = 'owner' WHERE team_id = $1 AND user_id = $2",
                )
                .bind(team.id)
                .bind(other)
                .execute(&mut *tx)
                .await?;
                // The new owner keeps the tokens: the API checks the abilities against the role
                // on each request.
            }
        }
        detach(&mut tx, team.id, user_id).await?;
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(deleted)
}

async fn detach(tx: &mut Transaction<'_, Postgres>, team_id: TeamId, user_id: i64) -> DbResult<()> {
    sqlx::query("DELETE FROM team_user WHERE team_id = $1 AND user_id = $2")
        .bind(team_id)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `DeleteUserAccount::handle` once the blockers are clear: `DeleteTeamData` for single-member
/// teams (empty, so only their settings go), `DeleteUserTeams`, `DeleteTeamlessUser`. Returns the
/// rows of the deleted teams, for their audit events.
pub async fn delete_account(
    db: &Db,
    user_id: i64,
    email: &str,
) -> DbResult<Vec<crate::records::Record>> {
    db.ensure_writable()?;
    let teams = user_teams(db, user_id).await?;
    let mut deleted = Vec::new();
    let mut tx = db.pool().begin().await?;
    revoke_user_tokens(&mut tx, user_id).await?;
    for team in &teams {
        if team.id == 0 {
            continue;
        }
        if team.members == 1 && team.role == "owner" {
            for table in [
                "scheduled_database_backups",
                "scheduled_tasks",
                "subscriptions",
            ] {
                sqlx::query(&format!("DELETE FROM {table} WHERE team_id = $1"))
                    .bind(team.id)
                    .execute(&mut *tx)
                    .await?;
            }
            if let Some(row) = team_row(db, team.id).await? {
                deleted.push(row);
            }
            sqlx::query("UPDATE users SET current_team_id = NULL WHERE current_team_id = $1")
                .bind(team.id)
                .execute(&mut *tx)
                .await?;
            detach(&mut tx, team.id, user_id).await?;
            delete_team_row(&mut tx, team.id).await?;
            continue;
        }
        if team.role == "owner" {
            // `DeleteUserTeams::findNewOwner`: an admin takes over from the only owner.
            let others: Vec<(i64, String)> = sqlx::query_as(
                "SELECT user_id, role FROM team_user WHERE team_id = $1 AND user_id <> $2 ORDER BY id",
            )
            .bind(team.id)
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
            if !others.iter().any(|(_, r)| r == "owner")
                && let Some((admin, _)) = others.iter().find(|(_, r)| r == "admin")
            {
                sqlx::query(
                    "UPDATE team_user SET role = 'owner' WHERE team_id = $1 AND user_id = $2",
                )
                .bind(team.id)
                .bind(admin)
                .execute(&mut *tx)
                .await?;
                // The new owner keeps the tokens: the API checks the abilities against the role
                // on each request.
            }
        }
        detach(&mut tx, team.id, user_id).await?;
    }
    sqlx::query("DELETE FROM team_user WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM oauth_identities WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_changelog_reads WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM password_reset_tokens WHERE email = $1")
        .bind(email)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(deleted)
}
