//! What upstream's container does at every start after its migrations: `ProductionSeeder` with the
//! seeders it calls (`start:seeder`), then the parts of `app:init` that apply here.
//!
//! Each row is created only when it is missing, so this is safe on a database that is already in
//! use, as upstream's seeders are. It runs in `coolify-rs boot` only, never in `coolify-rs serve`.

use std::path::Path;

use chrono::{NaiveDateTime, Utc};
use coolify_compat::Encrypter;
use coolify_compat::sshkey::{format_private_key, inspect};
use coolify_db::{Db, DbResult};
use rand::Rng;
use serde_json::json;

use crate::Jobs;
use crate::server_ops::{Srv, proxy};

/// `OauthSettingSeeder`'s providers, in its order.
const OAUTH_PROVIDERS: [&str; 11] = [
    "azure",
    "bitbucket",
    "clerk",
    "discord",
    "github",
    "gitlab",
    "google",
    "oidc",
    "authentik",
    "infomaniak",
    "zitadel",
];

/// `Team::created`'s notification settings.
const NOTIFICATION_TABLES: [&str; 6] = [
    "email_notification_settings",
    "discord_notification_settings",
    "slack_notification_settings",
    "telegram_notification_settings",
    "pushover_notification_settings",
    "webhook_notification_settings",
];

/// What the seeder found, for the steps that run once the server is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seeded {
    /// `Server::find(0)` was missing, so `StartProxy` and `CheckAndStartSentinelJob` follow.
    pub server_created: bool,
}

async fn exists(db: &Db, table: &'static str) -> DbResult<bool> {
    let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = 0)");
    Ok(sqlx::query_scalar(&sql).fetch_one(db.pool()).await?)
}

/// `InstanceSettings::randomAutoUpdateFrequency`.
fn random_auto_update_frequency() -> String {
    format!("{} 0 * * *", rand::thread_rng().gen_range(0..60))
}

/// The installer's key: `id.<user>@host.docker.internal` in the ssh key folder.
fn find_host_key(keys_dir: &Path) -> Option<(String, String)> {
    let entries = std::fs::read_dir(keys_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(user) = name
            .split_once('@')
            .filter(|(_, host)| host.starts_with("host.docker.internal"))
            .and_then(|(left, _)| left.strip_prefix("id."))
        {
            let key = std::fs::read_to_string(entry.path()).ok()?;
            return Some((user.to_owned(), key));
        }
    }
    None
}

async fn seed_team(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    if exists(db, "teams").await? {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO teams (id, name, description, personal_team, show_boarding, created_at, updated_at) \
         VALUES (0, 'Root Team', 'The root team', true, true, $1, $1)",
    )
    .bind(now)
    .execute(db.pool())
    .await?;
    for table in NOTIFICATION_TABLES {
        sqlx::query(&format!("INSERT INTO {table} (team_id) VALUES (0)"))
            .execute(db.pool())
            .await?;
    }
    Ok(())
}

