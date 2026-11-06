//! `applicationParser()` against what upstream recorded for the same inputs
//! (tests/fixtures/compose_parser: `harness.php` ran each case on the reference instance inside a
//! rolled-back transaction). Each case seeds a fresh database with the recorded rows, runs the
//! Rust parser once per recorded parse and compares the returned compose, the application
//! columns, the environment variables (in insertion order, decrypted), the file storages and the
//! persistent volumes. Generated secrets are compared by key, length and character class.
//! The fixtures were recorded at b5823c7; the port-suffixed `SERVICE_URL_*_<port>` and
//! `SERVICE_FQDN_*_<port>` values were updated by hand for v4.5.3 (1440ea83d,
//! `syncComposeServiceUrlVariables`), which keeps them in sync with the service domain, and the
//! clone line of the `load-*` read commands for 1a2a5362a and 56af27d46 (`exec >&2`, the Git LFS
//! skip, and the submodule step inside the clone script). For 1a2a5362a the preview copies the
//! `created` hook made of generated magic variables (`SERVICE_PASSWORD_*` and friends) of these
//! compose applications were removed by hand: a compose preview now generates its own.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::NaiveDateTime;
use coolify_compat::Encrypter;
use coolify_db::testing::TestDb;
use coolify_jobs::Jobs;
use coolify_jobs::compose_parser;
use serde_json::{Value, json};

/// A test key (never a real instance's key).
const APP_KEY: &str = "base64:JgOyH5GGzVhKpVXBi9VwoVxVN2T8j3tfdGFmFzT0g5E=";

const SEED: &str = "INSERT INTO instance_settings (id) VALUES (0);";

fn insert_row(table: &str) -> String {
    format!("INSERT INTO {table} SELECT * FROM json_populate_record(NULL::{table}, $1::json)")
}

