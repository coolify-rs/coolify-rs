//! `serviceParser()` against what upstream recorded for the same inputs
//! (tests/fixtures/service_parser: `harness.php` ran each case on the reference instance inside a
//! rolled-back transaction). Each case seeds a fresh database with the recorded rows, runs the
//! steps and compares the returned compose, the service columns, the parts, the variables and the
//! storages. Generated secrets are compared by key, length and character class.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::NaiveDateTime;
use coolify_compat::Encrypter;
use coolify_db::services::{self as svc, SubKind};
use coolify_db::testing::TestDb;
use coolify_jobs::Jobs;
use coolify_jobs::compose_parser::service_parser::{self, ServiceRun};
use serde_json::{Value, json};

/// A test key (never a real instance's key).
const APP_KEY: &str = "base64:JgOyH5GGzVhKpVXBi9VwoVxVN2T8j3tfdGFmFzT0g5E=";

const SEED: &str = "INSERT INTO instance_settings (id) VALUES (0);";

async fn insert(pool: &sqlx::PgPool, table: &str, row: &Value) {
    // The harness reads json columns as text; they are written back with that exact text.
    let json_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_name = $1 AND data_type IN ('json', 'jsonb')",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut row = row.clone();
    let mut texts: Vec<(String, String)> = Vec::new();
    for c in json_columns {
        if let Some(Value::String(text)) = row.get(&c).cloned() {
            row[&c] = Value::Null;
            texts.push((c, text));
        }
    }
    sqlx::query(&format!(
        "INSERT INTO {table} SELECT * FROM json_populate_record(NULL::{table}, $1::json)"
    ))
    .bind(row.to_string())
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seeding {table}: {e}"));
    for (c, text) in texts {
        sqlx::query(&format!(
            "UPDATE {table} SET \"{c}\" = $1::json WHERE id = $2"
        ))
        .bind(text)
        .bind(row["id"].as_i64())
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seeding {table}.{c}: {e}"));
    }
}

async fn seed(pool: &sqlx::PgPool, ctx: &Value) -> i64 {
    insert(pool, "teams", &ctx["team"]).await;
    let team = ctx["team"]["id"].as_i64().unwrap();
    sqlx::query("INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES ($1, 'pk', 'k', 'x', $2)")
        .bind(ctx["server"]["private_key_id"].as_i64().unwrap())
        .bind(team)
        .execute(pool)
        .await
        .unwrap();
    insert(pool, "projects", &ctx["project"]).await;
    insert(pool, "environments", &ctx["environment"]).await;
    insert(pool, "servers", &ctx["server"]).await;
    insert(pool, "server_settings", &ctx["server_settings"]).await;
    insert(pool, "standalone_dockers", &ctx["standalone_docker"]).await;
    insert(pool, "services", &ctx["service"]).await;
    ctx["service"]["id"].as_i64().unwrap()
}

async fn parts(pool: &sqlx::PgPool, enc: &Encrypter, table: &str, morph: &str, sid: i64) -> Value {
    let rows: Vec<(String,)> = sqlx::query_as(&format!(
        "SELECT row_to_json(p)::text FROM {table} p WHERE p.service_id = $1 ORDER BY p.id"
    ))
    .bind(sid)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out = Vec::new();
    for (text,) in rows {
        let mut p: Value = serde_json::from_str(&text).unwrap();
        let id = p["id"].as_i64().unwrap();
        let map = p.as_object_mut().unwrap();
        for k in [
            "id",
            "uuid",
            "service_id",
            "created_at",
            "updated_at",
            "last_online_at",
        ] {
            map.remove(k);
        }
        type F = (String, Option<String>, Option<String>, bool, bool);
        let files: Vec<F> = sqlx::query_as(
            "SELECT fs_path, mount_path, content, is_directory, is_host_file FROM local_file_volumes \
             WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
        )
        .bind(morph)
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap();
        map.insert(
            "file_volumes".into(),
            json!(files
                .into_iter()
                .map(|f| json!({
                    "fs_path": f.0, "mount_path": f.1,
                    "content": f.2.map(|c| enc.decrypt_string(&c).unwrap()),
                    "is_directory": f.3, "is_host_file": f.4,
                }))
                .collect::<Vec<_>>()),
        );
        let vols: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, mount_path FROM local_persistent_volumes WHERE resource_type = $1 AND resource_id = $2 ORDER BY id",
        )
        .bind(morph)
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap();
        map.insert(
            "persistent_volumes".into(),
            json!(vols
                .into_iter()
                .map(|v| json!({"name": v.0, "mount_path": v.1}))
                .collect::<Vec<_>>()),
        );
        out.push(p);
    }
    Value::Array(out)
}

