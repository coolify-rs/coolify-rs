//! Plugin events (ADR 0010): every notification is also offered to plugins, under a stable name from
//! `coolify_domain::plugin_events`. This only adds a call; the notification is delivered, throttled and
//! rate-limited exactly as before.

use std::sync::OnceLock;

use coolify_domain::plugin_events as kind;
use serde_json::Value;

use super::Notification;

/// Receives plugin events. Set once at startup; without one, events are dropped.
pub trait EventSink: Send + Sync {
    /// Called on the delivering task: hand the event off and return at once.
    fn emit(&self, team_id: i64, kind: &'static str, payload: Value);
}

static SINK: OnceLock<Box<dyn EventSink>> = OnceLock::new();

/// Sets the sink. Returns false when one was already set.
pub fn set_event_sink(sink: Box<dyn EventSink>) -> bool {
    SINK.set(sink).is_ok()
}

pub(super) fn emit(team_id: i64, notification: &Notification) {
    let Some(sink) = SINK.get() else { return };
    let Some(kind) = event_kind(notification) else {
        return;
    };
    sink.emit(team_id, kind, payload(notification));
}

/// The event name of a notification; `None` for test notifications, which are not events.
pub fn event_kind(n: &Notification) -> Option<&'static str> {
    Some(match n {
        Notification::DeploymentSuccess(_) => kind::DEPLOYMENT_SUCCEEDED,
        Notification::DeploymentFailed(_) => kind::DEPLOYMENT_FAILED,
        Notification::StatusChanged { .. } => kind::APPLICATION_STATUS_CHANGED,
        Notification::RestartLimitReached(_) => kind::CONTAINER_RESTART_LIMIT_REACHED,
        Notification::ContainerRestarted { .. } => kind::CONTAINER_RESTARTED,
        Notification::BackupSuccess(_) => kind::BACKUP_SUCCEEDED,
        Notification::BackupFailed { .. } => kind::BACKUP_FAILED,
        Notification::BackupMissing { .. } => kind::BACKUP_MISSING,
        Notification::BackupSuccessWithS3Warning { .. } => kind::BACKUP_SUCCEEDED_WITH_S3_WARNING,
        Notification::General { .. } => kind::GENERAL,
        Notification::TaskSuccess(_) => kind::TASK_SUCCEEDED,
        Notification::TaskFailed(_) => kind::TASK_FAILED,
        Notification::DockerCleanupSuccess { .. } => kind::DOCKER_CLEANUP_SUCCEEDED,
        Notification::DockerCleanupFailed { .. } => kind::DOCKER_CLEANUP_FAILED,
        Notification::ForceDisabled { .. } => kind::SERVER_FORCE_DISABLED,
        Notification::ForceEnabled { .. } => kind::SERVER_FORCE_ENABLED,
        Notification::HetznerDeletionFailed { .. } => kind::SERVER_HETZNER_DELETION_FAILED,
        Notification::HighDiskUsage { .. } => kind::SERVER_HIGH_DISK_USAGE,
        Notification::Reachable { .. } => kind::SERVER_REACHABLE,
        Notification::Unreachable { .. } => kind::SERVER_UNREACHABLE,
        Notification::ServerPatchCheck { .. } => kind::SERVER_PATCH_CHECK,
        Notification::TraefikVersionOutdated { .. } => kind::PROXY_TRAEFIK_OUTDATED,
        Notification::VolumeBackupSuccess { .. } => kind::VOLUME_BACKUP_SUCCEEDED,
        Notification::VolumeBackupFailed { .. } => kind::VOLUME_BACKUP_FAILED,
        Notification::VolumeBackupMissing { .. } => kind::VOLUME_BACKUP_MISSING,
        Notification::VolumeBackupRecoveryFailed { .. } => kind::VOLUME_BACKUP_RECOVERY_FAILED,
        Notification::ApiTokenExpiring { .. } => kind::API_TOKEN_EXPIRING,
        Notification::SslExpiration { .. } => kind::SSL_CERTIFICATES_EXPIRING,
        Notification::Test { .. } => return None,
    })
}

/// The notification's data as a JSON object, without the internal `type` tag and throttle key.
fn payload(n: &Notification) -> Value {
    let mut v = serde_json::to_value(n).unwrap_or_default();
    if let Some(map) = v.as_object_mut() {
        map.shift_remove("type");
        map.shift_remove("throttle");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_known() {
        let n = Notification::General {
            message: "m".into(),
            success: true,
        };
        assert_eq!(event_kind(&n), Some(kind::GENERAL));
        assert_eq!(
            payload(&n),
            serde_json::json!({"message": "m", "success": true})
        );
        assert!(kind::KINDS.contains(&kind::GENERAL));
    }
}