async fn seed(db: &coolify_db::Db, enc: &Encrypter, ctx: &Value, before: &Value) -> i64 {
    let pool = db.pool();
    let exec = |table: &'static str, mut row: Value| async move {
        // The harness reads json columns as text; they are written back with that exact text.
        let json_columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns WHERE table_name = $1 AND data_type IN ('json', 'jsonb')",
        )
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap();
        let mut json_texts: Vec<(String, String)> = Vec::new();
        for c in json_columns {
            if let Some(Value::String(text)) = row.get(&c).cloned() {
                row[&c] = Value::Null;
                json_texts.push((c, text));
            }
        }
        sqlx::query(&insert_row(table))
            .bind(row.to_string())
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("seeding {table}: {e}"));
        for (c, text) in json_texts {
            sqlx::query(&format!(
                "UPDATE {table} SET \"{c}\" = $1::json WHERE id = $2"
            ))
            .bind(text)
            .bind(row["id"].as_i64())
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("seeding {table}.{c}: {e}"));
        }
    };
    exec("teams", ctx["team"].clone()).await;
    let key_id = ctx["server"]["private_key_id"].as_i64().unwrap();
    sqlx::query("INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES ($1, 'pk', 'k', 'x', $2)")
        .bind(key_id)
        .bind(ctx["team"]["id"].as_i64().unwrap())
        .execute(pool)
        .await
        .unwrap();
    exec("projects", ctx["project"].clone()).await;
    exec("environments", ctx["environment"].clone()).await;
    exec("servers", ctx["server"].clone()).await;
    exec("server_settings", ctx["server_settings"].clone()).await;
    exec("standalone_dockers", ctx["standalone_docker"].clone()).await;
    if ctx["github_app"].is_object() {
        if let Some(pk) = ctx["github_app"]["private_key_id"].as_i64()
            && pk != key_id
        {
            sqlx::query("INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES ($1, 'gh-pk', 'k', 'x', $2)")
                .bind(pk)
                .bind(ctx["team"]["id"].as_i64().unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        exec("github_apps", ctx["github_app"].clone()).await;
    }
    exec("applications", ctx["application"].clone()).await;
    exec("application_settings", ctx["application_settings"].clone()).await;
    let app_id = ctx["application"]["id"].as_i64().unwrap();
    for s in ctx["shared_variables"].as_array().unwrap() {
        sqlx::query(
            "INSERT INTO shared_environment_variables (key, value, type, team_id, project_id, environment_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(s["key"].as_str())
        .bind(s["value"].as_str().map(|v| enc.encrypt_string(v)))
        .bind(s["type"].as_str())
        .bind(ctx["team"]["id"].as_i64())
        .bind(ctx["project"]["id"].as_i64())
        .bind(ctx["environment"]["id"].as_i64())
        .execute(pool)
        .await
        .unwrap();
    }
    for (i, v) in before["environment_variables"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO environment_variables (key, value, is_preview, is_required, is_shared, is_runtime, is_buildtime, \
             is_multiline, is_literal, is_shown_once, preserve_compose_null, comment, version, resourceable_type, resourceable_id, uuid) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'App\\Models\\Application', $14, $15)",
        )
        .bind(v["key"].as_str())
        .bind(v["value"].as_str().map(|x| enc.encrypt_serialized(x)))
        .bind(v["is_preview"].as_bool())
        .bind(v["is_required"].as_bool())
        .bind(v["is_shared"].as_bool())
        .bind(v["is_runtime"].as_bool())
        .bind(v["is_buildtime"].as_bool())
        .bind(v["is_multiline"].as_bool())
        .bind(v["is_literal"].as_bool())
        .bind(v["is_shown_once"].as_bool())
        .bind(v["preserve_compose_null"].as_bool())
        .bind(v["comment"].as_str())
        .bind(v["version"].as_str())
        .bind(app_id)
        .bind(format!("seed-env-{i}"))
        .execute(pool)
        .await
        .unwrap();
    }
    for (i, f) in before["file_volumes"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO local_file_volumes (fs_path, mount_path, content, is_directory, is_based_on_git, is_host_file, \
             is_preview_suffix_enabled, resource_type, resource_id, uuid) VALUES ($1, $2, $3, $4, $5, $6, $7, 'App\\Models\\Application', $8, $9)",
        )
        .bind(f["fs_path"].as_str())
        .bind(f["mount_path"].as_str())
        .bind(f["content"].as_str().map(|c| enc.encrypt_string(c)))
        .bind(f["is_directory"].as_bool())
        .bind(f["is_based_on_git"].as_bool())
        .bind(f["is_host_file"].as_bool())
        .bind(f["is_preview_suffix_enabled"].as_bool())
        .bind(app_id)
        .bind(format!("seed-file-{i}"))
        .execute(pool)
        .await
        .unwrap();
    }
    for (i, v) in before["persistent_volumes"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO local_persistent_volumes (name, mount_path, host_path, is_preview_suffix_enabled, ignores_compose_driver_options, \
             resource_type, resource_id, uuid) VALUES ($1, $2, $3, $4, $5, 'App\\Models\\Application', $6, $7)",
        )
        .bind(v["name"].as_str())
        .bind(v["mount_path"].as_str())
        .bind(v["host_path"].as_str())
        .bind(v["is_preview_suffix_enabled"].as_bool())
        .bind(v["ignores_compose_driver_options"].as_bool())
        .bind(app_id)
        .bind(format!("seed-volume-{i}"))
        .execute(pool)
        .await
        .unwrap();
    }
    app_id
}

