//! Prints what coolify-rs computes for an application, to compare with upstream:
//! `DATABASE_URL=... APP_KEY=... cargo run -p coolify-jobs --example application -- <team id> <uuid> [--hash|--labels]`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use coolify_compat::Encrypter;
use coolify_db::{Db, DbConfig, applications as apps, servers};
use coolify_jobs::applications::{labels, snapshot};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let team: i64 = args[1].parse().unwrap();
    let db = Db::connect(&DbConfig {
        url: std::env::var("DATABASE_URL").unwrap(),
        max_connections: 2,
        acquire_timeout: Duration::from_secs(5),
        allow_unknown_schema: false,
    })
    .await
    .unwrap();
    let enc = Encrypter::new(&std::env::var("APP_KEY").unwrap()).unwrap();
    let a = apps::find_in_team(&db, team, &args[2])
        .await
        .unwrap()
        .expect("application");
    match args.get(3).map(String::as_str) {
        Some("--hash") => println!("{}", snapshot::config_hash(&db, &enc, &a).await.unwrap()),
        _ => {
            let server = servers::find_by_id(&db, a.server_id().unwrap())
                .await
                .unwrap()
                .unwrap();
            println!(
                "{}",
                labels::application(&labels::LabelInput::new(&a, &server)).join("\n")
            );
        }
    }
}
