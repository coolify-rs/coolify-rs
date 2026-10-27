//! Connection URLs of a database (`internal_db_url` / `external_db_url`) and the
//! credentials they use.

use std::collections::HashMap;
use std::sync::LazyLock;

use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::databases::{DatabaseRow, Kind};
use regex::Regex;

use super::env::{self, Var};

/// The decrypted credentials of a database.
pub struct Credentials {
    pub secrets: HashMap<&'static str, String>,
    pub variables: Vec<Var>,
    /// `isCredentialLocked()`: the password variable is shown once or reads a shown-once shared variable.
    pub locked: bool,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("locked", &self.locked)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn secret(&self, column: &str) -> String {
        self.secrets.get(column).cloned().unwrap_or_default()
    }

    fn variable(&self, key: &str) -> Option<&Var> {
        self.variables.iter().find(|v| v.key == key)
    }

    /// `connectionPassword()` of Redis, KeyDB and Dragonfly: the resolved password variable
    /// (unless the database keeps its legacy server password), else the stored password.
    pub fn connection_password(&self, d: &DatabaseRow) -> String {
        let legacy = d.bool("legacy_password_quoting");
        if !legacy
            && let Some(v) = self.variable("REDIS_PASSWORD")
            && let Some(p) = v.resolved.as_deref().filter(|p| !p.is_empty())
        {
            return p.to_owned();
        }
        match d.kind {
            Kind::Redis => self
                .variable("REDIS_PASSWORD")
                .and_then(|v| v.value.clone())
                .unwrap_or_default(),
            Kind::Keydb => self.secret("keydb_password"),
            Kind::Dragonfly => self.secret("dragonfly_password"),
            _ => String::new(),
        }
    }

    /// The `redis_username` accessor.
    pub fn redis_username(&self) -> String {
        self.variable("REDIS_USERNAME")
            .and_then(|v| v.value.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "default".into())
    }
}

static SHARED_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*(\w+)\.([^}\s]+)\s*\}\}").unwrap_or_else(|_| unreachable!())
});

/// Loads and decrypts the credentials of a database.
pub async fn credentials(db: &Db, enc: &Encrypter, d: &DatabaseRow) -> Result<Credentials, String> {
    let mut secrets = HashMap::new();
    for column in d.kind.encrypted_columns() {
        if let Some(cipher) = d.str(column) {
            secrets.insert(
                *column,
                enc.decrypt_string(cipher)
                    .map_err(|_| "The MAC is invalid.".to_owned())?,
            );
        }
    }
    let variables = match env::load(db, enc, d, d.server_id()).await {
        Ok(v) => v,
        Err(e) if e == env::SECRET_MANAGER_NOT_PORTED => env::load_stored(db, enc, d).await?,
        Err(e) => return Err(e),
    };
    let mut locked = false;
    // Redis, KeyDB and Dragonfly read their server password from REDIS_PASSWORD.
    let password_key =
        matches!(d.kind, Kind::Redis | Kind::Keydb | Kind::Dragonfly).then_some("REDIS_PASSWORD");
    // KeyDB and Dragonfly look at `serverPasswordEnvironmentVariable()`, which legacy databases do not have.
    let legacy_ignored = d.kind != Kind::Redis && d.bool("legacy_password_quoting");
    if let Some(v) = password_key
        .filter(|_| !legacy_ignored)
        .and_then(|k| variables.iter().find(|v| v.key == k))
    {
        let references: Vec<(String, String)> = SHARED_REFERENCE
            .captures_iter(v.value.as_deref().unwrap_or_default())
            .map(|c| (c[1].to_owned(), c[2].to_owned()))
            .collect();
        locked = v.is_shown_once
            || (!references.is_empty()
                && coolify_db::environment_variables::references_locked_shared(
                    db,
                    d.team_id,
                    &references,
                )
                .await
                .map_err(|e| e.to_string())?);
    }
    Ok(Credentials {
        secrets,
        variables,
        locked,
    })
}