/// The same state `harness.php` prints.
async fn state(db: &coolify_db::Db, enc: &Encrypter, app_id: i64) -> Value {
    let pool = db.pool();
    type AppRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let a: AppRow = sqlx::query_as(
        "SELECT fqdn, domain_port_overrides::text, noindex_domains::text, docker_compose_domains, docker_compose, docker_compose_raw \
         FROM applications WHERE id = $1",
    )
    .bind(app_id)
    .fetch_one(pool)
    .await
    .unwrap();
    type EnvRow = (
        String,
        Option<String>,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        Option<String>,
        String,
    );
    let envs: Vec<EnvRow> = sqlx::query_as(
        "SELECT key, value, is_preview, is_required, is_shared, is_runtime, is_buildtime, is_multiline, is_literal, is_shown_once, \
         preserve_compose_null, comment, version FROM environment_variables WHERE resourceable_type = 'App\\Models\\Application' \
         AND resourceable_id = $1 ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await
    .unwrap();
    type FileRow = (
        String,
        Option<String>,
        Option<String>,
        bool,
        bool,
        bool,
        bool,
    );
    let files: Vec<FileRow> = sqlx::query_as(
        "SELECT fs_path, mount_path, content, is_directory, is_based_on_git, is_host_file, is_preview_suffix_enabled \
         FROM local_file_volumes WHERE resource_type = 'App\\Models\\Application' AND resource_id = $1 ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await
    .unwrap();
    type VolRow = (String, String, Option<String>, bool, bool);
    let vols: Vec<VolRow> = sqlx::query_as(
        "SELECT name, mount_path, host_path, is_preview_suffix_enabled, ignores_compose_driver_options \
         FROM local_persistent_volumes WHERE resource_type = 'App\\Models\\Application' AND resource_id = $1 ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await
    .unwrap();
    json!({
        "application": {
            "fqdn": a.0, "domain_port_overrides": a.1, "noindex_domains": a.2,
            "docker_compose_domains": a.3, "docker_compose": a.4, "docker_compose_raw": a.5,
        },
        "environment_variables": envs.into_iter().map(|e| json!({
            "key": e.0,
            "value": e.1.filter(|v| !v.is_empty()).map(|v| enc.decrypt_maybe_serialized(&v).unwrap().trim().to_owned()),
            "is_preview": e.2, "is_required": e.3, "is_shared": e.4, "is_runtime": e.5, "is_buildtime": e.6,
            "is_multiline": e.7, "is_literal": e.8, "is_shown_once": e.9, "preserve_compose_null": e.10,
            "comment": e.11, "version": e.12,
        })).collect::<Vec<_>>(),
        "file_volumes": files.into_iter().map(|f| json!({
            "fs_path": f.0, "mount_path": f.1,
            "content": f.2.map(|c| enc.decrypt_string(&c).unwrap()),
            "is_directory": f.3, "is_based_on_git": f.4, "is_host_file": f.5, "is_preview_suffix_enabled": f.6,
        })).collect::<Vec<_>>(),
        "persistent_volumes": vols.into_iter().map(|v| json!({
            "name": v.0, "mount_path": v.1, "host_path": v.2,
            "is_preview_suffix_enabled": v.3, "ignores_compose_driver_options": v.4,
        })).collect::<Vec<_>>(),
    })
}

fn char_class(v: &str) -> &'static str {
    if v.bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        "hex"
    } else if v.bytes().all(|b| b.is_ascii_alphanumeric()) {
        "alnum"
    } else if v
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        "base64"
    } else {
        "symbols"
    }
}

/// Replaces generated secrets (`SERVICE_PASSWORD_*` and friends) with `<generated:KEY:len:class>`.
fn normalize(step: &Value) -> Value {
    let re =
        regex::Regex::new(r"^SERVICE_(PASSWORD|BASE64|REALBASE64|HEX|USER|LOWERCASEUSER|SUPABASE)")
            .unwrap();
    let mut replacements: Vec<(String, String)> = Vec::new();
    for v in step["environment_variables"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let (Some(k), Some(val)) = (v["key"].as_str(), v["value"].as_str())
            && re.is_match(k)
            && !val.is_empty()
        {
            replacements.push((
                val.to_owned(),
                format!("<generated:{k}:{}:{}>", val.len(), char_class(val)),
            ));
        }
    }
    replacements.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
    fn walk(v: &Value, r: &[(String, String)]) -> Value {
        match v {
            Value::String(s) => {
                let mut s = s.clone();
                for (from, to) in r {
                    s = s.replace(from, to);
                }
                // The test VM's address changes between runs; its host name is what matters. Where the
                // name doesn't resolve (CI), `sslip()` keeps it, as `gethostbyname()` does upstream.
                let ip = regex::Regex::new(
                    r"(\d+\.\d+\.\d+\.\d+|coolify-rs-e2e\.orb\.local)\.sslip\.io",
                )
                .unwrap();
                Value::String(ip.replace_all(&s, "<vm-ip>.sslip.io").into_owned())
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, r)).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, x)| (k.clone(), walk(x, r))).collect())
            }
            other => other.clone(),
        }
    }
    walk(step, &replacements)
}

fn diff_report(name: &str, step: usize, expected: &Value, actual: &Value) -> Option<String> {
    let (e, a) = (normalize(expected), normalize(actual));
    if e == a {
        return None;
    }
    let mut out = format!("case {name}, parse {}:\n", step + 1);
    for key in [
        "error",
        "result",
        "warnings",
        "application",
        "environment_variables",
        "file_volumes",
        "persistent_volumes",
    ] {
        if e.get(key) != a.get(key) {
            out.push_str(&format!(
                "--- {key} expected:\n{}\n+++ {key} actual:\n{}\n",
                serde_json::to_string_pretty(&e.get(key)).unwrap(),
                serde_json::to_string_pretty(&a.get(key)).unwrap()
            ));
        }
    }
    Some(out)
}

