//! `VolumeBackupJob`, `DeleteScheduledVolumeBackup` and `VolumeBackupRecoveryJob`. The job's own
//! recovery runs once; `retry_volume_recovery` is `retryRecovery`. Starting recovery and missing
//! backup checks from the scheduler is not ported.

use chrono::{Duration, Utc};
use coolify_db::database_parts as parts;
use coolify_db::records::Record;
use coolify_db::servers;
use coolify_db::volume_backups::{self as vb, BackupTarget, ExecutionField as F, FILE_MORPH};
use coolify_remote::{RunOptions, Target, shell::quote};
use regex::Regex;
use serde_json::Value;

use super::backup::compressor_command;
use super::common::{helper_image, now, slug};
use super::s3::{S3Target, minio_resolve_options};
use crate::Jobs;
use crate::notifications::{Notification, VolumeBackup, notify};
use crate::server::ServerInfo;

const OP: &str = "volume.backup";

/// `VolumeBackupJob::archiveScript`.
pub fn archive_script(percentage: i64) -> String {
    format!(
        "compressor=$({}); {{ tar -I \"$compressor\" -cf - -C /volume . || {{ status=$?; [ $status -eq 1 ] || exit $status; }}; }}",
        compressor_command(percentage)
    )
}

/// `VolumeBackupRecoveryJob::stateFile`.
pub fn state_file(execution_uuid: &str) -> String {
    format!("/tmp/coolify-volume-backup-{execution_uuid}.stopped")
}

/// `archiveWithStoppedContainers`.
pub fn archive_with_stopped_containers(
    archive: &str,
    containers: &[String],
    state_file: &str,
) -> String {
    let list = containers.join(" ");
    let script = format!(
        "set -eu\nstate_file={}\n: > \"$state_file\"\ncleanup() {{ status=$?; trap - EXIT HUP INT TERM; while IFS= read -r container; do docker start \"$container\" >/dev/null || status=1; done < \"$state_file\"; [ $status -eq 0 ] && rm -f \"$state_file\"; exit $status; }}\ntrap cleanup EXIT\ntrap 'exit 129' HUP\ntrap 'exit 130' INT\ntrap 'exit 143' TERM\nfor container in {list}; do echo \"$container\" >> \"$state_file\"; docker stop \"$container\" >/dev/null; done\n{archive}",
        quote(state_file)
    );
    format!("sh -c {}", quote(&script))
}

/// `containersUsingVolume`.
fn containers_command(source: &str) -> String {
    format!(
        "containers=$(docker ps -q) || exit 1; for container in $containers; do mounts=$(docker inspect --format '{{{{if not .State.Paused}}}}{{{{range .Mounts}}}}{{{{println .Source}}}}{{{{println .Name}}}}{{{{end}}}}{{{{end}}}}' \"$container\" 2>/dev/null) || {{ remaining=$(docker ps -aq --filter \"id=$container\") || exit 1; [ -z \"$remaining\" ] && continue; docker inspect \"$container\" >/dev/null; exit 1; }}; if printf '%s\\n' \"$mounts\" | grep -Fqx -- {}; then echo \"$container\"; fi; done",
        quote(source)
    )
}