async fn seed_instance_settings(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    if exists(db, "instance_settings").await? {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO instance_settings (id, auto_update_frequency, created_at, updated_at) \
         VALUES (0, $1, $2, $2)",
    )
    .bind(random_auto_update_frequency())
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

async fn seed_git_apps(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    if !exists(db, "github_apps").await? {
        sqlx::query(
            "INSERT INTO github_apps (id, uuid, name, api_url, html_url, is_public, team_id, created_at, updated_at) \
             VALUES (0, $1, 'Public GitHub', 'https://api.github.com', 'https://github.com', true, 0, $2, $2)",
        )
        .bind(coolify_domain::new_public_id())
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    if !exists(db, "gitlab_apps").await? {
        sqlx::query(
            "INSERT INTO gitlab_apps (id, uuid, name, api_url, html_url, is_public, team_id, created_at, updated_at) \
             VALUES (0, $1, 'Public GitLab', 'https://gitlab.com/api/v4', 'https://gitlab.com', true, 0, $2, $2)",
        )
        .bind(coolify_domain::new_public_id())
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// The localhost key. Returns the ssh user, which the file name holds, when the key was stored now.
async fn seed_private_key(
    db: &Db,
    encrypter: &Encrypter,
    keys_dir: &Path,
    now: NaiveDateTime,
) -> DbResult<Option<String>> {
    if exists(db, "private_keys").await? {
        return Ok(None);
    }
    let Some((user, key)) = find_host_key(keys_dir) else {
        tracing::warn!(
            "No SSH key found for the host machine (localhost). Your localhost connection won't work until then."
        );
        return Ok(None);
    };
    let stored = format_private_key(&key);
    let fingerprint = inspect(&stored).map(|info| info.fingerprint);
    sqlx::query(
        "INSERT INTO private_keys (id, uuid, name, description, private_key, team_id, fingerprint, created_at, updated_at) \
         VALUES (0, $1, 'localhost''s key', 'The private key for the Coolify host machine (localhost).', $2, 0, $3, $4, $4)",
    )
    .bind(coolify_domain::new_public_id())
    .bind(encrypter.encrypt_string(&stored))
    .bind(fingerprint)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(Some(user))
}

/// `Server::create` with the `Server::created` and `ServerSetting::creating` hooks, then the
/// seeder's `is_reachable` and `is_usable`. Returns whether the server was created.
async fn seed_server(
    db: &Db,
    encrypter: &Encrypter,
    user: &str,
    now: NaiveDateTime,
) -> DbResult<bool> {
    if exists(db, "servers").await? {
        sqlx::query(
            "UPDATE server_settings SET is_reachable = true, is_usable = true, updated_at = $1 WHERE server_id = 0",
        )
        .bind(now)
        .execute(db.pool())
        .await?;
        return Ok(false);
    }
    let uuid = coolify_domain::new_public_id();
    let name = "localhost";
    let proxy = json!({
        "type": "TRAEFIK",
        "status": "exited",
        "last_saved_settings": null,
        "last_applied_settings": null,
        "redirect_enabled": true,
    });
    let token = encrypter
        .encrypt_string(&encrypter.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()));
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "INSERT INTO servers (id, uuid, name, description, ip, \"user\", team_id, private_key_id, proxy, created_at, updated_at) \
         VALUES (0, $1, $2, 'This is the server where Coolify is running on. Don''t delete this!', 'host.docker.internal', $3, 0, 0, $4::json, $5, $5)",
    )
    .bind(&uuid)
    .bind(name)
    .bind(user)
    .bind(proxy.to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO server_settings (server_id, is_reachable, is_usable, sentinel_token, sentinel_custom_url, created_at, updated_at) \
         VALUES (0, true, true, $1, 'http://coolify:8080', $2, $2)",
    )
    .bind(token)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO standalone_dockers (id, name, uuid, network, server_id, created_at, updated_at) \
         VALUES (0, 'coolify', $1, 'coolify', 0, $2, $2)",
    )
    .bind(coolify_domain::new_public_id())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    for (key, value) in [
        ("COOLIFY_SERVER_UUID", &*uuid),
        ("COOLIFY_SERVER_NAME", name),
    ] {
        sqlx::query(
            "INSERT INTO shared_environment_variables (key, value, type, server_id, team_id, is_literal, created_at, updated_at) \
             VALUES ($1, $2, 'server', 0, 0, true, $3, $3)",
        )
        .bind(key)
        .bind(encrypter.encrypt_string(value))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// `OauthSettingSeeder`. Its other branch rebuilds rows of a release before `authentik`, which the
/// schema guard's baseline is newer than.
async fn seed_oauth(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    for provider in OAUTH_PROVIDERS {
        sqlx::query(
            "INSERT INTO oauth_settings (provider, created_at, updated_at) \
             SELECT $1, $2, $2 WHERE NOT EXISTS (SELECT 1 FROM oauth_settings WHERE provider = $1)",
        )
        .bind(provider)
        .bind(now)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// `SentinelSeeder`: a server without a token or URL gets one.
async fn seed_sentinel(db: &Db, encrypter: &Encrypter, now: NaiveDateTime) -> DbResult<()> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT s.id, s.uuid FROM servers s JOIN server_settings ss ON ss.server_id = s.id \
         WHERE COALESCE(ss.sentinel_token, '') = ''",
    )
    .fetch_all(db.pool())
    .await?;
    for (id, uuid) in rows {
        let token = encrypter.encrypt_string(
            &encrypter.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()),
        );
        sqlx::query(
            "UPDATE server_settings SET sentinel_token = $1, updated_at = $2 WHERE server_id = $3",
        )
        .bind(token)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    sqlx::query(
        "UPDATE server_settings SET sentinel_custom_url = 'http://coolify:8080', updated_at = $1 \
         WHERE server_id = 0 AND COALESCE(sentinel_custom_url, '') = ''",
    )
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `update_helper_version(get_versions_data())` of `CheckHelperImageJob`.
async fn update_helper_version(db: &Db, now: NaiveDateTime) -> DbResult<()> {
    let helper = crate::server_ops::helper_version(None);
    sqlx::query(
        "UPDATE instance_settings SET helper_version = $1, updated_at = $2 WHERE id = 0 AND helper_version IS DISTINCT FROM $1",
    )
    .bind(helper)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `ProductionSeeder::run`, without `RootUserSeeder`, which runs in the API crate.
pub async fn production_seed(db: &Db, encrypter: &Encrypter, keys_dir: &Path) -> DbResult<Seeded> {
    let now = Utc::now().naive_utc();
    seed_team(db, now).await?;
    seed_instance_settings(db, now).await?;
    seed_git_apps(db, now).await?;
    let user = seed_private_key(db, encrypter, keys_dir, now).await?;
    let server_created = seed_server(db, encrypter, user.as_deref().unwrap_or("root"), now).await?;
    seed_oauth(db, now).await?;
    seed_sentinel(db, encrypter, now).await?;
    update_helper_version(db, now).await?;
    Ok(Seeded { server_created })
}

/// The server work of `ProductionSeeder` and `app:init`, once the API answers: both reach back to
/// this server (Sentinel pushes), so they cannot run before it listens.
pub fn spawn_after_start(jobs: &Jobs, seeded: Seeded, start_server_scheduler: bool) {
    let jobs = jobs.clone();
    tokio::spawn(async move {
        let mut srv = match Srv::load(&jobs, 0).await {
            Ok(srv) => srv,
            Err(e) => {
                tracing::warn!(error = %e, "localhost server is missing");
                return;
            }
        };
        let start = if seeded.server_created {
            true
        } else {
            match proxy::check_proxy(&mut srv, false).await {
                Ok(should_start) => should_start,
                Err(e) => {
                    tracing::warn!(error = %e, "could not check the proxy of localhost");
                    false
                }
            }
        };
        if start && let Err(e) = proxy::start_proxy(&mut srv, true, false, false).await {
            tracing::warn!(error = %e, "could not start the proxy of localhost");
        }
        if let Err(e) = proxy::setup_dynamic_proxy_configuration(&srv).await {
            tracing::warn!(error = %e, "could not set up the dynamic proxy configuration");
        }
        if let Err(e) = crate::server_ops::sentinel::check_and_start_sentinel(&jobs, 0).await {
            tracing::warn!(error = %e, "could not start Sentinel on localhost");
        }
        // Its first tick runs at once and starts what is not running, so it waits for the two above.
        if start_server_scheduler {
            jobs.start_server_scheduler();
        }
    });
}