thread_local! {
    static LOAD_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// `instant_remote_process()` through `orb` on the test VM (`COMPOSE_PARSER_E2E_VM`).
fn run_on_vm(vm: &str, commands: &[String]) -> Result<Option<String>, String> {
    use std::io::Write as _;
    // Upstream's remote shell: the script on stdin of `bash -se`.
    let mut child = std::process::Command::new("orb")
        .args(["-m", vm, "-u", "root", "bash", "-se"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("orb");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", commands.join("\n")).as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("orb");
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if err.is_empty() {
            format!(
                "SSH command failed with exit code: {}",
                out.status.code().unwrap_or(-1)
            )
        } else {
            err
        })
    }
}

/// A `loadComposeFile()` case: the commands must equal upstream's; their results come from the
/// fixture, or from the test VM when `COMPOSE_PARSER_E2E_VM` names one (and must equal upstream's).
async fn load_case(jobs: &Jobs, app_id: i64, fx: &Value, now: NaiveDateTime) -> Option<String> {
    use coolify_jobs::compose_parser::load;
    let name = fx["name"].as_str().unwrap();
    let mut run = compose_parser::application::Run::load(jobs, app_id, now)
        .await
        .unwrap();
    let plan = match load::plan(jobs, &run, "composeparserloaduuid001").await {
        Ok(p) => p,
        Err(e) => return Some(format!("case {name}: plan failed: {e}")),
    };
    let mut report = String::new();
    if plan.ls_remote != fx["commands"]["ls_remote"].as_str().unwrap() {
        report.push_str(&format!(
            "case {name}: ls-remote command\n  expected {}\n  actual   {}\n",
            fx["commands"]["ls_remote"], plan.ls_remote
        ));
    }
    let expected_read: Vec<String> =
        serde_json::from_value(fx["commands"]["read"].clone()).unwrap();
    if plan.read != expected_read {
        report.push_str(&format!(
            "case {name}: read commands\n  expected {expected_read:#?}\n  actual   {:#?}\n",
            plan.read
        ));
    }
    let recorded = |what: &str| -> Result<Option<String>, String> {
        let r = &fx["remote"][what];
        match r.get("error") {
            Some(e) => Err(e.as_str().unwrap().to_owned()),
            None => Ok(r["output"].as_str().map(str::to_owned)),
        }
    };
    let (ls_remote, read) = match std::env::var("COMPOSE_PARSER_E2E_VM") {
        Ok(vm) => {
            let ls = run_on_vm(&vm, std::slice::from_ref(&plan.ls_remote))
                .map(|_| Some("ok".to_owned()));
            let read = run_on_vm(&vm, &plan.read);
            let _ = run_on_vm(&vm, std::slice::from_ref(&plan.cleanup));
            if ls != recorded("ls_remote") {
                report.push_str(&format!(
                    "case {name}: ls-remote on the VM {ls:?} != recorded\n"
                ));
            }
            if read != recorded("read") {
                report.push_str(&format!(
                    "case {name}: read on the VM {read:?} != recorded\n"
                ));
            }
            (ls, read)
        }
        Err(_) => (recorded("ls_remote"), recorded("read")),
    };
    let read = ls_remote.is_ok().then_some(read);
    if let Err(e) = load::apply(&mut run, &plan, ls_remote.map(|_| ()), read).await {
        let class = match e {
            load::LoadError::Runtime(_) => "RuntimeException",
            load::LoadError::Deployment(_) => "DeploymentException",
        };
        LOAD_ERROR.with(|c| *c.borrow_mut() = Some(format!("{class}: {}", e.message())));
    }
    (!report.is_empty()).then_some(report)
}

#[tokio::test]
async fn application_parser_matches_upstream() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compose_parser/expected");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let only = std::env::var("COMPOSE_CASE").ok();
    let enc = Encrypter::new(APP_KEY).unwrap();
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0;
    for path in files {
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let name = fx["name"].as_str().unwrap().to_owned();
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let tdb = TestDb::new(SEED).await;
        let app_id = seed(&tdb.db, &enc, &fx["context"], &fx["before"]).await;
        let data = tempfile::tempdir().unwrap();
        let remote =
            coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(data.path())).unwrap();
        let jobs = Jobs::new(tdb.db.clone(), remote, Some(enc.clone()));
        jobs.set_version(fx["context"]["version"].as_str().unwrap().to_owned());
        let now = NaiveDateTime::parse_from_str(
            fx["context"]["now"].as_str().unwrap(),
            "%Y-%m-%d %H:%M:%S",
        )
        .unwrap();
        let commit = fx["context"]["commit"].as_str();
        if fx["mode"] == "load" {
            if let Some(report) = load_case(&jobs, app_id, &fx, now).await {
                failures.push(report);
            }
            let st = state(&tdb.db, &enc, app_id).await;
            let expected = &fx["steps"][0];
            let mut actual = st;
            actual["loaded"] = expected.get("loaded").cloned().unwrap_or(Value::Null);
            if let Some(e) = LOAD_ERROR.with(|c| c.borrow_mut().take()) {
                actual["error"] = Value::String(e);
                actual.as_object_mut().unwrap().remove("loaded");
            } else if expected.get("loaded").is_none() {
                actual.as_object_mut().unwrap().remove("loaded");
            }
            compared += 1;
            if let Some(report) = diff_report(&name, 0, expected, &actual) {
                failures.push(report);
            }
            tdb.drop_db().await;
            continue;
        }
        if fx["mode"] == "raw" {
            let raw = fx["context"]["application"]["docker_compose_raw"]
                .as_str()
                .unwrap_or_default();
            let uuid = fx["context"]["application"]["uuid"].as_str().unwrap();
            let actual = match compose_parser::raw::raw_compose(raw, uuid) {
                Ok((compose, _)) => json!({"raw": compose}),
                Err(e) => json!({"error": e}),
            };
            compared += 1;
            if actual != fx["steps"][0] {
                failures.push(format!(
                    "case {name}: expected {}\nactual {}",
                    fx["steps"][0], actual
                ));
            }
            tdb.drop_db().await;
            continue;
        }
        for (i, expected) in fx["steps"].as_array().unwrap().iter().enumerate() {
            let mut actual =
                match compose_parser::application_parser_at(&jobs, app_id, 0, commit, now).await {
                    Ok(parsed) => json!({"result": parsed.to_yaml(2), "warnings": parsed.warnings}),
                    Err(e) => json!({"error": e}),
                };
            let st = state(&tdb.db, &enc, app_id).await;
            for (k, v) in st.as_object().unwrap() {
                actual[k] = v.clone();
            }
            compared += 1;
            if let Ok(dir) = std::env::var("COMPOSE_PARSER_DUMP") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    format!("{dir}/{name}-{i}.json"),
                    serde_json::to_string_pretty(
                        &json!({"expected": normalize(expected), "actual": normalize(&actual)}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            if let Some(report) = diff_report(&name, i, expected, &actual) {
                failures.push(report);
            }
        }
        tdb.drop_db().await;
    }
    assert!(compared > 0, "no compose parser fixtures");
    assert!(
        failures.is_empty(),
        "{} of {compared} parses differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Seeds the `reference` fixture: the database, the jobs, the application id and `now`.
async fn seed_reference(enc: &Encrypter) -> (TestDb, Jobs, i64, NaiveDateTime) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compose_parser/expected/reference.json");
    let fx: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let tdb = TestDb::new(SEED).await;
    let app_id = seed(&tdb.db, enc, &fx["context"], &fx["before"]).await;
    let data = tempfile::tempdir().unwrap();
    let remote =
        coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(data.path())).unwrap();
    let jobs = Jobs::new(tdb.db.clone(), remote, Some(enc.clone()));
    jobs.set_version(fx["context"]["version"].as_str().unwrap().to_owned());
    let now =
        NaiveDateTime::parse_from_str(fx["context"]["now"].as_str().unwrap(), "%Y-%m-%d %H:%M:%S")
            .unwrap();
    (tdb, jobs, app_id, now)
}

type Variables = Vec<(String, Option<String>)>;

/// The application's production or preview variables: `(key, decrypted value)` by id.
async fn variables(db: &coolify_db::Db, enc: &Encrypter, app_id: i64, preview: bool) -> Variables {
    let rows: Variables = sqlx::query_as(
        "SELECT key, value FROM environment_variables WHERE resourceable_type = 'App\\Models\\Application' \
         AND resourceable_id = $1 AND is_preview = $2 ORDER BY id",
    )
    .bind(app_id)
    .bind(preview)
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(k, v)| (k, v.map(|v| enc.decrypt_maybe_serialized(&v).unwrap())))
        .collect()
}

