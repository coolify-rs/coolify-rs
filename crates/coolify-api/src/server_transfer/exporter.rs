//! `ServerTransferExporter`: writes the bundle of a server. Encrypted columns are read through
//! their casts, so the bundle holds plaintext.

use std::collections::BTreeMap;

use coolify_compat::Encrypter;
use coolify_domain::new_public_id;
use serde_json::{Map, Value, json};

use super::importer::{APPLICATION_FILLABLE, APPLICATION_SETTING_FILLABLE, SERVICE_FILLABLE};
use super::{instance_url, now_iso, text, truthy};
use crate::state::AppState;

type R<T> = Result<T, String>;

const GITHUB: &str = "App\\Models\\GithubApp";
const GITLAB: &str = "App\\Models\\GitlabApp";
const GITEA: &str = "App\\Models\\GiteaApp";
/// `ManagedDnsRecord::OWNERSHIP_COMMENT_PREFIX`.
const OWNERSHIP_COMMENT_PREFIX: &str = "managed-by: coolify";

/// `SKIP_SERVER_SETTINGS`.
const SKIP_SERVER_SETTINGS: &[&str] = &[
    "id",
    "server_id",
    "created_at",
    "updated_at",
    "is_reachable",
    "is_usable",
    "force_disabled",
    "sentinel_token",
    "sentinel_custom_url",
    "is_sentinel_enabled",
    "is_sentinel_debug_enabled",
    "cloudflare_tunnel_integration_token_id",
    "patch_check_result",
    "patch_check_error",
    "patch_checked_at",
];

/// `SKIP_APPLICATION_ATTRS`.
const SKIP_APPLICATION: &[&str] = &[
    "id",
    "created_at",
    "updated_at",
    "deleted_at",
    "environment_id",
    "destination_id",
    "destination_type",
    "source_id",
    "source_type",
    "private_key_id",
];

/// The standalone database tables: table, type, and `$fillable` is taken from the row.
const DATABASES: &[(&str, &str)] = &[
    ("standalone_postgresqls", "StandalonePostgresql"),
    ("standalone_redis", "StandaloneRedis"),
    ("standalone_mongodbs", "StandaloneMongodb"),
    ("standalone_mysqls", "StandaloneMysql"),
    ("standalone_mariadbs", "StandaloneMariadb"),
    ("standalone_keydbs", "StandaloneKeydb"),
    ("standalone_dragonflies", "StandaloneDragonfly"),
    ("standalone_clickhouses", "StandaloneClickhouse"),
    ("standalone_sqlites", "StandaloneSqlite"),
];

/// Columns of the standalone databases that are not in their `$fillable`.
const DATABASE_NOT_FILLABLE: &[&str] = &[
    "id",
    "created_at",
    "updated_at",
    "deleted_at",
    "environment_id",
    "destination_id",
    "destination_type",
    "legacy_password_quoting",
    "config_hash",
];

const DEST: &str = "((x.destination_type = 'App\\Models\\StandaloneDocker' AND x.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
    OR (x.destination_type = 'App\\Models\\SwarmDocker' AND x.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))";

#[derive(Debug)]
pub struct Exporter<'a> {
    state: &'a AppState,
    enc: &'a Encrypter,
    team_id: i64,
    warnings: Vec<String>,
    left_out_links: i64,
    private_key_ids: Vec<i64>,
    github_ids: Vec<i64>,
    gitlab_ids: Vec<i64>,
    gitea_ids: Vec<i64>,
    s3_ids: Vec<i64>,
    token_ids: Vec<i64>,
    volume_backups: Vec<Value>,
    seen_backups: Vec<i64>,
}

fn db_error(e: sqlx::Error) -> String {
    tracing::warn!(error = %e, "server transfer export query failed");
    "A database error occurred.".to_owned()
}

fn push_unique(list: &mut Vec<i64>, id: i64) {
    if !list.contains(&id) {
        list.push(id);
    }
}

fn pick(row: &Value, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .map(|f| ((*f).to_owned(), row.get(*f).cloned().unwrap_or(Value::Null)))
        .collect()
}

fn id_of(row: &Value) -> i64 {
    row.get("id").map_or(0, super::as_int)
}

/// `Y-m-d\TH:i:s.u\Z` of a `datetime` cast in `jsonSerialize`.
fn cast_datetime(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => {
            let base = s.split('.').next().unwrap_or(s).trim_end_matches('Z');
            json!(format!("{base}.000000Z"))
        }
        None => Value::Null,
    }
}

/// A timestamp column without a cast: `Y-m-d H:i:s`.
fn raw_timestamp(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => json!(s.split('.').next().unwrap_or(s).replace('T', " ")),
        None => Value::Null,
    }
}

impl<'a> Exporter<'a> {
    pub fn new(state: &'a AppState, enc: &'a Encrypter, team_id: i64) -> Self {
        Self {
            state,
            enc,
            team_id,
            warnings: Vec::new(),
            left_out_links: 0,
            private_key_ids: Vec::new(),
            github_ids: Vec::new(),
            gitlab_ids: Vec::new(),
            gitea_ids: Vec::new(),
            s3_ids: Vec::new(),
            token_ids: Vec::new(),
            volume_backups: Vec::new(),
            seen_backups: Vec::new(),
        }
    }