async fn state(pool: &sqlx::PgPool, enc: &Encrypter, sid: i64) -> Value {
    let s: (Option<String>, String) =
        sqlx::query_as("SELECT docker_compose, docker_compose_raw FROM services WHERE id = $1")
            .bind(sid)
            .fetch_one(pool)
            .await
            .unwrap();
    type E = (String, Option<String>, bool, bool, Option<String>);
    let envs: Vec<E> = sqlx::query_as(
        "SELECT key, value, is_required, is_shared, comment FROM environment_variables \
         WHERE resourceable_type = $1 AND resourceable_id = $2 ORDER BY id",
    )
    .bind(svc::MORPH)
    .bind(sid)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut part_uuids = serde_json::Map::new();
    for table in ["service_applications", "service_databases"] {
        let rows: Vec<(String, String)> = sqlx::query_as(&format!(
            "SELECT name, uuid FROM {table} WHERE service_id = $1"
        ))
        .bind(sid)
        .fetch_all(pool)
        .await
        .unwrap();
        for (name, uuid) in rows {
            part_uuids.insert(name, json!(uuid));
        }
    }
    json!({
        "part_uuids": part_uuids,
        "docker_compose": s.0,
        "docker_compose_raw": s.1,
        "applications": parts(pool, enc, "service_applications", svc::APPLICATION_MORPH, sid).await,
        "databases": parts(pool, enc, "service_databases", svc::DATABASE_MORPH, sid).await,
        "environment_variables": envs.into_iter().map(|e| json!({
            "key": e.0,
            "value": e.1.filter(|v| !v.is_empty()).map(|v| enc.decrypt_maybe_serialized(&v).unwrap().trim().to_owned()),
            "is_required": e.2, "is_shared": e.3, "comment": e.4,
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

/// Replaces generated secrets with `<generated:KEY:len:class>`, also where a value repeats them.
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
            // Supabase tokens are JWTs of varying length: only the key counts.
            let text = if k.contains("SUPABASE") {
                format!("<generated:{k}>")
            } else {
                format!("<generated:{k}:{}:{}>", val.len(), char_class(val))
            };
            replacements.push((val.to_owned(), text));
        }
    }
    // Part uuids are random on each run.
    for (name, uuid) in step["part_uuids"].as_object().into_iter().flatten() {
        replacements.push((uuid.as_str().unwrap().to_owned(), format!("<part:{name}>")));
    }
    replacements.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
    fn walk(v: &Value, r: &[(String, String)]) -> Value {
        match v {
            Value::String(s) => {
                let mut s = s.clone();
                for (from, to) in r {
                    s = s.replace(from, to);
                }
                Value::String(s)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, r)).collect()),
            Value::Object(o) => {
                // The harness prints json columns as the text Postgres stores.
                let parsed = |k: &str, x: &Value| match x {
                    Value::String(s)
                        if ["domain_port_overrides", "noindex_domains", "domain_dns_statuses"]
                            .contains(&k) =>
                    {
                        serde_json::from_str(s).unwrap_or_else(|_| x.clone())
                    }
                    other => other.clone(),
                };
                Value::Object(
                    o.iter()
                        .map(|(k, x)| (k.clone(), walk(&parsed(k, x), r)))
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }
    walk(step, &replacements)
}

fn report(name: &str, i: usize, expected: &Value, actual: &Value) -> Option<String> {
    let (e, a) = (normalize(expected), normalize(actual));
    let mut out = String::new();
    for key in [
        "error",
        "result",
        "warnings",
        "docker_compose",
        "docker_compose_raw",
        "applications",
        "databases",
        "environment_variables",
    ] {
        let (x, y) = (e.get(key), a.get(key));
        if x != y {
            if let (Some(Value::String(x)), Some(Value::String(y))) = (x, y) {
                let first = x.lines().zip(y.lines()).position(|(p, q)| p != q);
                out.push_str(&format!(
                    "--- {key}: first differing line {first:?}\n  expected {:?}\n  actual   {:?}\n",
                    first.and_then(|l| x.lines().nth(l)),
                    first.and_then(|l| y.lines().nth(l)),
                ));
            } else {
                let text = |v: Option<&Value>| {
                    let t = serde_json::to_string(&v).unwrap();
                    t.chars().take(2500).collect::<String>()
                };
                out.push_str(&format!(
                    "--- {key}\n  expected {}\n  actual   {}\n",
                    text(x),
                    text(y)
                ));
            }
        }
    }
    (!out.is_empty()).then(|| format!("case {name}, step {}:\n{out}", i + 1))
}

#[tokio::test]
async fn service_parser_matches_upstream() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/service_parser/expected");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let only = std::env::var("SERVICE_CASE").ok();
    let enc = Encrypter::new(APP_KEY).unwrap();
    let now = NaiveDateTime::parse_from_str("2026-01-02 03:04:05", "%Y-%m-%d %H:%M:%S").unwrap();
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0;
    for path in files {
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let name = fx["name"].as_str().unwrap().to_owned();
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let tdb = TestDb::new(SEED).await;
        let pool = tdb.db.pool().clone();
        let sid = seed(&pool, &fx["context"]).await;
        let data = tempfile::tempdir().unwrap();
        let remote =
            coolify_remote::Remote::new(coolify_remote::RemoteConfig::new(data.path())).unwrap();
        let jobs = Jobs::new(tdb.db.clone(), remote, Some(enc.clone()));
        jobs.set_version(fx["context"]["version"].as_str().unwrap().to_owned());
        for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
            let mut actual = json!({"action": step["action"]});
            if step["action"] == json!("parse") {
                let mut run = ServiceRun::load(&jobs, sid, now).await.unwrap();
                match service_parser::service_parser(&mut run).await {
                    Ok(p) => {
                        actual["result"] = json!(p.to_yaml(2));
                        actual["warnings"] = json!(p.warnings);
                    }
                    Err(e) => actual["error"] = json!(e),
                }
            } else {
                let part = step["action"]["name"].as_str().unwrap();
                let url = step["action"]["url"].as_str().unwrap();
                let row = svc::sub_by_name(&tdb.db, SubKind::Application, sid, part)
                    .await
                    .unwrap()
                    .unwrap();
                let run = ServiceRun::load(&jobs, sid, now).await.unwrap();
                run.save_part_fqdn(&row, Some(url.to_owned())).await.unwrap();
                drop(run);
                if let Err(e) =
                    service_parser::update_compose(&jobs, sid, SubKind::Application, part, now)
                        .await
                {
                    actual["error"] = json!(e);
                }
            }
            let st = state(&pool, &enc, sid).await;
            for (k, v) in st.as_object().unwrap() {
                actual[k] = v.clone();
            }
            compared += 1;
            if let Some(r) = report(&name, i, step, &actual) {
                failures.push(r);
            }
        }
        tdb.drop_db().await;
    }
    assert!(compared > 0, "no case compared");
    assert!(
        failures.is_empty(),
        "{} of {compared} steps differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