fn value_of(rows: &Variables, key: &str) -> Option<Option<String>> {
    rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

async fn set_value(
    db: &coolify_db::Db,
    enc: &Encrypter,
    (app_id, preview): (i64, bool),
    key: &str,
    value: &str,
) {
    let done = sqlx::query(
        "UPDATE environment_variables SET value = $1 WHERE resourceable_type = 'App\\Models\\Application' \
         AND resourceable_id = $2 AND key = $3 AND is_preview = $4",
    )
    .bind(enc.encrypt_serialized(value))
    .bind(app_id)
    .bind(key)
    .bind(preview)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(done.rows_affected(), 1, "{key} (preview: {preview})");
}

/// The value of `services.<service>.environment.<key>` in the parsed compose.
fn service_env(parsed: &compose_parser::ParsedCompose, service: &str, key: &str) -> Option<String> {
    match compose_parser::php::data_get(
        &parsed.top_level,
        &format!("services.{service}.environment.{key}"),
    ) {
        Some(coolify_compat::yaml::PhpValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

const PR: i64 = 7;
const GENERATED_KEYS: [&str; 7] = [
    "SERVICE_PASSWORD_WEB",
    "SERVICE_BASE64_64_WEB",
    "SERVICE_HEX_32_WEB",
    "SERVICE_USER_WEB",
    "SERVICE_USER_DB",
    "SERVICE_PASSWORD_DB",
    "SERVICE_PASSWORD_NOBRACES",
];

/// 1a2a5362a: a pull request parse keeps the generated magic variables in the preview variables
/// and stores no production variables for them or for the service domains; the `created` hook
/// does not copy the production secrets of a compose application to the preview.
#[tokio::test]
async fn pull_request_parse_generates_preview_magic_variables() {
    let enc = Encrypter::new(APP_KEY).unwrap();
    let (tdb, jobs, app_id, now) = seed_reference(&enc).await;

    // A pull request parse first: no production variables for magic variables.
    compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    let production = variables(&tdb.db, &enc, app_id, false).await;
    let preview = variables(&tdb.db, &enc, app_id, true).await;
    for key in GENERATED_KEYS {
        assert_eq!(value_of(&production, key), None, "production {key}");
        let value = value_of(&preview, key).flatten();
        assert!(value.is_some_and(|v| !v.is_empty()), "preview {key}");
    }
    // The domains of `SERVICE_FQDN_API: /api`, `SERVICE_FQDN_ADMIN_8080` and the magic
    // references. (A bare `SERVICE_URL_MY_APP_80` is a pass-through variable upstream as well.)
    for key in [
        "SERVICE_FQDN_API",
        "SERVICE_URL_API",
        "SERVICE_FQDN_ADMIN",
        "SERVICE_FQDN_WEB",
        "SERVICE_URL_WEB",
        "SERVICE_FQDN_WEB_3000",
        "SERVICE_URL_WEB_3000",
    ] {
        assert_eq!(value_of(&production, key), None, "production {key}");
    }

    // The production parse generates its own secrets; the hook leaves the preview ones alone.
    compose_parser::application_parser_at(&jobs, app_id, 0, None, now)
        .await
        .unwrap();
    let production = variables(&tdb.db, &enc, app_id, false).await;
    let preview_after = variables(&tdb.db, &enc, app_id, true).await;
    for key in GENERATED_KEYS {
        let prod = value_of(&production, key).flatten().unwrap();
        let prev = value_of(&preview_after, key).flatten().unwrap();
        assert_ne!(prod, prev, "{key}");
        assert_eq!(value_of(&preview, key).flatten().unwrap(), prev, "{key}");
        assert_eq!(
            preview_after.iter().filter(|(k, _)| k == key).count(),
            1,
            "{key}"
        );
    }

    // Another pull request parse changes no production variable.
    compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    assert_eq!(variables(&tdb.db, &enc, app_id, false).await, production);
    tdb.drop_db().await;
}

/// A production parse of a compose application: the generated magic variables get no preview
/// copy, the other variables do.
#[tokio::test]
async fn production_parse_does_not_copy_generated_magic_variables_to_preview() {
    let enc = Encrypter::new(APP_KEY).unwrap();
    let (tdb, jobs, app_id, now) = seed_reference(&enc).await;
    compose_parser::application_parser_at(&jobs, app_id, 0, None, now)
        .await
        .unwrap();
    let production = variables(&tdb.db, &enc, app_id, false).await;
    let preview = variables(&tdb.db, &enc, app_id, true).await;
    for key in GENERATED_KEYS {
        assert!(value_of(&production, key).is_some(), "production {key}");
        assert_eq!(value_of(&preview, key), None, "preview {key}");
    }
    assert!(value_of(&preview, "BARE_PASSTHROUGH").is_some());
    assert!(value_of(&preview, "SERVICE_URL_WEB").is_some());
    tdb.drop_db().await;
}

/// The final environment of a pull request parse: an empty value reads the preview variable; a
/// public preview ignores preview variables with the production value, gets its own secrets and
/// does not resolve `{{...}}` written in the compose file.
#[tokio::test]
async fn public_pull_request_parse_does_not_use_production_values() {
    let enc = Encrypter::new(APP_KEY).unwrap();
    let (tdb, jobs, app_id, now) = seed_reference(&enc).await;
    let (db, prod, prev) = (&tdb.db, (app_id, false), (app_id, true));
    compose_parser::application_parser_at(&jobs, app_id, 0, None, now)
        .await
        .unwrap();
    set_value(db, &enc, prod, "BARE_PASSTHROUGH", "prod-value").await;
    set_value(db, &enc, prev, "BARE_PASSTHROUGH", "preview-value").await;

    let parsed = compose_parser::application_parser_at(&jobs, app_id, 0, None, now)
        .await
        .unwrap();
    assert_eq!(
        service_env(&parsed, "web", "BARE_PASSTHROUGH").as_deref(),
        Some("prod-value")
    );
    let parsed = compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    assert_eq!(
        service_env(&parsed, "web-pr-7", "BARE_PASSTHROUGH").as_deref(),
        Some("preview-value")
    );
    assert_eq!(
        service_env(&parsed, "web-pr-7", "SHARED").as_deref(),
        Some("shared-value")
    );

    // A private preview keeps a preview secret equal to the production one.
    let production = variables(db, &enc, app_id, false).await;
    let prod_password = value_of(&production, "SERVICE_PASSWORD_DB")
        .flatten()
        .unwrap();
    set_value(db, &enc, prev, "SERVICE_PASSWORD_DB", &prod_password).await;
    compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    let preview = variables(db, &enc, app_id, true).await;
    assert_eq!(
        value_of(&preview, "SERVICE_PASSWORD_DB").flatten(),
        Some(prod_password.clone())
    );

    sqlx::query(
        "UPDATE application_settings SET is_pr_deployments_public_enabled = true WHERE application_id = $1",
    )
    .bind(app_id)
    .execute(db.pool())
    .await
    .unwrap();
    set_value(db, &enc, prev, "BARE_PASSTHROUGH", "prod-value").await;
    let parsed = compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    assert_eq!(
        service_env(&parsed, "web-pr-7", "BARE_PASSTHROUGH").as_deref(),
        Some("")
    );
    assert_eq!(
        service_env(&parsed, "web-pr-7", "SHARED").as_deref(),
        Some("{{environment.SHARED_KEY}}")
    );
    let preview = variables(db, &enc, app_id, true).await;
    let regenerated = value_of(&preview, "SERVICE_PASSWORD_DB").flatten().unwrap();
    assert_ne!(regenerated, prod_password);
    assert_eq!(regenerated.len(), prod_password.len());
    assert_eq!(variables(db, &enc, app_id, false).await, production);

    // A preview value of its own is used, and `{{...}}` in it is resolved.
    set_value(
        db,
        &enc,
        prev,
        "BARE_PASSTHROUGH",
        "{{environment.SHARED_KEY}}",
    )
    .await;
    let parsed = compose_parser::application_parser_at(&jobs, app_id, PR, None, now)
        .await
        .unwrap();
    assert_eq!(
        service_env(&parsed, "web-pr-7", "BARE_PASSTHROUGH").as_deref(),
        Some("shared-value")
    );
    tdb.drop_db().await;
}