    async fn rows(&self, sql: &str, binds: &[Bind<'_>]) -> R<Vec<Value>> {
        let wrapped = format!("SELECT row_to_json(t)::text FROM ({sql}) t");
        let mut q = sqlx::query_scalar::<_, String>(&wrapped);
        for b in binds {
            q = match b {
                Bind::I(i) => q.bind(*i),
                Bind::S(s) => q.bind(*s),
            };
        }
        let raw = q.fetch_all(self.state.db.pool()).await.map_err(db_error)?;
        raw.iter()
            .map(|r| serde_json::from_str(r).map_err(|e| e.to_string()))
            .collect()
    }

    async fn one(&self, sql: &str, binds: &[Bind<'_>]) -> R<Option<Value>> {
        Ok(self.rows(sql, binds).await?.into_iter().next())
    }

    /// `encrypted` cast.
    fn dec(&self, v: Option<&Value>) -> Value {
        match v.and_then(Value::as_str) {
            Some(s) => self.enc.decrypt_string(s).map_or(Value::Null, |p| json!(p)),
            None => Value::Null,
        }
    }

    fn dec_fields(&self, row: &Value, out: &mut Map<String, Value>, fields: &[&str]) {
        for f in fields {
            out.insert((*f).to_owned(), self.dec(row.get(*f)));
        }
    }

    /// `findTeamRecord`.
    async fn team_record(&self, table: &str, id: i64, label: &str) -> R<Value> {
        self.one(
            &format!("SELECT * FROM {table} WHERE team_id = $1 AND id = $2"),
            &[Bind::I(self.team_id), Bind::I(id)],
        )
        .await?
        .ok_or_else(|| {
            format!("A {label} used on this server does not belong to the server team. Select a {label} of this team first.")
        })
    }

    /// `findGitSource`: `(row, is_system_wide)`.
    async fn git_source(&self, table: &str, id: i64) -> R<Option<Value>> {
        let row = self
            .one(
                &format!("SELECT * FROM {table} WHERE id = $1"),
                &[Bind::I(id)],
            )
            .await?;
        if let Some(r) = &row
            && !truthy(r.get("is_system_wide"))
            && r.get("team_id").map_or(-1, super::as_int) != self.team_id
        {
            return Err("A git source used on this server does not belong to the server team. Select a git source of this team first.".into());
        }
        Ok(row)
    }

    async fn collect_git_source(&mut self, table: &str, id: i64) -> R<()> {
        if let Some(row) = self.git_source(table, id).await?
            && !truthy(row.get("is_system_wide"))
        {
            let list = match table {
                "github_apps" => &mut self.github_ids,
                "gitlab_apps" => &mut self.gitlab_ids,
                _ => &mut self.gitea_ids,
            };
            push_unique(list, id_of(&row));
            if let Some(key) = row.get("private_key_id").and_then(Value::as_i64) {
                self.team_record("private_keys", key, "private key").await?;
                push_unique(&mut self.private_key_ids, key);
            }
        }
        Ok(())
    }

    async fn export_private_key(&self, id: i64) -> R<Value> {
        let k = self
            .one("SELECT * FROM private_keys WHERE id = $1", &[Bind::I(id)])
            .await?
            .ok_or("Server has no private key to export.")?;
        let name = text(&k["name"]);
        let uuid = text(&k["uuid"]);
        let material = k["private_key"]
            .as_str()
            .and_then(|s| self.enc.decrypt_string(s).ok())
            .ok_or_else(|| {
                format!(
                    "Cannot decrypt private key \"{name}\" (uuid={uuid}). This instance APP_KEY does not match the key that encrypted the data (common after regenerating .dev-instances/*{}). Restore the original APP_KEY or re-save the private key material. Original error: The payload is invalid.",
                    concat!(".", "env")
                )
            })?;
        Ok(json!({
            "uuid": uuid,
            "name": name,
            "description": k["description"],
            "private_key": material,
            "is_git_related": truthy(k.get("is_git_related")),
            "fingerprint": k["fingerprint"],
        }))
    }

    /// `exportEnvVars`. The value is `trim(decrypt($value))`; the key order is the relation's.
    async fn env_vars(&self, morph: &str, id: i64, preview: Option<bool>) -> R<Value> {
        let filter = match preview {
            Some(true) => " AND is_preview = true",
            Some(false) => " AND is_preview = false",
            None => "",
        };
        let rows = self
            .rows(
                &format!("SELECT * FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2{filter} ORDER BY lower(key), id"),
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        let mut out = Vec::new();
        for v in rows {
            let value = v["value"]
                .as_str()
                .filter(|s| !s.is_empty())
                .and_then(|s| self.enc.decrypt_maybe_serialized(s).ok())
                .map_or(Value::Null, |p| json!(p.trim()));
            out.push(json!({
                "uuid": v["uuid"],
                "key": v["key"],
                "value": value,
                "is_literal": truthy(v.get("is_literal")),
                "is_multiline": truthy(v.get("is_multiline")),
                "is_preview": truthy(v.get("is_preview")),
                "is_runtime": truthy(v.get("is_runtime")),
                "is_buildtime": truthy(v.get("is_buildtime")),
                "is_shown_once": truthy(v.get("is_shown_once")),
                "is_shared": truthy(v.get("is_shared")),
                "is_required": truthy(v.get("is_required")),
                "uses_legacy_escaping": truthy(v.get("uses_legacy_escaping")),
                "preserve_compose_null": truthy(v.get("preserve_compose_null")),
                "comment": v["comment"],
                "order": v["order"],
                "version": v["version"],
            }));
        }
        Ok(json!(out))
    }

    /// `exportSharedEnvVars`.
    async fn shared_vars(&self, column: &str, kind: &str, id: i64) -> R<Vec<Value>> {
        let rows = self
            .rows(
                &format!(
                    "SELECT * FROM shared_environment_variables WHERE type = $1 AND {column} = $2"
                ),
                &[Bind::S(kind), Bind::I(id)],
            )
            .await?;
        Ok(rows.iter().filter_map(|v| self.shared_var(v)).collect())
    }

    fn shared_var(&self, v: &Value) -> Option<Value> {
        if matches!(
            v["key"].as_str(),
            Some("COOLIFY_SERVER_UUID" | "COOLIFY_SERVER_NAME")
        ) {
            return None;
        }
        Some(json!({
            "key": v["key"],
            "value": self.dec(v.get("value")),
            "is_multiline": truthy(v.get("is_multiline")),
            "is_literal": truthy(v.get("is_literal")),
            "is_shown_once": truthy(v.get("is_shown_once")),
            "comment": v["comment"],
        }))
    }

    /// Volumes and file storages of a resource, with their backups (`exportVolumeBackups`).
    async fn storages(&mut self, morph: &str, id: i64) -> R<(Value, Value)> {
        let volumes = self
            .rows(
                "SELECT v.*, s.uuid AS sqlite_uuid FROM local_persistent_volumes v LEFT JOIN standalone_sqlites s ON s.id = v.standalone_sqlite_id WHERE v.resource_type = $1 AND v.resource_id = $2",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        let mut persistent = Vec::new();
        for v in &volumes {
            persistent.push(json!({
                "uuid": v["uuid"],
                "name": v["name"],
                "mount_path": v["mount_path"],
                "host_path": v["host_path"],
                "is_preview_suffix_enabled": truthy(v.get("is_preview_suffix_enabled")),
                "ignores_compose_driver_options": truthy(v.get("ignores_compose_driver_options")),
                "standalone_sqlite_uuid": v["sqlite_uuid"],
            }));
            self.storage_backups(
                "App\\Models\\LocalPersistentVolume",
                "persistent_volume",
                id_of(v),
                &v["uuid"],
            )
            .await?;
        }
        let files = self
            .rows(
                "SELECT * FROM local_file_volumes WHERE resource_type = $1 AND resource_id = $2",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        let mut file_out = Vec::new();
        for f in &files {
            file_out.push(json!({
                "uuid": f["uuid"],
                "fs_path": f["fs_path"],
                "mount_path": f["mount_path"],
                "content": self.dec(f.get("content")),
                "is_directory": truthy(f.get("is_directory")),
                "is_host_file": truthy(f.get("is_host_file")),
                "chown": f["chown"],
                "chmod": f["chmod"],
                "is_based_on_git": truthy(f.get("is_based_on_git")),
                "is_preview_suffix_enabled": truthy(f.get("is_preview_suffix_enabled")),
            }));
            self.storage_backups(
                "App\\Models\\LocalFileVolume",
                "file_volume",
                id_of(f),
                &f["uuid"],
            )
            .await?;
        }
        Ok((json!(persistent), json!(file_out)))
    }

    async fn s3_uuids(
        &mut self,
        table: &str,
        column: &str,
        id: i64,
        primary: Option<i64>,
    ) -> R<(Value, Vec<String>)> {
        let joined = self
            .rows(
                &format!("SELECT s.id, s.uuid FROM {table} j JOIN s3_storages s ON s.id = j.s3_storage_id WHERE j.{column} = $1"),
                &[Bind::I(id)],
            )
            .await?;
        let mut uuids = Vec::new();
        let mut primary_uuid = Value::Null;
        if let Some(p) = primary {
            push_unique(&mut self.s3_ids, p);
            if let Some(r) = self
                .one("SELECT uuid FROM s3_storages WHERE id = $1", &[Bind::I(p)])
                .await?
            {
                primary_uuid = r["uuid"].clone();
            }
        }
        for r in &joined {
            push_unique(&mut self.s3_ids, id_of(r));
            uuids.push(text(&r["uuid"]));
        }
        Ok((primary_uuid, uuids))
    }

    async fn storage_backups(&mut self, morph: &str, kind: &str, id: i64, uuid: &Value) -> R<()> {
        let backups = self
            .rows(
                "SELECT * FROM scheduled_volume_backups WHERE backupable_type = $1 AND backupable_id = $2",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        for b in backups {
            let backup_id = id_of(&b);
            if self.seen_backups.contains(&backup_id) {
                continue;
            }
            self.seen_backups.push(backup_id);
            let primary = b["s3_storage_id"].as_i64();
            let (s3_uuid, uuids) = self
                .s3_uuids(
                    "scheduled_volume_backup_s3_storage",
                    "scheduled_volume_backup_id",
                    backup_id,
                    primary,
                )
                .await?;
            self.volume_backups.push(json!({
                "uuid": b["uuid"],
                "backupable_kind": kind,
                "backupable_uuid": uuid,
                "frequency": b["frequency"],
                "enabled": truthy(b.get("enabled")),
                "save_s3": truthy(b.get("save_s3")),
                "disable_local_backup": truthy(b.get("disable_local_backup")),
                "stop_during_backup": truthy(b.get("stop_during_backup")),
                "retention_amount_locally": b["retention_amount_locally"],
                "retention_days_locally": b["retention_days_locally"],
                "retention_max_storage_locally": b["retention_max_storage_locally"],
                "retention_amount_s3": b["retention_amount_s3"],
                "retention_days_s3": b["retention_days_s3"],
                "retention_max_storage_s3": b["retention_max_storage_s3"],
                "timeout": b["timeout"],
                "missing_backup_notification_days": b["missing_backup_notification_days"],
                "s3_storage_uuid": s3_uuid,
                "s3_storage_uuids": uuids,
                "had_s3_storage": primary.is_some(),
            }));
        }
        Ok(())
    }

    /// `exportScheduledBackups`.
    async fn scheduled_backups(&mut self, morph: &str, id: i64) -> R<Value> {
        let rows = self
            .rows(
                "SELECT * FROM scheduled_database_backups WHERE database_type = $1 AND database_id = $2",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        let mut out = Vec::new();
        for b in rows {
            let primary = b["s3_storage_id"].as_i64();
            let (s3_uuid, uuids) = self
                .s3_uuids(
                    "scheduled_database_backup_s3_storage",
                    "scheduled_database_backup_id",
                    id_of(&b),
                    primary,
                )
                .await?;
            let mut item = pick(
                &b,
                &[
                    "uuid",
                    "description",
                    "frequency",
                    "databases_to_backup",
                    "database_backup_retention_amount_locally",
                    "database_backup_retention_days_locally",
                    "database_backup_retention_max_storage_locally",
                    "database_backup_retention_amount_s3",
                    "database_backup_retention_days_s3",
                    "database_backup_retention_max_storage_s3",
                    "timeout",
                    "missing_backup_notification_days",
                ],
            );
            for f in ["enabled", "save_s3", "dump_all", "disable_local_backup"] {
                item.insert(f.to_owned(), json!(truthy(b.get(f))));
            }
            item.insert("s3_storage_uuid".into(), s3_uuid);
            item.insert("s3_storage_uuids".into(), json!(uuids));
            item.insert("had_s3_storage".into(), json!(primary.is_some()));
            out.push(Value::Object(item));
        }
        Ok(json!(out))
    }

    async fn scheduled_tasks(&self, column: &str, id: i64) -> R<Value> {
        let rows = self
            .rows(
                &format!("SELECT * FROM scheduled_tasks WHERE {column} = $1"),
                &[Bind::I(id)],
            )
            .await?;
        Ok(json!(rows.iter().map(|t| json!({
            "uuid": t["uuid"], "name": t["name"], "command": t["command"], "frequency": t["frequency"],
            "container": t["container"], "timeout": t["timeout"], "enabled": t.get("enabled").is_none_or(|v| crate::serialize::php_truthy(v)),
        })).collect::<Vec<_>>()))
    }

    async fn tags(&self, morph: &str, id: i64) -> R<Value> {
        let rows = self
            .rows(
                "SELECT t.uuid, t.name FROM tags t JOIN taggables x ON x.tag_id = t.id WHERE x.taggable_type = $1 AND x.taggable_id = $2",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        Ok(json!(rows))
    }

    /// `exportSecretManagerLink`; the token goes in the bundle.
    async fn secret_link(&mut self, morph: &str, id: i64) -> R<Value> {
        let link = self
            .one(
                "SELECT l.uuid, l.settings, l.integration_token_id, t.uuid AS token_uuid FROM secret_manager_links l JOIN integration_tokens t ON t.id = l.integration_token_id WHERE l.resourceable_type = $1 AND l.resourceable_id = $2 LIMIT 1",
                &[Bind::S(morph), Bind::I(id)],
            )
            .await?;
        Ok(match link {
            None => Value::Null,
            Some(l) => {
                if let Some(t) = l["integration_token_id"].as_i64() {
                    push_unique(&mut self.token_ids, t);
                }
                json!({"uuid": l["uuid"], "integration_token_uuid": l["token_uuid"], "settings": l["settings"]})
            }
        })
    }

    /// Reads the fillable columns of an application through their accessors.
    fn application_attrs(&self, row: &Value) -> Map<String, Value> {
        let mut attrs = Map::new();
        let mut fields: Vec<&str> = APPLICATION_FILLABLE
            .iter()
            .copied()
            .filter(|f| !SKIP_APPLICATION.contains(f) && row.get(*f).is_some())
            .collect();
        for f in ["is_maintenance_enabled", "maintenance_page"] {
            if row.get(f).is_some() {
                fields.push(f);
            }
        }
        for f in fields {
            let v = &row[f];
            let value = match f {
                "http_basic_auth_password"
                | "manual_webhook_secret_github"
                | "manual_webhook_secret_gitlab"
                | "manual_webhook_secret_bitbucket"
                | "manual_webhook_secret_gitea" => self.dec(Some(v)),
                "custom_nginx_configuration" => v
                    .as_str()
                    .map(|s| {
                        use base64::Engine;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(s)
                            .unwrap_or_default();
                        json!(String::from_utf8_lossy(&bytes))
                    })
                    .unwrap_or(Value::Null),
                "last_restart_at" => cast_datetime(v),
                "last_online_at" => raw_timestamp(v),
                _ => v.clone(),
            };
            attrs.insert(f.to_owned(), value);
        }
        attrs
    }

    async fn export_application(
        &mut self,
        app: &Value,
        dest: &BTreeMap<String, String>,
    ) -> R<Value> {
        let id = id_of(app);
        let morph = "App\\Models\\Application";
        let settings_row = self
            .one(
                "SELECT * FROM application_settings WHERE application_id = $1",
                &[Bind::I(id)],
            )
            .await?;
        let settings = settings_row.map_or(json!({}), |s| {
            Value::Object(pick(&s, APPLICATION_SETTING_FILLABLE))
        });
        let source = self.application_source(app).await?;
        let private_key_uuid = match app["private_key_id"].as_i64() {
            Some(k) => self
                .one("SELECT uuid FROM private_keys WHERE id = $1", &[Bind::I(k)])
                .await?
                .map_or(Value::Null, |r| r["uuid"].clone()),
            None => Value::Null,
        };
        let (persistent, files) = self.storages(morph, id).await?;
        let previews = self.previews(id).await?;
        Ok(json!({
            "type": "application",
            "uuid": app["uuid"],
            "destination_uuid": dest.get(&format!("{}:{}", text(&app["destination_type"]), text(&app["destination_id"]))),
            "attributes": self.application_attrs(app),
            "settings": settings,
            "environment_variables": self.env_vars(morph, id, Some(false)).await?,
            "environment_variables_preview": self.env_vars(morph, id, Some(true)).await?,
            "persistent_storages": persistent,
            "file_storages": files,
            "scheduled_tasks": self.scheduled_tasks("application_id", id).await?,
            "tags": self.tags(morph, id).await?,
            "previews": previews,
            "secret_manager_link": self.secret_link(morph, id).await?,
            "source": source,
            "private_key_uuid": private_key_uuid,
            "had_git_source": !super::blank(app.get("source_id")),
            "had_private_key": !super::blank(app.get("private_key_id")),
        }))
    }

    async fn application_source(&self, app: &Value) -> R<Value> {
        let (kind, table) = match app["source_type"].as_str() {
            Some(GITHUB) => ("github_app", "github_apps"),
            Some(GITLAB) => ("gitlab_app", "gitlab_apps"),
            Some(GITEA) => ("gitea_app", "gitea_apps"),
            _ => return Ok(Value::Null),
        };
        let Some(id) = app["source_id"].as_i64() else {
            return Ok(Value::Null);
        };
        let row = self
            .one(
                &format!("SELECT uuid, is_system_wide FROM {table} WHERE id = $1"),
                &[Bind::I(id)],
            )
            .await?;
        Ok(row.map_or(Value::Null, |r| {
            json!({"type": kind, "uuid": r["uuid"], "system_wide": truthy(r.get("is_system_wide"))})
        }))
    }

    async fn previews(&mut self, application_id: i64) -> R<Value> {
        let rows = self
            .rows(
                "SELECT * FROM application_previews WHERE application_id = $1 AND deleted_at IS NULL",
                &[Bind::I(application_id)],
            )
            .await?;
        let mut out = Vec::new();
        for p in rows {
            let (persistent, _files) = self
                .storages("App\\Models\\ApplicationPreview", id_of(&p))
                .await?;
            out.push(json!({
                "uuid": p["uuid"],
                "pull_request_id": p["pull_request_id"],
                "pull_request_html_url": p["pull_request_html_url"],
                "pull_request_issue_comment_id": p["pull_request_issue_comment_id"],
                "fqdn": p["fqdn"],
                "status": p["status"],
                "git_type": p["git_type"],
                "docker_compose_domains": p["docker_compose_domains"],
                "docker_registry_image_tag": p["docker_registry_image_tag"],
                "domain_port_overrides": p["domain_port_overrides"],
                "max_restart_count": p["max_restart_count"],
                "persistent_storages": persistent,
            }));
        }
        Ok(json!(out))
    }

    async fn export_database(
        &mut self,
        db: &Value,
        type_name: &str,
        dest: &BTreeMap<String, String>,
    ) -> R<Value> {
        let id = id_of(db);
        let morph = format!("App\\Models\\{type_name}");
        let mut attrs = Map::new();
        if let Value::Object(o) = db {
            for (k, v) in o {
                if DATABASE_NOT_FILLABLE.contains(&k.as_str()) {
                    continue;
                }
                let value = if k.contains("password") && type_name != "StandaloneRedis" {
                    self.dec(Some(v))
                } else if k == "last_restart_at" {
                    cast_datetime(v)
                } else if matches!(k.as_str(), "started_at" | "last_online_at") {
                    raw_timestamp(v)
                } else {
                    v.clone()
                };
                attrs.insert(k.clone(), value);
            }
        }
        let (persistent, files) = self.storages(&morph, id).await?;
        Ok(json!({
            "type": type_name,
            "model": morph,
            "uuid": db["uuid"],
            "destination_uuid": dest.get(&format!("{}:{}", text(&db["destination_type"]), text(&db["destination_id"]))),
            "attributes": attrs,
            "environment_variables": self.env_vars(&morph, id, None).await?,
            "persistent_storages": persistent,
            "file_storages": files,
            "tags": self.tags(&morph, id).await?,
            "scheduled_backups": self.scheduled_backups(&morph, id).await?,
            "secret_manager_link": self.secret_link(&morph, id).await?,
        }))
    }

    async fn export_service(
        &mut self,
        service: &Value,
        dest: &BTreeMap<String, String>,
    ) -> R<Value> {
        let id = id_of(service);
        let morph = "App\\Models\\Service";
        let mut attrs = Map::new();
        for f in SERVICE_FILLABLE {
            if !matches!(
                *f,
                "environment_id" | "destination_id" | "destination_type" | "server_id"
            ) && service.get(*f).is_some()
            {
                attrs.insert((*f).to_owned(), service[*f].clone());
            }
        }
        for f in [
            "is_maintenance_enabled",
            "maintenance_page",
            "include_coolify_default_variables",
        ] {
            if service.get(f).is_some() {
                attrs.insert(f.to_owned(), service[f].clone());
            }
        }
        let mut applications = Vec::new();
        for sa in self
            .rows(
                "SELECT * FROM service_applications WHERE service_id = $1",
                &[Bind::I(id)],
            )
            .await?
        {
            applications.push(self.service_application(&sa).await?);
        }
        let mut databases = Vec::new();
        for sd in self
            .rows(
                "SELECT * FROM service_databases WHERE service_id = $1",
                &[Bind::I(id)],
            )
            .await?
        {
            databases.push(self.service_database(&sd).await?);
        }
        Ok(json!({
            "type": "service",
            "uuid": service["uuid"],
            "destination_uuid": dest.get(&format!("{}:{}", text(&service["destination_type"]), text(&service["destination_id"]))),
            "attributes": attrs,
            "environment_variables": self.env_vars(morph, id, None).await?,
            "scheduled_tasks": self.scheduled_tasks("service_id", id).await?,
            "tags": self.tags(morph, id).await?,
            "secret_manager_link": self.secret_link(morph, id).await?,
            "applications": applications,
            "databases": databases,
        }))
    }

    async fn service_application(&mut self, a: &Value) -> R<Value> {
        let morph = "App\\Models\\ServiceApplication";
        let id = id_of(a);
        let mut attrs = pick(
            a,
            &[
                "name",
                "human_name",
                "description",
                "fqdn",
                "noindex_domains",
                "redirect",
                "domain_dns_statuses",
                "domain_port_overrides",
                "ports",
                "exposes",
                "status",
                "exclude_from_status",
                "required_fqdn",
                "image",
                "is_log_drain_enabled",
                "is_include_timestamps",
                "is_gzip_enabled",
                "is_stripprefix_enabled",
                "is_force_https_enabled",
                "is_permanent_redirect_enabled",
                "traefik_certificate_resolver",
                "is_migrated",
                "restart_count",
                "max_restart_count",
                "restart_limit_reached",
                "last_restart_type",
            ],
        );
        attrs.insert("last_online_at".into(), raw_timestamp(&a["last_online_at"]));
        attrs.insert(
            "last_restart_at".into(),
            cast_datetime(&a["last_restart_at"]),
        );
        let (persistent, files) = self.storages(morph, id).await?;
        Ok(json!({
            "uuid": a["uuid"], "attributes": attrs, "name": a["name"], "fqdn": a["fqdn"], "image": a["image"],
            "ports": a["ports"], "exposes": a["exposes"],
            "exclude_from_status": truthy(a.get("exclude_from_status")),
            "required_fqdn": truthy(a.get("required_fqdn")),
            "human_name": a["human_name"], "description": a["description"],
            "is_log_drain_enabled": truthy(a.get("is_log_drain_enabled")),
            "is_include_timestamps": truthy(a.get("is_include_timestamps")),
            "is_gzip_enabled": a.get("is_gzip_enabled").is_none_or(|v| v.is_null() || crate::serialize::php_truthy(v)),
            "is_stripprefix_enabled": a.get("is_stripprefix_enabled").is_none_or(|v| v.is_null() || crate::serialize::php_truthy(v)),
            "traefik_certificate_resolver": a["traefik_certificate_resolver"],
            "environment_variables": self.env_vars(morph, id, None).await?,
            "persistent_storages": persistent,
            "file_storages": files,
        }))
    }

    async fn service_database(&mut self, d: &Value) -> R<Value> {
        let morph = "App\\Models\\ServiceDatabase";
        let id = id_of(d);
        let mut attrs = pick(
            d,
            &[
                "name",
                "human_name",
                "description",
                "ports",
                "exposes",
                "status",
                "exclude_from_status",
                "image",
                "public_port",
                "is_public",
                "is_log_drain_enabled",
                "is_include_timestamps",
                "is_gzip_enabled",
                "is_stripprefix_enabled",
                "is_migrated",
                "custom_type",
                "public_port_timeout",
            ],
        );
        attrs.insert("last_online_at".into(), raw_timestamp(&d["last_online_at"]));
        let (persistent, files) = self.storages(morph, id).await?;
        Ok(json!({
            "uuid": d["uuid"], "attributes": attrs, "name": d["name"], "human_name": d["human_name"],
            "description": d["description"], "image": d["image"], "ports": d["ports"], "exposes": d["exposes"],
            "exclude_from_status": truthy(d.get("exclude_from_status")),
            "is_log_drain_enabled": truthy(d.get("is_log_drain_enabled")),
            "is_include_timestamps": truthy(d.get("is_include_timestamps")),
            "is_gzip_enabled": d.get("is_gzip_enabled").is_none_or(|v| v.is_null() || crate::serialize::php_truthy(v)),
            "is_stripprefix_enabled": d.get("is_stripprefix_enabled").is_none_or(|v| v.is_null() || crate::serialize::php_truthy(v)),
            "public_port": d["public_port"], "public_port_timeout": d["public_port_timeout"],
            "is_public": truthy(d.get("is_public")), "custom_type": d["custom_type"],
            "persistent_storages": persistent,
            "file_storages": files,
            "scheduled_backups": self.scheduled_backups(morph, id).await?,
        }))
    }

    /// `exportServer`.
    async fn export_server(&self, server: &Value, settings: &Value) -> R<Value> {
        let mut out = Map::new();
        if let Value::Object(o) = settings {
            for (k, v) in o {
                if SKIP_SERVER_SETTINGS.contains(&k.as_str()) {
                    continue;
                }
                let value = match k.as_str() {
                    "logdrain_custom_config" | "geoip_maxmind_license_key" => self.dec(Some(v)),
                    "docker_version_checked_at" | "compose_version_checked_at" => cast_datetime(v),
                    "docker_cleanup_frequency" => {
                        json!(coolify_compat::cron::translate_cron_expression(&text(v)))
                    }
                    _ => v.clone(),
                };
                out.insert(k.clone(), value);
            }
        }
        let token_uuid = match settings["cloudflare_tunnel_integration_token_id"].as_i64() {
            Some(t) => self
                .one(
                    "SELECT uuid FROM integration_tokens WHERE id = $1",
                    &[Bind::I(t)],
                )
                .await?
                .map_or(Value::Null, |r| r["uuid"].clone()),
            None => Value::Null,
        };
        out.insert(
            "cloudflare_tunnel_integration_token_uuid".into(),
            token_uuid,
        );
        let cloud = match server["cloud_provider_token_id"].as_i64() {
            Some(t) => self
                .one(
                    "SELECT uuid FROM cloud_provider_tokens WHERE id = $1",
                    &[Bind::I(t)],
                )
                .await?
                .map_or(Value::Null, |r| r["uuid"].clone()),
            None => Value::Null,
        };
        let mut role = settings["server_role"]
            .as_str()
            .unwrap_or("both")
            .to_owned();
        if !matches!(role.as_str(), "both" | "build" | "deployment") {
            role = if truthy(settings.get("is_build_server")) {
                "build".into()
            } else {
                "both".into()
            };
        }
        Ok(json!({
            "uuid": server["uuid"],
            "name": server["name"],
            "description": server["description"],
            "ip": text(&server["ip"]),
            "port": if truthy(server.get("port")) { server["port"].clone() } else { json!(22) },
            "user": text(&server["user"]),
            "proxy": if server["proxy"].is_object() { server["proxy"].clone() } else { json!([]) },
            "server_role": role,
            "is_build_server": role == "build",
            "cloud_provider_token_uuid": cloud,
            "hetzner_server_id": server["hetzner_server_id"],
            "vultr_instance_id": server["vultr_instance_id"],
            "digitalocean_droplet_id": server["digitalocean_droplet_id"],
            "hostinger_virtual_machine_id": server["hostinger_virtual_machine_id"],
            "proxy_error_page": server["proxy_error_page"],
            "swarm_cluster": server["swarm_cluster"],
            "settings": out,
        }))
    }

    /// `ServerTransferExporter::export`.
    pub async fn export(mut self, server_id: i64) -> R<Value> {
        if server_id == 0 {
            return Err(
                "The Coolify host (localhost) cannot be transferred between instances.".into(),
            );
        }
        let server = self
            .one("SELECT * FROM servers WHERE id = $1", &[Bind::I(server_id)])
            .await?
            .ok_or("Server not found.")?;
        let settings = self
            .one(
                "SELECT * FROM server_settings WHERE server_id = $1",
                &[Bind::I(server_id)],
            )
            .await?
            .unwrap_or(Value::Null);
        let key_id = server["private_key_id"]
            .as_i64()
            .ok_or("Server has no private key to export.")?;
        let key = self
            .one(
                "SELECT team_id FROM private_keys WHERE id = $1",
                &[Bind::I(key_id)],
            )
            .await?
            .ok_or("Server has no private key to export.")?;
        if key.get("team_id").map_or(-1, super::as_int) != self.team_id {
            return Err("The server private key does not belong to the server team. Select a private key of this team first.".into());
        }
        push_unique(&mut self.private_key_ids, key_id);

        let applications = self
            .rows(
                &format!("SELECT x.* FROM applications x WHERE {DEST} AND x.deleted_at IS NULL"),
                &[Bind::I(server_id)],
            )
            .await?;
        let mut databases: Vec<(Value, &str)> = Vec::new();
        for (table, type_name) in DATABASES {
            for row in self
                .rows(
                    &format!("SELECT x.* FROM {table} x WHERE {DEST} AND x.deleted_at IS NULL"),
                    &[Bind::I(server_id)],
                )
                .await?
            {
                databases.push((row, type_name));
            }
        }
        let services = self
            .rows(
                "SELECT * FROM services WHERE server_id = $1 AND deleted_at IS NULL",
                &[Bind::I(server_id)],
            )
            .await?;

        // `assertNoAdditionalDestinations`.
        let additional = self
            .one(
                &format!("SELECT 1 FROM additional_destinations d WHERE d.server_id = $1 OR d.application_id IN (SELECT x.id FROM applications x WHERE {DEST}) LIMIT 1"),
                &[Bind::I(server_id)],
            )
            .await?;
        if additional.is_some() {
            return Err("This server cannot be transferred because one or more applications use additional destinations (multi-server / multi-destination deploy). Remove all additional destinations from those applications, then retry the export.".into());
        }

        // `collectProjectIds`.
        let mut env_ids: Vec<i64> = applications
            .iter()
            .chain(services.iter())
            .filter_map(|r| r["environment_id"].as_i64())
            .collect();
        env_ids.extend(
            databases
                .iter()
                .filter_map(|(r, _)| r["environment_id"].as_i64()),
        );
        env_ids.sort_unstable();
        env_ids.dedup();
        let mut project_ids = Vec::new();
        for env in &env_ids {
            if let Some(r) = self
                .one(
                    "SELECT project_id FROM environments WHERE id = $1",
                    &[Bind::I(*env)],
                )
                .await?
                && let Some(p) = r["project_id"].as_i64()
            {
                push_unique(&mut project_ids, p);
            }
        }
        let mut projects = self
            .rows(
                "SELECT * FROM projects WHERE team_id = $1 ORDER BY name",
                &[Bind::I(self.team_id)],
            )
            .await?;
        projects.retain(|p| project_ids.contains(&id_of(p)));
        if projects.len() != project_ids.len() {
            return Err("Some resources on this server belong to a project of another team. Move them to a project of this team first.".into());
        }

        // `destinationUuidMap` and `exportDestinations`.
        let mut dest = BTreeMap::new();
        let mut destinations = Vec::new();
        for (table, morph, kind) in [
            (
                "standalone_dockers",
                "App\\Models\\StandaloneDocker",
                "standalone",
            ),
            ("swarm_dockers", "App\\Models\\SwarmDocker", "swarm"),
        ] {
            for d in self
                .rows(
                    &format!("SELECT * FROM {table} WHERE server_id = $1"),
                    &[Bind::I(server_id)],
                )
                .await?
            {
                dest.insert(format!("{morph}:{}", id_of(&d)), text(&d["uuid"]));
                destinations.push(json!({"uuid": d["uuid"], "name": d["name"], "network": d["network"], "type": kind}));
            }
        }

        // `collectDependencies`.
        let runner = self
            .one(
                "SELECT c.*, g.uuid AS app_uuid FROM github_runner_configs c LEFT JOIN github_apps g ON g.id = c.github_app_id WHERE c.server_id = $1 LIMIT 1",
                &[Bind::I(server_id)],
            )
            .await?;
        if let Some(r) = &runner
            && let Some(a) = r["github_app_id"].as_i64()
        {
            self.collect_git_source("github_apps", a).await?;
        }
        for app in &applications {
            if let Some(k) = app["private_key_id"].as_i64() {
                self.team_record("private_keys", k, "private key").await?;
                push_unique(&mut self.private_key_ids, k);
            }
            if let Some(source) = app["source_id"].as_i64() {
                let table = match app["source_type"].as_str() {
                    Some(GITHUB) => "github_apps",
                    Some(GITLAB) => "gitlab_apps",
                    Some(GITEA) => "gitea_apps",
                    _ => continue,
                };
                self.collect_git_source(table, source).await?;
            }
        }
        let cloud = server["cloud_provider_token_id"].as_i64();
        let cloud_tokens = match cloud {
            Some(t) => {
                let r = self
                    .team_record("cloud_provider_tokens", t, "cloud provider token")
                    .await?;
                vec![
                    json!({"uuid": r["uuid"], "provider": r["provider"], "token": self.dec(r.get("token")), "name": r["name"], "description": r["description"]}),
                ]
            }
            None => Vec::new(),
        };

        if let Some(t) = settings["cloudflare_tunnel_integration_token_id"].as_i64() {
            push_unique(&mut self.token_ids, t);
        }

        let dns = self
            .managed_dns(server_id, &server, &applications, &services)
            .await?;
        let server_shared = self.shared_vars("server_id", "server", server_id).await?;

        // `exportProject`.
        let mut project_payloads = Vec::new();
        for project in &projects {
            let project_id = id_of(project);
            let mut environments = Vec::new();
            for env in self
                .rows(
                    "SELECT * FROM environments WHERE project_id = $1 ORDER BY name",
                    &[Bind::I(project_id)],
                )
                .await?
            {
                let env_id = id_of(&env);
                if !env_ids.contains(&env_id) {
                    continue;
                }
                let in_env = |r: &Value| r["environment_id"].as_i64() == Some(env_id);
                let mut apps = Vec::new();
                let mut uuids: Vec<String> = Vec::new();
                let mut ends = BTreeMap::new();
                for a in applications.iter().filter(|a| in_env(a)) {
                    apps.push(self.export_application(a, &dest).await?);
                    uuids.push(text(&a["uuid"]));
                    ends.insert(
                        format!("App\\Models\\Application:{}", id_of(a)),
                        json!({"kind": "application", "uuid": a["uuid"]}),
                    );
                }
                let mut dbs = Vec::new();
                for (d, t) in databases.iter().filter(|(d, _)| in_env(d)) {
                    dbs.push(self.export_database(d, t, &dest).await?);
                    uuids.push(text(&d["uuid"]));
                    ends.insert(
                        format!("App\\Models\\{t}:{}", id_of(d)),
                        json!({"kind": "database", "uuid": d["uuid"]}),
                    );
                }
                let mut svcs = Vec::new();
                for s in services.iter().filter(|s| in_env(s)) {
                    svcs.push(self.export_service(s, &dest).await?);
                    uuids.push(text(&s["uuid"]));
                    ends.insert(
                        format!("App\\Models\\Service:{}", id_of(s)),
                        json!({"kind": "service", "uuid": s["uuid"]}),
                    );
                }
                let mut positions = Map::new();
                if let Value::Object(p) = &env["canvas_positions"] {
                    for (k, v) in p {
                        if uuids.contains(k) {
                            positions.insert(k.clone(), v.clone());
                        }
                    }
                }
                let mut links = Vec::new();
                for link in self
                    .rows(
                        "SELECT * FROM resource_links WHERE environment_id = $1 ORDER BY id",
                        &[Bind::I(env_id)],
                    )
                    .await?
                {
                    let source = ends.get(&format!(
                        "{}:{}",
                        text(&link["source_type"]),
                        text(&link["source_id"])
                    ));
                    let target = ends.get(&format!(
                        "{}:{}",
                        text(&link["target_type"]),
                        text(&link["target_id"])
                    ));
                    match (source, target) {
                        (Some(s), Some(t)) => links.push(json!({"uuid": link["uuid"], "label": link["label"], "source": s, "target": t})),
                        _ => self.left_out_links += 1,
                    }
                }
                environments.push(json!({
                    "uuid": env["uuid"],
                    "name": env["name"],
                    "description": env["description"],
                    "canvas_positions": positions,
                    "shared_environment_variables": self.shared_vars("environment_id", "environment", env_id).await?,
                    "applications": apps,
                    "databases": dbs,
                    "services": svcs,
                    "resource_links": links,
                }));
            }
            let mut icon = Value::Null;
            if !super::blank(project.get("icon_path")) {
                self.warnings.push(format!(
                    "The icon of project {} cannot be read, so it is not transferred. Upload it again on the target instance.",
                    text(&project["name"])
                ));
                icon = Value::Null;
            }
            project_payloads.push(json!({
                "uuid": project["uuid"],
                "name": project["name"],
                "description": project["description"],
                "icon": icon,
                "shared_environment_variables": self.shared_vars("project_id", "project", project_id).await?,
                "environments": environments,
            }));
        }

        // `buildWarnings`.
        let mut warnings = Vec::new();
        for (morph, table, name) in [
            (GITHUB, "github_apps", "GitHub"),
            (GITLAB, "gitlab_apps", "GitLab"),
            (GITEA, "gitea_apps", "Gitea"),
        ] {
            let mut uses = false;
            for a in applications
                .iter()
                .filter(|a| a["source_type"].as_str() == Some(morph))
            {
                if let Some(id) = a["source_id"].as_i64()
                    && let Some(r) = self
                        .one(
                            &format!("SELECT is_system_wide FROM {table} WHERE id = $1"),
                            &[Bind::I(id)],
                        )
                        .await?
                    && truthy(r.get("is_system_wide"))
                {
                    uses = true;
                }
            }
            if uses {
                warnings.push(format!("Some applications use a system-wide {name} App, which is not exported. On import, Coolify will re-link to a matching system-wide {name} App on the target instance (by UUID) if one exists."));
            }
        }
        if !self.s3_ids.is_empty() {
            warnings.push("S3 storage credentials were exported. Confirm endpoint access from the target instance and that bucket policies still apply.".to_owned());
        }
        warnings.append(&mut self.warnings);
        if self.left_out_links > 0 {
            warnings.push(format!(
                "{} {} between resources on the environment canvas {} not transferred, because one of the linked resources stays on this instance.",
                self.left_out_links,
                if self.left_out_links == 1 { "link" } else { "links" },
                if self.left_out_links == 1 { "is" } else { "are" },
            ));
        }

        // S3 storages must be in the team.
        let mut s3_out = Vec::new();
        for id in self.s3_ids.clone() {
            let Some(s) = self
                .one(
                    "SELECT * FROM s3_storages WHERE team_id = $1 AND id = $2",
                    &[Bind::I(self.team_id), Bind::I(id)],
                )
                .await?
            else {
                return Err("An S3 storage used by a backup on this server does not belong to the server team. Select an S3 storage of this team first.".into());
            };
            s3_out.push(json!({
                "uuid": s["uuid"], "name": s["name"], "description": s["description"], "region": s["region"],
                "key": self.dec(s.get("key")), "secret": self.dec(s.get("secret")), "bucket": s["bucket"],
                "endpoint": s["endpoint"], "is_usable": truthy(s.get("is_usable")),
            }));
        }

        let mut private_keys = Vec::new();
        for id in self.private_key_ids.clone() {
            private_keys.push(self.export_private_key(id).await?);
        }
        let (github, gitlab, gitea, checklist_names) = self.export_git_apps().await?;
        let mut tokens = Vec::new();
        for id in self.token_ids.clone() {
            let t = self
                .team_record("integration_tokens", id, "integration token")
                .await?;
            tokens.push(json!({"uuid": t["uuid"], "provider": t["provider"], "name": t["name"], "token": self.dec(t.get("token")), "capabilities": t["capabilities"], "metadata": t["metadata"]}));
        }

        let team_shared = self
            .referenced_team_vars(&server_shared, &project_payloads)
            .await?;
        let certificates = self.ssl_certificates(server_id).await?;
        let checklist = self
            .checklist(&applications, &checklist_names, runner.is_some())
            .await?;
        let runner_payload = runner.as_ref().map(|c| json!({
            "uuid": c["uuid"], "github_app_uuid": c["app_uuid"], "labels": c["labels"],
            "is_enabled": truthy(c.get("is_enabled")), "is_dedicated": truthy(c.get("is_dedicated")),
            "allow_pull_requests": truthy(c.get("allow_pull_requests")), "max_runners": c["max_runners"],
            "docker_mode": c["docker_mode"], "runner_image": c["runner_image"], "cpu_limit": c["cpu_limit"],
            "memory_limit": c["memory_limit"], "capacity_wait_timeout": c["capacity_wait_timeout"],
            "idle_timeout": c["idle_timeout"], "job_timeout": c["job_timeout"],
        }));

        let source_url = instance_url(&self.state.db).await;
        Ok(json!({
            "schema_version": super::bundle::SCHEMA_VERSION,
            "coolify_version": self.state.version,
            "exported_at": now_iso(),
            "export_id": new_public_id(),
            "source_instance": {"url": source_url, "name": "coolify-rs"},
            "warnings": warnings,
            "post_transfer_checklist": checklist,
            "private_key": self.export_private_key(key_id).await?,
            "private_keys": private_keys,
            "github_apps": github,
            "gitlab_apps": gitlab,
            "gitea_apps": gitea,
            "s3_storages": s3_out,
            "cloud_provider_tokens": cloud_tokens,
            "integration_tokens": tokens,
            "dns_zones": dns.0,
            "managed_dns_records": dns.1,
            "ssl_certificates": certificates,
            "volume_backups": self.volume_backups,
            "server": self.export_server(&server, &settings).await?,
            "destinations": destinations,
            "github_runner_config": runner_payload,
            "shared_environment_variables": {"server": server_shared, "team": team_shared},
            "projects": project_payloads,
        }))
    }

    async fn export_git_apps(&self) -> R<(Vec<Value>, Vec<Value>, Vec<Value>, GitNames)> {
        let key_uuid = |row: Value| async move {
            match row["private_key_id"].as_i64() {
                Some(k) => self
                    .one("SELECT uuid FROM private_keys WHERE id = $1", &[Bind::I(k)])
                    .await
                    .ok()
                    .flatten()
                    .map_or(Value::Null, |r| r["uuid"].clone()),
                None => Value::Null,
            }
        };
        let mut names = GitNames::default();
        let mut github = Vec::new();
        for id in &self.github_ids {
            let a = self
                .one("SELECT * FROM github_apps WHERE id = $1", &[Bind::I(*id)])
                .await?
                .ok_or("missing")?;
            names.github.push(text(&a["name"]));
            let mut o = pick(
                &a,
                &[
                    "uuid",
                    "name",
                    "organization",
                    "api_url",
                    "html_url",
                    "custom_user",
                    "custom_port",
                    "app_id",
                    "installation_id",
                    "client_id",
                ],
            );
            self.dec_fields(&a, &mut o, &["client_secret", "webhook_secret"]);
            o.insert("is_system_wide".into(), json!(false));
            o.insert("is_public".into(), json!(truthy(a.get("is_public"))));
            o.extend(pick(
                &a,
                &[
                    "contents",
                    "metadata",
                    "pull_requests",
                    "administration",
                    "organization_self_hosted_runners",
                    "actions",
                    "webhook_events",
                    "runner_group_id",
                ],
            ));
            o.insert("private_key_uuid".into(), key_uuid(a.clone()).await);
            o.insert(
                "webhook_url_hint".into(),
                json!("/webhooks/source/github/events"),
            );
            github.push(Value::Object(o));
        }
        let mut gitlab = Vec::new();
        for id in &self.gitlab_ids {
            let a = self
                .one("SELECT * FROM gitlab_apps WHERE id = $1", &[Bind::I(*id)])
                .await?
                .ok_or("missing")?;
            names.gitlab.push(text(&a["name"]));
            let mut o = pick(
                &a,
                &[
                    "uuid",
                    "name",
                    "organization",
                    "api_url",
                    "html_url",
                    "custom_port",
                    "custom_user",
                    "app_id",
                    "oauth_id",
                    "client_id",
                    "expires_at",
                    "redirect_uri",
                    "group_name",
                    "public_key",
                    "webhook_token",
                    "deploy_key_id",
                ],
            );
            self.dec_fields(
                &a,
                &mut o,
                &[
                    "app_secret",
                    "client_secret",
                    "access_token",
                    "refresh_token",
                ],
            );
            o.insert("is_system_wide".into(), json!(false));
            o.insert("is_public".into(), json!(truthy(a.get("is_public"))));
            o.insert("private_key_uuid".into(), key_uuid(a.clone()).await);
            o.insert(
                "webhook_url_hint".into(),
                json!("/webhooks/source/gitlab/events"),
            );
            gitlab.push(Value::Object(o));
        }
        let mut gitea = Vec::new();
        for id in &self.gitea_ids {
            let a = self
                .one("SELECT * FROM gitea_apps WHERE id = $1", &[Bind::I(*id)])
                .await?
                .ok_or("missing")?;
            names.gitea.push(text(&a["name"]));
            let mut o = pick(
                &a,
                &[
                    "uuid",
                    "name",
                    "organization",
                    "api_url",
                    "html_url",
                    "custom_port",
                    "custom_user",
                    "client_id",
                    "expires_at",
                    "redirect_uri",
                ],
            );
            self.dec_fields(
                &a,
                &mut o,
                &[
                    "client_secret",
                    "access_token",
                    "refresh_token",
                    "webhook_secret",
                ],
            );
            o.insert("is_system_wide".into(), json!(false));
            o.insert("is_public".into(), json!(truthy(a.get("is_public"))));
            o.insert("private_key_uuid".into(), key_uuid(a.clone()).await);
            o.insert(
                "webhook_url_hint".into(),
                json!("/webhooks/source/gitea/events"),
            );
            gitea.push(Value::Object(o));
        }
        Ok((github, gitlab, gitea, names))
    }

    /// `exportReferencedTeamSharedEnvVars`.
    async fn referenced_team_vars(&self, server: &[Value], projects: &[Value]) -> R<Vec<Value>> {
        static TEAM_REF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"\{\{\s*team\.(.+?)\s*\}\}").expect("regex")
        });
        let mut keys: Vec<String> = Vec::new();
        let mut scan = |v: &Value| {
            fn walk(v: &Value, keys: &mut Vec<String>) {
                match v {
                    Value::Object(o) => {
                        if let Some(Value::String(s)) = o.get("value")
                            && o.contains_key("key")
                        {
                            for c in TEAM_REF.captures_iter(s) {
                                keys.push(c[1].to_owned());
                            }
                        }
                        o.values().for_each(|x| walk(x, keys));
                    }
                    Value::Array(a) => a.iter().for_each(|x| walk(x, keys)),
                    _ => {}
                }
            }
            walk(v, &mut keys);
        };
        server.iter().for_each(&mut scan);
        projects.iter().for_each(&mut scan);
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .rows(
                "SELECT * FROM shared_environment_variables WHERE type = 'team' AND team_id = $1",
                &[Bind::I(self.team_id)],
            )
            .await?;
        Ok(rows
            .iter()
            .filter(|r| keys.contains(&text(&r["key"])))
            .filter_map(|r| self.shared_var(r))
            .collect())
    }

    /// `exportSslCertificates`.
    async fn ssl_certificates(&self, server_id: i64) -> R<Vec<Value>> {
        let mut out = Vec::new();
        for c in self
            .rows(
                "SELECT * FROM ssl_certificates WHERE server_id = $1",
                &[Bind::I(server_id)],
            )
            .await?
        {
            let (mut kind, mut uuid) = (Value::Null, Value::Null);
            if let (Some(t), Some(id)) = (c["resource_type"].as_str(), c["resource_id"].as_i64()) {
                let (table, name) = match t {
                    "App\\Models\\Application" => (Some("applications"), "application".to_owned()),
                    "App\\Models\\Service" => (Some("services"), "service".to_owned()),
                    other => {
                        let base = other.rsplit('\\').next().unwrap_or(other).to_owned();
                        (
                            DATABASES.iter().find(|(_, n)| *n == base).map(|(t, _)| *t),
                            base,
                        )
                    }
                };
                if let Some(table) = table
                    && let Some(r) = self
                        .one(
                            &format!("SELECT uuid FROM {table} WHERE id = $1"),
                            &[Bind::I(id)],
                        )
                        .await?
                {
                    uuid = r["uuid"].clone();
                    kind = json!(name);
                }
            }
            let valid = c["valid_until"].as_str().map_or(Value::Null, |s| {
                json!(format!("{}+00:00", s.split('.').next().unwrap_or(s)))
            });
            out.push(json!({
                "ssl_certificate": self.dec(c.get("ssl_certificate")),
                "ssl_private_key": self.dec(c.get("ssl_private_key")),
                "configuration_dir": c["configuration_dir"],
                "mount_path": c["mount_path"],
                "common_name": c["common_name"],
                "subject_alternative_names": c["subject_alternative_names"],
                "valid_until": valid,
                "is_ca_certificate": truthy(c.get("is_ca_certificate")),
                "resource_kind": kind,
                "resource_uuid": uuid,
            }));
        }
        Ok(out)
    }

    /// `postTransferChecklist`.
    async fn checklist(
        &self,
        applications: &[Value],
        names: &GitNames,
        has_runner: bool,
    ) -> R<Vec<String>> {
        let mut steps = Vec::new();
        let mut manual = Vec::new();
        for a in applications {
            if a["build_pack"].as_str() == Some("dockerimage")
                || super::blank(a.get("git_repository"))
            {
                continue;
            }
            let public = match (a["source_type"].as_str(), a["source_id"].as_i64()) {
                (Some(GITHUB), Some(id)) => {
                    self.one(
                        "SELECT is_public FROM github_apps WHERE id = $1",
                        &[Bind::I(id)],
                    )
                    .await?
                }
                (Some(GITLAB), Some(id)) => {
                    self.one(
                        "SELECT is_public FROM gitlab_apps WHERE id = $1",
                        &[Bind::I(id)],
                    )
                    .await?
                }
                (Some(GITEA), Some(id)) => {
                    self.one(
                        "SELECT is_public FROM gitea_apps WHERE id = $1",
                        &[Bind::I(id)],
                    )
                    .await?
                }
                _ => None,
            };
            if public.is_none_or(|p| truthy(p.get("is_public"))) {
                manual.push(text(&a["name"]));
            }
        }
        if !manual.is_empty() {
            steps.push(format!(
                "If a repository of these applications has a manual webhook to Coolify, change its URL to the target: {}. Use {{target_url}}/webhooks/source/github/events/manual, {{target_url}}/webhooks/source/gitlab/events/manual, {{target_url}}/webhooks/source/bitbucket/events/manual, or {{target_url}}/webhooks/source/gitea/events/manual.",
                manual.join(", ")
            ));
        }
        for n in &names.github {
            steps.push(format!("On GitHub, open the settings of GitHub App {n}. Set the Webhook URL to {{target_url}}/webhooks/source/github/events, the Setup URL to {{target_url}}/webhooks/source/github/install, and the Callback URL to {{target_url}}/login/github/app."));
        }
        for n in &names.gitlab {
            steps.push(format!("In GitLab, open Settings > Webhooks of every project that uses GitLab App {n}, and set the webhook URL to {{target_url}}/webhooks/source/gitlab/events."));
        }
        for n in &names.gitea {
            steps.push(format!("In Gitea, open the OAuth2 application of Gitea App {n} and set the redirect URI to {{target_url}}/webhooks/source/gitea/redirect. Then set the webhook URL of every repository that uses it to {{target_url}}/webhooks/source/gitea/events."));
        }
        if has_runner {
            steps.push("After the GitHub App sends webhooks to {target_url}, open this server there, check the runner requirements, and enable GitHub runners. Enabling them stops runners that another Coolify instance started on this server.".to_owned());
        }
        steps.push("This instance keeps a copy of the SSH key of the server. On {target_url}, add a new SSH key to the server and select it there, then remove the old public key from ~/.ssh/authorized_keys on the server.".to_owned());
        Ok(steps)
    }
}

impl Exporter<'_> {
    /// `collectManagedDnsRecords`, `exportDnsZone` and `exportManagedDnsRecord`. A record that a
    /// resource staying on this instance also uses is not exported.
    async fn managed_dns(
        &mut self,
        server_id: i64,
        server: &Value,
        applications: &[Value],
        services: &[Value],
    ) -> R<(Vec<Value>, Vec<Value>)> {
        let mut keys: BTreeMap<String, Option<Value>> = BTreeMap::new();
        keys.insert(
            format!("App\\Models\\Server:{server_id}"),
            Some(json!({"kind": "server", "uuid": server["uuid"]})),
        );
        for a in applications {
            keys.insert(
                format!("App\\Models\\Application:{}", id_of(a)),
                Some(json!({"kind": "application", "uuid": a["uuid"]})),
            );
            for p in self.rows("SELECT id, uuid, deleted_at FROM application_previews WHERE application_id = $1", &[Bind::I(id_of(a))]).await? {
                let live = p["deleted_at"].is_null().then(|| json!({"kind": "preview", "uuid": p["uuid"]}));
                keys.insert(format!("App\\Models\\ApplicationPreview:{}", id_of(&p)), live);
            }
        }
        for s in services {
            for sa in self
                .rows(
                    "SELECT id, uuid FROM service_applications WHERE service_id = $1",
                    &[Bind::I(id_of(s))],
                )
                .await?
            {
                keys.insert(
                    format!("App\\Models\\ServiceApplication:{}", id_of(&sa)),
                    Some(json!({"kind": "service_application", "uuid": sa["uuid"]})),
                );
            }
        }
        let records = self
            .rows(
                "SELECT r.*, t.uuid AS token_uuid, t.team_id AS token_team, z.uuid AS zone_uuid, z.provider_zone_id, z.name AS zone_name, z.account_id, z.account_name, z.integration_token_id AS zone_token, zt.uuid AS zone_token_uuid, zt.team_id AS zone_team \
                 FROM managed_dns_records r LEFT JOIN integration_tokens t ON t.id = r.integration_token_id LEFT JOIN dns_provider_zones z ON z.id = r.dns_provider_zone_id LEFT JOIN integration_tokens zt ON zt.id = z.integration_token_id \
                 WHERE EXISTS (SELECT 1 FROM managed_dns_record_references x WHERE x.managed_dns_record_id = r.id) ORDER BY r.name",
                &[],
            )
            .await?;
        let instance_id: String = std::env::var("APP_ID")
            .ok()
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| "default".to_owned())
            .chars()
            .take(40)
            .collect();
        let (mut zones, mut out, mut zone_ids) = (Vec::new(), Vec::new(), Vec::new());
        for r in records {
            let refs = self.rows("SELECT resource_type, resource_id FROM managed_dns_record_references WHERE managed_dns_record_id = $1", &[Bind::I(id_of(&r))]).await?;
            let ref_keys: Vec<String> = refs
                .iter()
                .map(|x| format!("{}:{}", text(&x["resource_type"]), text(&x["resource_id"])))
                .collect();
            if !ref_keys.iter().any(|k| keys.contains_key(k)) {
                continue;
            }
            let name = text(&r["name"]);
            let team = |k: &str| r.get(k).map_or(-1, super::as_int);
            if team("team_id") != self.team_id
                || team("token_team") != self.team_id
                || team("zone_team") != self.team_id
            {
                self.warnings.push(format!("DNS record {name} is not transferred, because its DNS provider token belongs to another team. The target instance does not manage it."));
                continue;
            }
            if ref_keys.iter().any(|k| !keys.contains_key(k)) {
                self.warnings.push(format!("DNS record {name} stays managed by this instance, because a resource that is not transferred also uses it. The target instance does not update or delete it."));
                continue;
            }
            let references: Vec<Value> = ref_keys
                .iter()
                .filter_map(|k| keys.get(k).cloned().flatten())
                .collect();
            if references.is_empty() {
                continue;
            }
            for t in [r["integration_token_id"].as_i64(), r["zone_token"].as_i64()]
                .into_iter()
                .flatten()
            {
                push_unique(&mut self.token_ids, t);
            }
            let zone_id = r["dns_provider_zone_id"].as_i64().unwrap_or(0);
            if !zone_ids.contains(&zone_id) {
                zone_ids.push(zone_id);
                zones.push(json!({
                    "uuid": r["zone_uuid"], "integration_token_uuid": r["zone_token_uuid"], "provider_zone_id": r["provider_zone_id"],
                    "name": r["zone_name"], "account_id": r["account_id"], "account_name": r["account_name"],
                }));
            }
            out.push(json!({
                "uuid": r["uuid"], "integration_token_uuid": r["token_uuid"], "dns_provider_zone_uuid": r["zone_uuid"],
                "provider_record_id": r["provider_record_id"], "type": r["type"], "name": name, "content": r["content"],
                "owned": truthy(r.get("owned")),
                "ownership_comment": format!("{OWNERSHIP_COMMENT_PREFIX} {instance_id}/{}", text(&r["uuid"])),
                "references": references,
            }));
        }
        Ok((zones, out))
    }
}

#[derive(Default)]
struct GitNames {
    github: Vec<String>,
    gitlab: Vec<String>,
    gitea: Vec<String>,
}

enum Bind<'b> {
    I(i64),
    S(&'b str),
}
