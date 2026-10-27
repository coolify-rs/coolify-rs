//! Prints the Docker Compose file coolify-rs would write for a database:
//! `DATABASE_URL=... APP_KEY=... cargo run -p coolify-jobs --example database_compose -- <team id> <uuid>`.
//! Used to compare with the file upstream writes on a server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use coolify_compat::Encrypter;
use coolify_db::databases as dbs;
use coolify_db::{Db, DbConfig, servers};
use coolify_jobs::databases::{PostgresStart, postgres_start_commands};
use coolify_jobs::server::ServerInfo;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let team: i64 = args[1].parse().unwrap();
    let uuid = &args[2];
    let db = Db::connect(&DbConfig {
        url: std::env::var("DATABASE_URL").unwrap(),
        max_connections: 2,
        acquire_timeout: Duration::from_secs(5),
        allow_unknown_schema: false,
    })
    .await
    .unwrap();
    let enc = Encrypter::new(&std::env::var("APP_KEY").unwrap()).unwrap();
    let d = dbs::find_in_team(&db, team, uuid)
        .await
        .unwrap()
        .expect("database");
    let server = ServerInfo::from_row(
        &servers::find_by_id(&db, d.server_id().unwrap())
            .await
            .unwrap()
            .unwrap(),
    );
    let password = enc
        .decrypt_string(d.str("postgres_password").unwrap())
        .unwrap();
    let network = d.destination.as_ref().unwrap()["network"]
        .as_str()
        .unwrap()
        .to_owned();
    let volumes = dbs::persistent_volumes(&db, d.kind, d.id).await.unwrap();
    let start = PostgresStart {
        db: &d,
        password,
        server: &server,
        network,
        volumes,
        now: chrono::Utc::now().naive_utc(),
    };
    let (commands, compose) = postgres_start_commands(&start);
    if args.get(3).map(String::as_str) == Some("--commands") {
        for c in commands {
            println!("{c}");
        }
    } else {
        print!("{compose}");
    }
}