fn str_of<'a>(r: &'a Record, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn number(r: &Record, k: &str) -> f64 {
    match r.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `$backup->sourcePath()`.
async fn source_path(jobs: &Jobs, target: &BackupTarget) -> Result<String, String> {
    if target.morph != FILE_MORPH {
        let host = str_of(&target.row, "host_path");
        return Ok(if host.is_empty() {
            str_of(&target.row, "name").to_owned()
        } else {
            host.to_owned()
        });
    }
    if target.row.get("is_directory").and_then(Value::as_bool) != Some(true) {
        return Err("The backup target is not a directory or persistent volume.".to_owned());
    }
    let path = str_of(&target.row, "fs_path");
    if let Some(rest) = path.strip_prefix('.') {
        if str_of(&target.row, "resource_type") == r"App\Models\Application"
            && let Some(id) = target.row.get("resource_id").and_then(Value::as_i64)
            && let Ok(Some(uuid)) = vb::application_uuid(&jobs.0.db, id).await
        {
            return Ok(format!("/data/coolify/applications/{uuid}{rest}"));
        }
        return Err("The directory backup workdir is unavailable.".to_owned());
    }
    Ok(path.to_owned())
}

fn outside_retention(rows: &[Record], amount: i64, days: i64, max_gb: f64) -> Vec<Record> {
    if amount == 0 && days == 0 && max_gb == 0.0 {
        return vec![];
    }
    let mut ids: Vec<i64> = Vec::new();
    let id_of = |r: &Record| r.get("id").and_then(Value::as_i64).unwrap_or_default();
    if amount > 0 {
        ids.extend(rows.iter().skip(amount as usize).map(id_of));
    }
    if days > 0 {
        let oldest = Utc::now().naive_utc() - Duration::days(days);
        for r in rows {
            let created = str_of(r, "created_at");
            let parsed = chrono::NaiveDateTime::parse_from_str(
                &created[..created.len().min(19)],
                "%Y-%m-%dT%H:%M:%S",
            );
            if parsed.is_ok_and(|c| c < oldest) {
                ids.push(id_of(r));
            }
        }
    }
    if max_gb > 0.0 {
        let max = max_gb * 1024f64.powi(3);
        let mut total = 0f64;
        for (i, r) in rows.iter().enumerate() {
            total += number(r, "size");
            if i > 0 && total > max {
                ids.extend(rows[i..].iter().map(id_of));
                break;
            }
        }
    }
    rows.iter()
        .filter(|r| ids.contains(&id_of(r)))
        .cloned()
        .collect()
}

struct Run {
    execution: i64,
    target: Target,
    server: ServerInfo,
    timeout: std::time::Duration,
    /// The key and secret of every destination (`redact`).
    secrets: Vec<String>,
}

impl Run {
    fn redact(&self, text: &str) -> String {
        let locked: Vec<&str> = self.secrets.iter().map(String::as_str).collect();
        coolify_compat::redact::redact_text(text, &[], &locked)
    }
}

impl Jobs {
    /// `VolumeBackupJob::dispatch`.
    pub fn spawn_volume_backup(&self, backup_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = jobs.run_volume_backup(backup_id).await {
                tracing::warn!(backup_id, error = %e, "storage backup failed");
            }
        });
    }

    /// `VolumeBackupJob::handle`.
    pub async fn run_volume_backup(&self, backup_id: i64) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let Some(backup) = vb::schedule_by_id(db, backup_id).await.map_err(text)? else {
            return Ok(());
        };
        // `withoutOverlapping(...)->dontRelease()`.
        if vb::has_running_execution(db, backup_id)
            .await
            .map_err(text)?
        {
            return Ok(());
        }
        let missing = "The storage backup resource, team, or server no longer exists.";
        let target = vb::target_of(db, &backup)
            .await
            .map_err(text)?
            .ok_or(missing)?;
        let team_id = backup
            .get("team_id")
            .and_then(Value::as_i64)
            .ok_or(missing)?;
        let team = vb::team_name(db, team_id)
            .await
            .map_err(text)?
            .ok_or(missing)?;
        let server_id = vb::resource_server(
            db,
            str_of(&target.row, "resource_type"),
            target
                .row
                .get("resource_id")
                .and_then(Value::as_i64)
                .unwrap_or(-1),
        )
        .await
        .map_err(text)?
        .ok_or(missing)?;
        let server_row = servers::find_by_id(db, server_id)
            .await
            .map_err(text)?
            .ok_or(missing)?;
        let server = ServerInfo::from_row(&server_row);

        let save_s3 = backup.get("save_s3").and_then(Value::as_bool) == Some(true);
        let destinations = if save_s3 {
            vb::selected_storages(db, &backup).await.map_err(text)?
        } else {
            vec![]
        };
        let (execution, execution_uuid) =
            vb::create_execution(db, backup_id, destinations.first().map(parts::id_of), now())
                .await
                .map_err(text)?;
        for s3 in &destinations {
            vb::create_replica(db, execution, parts::id_of(s3), now())
                .await
                .map_err(text)?;
        }

        let directory = format!(
            "/data/coolify/backups/volumes/{}-{team_id}/{}",
            slug(&team),
            str_of(&target.row, "uuid")
        );
        let is_file = target.morph == FILE_MORPH;
        let (kind, name) = if is_file {
            ("directory", str_of(&target.row, "fs_path"))
        } else {
            ("volume", str_of(&target.row, "name"))
        };
        let location = format!(
            "{directory}/{kind}-{}-{}.tar.gz",
            slug(name),
            Utc::now().timestamp()
        );
        vb::update_execution(db, execution, &[F::Filename(Some(location.clone()))], now())
            .await
            .map_err(text)?;
        let disable_local =
            backup.get("disable_local_backup").and_then(Value::as_bool) == Some(true);
        let stream = disable_local && destinations.len() == 1;

        let t = match server.target(self).await {
            Ok(t) => t,
            Err(e) => {
                self.fail_volume_execution(execution, &e, &location, stream, None)
                    .await;
                return Err(e);
            }
        };
        let enc = self.0.encrypter.as_ref();
        let mut secrets = Vec::new();
        for s3 in &destinations {
            if let Some(enc) = enc
                && let Ok(target) = S3Target::from_row(enc, s3)
            {
                secrets.push(target.key.clone());
                secrets.push(target.secret.clone());
            }
        }
        let run = Run {
            execution,
            target: t,
            server,
            timeout: std::time::Duration::from_secs(
                backup
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .unwrap_or(36000),
            ),
            secrets,
        };
        let result = self
            .volume_backup_steps(
                &backup,
                &target,
                &destinations,
                &run,
                &execution_uuid,
                &location,
                &directory,
                stream,
            )
            .await;
        let outcome = match result {
            Ok(warning) => Ok(warning),
            Err(e) => {
                let message = if stream {
                    format!(
                        "S3-only streaming backup failed: {e}. The S3 destination may not support streaming uploads. Enable local backups to use the local archive upload path."
                    )
                } else {
                    e
                };
                let safe = run.redact(&message);
                self.fail_volume_execution(execution, &safe, &location, stream, Some(&run))
                    .await;
                Err(safe)
            }
        };
        let _ = vb::update_execution(db, execution, &[F::Finished], now()).await;
        // `$team->notify(new BackupSuccess(...))` and `failed()`.
        let details = self.volume_backup_details(&backup, &target).await;
        let (notification, result) = match outcome {
            Ok(warning) => (
                Notification::VolumeBackupSuccess {
                    backup: details,
                    warning,
                },
                Ok(()),
            ),
            Err(output) => (
                Notification::VolumeBackupFailed {
                    backup: details,
                    output: output.clone(),
                },
                Err(output),
            ),
        };
        notify(db, self.0.encrypter.as_ref(), team_id, notification).await;
        result
    }

    /// The fields the `VolumeBackup` notifications read from the schedule.
    async fn volume_backup_details(&self, backup: &Record, target: &BackupTarget) -> VolumeBackup {
        let db = &self.0.db;
        let (kind, name) = if target.morph == FILE_MORPH {
            ("Directory", str_of(&target.row, "fs_path"))
        } else {
            ("Volume", str_of(&target.row, "name"))
        };
        let (resource_name, url) = vb::resource_link(
            db,
            str_of(&target.row, "resource_type"),
            target
                .row
                .get("resource_id")
                .and_then(Value::as_i64)
                .unwrap_or(-1),
        )
        .await
        .unwrap_or_default();
        let base = crate::notifications::Ctx::load(db).await.base_url;
        VolumeBackup {
            resource_name,
            target: format!("{kind} {name}"),
            frequency: str_of(backup, "frequency").to_owned(),
            url: url.map(|path| format!("{}{path}", base.trim_end_matches('/'))),
        }
    }

    /// `VolumeBackups::retryRecovery`, then `VolumeBackupRecoveryJob::recoverWithBounds`: one attempt
    /// whose outcome is recorded. An unreachable server waits; any other failure needs attention
    /// and notifies the team.
    pub async fn retry_volume_recovery(&self, execution_id: i64) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let (execution, backup) = vb::execution_by_id(db, execution_id)
            .await
            .map_err(text)?
            .ok_or("Backup execution not found.")?;
        let flag = |k: &str| execution.get(k).and_then(Value::as_bool) == Some(true);
        let (stop_pending, cleanup_pending) =
            (flag("stop_recovery_pending"), flag("s3_cleanup_pending"));
        if !stop_pending && !cleanup_pending {
            return Ok(());
        }
        let target = vb::target_of(db, &backup).await.map_err(text)?;
        let mut failure: Option<&'static str> = None;
        if stop_pending {
            let server = match &target {
                Some(t) => vb::resource_server(
                    db,
                    str_of(&t.row, "resource_type"),
                    t.row
                        .get("resource_id")
                        .and_then(Value::as_i64)
                        .unwrap_or(-1),
                )
                .await
                .map_err(text)?,
                None => None,
            };
            let server = match server {
                Some(id) => servers::find_by_id(db, id)
                    .await
                    .map_err(text)?
                    .map(|r| ServerInfo::from_row(&r)),
                None => None,
            };
            match server {
                Some(server) if server.is_functional() => match server.target(self).await {
                    Ok(t) => {
                        let run = Run {
                            execution: execution_id,
                            target: t,
                            server,
                            timeout: std::time::Duration::from_secs(300),
                            secrets: Vec::new(),
                        };
                        if self
                            .recover_volume_containers(&run, str_of(&execution, "uuid"))
                            .await
                            .is_err()
                        {
                            failure = Some("remote_command");
                        }
                    }
                    Err(_) => failure = Some("server_unreachable"),
                },
                _ => failure = Some("server_unreachable"),
            }
        }
        if cleanup_pending
            && let Err(e) = self
                .cleanup_volume_upload(execution_id, str_of(&execution, "filename"))
                .await
            && failure.is_none()
        {
            failure = Some(if e.contains("NoSuchBucket") {
                "s3_bucket"
            } else if [
                "InvalidAccessKeyId",
                "SignatureDoesNotMatch",
                "AccessDenied",
            ]
            .iter()
            .any(|c| e.contains(c))
            {
                "s3_auth"
            } else {
                "unknown"
            });
        }
        let Some(category) = failure else {
            return vb::set_recovery_state(db, execution_id, None, now())
                .await
                .map_err(text);
        };
        let needs_attention = category != "server_unreachable";
        // `$execution` was read after `retryRecovery` cleared the flag, so a failure enters it again.
        vb::set_recovery_state(
            db,
            execution_id,
            Some((Some(category), needs_attention)),
            now(),
        )
        .await
        .map_err(text)?;
        if needs_attention && let Some(team_id) = backup.get("team_id").and_then(Value::as_i64) {
            let details = match &target {
                Some(t) => Some(self.volume_backup_details(&backup, t).await),
                None => None,
            };
            notify(
                db,
                self.0.encrypter.as_ref(),
                team_id,
                Notification::VolumeBackupRecoveryFailed {
                    backup: details,
                    execution_uuid: str_of(&execution, "uuid").to_owned(),
                    recovery_error: Some(category.to_owned()),
                    containers_stopped: stop_pending,
                },
            )
            .await;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn volume_backup_steps(
        &self,
        backup: &Record,
        target: &BackupTarget,
        destinations: &[Record],
        run: &Run,
        execution_uuid: &str,
        location: &str,
        directory: &str,
        stream: bool,
    ) -> Result<Option<String>, String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let execution = run.execution;
        let source = source_path(self, target).await?;
        let container = format!("volume-backup-{execution_uuid}");
        let image = helper_image(self);
        let percentage = run
            .server
            .settings
            .get("backup_compression_cpu_percentage")
            .and_then(Value::as_i64)
            .unwrap_or(25);
        let verify = if target.morph != FILE_MORPH && str_of(&target.row, "host_path").is_empty() {
            format!("docker volume inspect {} >/dev/null", quote(&source))
        } else {
            format!("test -d {}", quote(&source))
        };
        let script = archive_script(percentage);
        let mut archive = if stream {
            vb::update_execution(db, execution, &[F::LocalDeleted(true)], now())
                .await
                .map_err(text)?;
            let command = self
                .stream_to_s3_command(
                    &destinations[0],
                    &script,
                    location,
                    &source,
                    &container,
                    &image,
                )
                .await?;
            vb::update_execution(db, execution, &[F::CleanupPending(true)], now())
                .await
                .map_err(text)?;
            command
        } else {
            format!(
                "docker run --rm --name {} -v {} {} sh -c {} > {}",
                quote(&container),
                quote(&format!("{source}:/volume:ro")),
                quote(&image),
                quote(&script),
                quote(location)
            )
        };
        if backup.get("stop_during_backup").and_then(Value::as_bool) == Some(true) {
            let out = self
                .remote()
                .run(
                    &run.target,
                    &[containers_command(&source).as_str()],
                    RunOptions::op(OP),
                )
                .await
                .map_err(|e| e.message)?
                .unwrap_or_default();
            let id = Regex::new(r"(?i)^[a-f0-9]{6,64}$").map_err(|e| e.to_string())?;
            let containers: Vec<String> = out
                .split_whitespace()
                .filter(|c| id.is_match(c))
                .map(str::to_owned)
                .collect();
            if !containers.is_empty() {
                vb::update_execution(db, execution, &[F::StopPending(true)], now())
                    .await
                    .map_err(text)?;
                archive = archive_with_stopped_containers(
                    &archive,
                    &containers,
                    &state_file(execution_uuid),
                );
            }
        }
        let mkdir = format!("mkdir -p {}", quote(directory));
        let mut commands: Vec<&str> = vec![verify.as_str()];
        if !stream {
            commands.push(mkdir.as_str());
        }
        commands.push(archive.as_str());
        let output = self
            .remote()
            .run(
                &run.target,
                &commands,
                RunOptions::op(OP).timeout(run.timeout),
            )
            .await
            .map_err(|e| e.message)?
            .unwrap_or_default();
        vb::update_execution(
            db,
            execution,
            &[F::ClearStopContainers, F::StopPending(false)],
            now(),
        )
        .await
        .map_err(text)?;
        let size: i64 = if stream {
            output
                .trim()
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .trim()
                .parse()
                .unwrap_or(0)
        } else {
            let du = format!("du -b {} | cut -f1", quote(location));
            self.remote()
                .run(&run.target, &[du.as_str()], RunOptions::op(OP))
                .await
                .map_err(|e| e.message)?
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0)
        };
        if size <= 0 {
            return Err("The storage backup archive is empty or was not created.".to_owned());
        }
        let mut warning: Option<String> = None;
        let mut cleanup_pending = false;
        let mut local_deleted = stream;
        let save_s3 = backup.get("save_s3").and_then(Value::as_bool) == Some(true);
        if stream {
            vb::set_all_replicas_uploaded(db, execution, now())
                .await
                .map_err(text)?;
            vb::refresh_summary(db, execution, now())
                .await
                .map_err(text)?;
            vb::update_execution(db, execution, &[F::CleanupPending(false)], now())
                .await
                .map_err(text)?;
        } else if save_s3 && destinations.is_empty() {
            warning =
                Some("S3 upload failed: The selected S3 storage no longer exists.".to_owned());
        } else if save_s3 {
            cleanup_pending = true;
            vb::update_execution(db, execution, &[F::CleanupPending(true)], now())
                .await
                .map_err(text)?;
            let errors = self
                .upload_volume_archive(run, execution_uuid, location, directory, &image)
                .await?;
            if errors.is_empty() {
                cleanup_pending = false;
            } else {
                let mut w = format!("S3 upload failed: {}", errors.join(" "));
                match self.cleanup_volume_upload(run.execution, location).await {
                    Ok(()) => cleanup_pending = false,
                    Err(e) => w.push_str(&format!(" Partial S3 upload cleanup failed: {e}")),
                }
                warning = Some(w);
            }
            if errors.is_empty()
                && backup.get("disable_local_backup").and_then(Value::as_bool) == Some(true)
            {
                self.delete_local_backups(&[location.to_owned()], &run.server)
                    .await;
                local_deleted = true;
            }
        }
        let warning = warning.map(|w| run.redact(&w));
        vb::update_execution(
            db,
            execution,
            &[
                F::Status("success"),
                F::Message(warning.clone()),
                F::Size(size),
                F::Filename(Some(location.to_owned())),
                F::CleanupPending(cleanup_pending),
                F::LocalDeleted(local_deleted),
            ],
            now(),
        )
        .await
        .map_err(text)?;
        if let Err(e) = self
            .remove_expired_volume_backups(backup, &run.server)
            .await
        {
            tracing::warn!(backup = backup_id_of(backup), error = %run.redact(&e), "Volume backup retention cleanup failed");
        }
        Ok(warning)
    }

    /// `uploadToDestinations` and `uploadToS3`.
    async fn upload_volume_archive(
        &self,
        run: &Run,
        execution_uuid: &str,
        location: &str,
        directory: &str,
        image: &str,
    ) -> Result<Vec<String>, String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let mut errors = Vec::new();
        for (replica, storage, _, _) in vb::replicas_of(db, run.execution).await.map_err(text)? {
            let row = match storage {
                Some(s) => parts::s3_by_id(db, s).await.map_err(text)?,
                None => None,
            };
            let name = row.as_ref().map_or("Deleted storage".to_owned(), |r| {
                str_of(r, "name").to_owned()
            });
            let result = match &row {
                None => Err("The selected S3 storage no longer exists.".to_owned()),
                Some(r) => {
                    self.upload_volume_one(run, enc, r, execution_uuid, location, directory, image)
                        .await
                }
            };
            match result {
                Ok(()) => vb::set_replica_result(db, replica, true, None, now())
                    .await
                    .map_err(text)?,
                Err(e) => {
                    let message = run.redact(&e);
                    vb::set_replica_result(db, replica, false, Some(&message), now())
                        .await
                        .map_err(text)?;
                    errors.push(format!("{name}: {message}"));
                }
            }
        }
        vb::refresh_summary(db, run.execution, now())
            .await
            .map_err(text)?;
        Ok(errors)
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_volume_one(
        &self,
        run: &Run,
        enc: &coolify_compat::Encrypter,
        row: &Record,
        execution_uuid: &str,
        location: &str,
        directory: &str,
        image: &str,
    ) -> Result<(), String> {
        self.test_s3_connection(row).await?;
        let s3 = S3Target::from_row(enc, row)?;
        let name = quote(&format!("volume-upload-{execution_uuid}"));
        let resolve = resolve_flags(&s3.endpoint).await;
        let commands = [
            format!("docker rm -f {name} >/dev/null 2>&1 || true"),
            format!(
                "docker run -d --name {name} --rm -e {} -v {} {}",
                quote(&format!("MC_REGION={}", s3.region)),
                quote(&format!("{location}:{location}:ro")),
                quote(image)
            ),
            format!(
                "docker exec {name} mc alias set{resolve} temporary {} {} {}",
                quote(&s3.endpoint),
                quote(&s3.key),
                quote(&s3.secret)
            ),
            format!(
                "docker exec {name} mc cp{resolve} {} {}",
                quote(location),
                quote(&format!("temporary/{}{directory}/", s3.bucket))
            ),
        ];
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let result = self
            .remote()
            .run(&run.target, &refs, RunOptions::op(OP).timeout(run.timeout))
            .await;
        let remove = format!("docker rm -f {name}");
        self.remote()
            .run_quiet(&run.target, &[remove.as_str()], RunOptions::op(OP))
            .await;
        result.map(|_| ()).map_err(|e| e.message)
    }

    /// `streamToS3Command`.
    async fn stream_to_s3_command(
        &self,
        row: &Record,
        script: &str,
        location: &str,
        source: &str,
        container: &str,
        image: &str,
    ) -> Result<String, String> {
        self.test_s3_connection(row).await?;
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let s3 = S3Target::from_row(enc, row)?;
        let resolve = resolve_flags(&s3.endpoint).await;
        let destination = quote(&format!("temporary/{}{location}", s3.bucket));
        let log = "/tmp/mc-pipe.log";
        let stream = format!(
            "set -o pipefail; mc alias set{resolve} temporary {} {} {} >/dev/null && lock=$(mc retention info --default --json{resolve} {}) && case \"$lock\" in *'\"enabled\":\"Enabled\"'*) checksum=--checksum=MD5 ;; *) checksum= ;; esac && ({script} | mc pipe{resolve} $checksum {destination} >/dev/null 2>{log} || {{ status=$?; awk -F {} {} {log} >&2; exit $status; }}) && mc stat --json{resolve} {destination} | sed -n {}",
            quote(&s3.endpoint),
            quote(&s3.key),
            quote(&s3.secret),
            quote(&format!("temporary/{}", s3.bucket)),
            quote("\r"),
            quote("{ print $NF }"),
            quote(r#"s/.*"size":\([0-9][0-9]*\).*/\1/p"#)
        );
        Ok(format!(
            "docker run --rm --name {} -e {} -v {} {} sh -c {}",
            quote(container),
            quote(&format!("MC_REGION={}", s3.region)),
            quote(&format!("{source}:/volume:ro")),
            quote(image),
            quote(&stream)
        ))
    }

    /// `VolumeBackupRecoveryJob::cleanupS3Upload`: the partial copy of every destination that did not finish.
    async fn cleanup_volume_upload(&self, execution: i64, location: &str) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let mut first: Result<(), String> = Ok(());
        for (_, storage, uploaded, _) in vb::replicas_of(db, execution).await.map_err(text)? {
            if uploaded == Some(true) {
                continue;
            }
            let Some(row) = (match storage {
                Some(s) => parts::s3_by_id(db, s).await.map_err(text)?,
                None => None,
            }) else {
                continue;
            };
            // The endpoint check of `testConnection` also guards this request.
            let result = async {
                self.check_s3_connection(&row).await?;
                S3Target::from_row(enc, &row)?
                    .delete_objects(&[location.to_owned()])
                    .await
            }
            .await;
            if first.is_ok() {
                first = result;
            }
        }
        if first.is_ok() {
            vb::update_execution(db, execution, &[F::CleanupPending(false)], now())
                .await
                .map_err(text)?;
        }
        first
    }

    /// `recoverContainers`.
    async fn recover_volume_containers(
        &self,
        run: &Run,
        execution_uuid: &str,
    ) -> Result<(), String> {
        let state = quote(&state_file(execution_uuid));
        let remaining = quote(&format!("{}.remaining", state_file(execution_uuid)));
        let script = format!(
            "status=0; : > {remaining}; if [ -f {state} ]; then while IFS= read -r container; do [ -z \"$container\" ] && continue; running=$(docker inspect --format '{{{{.State.Running}}}}' \"$container\" 2>&1) || {{ case \"$running\" in *\"o such object\"*|*\"o such container\"*) continue ;; esac; echo \"$container\" >> {remaining}; status=1; continue; }}; if [ \"$running\" != true ] && ! docker start \"$container\" >/dev/null; then echo \"$container\" >> {remaining}; status=1; fi; done < {state}; fi; if [ -s {remaining} ]; then mv {remaining} {state}; else rm -f {state} {remaining}; fi; exit $status"
        );
        let command = format!("sh -c {}", quote(&script));
        self.remote()
            .run(&run.target, &[command.as_str()], RunOptions::op(OP))
            .await
            .map_err(|e| e.message)?;
        vb::update_execution(
            &self.0.db,
            run.execution,
            &[F::ClearStopContainers, F::StopPending(false)],
            now(),
        )
        .await
        .map_err(|e| e.to_string())
    }

    /// The `catch` of `handle`.
    async fn fail_volume_execution(
        &self,
        execution: i64,
        message: &str,
        location: &str,
        stream: bool,
        run: Option<&Run>,
    ) {
        let db = &self.0.db;
        let mut recovery = String::new();
        let mut archive_deleted = stream;
        if let Some(run) = run {
            let uuid: Option<String> = sqlx::query_scalar(
                "SELECT uuid FROM scheduled_volume_backup_executions WHERE id = $1",
            )
            .bind(execution)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
            if let Some(uuid) = uuid
                && let Err(e) = self.recover_volume_containers(run, &uuid).await
            {
                recovery.push_str(&format!(" Container recovery failed: {e}"));
            }
            if !stream {
                self.delete_local_backups(&[location.to_owned()], &run.server)
                    .await;
                archive_deleted = true;
            }
            if let Err(e) = self.cleanup_volume_upload(execution, location).await {
                recovery.push_str(&format!(" Container recovery failed: {e}"));
            }
        }
        let _ = vb::fail_open_replicas(db, execution, now()).await;
        let _ = vb::refresh_summary(db, execution, now()).await;
        let pending: bool = sqlx::query_scalar(
            "SELECT s3_cleanup_pending FROM scheduled_volume_backup_executions WHERE id = $1",
        )
        .bind(execution)
        .fetch_one(db.pool())
        .await
        .unwrap_or(false);
        let filename = if stream {
            pending.then(|| location.to_owned())
        } else if archive_deleted && !pending {
            None
        } else {
            Some(location.to_owned())
        };
        let text = format!("{message}{recovery}");
        let _ = vb::update_execution(
            db,
            execution,
            &[
                F::Status("failed"),
                F::Message(Some(run.map_or(text.clone(), |r| r.redact(&text)))),
                F::Filename(filename),
                F::LocalDeleted(archive_deleted),
            ],
            now(),
        )
        .await;
    }

    /// `removeExpiredBackups`.
    async fn remove_expired_volume_backups(
        &self,
        backup: &Record,
        server: &ServerInfo,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let id = backup_id_of(backup);
        let int = |k: &str| backup.get(k).and_then(Value::as_i64).unwrap_or(0);
        let rows = vb::executions_of(db, id).await.map_err(text)?;
        let local: Vec<Record> = rows
            .iter()
            .filter(|e| {
                str_of(e, "status") == "success"
                    && e.get("local_storage_deleted") != Some(&Value::Bool(true))
            })
            .cloned()
            .collect();
        let expired = outside_retention(
            &local,
            int("retention_amount_locally"),
            int("retention_days_locally"),
            number(backup, "retention_max_storage_locally"),
        );
        let files: Vec<String> = expired
            .iter()
            .map(|e| str_of(e, "filename").to_owned())
            .filter(|f| !f.is_empty())
            .collect();
        if !files.is_empty() {
            self.delete_local_backups(&files, server).await;
            let ids: Vec<i64> = expired.iter().map(parts::id_of).collect();
            vb::mark_local_deleted(db, &ids, now())
                .await
                .map_err(text)?;
        }
        if backup.get("save_s3").and_then(Value::as_bool) == Some(true) {
            let enc = self
                .0
                .encrypter
                .as_ref()
                .ok_or("No application encryption key has been specified.")?;
            // Each destination keeps its own copies within the same retention settings.
            let mut live: Vec<(i64, Record, i64)> = Vec::new();
            for e in rows.iter().filter(|e| str_of(e, "status") == "success") {
                for (replica, storage, uploaded, deleted) in
                    vb::replicas_of(db, parts::id_of(e)).await.map_err(text)?
                {
                    if uploaded == Some(true) && !deleted {
                        live.push((storage.unwrap_or(-1), e.clone(), replica));
                    }
                }
            }
            let mut storages: Vec<i64> = live.iter().map(|l| l.0).collect();
            storages.sort_unstable();
            storages.dedup();
            for storage in storages {
                let copies: Vec<&(i64, Record, i64)> =
                    live.iter().filter(|l| l.0 == storage).collect();
                let rows: Vec<Record> = copies.iter().map(|c| c.1.clone()).collect();
                let expired = outside_retention(
                    &rows,
                    int("retention_amount_s3"),
                    int("retention_days_s3"),
                    number(backup, "retention_max_storage_s3"),
                );
                if expired.is_empty() {
                    continue;
                }
                if let Some(row) = parts::s3_by_id(db, storage).await.map_err(text)? {
                    let files: Vec<String> = expired
                        .iter()
                        .map(|e| str_of(e, "filename").to_owned())
                        .filter(|f| !f.is_empty())
                        .collect();
                    if !files.is_empty() {
                        S3Target::from_row(enc, &row)?
                            .delete_objects(&files)
                            .await?;
                    }
                }
                for e in &expired {
                    let eid = parts::id_of(e);
                    for c in copies.iter().filter(|c| parts::id_of(&c.1) == eid) {
                        vb::mark_replica_deleted(db, c.2, now())
                            .await
                            .map_err(text)?;
                    }
                    vb::refresh_summary(db, eid, now()).await.map_err(text)?;
                }
            }
        }
        vb::delete_exhausted(db, id).await.map_err(text)
    }

    /// `DeleteScheduledVolumeBackup::handle`: the error is the 409 message.
    pub async fn delete_volume_schedule(
        &self,
        backup: &Record,
        server: Option<&ServerInfo>,
        delete_local: bool,
        delete_s3: bool,
    ) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let id = backup_id_of(backup);
        if vb::has_active_executions(db, id).await.map_err(text)? {
            return Err("Wait for the running storage backup and recovery operations to finish before deleting this schedule.".to_owned());
        }
        let rows = vb::executions_of(db, id).await.map_err(text)?;
        let files: Vec<String> = rows
            .iter()
            .filter(|e| e.get("local_storage_deleted") != Some(&Value::Bool(true)))
            .map(|e| str_of(e, "filename").to_owned())
            .filter(|f| !f.is_empty())
            .collect();
        if delete_local && !files.is_empty() {
            let Some(server) = server else {
                return Err(
                    "The server is unavailable, so local backup archives cannot be deleted."
                        .to_owned(),
                );
            };
            self.delete_local_backups(&files, server).await;
        }
        if delete_s3 {
            for e in &rows {
                self.delete_volume_s3_copies(parts::id_of(e), str_of(e, "filename"))
                    .await?;
            }
        }
        vb::delete_schedule(db, id).await.map_err(text)
    }

    /// `ScheduledVolumeBackupExecution::deleteS3Copies`.
    pub async fn delete_volume_s3_copies(&self, execution: i64, file: &str) -> Result<(), String> {
        let db = &self.0.db;
        let text = |e: coolify_db::DbError| e.to_string();
        let enc = self.0.encrypter.as_ref();
        let result: Result<(), String> = async {
            for (replica, storage, uploaded, deleted) in
                vb::replicas_of(db, execution).await.map_err(text)?
            {
                if uploaded != Some(true) || deleted {
                    continue;
                }
                let row = match storage {
                    Some(s) => parts::s3_by_id(db, s).await.map_err(text)?,
                    None => None,
                };
                let row = row.ok_or("The S3 storage used by an existing backup is unavailable.")?;
                if !file.is_empty() {
                    let enc = enc.ok_or("No application encryption key has been specified.")?;
                    S3Target::from_row(enc, &row)?
                        .delete_objects(&[file.to_owned()])
                        .await?;
                }
                vb::mark_replica_deleted(db, replica, now())
                    .await
                    .map_err(text)?;
            }
            Ok(())
        }
        .await;
        vb::refresh_summary(db, execution, now())
            .await
            .map_err(text)?;
        result
    }
}

fn backup_id_of(backup: &Record) -> i64 {
    parts::id_of(backup)
}

async fn resolve_flags(endpoint: &str) -> String {
    let joined = minio_resolve_options(endpoint)
        .await
        .iter()
        .map(|o| format!("--resolve {}", quote(o)))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        joined
    } else {
        format!(" {joined}")
    }
}
