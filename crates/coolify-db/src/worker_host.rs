//! `RecordsWorkerHost`: scheduled task, database backup and volume backup executions store the
//! host that created them (`worker_host`, from Coolify v4.6.0), so `runningScheduledJobCount()`
//! counts only the runs of this host's workers. coolify-rs runs these jobs in-process, so "this
//! host" is the host of the coolify-rs process: `$HOSTNAME` (the container's host name, which is
//! what `gethostname()` returns in upstream's container), else `hostname`.
//!
//! The column is written only when the database has it (Coolify v4.6.0 and later). Like the
//! model's `$hidden`, API responses never show it.

use std::sync::OnceLock;

use crate::schema::SchemaInfo;

/// The column, hidden from API output.
pub const COLUMN: &str = "worker_host";

/// `gethostname()`.
pub fn worker_host() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        std::env::var("HOSTNAME")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
            })
            .map(|h| h.trim().to_owned())
            .unwrap_or_default()
    })
}

/// The extra insert column of `table`: `(", worker_host", ", $<param>", Some(host))`, or empty
/// strings and `None` on a schema without the column. Bind the host only when it is `Some`.
pub fn insert_parts(
    schema: &SchemaInfo,
    table: &str,
    param: usize,
) -> (String, String, Option<&'static str>) {
    if schema.has_column(table, COLUMN) {
        (
            format!(", {COLUMN}"),
            format!(", ${param}"),
            Some(worker_host()),
        )
    } else {
        (String::new(), String::new(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_is_written_only_when_it_exists() {
        let mut schema = SchemaInfo::default();
        assert_eq!(
            insert_parts(&schema, "scheduled_task_executions", 5),
            (String::new(), String::new(), None)
        );
        schema.columns.insert(
            "scheduled_task_executions".into(),
            vec!["id".into(), "worker_host".into()],
        );
        let (column, value, host) = insert_parts(&schema, "scheduled_task_executions", 5);
        assert_eq!(column, ", worker_host");
        assert_eq!(value, ", $5");
        assert_eq!(host, Some(worker_host()));
        assert_eq!(
            insert_parts(&schema, "scheduled_volume_backup_executions", 5).2,
            None
        );
    }
}
