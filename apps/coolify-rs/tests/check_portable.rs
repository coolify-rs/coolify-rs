//! `coolify-rs check-portable` against a test database (ADR 0012): exit codes, JSON and text reports.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Output;

use coolify_db::testing::TestDb;

/// The test database's URL: the admin URL with the database name swapped.
fn url(tdb: &TestDb) -> String {
    let admin = std::env::var("COOLIFY_RS_TEST_DATABASE_URL").unwrap();
    let base = admin.rsplit_once('/').map(|(b, _)| b).unwrap();
    format!("{base}/{}", tdb.name)
}

fn check(url: &str, json: bool) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_coolify-rs"));
    cmd.arg("check-portable").env("DATABASE_URL", url);
    if json {
        cmd.arg("--json");
    }
    cmd.output().unwrap()
}

const APP: &str = "INSERT INTO applications (id, uuid, name, git_repository, git_branch, build_pack, environment_id) VALUES ";

#[tokio::test]
async fn check_portable_reports_and_exits() {
    let tdb = TestDb::new(&format!(
        "{APP} (1, 'a1', 'web', 'r', 'main', 'railpack', 1)"
    ))
    .await;
    let url = url(&tdb);

    // No plugin ever ran: portable, and the schema is not created by the check.
    let out = check(&url, true);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({
            "portable": true,
            "owned_resources": [],
            "unknown_values": [],
            "plugin_schema": false,
        })
    );

    // A provider plugin's build pack, marked, and one nobody marked.
    coolify_plugins::store::migrate(&tdb.db).await.unwrap();
    sqlx::raw_sql(&format!(
        "{APP} (2, 'a2', 'go', 'r', 'main', 'acme-go', 1), (3, 'a3', 'rb', 'r', 'main', 'acme-ruby', 1)"
    ))
    .execute(tdb.db.pool())
    .await
    .unwrap();
    coolify_plugins::store::mark_owned(&tdb.db, "applications", 2, "acme/go-pack", "")
        .await
        .unwrap();
    let out = check(&url, true);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["portable"], false);
    assert_eq!(report["plugin_schema"], true);
    assert_eq!(
        report["owned_resources"],
        serde_json::json!([{"table": "applications", "row_id": 2, "plugin_id": "acme/go-pack", "note": ""}])
    );
    assert_eq!(
        report["unknown_values"],
        serde_json::json!([
            {"table": "applications", "column": "build_pack", "row_id": 2, "value": "acme-go", "owner": "acme/go-pack"},
            {"table": "applications", "column": "build_pack", "row_id": 3, "value": "acme-ruby", "owner": null},
        ])
    );
    let text = String::from_utf8(check(&url, false).stdout).unwrap();
    assert!(text.starts_with("Not portable"), "{text}");
    assert!(text.contains("applications #2 by acme/go-pack"), "{text}");
    assert!(
        text.contains("\"acme-ruby\" in row #3 (not marked by any plugin)"),
        "{text}"
    );
    assert!(
        text.contains("DROP SCHEMA coolify_rs_plugins CASCADE"),
        "{text}"
    );

    // Handed back: the values are Coolify's again; the plugin schema can stay.
    sqlx::raw_sql(
        "UPDATE applications SET build_pack = 'dockerfile' WHERE id IN (2, 3); \
         DELETE FROM coolify_rs_plugins.owned_resources",
    )
    .execute(tdb.db.pool())
    .await
    .unwrap();
    let out = check(&url, false);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Portable"));

    // A database it cannot reach is neither portable nor not.
    let out = check(&format!("{url}_missing"), true);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty());
    tdb.drop_db().await;
}
