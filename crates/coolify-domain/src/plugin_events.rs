//! Names of the events plugins can subscribe to (ADR 0010). Each comes from one of Coolify's
//! notifications; `coolify_jobs::notifications` maps them. These names are a public contract: add new
//! ones freely, but rename or remove one only with a new major version of the plugin interface.

/// The version of every payload shape below. Bump it for one kind when its payload changes incompatibly.
pub const PAYLOAD_VERSION: u32 = 1;

pub const DEPLOYMENT_SUCCEEDED: &str = "deployment.succeeded";
pub const DEPLOYMENT_FAILED: &str = "deployment.failed";
pub const APPLICATION_STATUS_CHANGED: &str = "application.status_changed";
pub const CONTAINER_RESTARTED: &str = "container.restarted";
pub const CONTAINER_RESTART_LIMIT_REACHED: &str = "container.restart_limit_reached";
pub const BACKUP_SUCCEEDED: &str = "backup.succeeded";
pub const BACKUP_SUCCEEDED_WITH_S3_WARNING: &str = "backup.succeeded_with_s3_warning";
pub const BACKUP_FAILED: &str = "backup.failed";
pub const BACKUP_MISSING: &str = "backup.missing";
pub const VOLUME_BACKUP_SUCCEEDED: &str = "volume_backup.succeeded";
pub const VOLUME_BACKUP_FAILED: &str = "volume_backup.failed";
pub const VOLUME_BACKUP_MISSING: &str = "volume_backup.missing";
pub const VOLUME_BACKUP_RECOVERY_FAILED: &str = "volume_backup.recovery_failed";
pub const TASK_SUCCEEDED: &str = "scheduled_task.succeeded";
pub const TASK_FAILED: &str = "scheduled_task.failed";
pub const DOCKER_CLEANUP_SUCCEEDED: &str = "server.docker_cleanup_succeeded";
pub const DOCKER_CLEANUP_FAILED: &str = "server.docker_cleanup_failed";
pub const SERVER_REACHABLE: &str = "server.reachable";
pub const SERVER_UNREACHABLE: &str = "server.unreachable";
pub const SERVER_FORCE_DISABLED: &str = "server.force_disabled";
pub const SERVER_FORCE_ENABLED: &str = "server.force_enabled";
pub const SERVER_HIGH_DISK_USAGE: &str = "server.high_disk_usage";
pub const SERVER_PATCH_CHECK: &str = "server.patch_check";
pub const SERVER_HETZNER_DELETION_FAILED: &str = "server.hetzner_deletion_failed";
pub const PROXY_TRAEFIK_OUTDATED: &str = "proxy.traefik_outdated";
pub const SSL_CERTIFICATES_EXPIRING: &str = "ssl.certificates_expiring";
pub const API_TOKEN_EXPIRING: &str = "api_token.expiring";
pub const GENERAL: &str = "general";

/// Every kind, for validating manifests.
pub const KINDS: &[&str] = &[
    DEPLOYMENT_SUCCEEDED,
    DEPLOYMENT_FAILED,
    APPLICATION_STATUS_CHANGED,
    CONTAINER_RESTARTED,
    CONTAINER_RESTART_LIMIT_REACHED,
    BACKUP_SUCCEEDED,
    BACKUP_SUCCEEDED_WITH_S3_WARNING,
    BACKUP_FAILED,
    BACKUP_MISSING,
    VOLUME_BACKUP_SUCCEEDED,
    VOLUME_BACKUP_FAILED,
    VOLUME_BACKUP_MISSING,
    VOLUME_BACKUP_RECOVERY_FAILED,
    TASK_SUCCEEDED,
    TASK_FAILED,
    DOCKER_CLEANUP_SUCCEEDED,
    DOCKER_CLEANUP_FAILED,
    SERVER_REACHABLE,
    SERVER_UNREACHABLE,
    SERVER_FORCE_DISABLED,
    SERVER_FORCE_ENABLED,
    SERVER_HIGH_DISK_USAGE,
    SERVER_PATCH_CHECK,
    SERVER_HETZNER_DELETION_FAILED,
    PROXY_TRAEFIK_OUTDATED,
    SSL_CERTIFICATES_EXPIRING,
    API_TOKEN_EXPIRING,
    GENERAL,
];