/// PHP `rawurlencode`.
pub fn rawurlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// PHP `version_compare($v, '6.0', '>=')` for the image tags Redis uses.
fn version_at_least_6(v: &str) -> bool {
    // version_compare canonicalizes `-`, `_`, `+` to `.` and splits letters from digits; a tag
    // like `7.2` or `7-alpine` compares by its leading number.
    let first: String = v.chars().take_while(char::is_ascii_digit).collect();
    match first.parse::<u64>() {
        Ok(n) => n >= 6,
        Err(_) => false,
    }
}

/// `formatHostForUrl`.
fn host_for_url(ip: &str) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]")
    } else {
        ip.to_owned()
    }
}

/// The URL for `host:port`, as the model's accessors build it (SSL is not ported).
fn url(d: &DatabaseRow, c: &Credentials, host: &str, port: i64) -> String {
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    let enc = rawurlencode;
    match d.kind {
        Kind::Postgresql => format!(
            "postgres://{}:{}@{host}:{port}/{}",
            enc(&s("postgres_user")),
            enc(&c.secret("postgres_password")),
            s("postgres_db")
        ),
        Kind::Redis => {
            let image = s("image");
            let version = image.split(':').nth(1).unwrap_or("0.0");
            let user = if version_at_least_6(version) {
                format!("{}:", enc(&c.redis_username()))
            } else {
                String::new()
            };
            format!(
                "redis://{user}{}@{host}:{port}/0",
                enc(&c.connection_password(d))
            )
        }
        Kind::Keydb | Kind::Dragonfly => format!(
            "redis://:{}@{host}:{port}/0",
            enc(&c.connection_password(d))
        ),
        Kind::Mongodb => format!(
            "mongodb://{}:{}@{host}:{port}/?directConnection=true",
            enc(&s("mongo_initdb_root_username")),
            enc(&c.secret("mongo_initdb_root_password"))
        ),
        Kind::Mysql => format!(
            "mysql://{}:{}@{host}:{port}/{}",
            enc(&s("mysql_user")),
            enc(&c.secret("mysql_password")),
            s("mysql_database")
        ),
        Kind::Mariadb => format!(
            "mysql://{}:{}@{host}:{port}/{}",
            enc(&s("mariadb_user")),
            enc(&c.secret("mariadb_password")),
            s("mariadb_database")
        ),
        Kind::Clickhouse => format!(
            "clickhouse://{}:{}@{host}:{port}/{}",
            enc(&s("clickhouse_admin_user")),
            enc(&c.secret("clickhouse_admin_password")),
            d.str("clickhouse_db").unwrap_or("default")
        ),
        Kind::Sqlite => String::new(),
    }
}

/// The container port each kind listens on.
pub fn internal_port(kind: Kind) -> i64 {
    match kind {
        Kind::Postgresql => 5432,
        Kind::Redis | Kind::Keydb | Kind::Dragonfly => 6379,
        Kind::Mongodb => 27017,
        Kind::Mysql | Kind::Mariadb => 3306,
        Kind::Clickhouse => 9000,
        Kind::Sqlite => 0,
    }
}

/// `internal_db_url`.
pub fn internal_url(d: &DatabaseRow, c: &Credentials) -> String {
    url(d, c, &d.uuid, internal_port(d.kind))
}

/// `external_db_url`: with a public port and a server IP.
pub fn external_url(d: &DatabaseRow, c: &Credentials, server_ip: Option<&str>) -> Option<String> {
    let port = d
        .int("public_port")
        .filter(|p| *p != 0 && d.bool("is_public"))?;
    let ip = server_ip.filter(|ip| !ip.is_empty())?;
    Some(url(d, c, &host_for_url(ip), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_versions() {
        assert!(version_at_least_6("7.2"));
        assert!(version_at_least_6("6.0"));
        assert!(!version_at_least_6("5.0.7"));
        assert!(!version_at_least_6("0.0"));
        assert!(!version_at_least_6("latest"));
        assert_eq!(rawurlencode("a b/c"), "a%20b%2Fc");
    }
}
