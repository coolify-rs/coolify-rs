//! The coolify-rs side of `compat/diff/notifications_e2e.py`: delivers cases for real. Reads the
//! cases (argument: a JSON file), `DATABASE_URL` and `APP_KEY` from the environment, and posts a
//! marker to the listener before each case.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use coolify_compat::Encrypter;
use coolify_db::{Db, DbConfig};
use coolify_jobs::notifications::{self, Mail, Notification};
use serde_json::{Map, Value, json};

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: notifications_send <cases.json>");
    let text = std::fs::read_to_string(path).expect("read the cases file");
    let input: Value = serde_json::from_str(&text).expect("parse the cases file");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let db = Db::connect(&DbConfig {
        url,
        max_connections: 4,
        acquire_timeout: Duration::from_secs(10),
        allow_unknown_schema: true,
    })
    .await
    .expect("connect");
    let key = std::env::var("APP_KEY").expect("APP_KEY");
    let encrypter = Encrypter::new(&key).expect("APP_KEY");
    let listener = input["listener"].as_str().expect("listener").to_owned();
    let team_id = input["team_id"].as_i64().expect("team_id");
    let client = reqwest::Client::new();
    let mut out = Map::new();
    for case in input["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        client
            .post(format!("{listener}/__case/{name}"))
            .body("[]")
            .send()
            .await
            .expect("marker");
        let result = if let Some(n) = case.get("notification") {
            let n: Notification =
                serde_json::from_value(n.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            notifications::deliver(&db, Some(&encrypter), team_id, n).await;
            "ok".to_owned()
        } else if let Some(m) = case.get("mail") {
            let m: Mail =
                serde_json::from_value(m.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let to = case["to"].as_str().expect("to");
            match notifications::send_transactional(&db, Some(&encrypter), to, m).await {
                Ok(()) => "ok".to_owned(),
                Err(e) => e,
            }
        } else {
            let t = &case["test"];
            let channel = t["channel"].as_str().expect("channel");
            let emails = t["emails"].as_str().map(str::to_owned);
            match notifications::notify_test(&db, Some(&encrypter), team_id, channel, emails).await
            {
                Ok(()) => "ok".to_owned(),
                Err(e) => e,
            }
        };
        out.insert(name.to_owned(), json!(result));
        // Detached work of `TracksDeliveryFailures` (general notifications).
        if case["settle"].as_bool().unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    println!("{}", Value::Object(out));
}
