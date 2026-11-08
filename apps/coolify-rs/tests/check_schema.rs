//! `coolify-rs check-schema` against test databases: the verdict as it will be after `boot`, and
//! migration names upstream renamed or deleted reported apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coolify_db::testing::{MIGRATIONS_4_5_3, SCHEMA_4_5_3, TestDb};
use serde_json::Value;

fn url(tdb: &TestDb) -> String {
    let admin = std::env::var("COOLIFY_RS_TEST_DATABASE_URL").unwrap();
    let base = admin.rsplit_once('/').map(|(b, _)| b).unwrap();
    format!("{base}/{}", tdb.name)
}

fn check_schema(url: &str) -> Value {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_coolify-rs"))
        .arg("check-schema")
        .env("DATABASE_URL", url)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json = &stdout[stdout.find('{').unwrap()..];
    serde_json::from_str(json).unwrap()
}

const HISTORICAL_ROWS: &str = "INSERT INTO migrations (migration, batch) VALUES \
     ('2018_08_08_100000_create_telescope_entries_table', 1), \
     ('2025_10_10_120000_create_cloud_init_scripts_table', 30), \
     ('2025_10_10_120000_create_webhook_notification_settings_table', 30), \
     ('2025_10_10_120001_populate_webhook_notification_settings_for_existing_teams', 30);";

#[tokio::test]
async fn check_schema_reports_what_boot_leaves() {
    // A long-lived Coolify 4.6.0: writable, the old names counted, nothing to apply.
    let tdb = TestDb::new(HISTORICAL_ROWS).await;
    let v = check_schema(&url(&tdb));
    assert_eq!(v["read_only_reason"], Value::Null, "{v}");
    assert_eq!(v["historical_migrations"], 4);
    assert_eq!(v["unknown_migrations"], serde_json::json!([]));
    assert_eq!(v["missing_migrations"], serde_json::json!([]));
    assert_eq!(v["applied_on_boot"], serde_json::json!([]));
    assert_eq!(v["applied_migrations"], 497);
    tdb.drop_db().await;

    // Coolify v4.5.3: boot applies the fifteen v4.6.0 migrations, then it is writable.
    let tdb = TestDb::with_baseline(SCHEMA_4_5_3, MIGRATIONS_4_5_3, "").await;
    let v = check_schema(&url(&tdb));
    assert_eq!(v["read_only_reason"], Value::Null, "{v}");
    assert_eq!(v["applied_on_boot"].as_array().unwrap().len(), 15);
    assert_eq!(v["missing_migrations"], serde_json::json!([]));
    assert_eq!(v["applied_migrations"], 478);
    tdb.drop_db().await;

    // A newer Coolify: read-only, and the unknown name is the one reported.
    let tdb = TestDb::new(&format!(
        "{HISTORICAL_ROWS} INSERT INTO migrations (migration, batch) VALUES \
         ('2026_11_20_120000_a_coolify_4_7_0_migration', 31);"
    ))
    .await;
    let v = check_schema(&url(&tdb));
    assert!(v["read_only_reason"].is_string(), "{v}");
    assert_eq!(
        v["unknown_migrations"],
        serde_json::json!(["2026_11_20_120000_a_coolify_4_7_0_migration"])
    );
    assert_eq!(v["historical_migrations"], 4);
    tdb.drop_db().await;
}
