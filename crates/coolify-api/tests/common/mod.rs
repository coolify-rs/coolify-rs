//! Shared harness for API contract tests: a fresh database with the upstream
//! baseline schema (upstream main @ b5823c7), a small fixture, and helpers to call the router in-process.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use coolify_api::AppState;
use coolify_api::ip::TrustedProxies;
use coolify_api::state::Options;
use coolify_compat::Encrypter;
use coolify_compat::hash_token;
use coolify_db::testing::TestDb;
use http_body_util::BodyExt;
use tower::ServiceExt;

pub const ACME: i64 = 1;
/// Throwaway test key (same as the encrypter fixture).
pub const APP_KEY: &str = "base64:hPAFGDXpvHW0ZnwoP/YKkC1WJ+9Bv2jZCwc1hGGaI3s=";
/// Password of every fixture user (bcrypt cost 4 hash in SEED).
pub const PASSWORD: &str = "secret-password";
pub const VERSION: &str = "4.5.1-dev.b5823c777";
pub const OTHER: i64 = 2;

/// Fixture mirroring compat/reference/seed-fixtures.php.
const SEED: &str = r#"
INSERT INTO instance_settings (id, is_api_enabled) VALUES (0, true);
INSERT INTO teams (id, name, description, personal_team, show_boarding, created_at, updated_at) VALUES
  (0, 'Root Team', 'The root team', true, true, '2026-01-01 00:00:00', '2026-01-01 00:00:00'),
  (1, 'Acme', 'Fixture team', false, false, '2026-01-02 00:00:00', '2026-01-02 00:00:00'),
  (2, 'Other', NULL, true, false, '2026-01-03 00:00:00', '2026-01-03 00:00:00');
INSERT INTO users (id, name, email, password, created_at, updated_at) VALUES
  (0, 'Root', 'root@example.test', '$2y$04$3EUJ/TMAepQkPElj6gMovusWMWBd2XclldAGdQYIBGuB7QCBai9jK', '2026-01-01 00:00:00', '2026-01-01 00:00:00'),
  (1, 'Alice Owner', 'alice@example.test', '$2y$04$3EUJ/TMAepQkPElj6gMovusWMWBd2XclldAGdQYIBGuB7QCBai9jK', '2026-01-02 00:00:00', '2026-01-02 00:00:00'),
  (2, 'Bob Member', 'bob@example.test', '$2y$04$3EUJ/TMAepQkPElj6gMovusWMWBd2XclldAGdQYIBGuB7QCBai9jK', '2026-01-02 00:00:00', '2026-01-02 00:00:00'),
  (3, 'Carol Other', 'carol@example.test', '$2y$04$3EUJ/TMAepQkPElj6gMovusWMWBd2XclldAGdQYIBGuB7QCBai9jK', '2026-01-03 00:00:00', '2026-01-03 00:00:00');
INSERT INTO team_user (team_id, user_id, role) VALUES (0, 0, 'owner'), (1, 1, 'owner'), (1, 2, 'member'), (2, 3, 'owner');
"#;

/// (fixture name, user id, team id as stored, abilities JSON, expires_at)
const TOKENS: &[(&str, i64, &str, &str, Option<&str>)] = &[
    ("root_root", 0, "0", r#"["root"]"#, None),
    ("root_read", 0, "0", r#"["read"]"#, None),
    ("acme_owner_root", 1, "1", r#"["root"]"#, None),
    ("acme_owner_read", 1, "1", r#"["read"]"#, None),
    ("acme_owner_write", 1, "1", r#"["write"]"#, None),
    (
        "acme_owner_sensitive",
        1,
        "1",
        r#"["read","read:sensitive"]"#,
        None,
    ),
    ("acme_member_read", 2, "1", r#"["read"]"#, None),
    ("acme_member_write_legacy", 2, "1", r#"["write"]"#, None),
    (
        "acme_owner_expired",
        1,
        "1",
        r#"["root"]"#,
        Some("2020-01-01 00:00:00"),
    ),
    ("carol_root", 3, "2", r#"["root"]"#, None),
    // Token for a team the user is not a member of.
    ("alice_foreign_team", 1, "2", r#"["read"]"#, None),
];

pub struct Harness {
    pub tdb: TestDb,
    pub app: Router,
    tokens: Vec<(String, String)>,
}

impl Harness {
    pub async fn new() -> Self {
        Self::with_extra_sql("").await
    }

    pub async fn with_extra_sql(extra: &str) -> Self {
        Self::build(TestDb::new(&format!("{SEED}\n{extra}")).await).await
    }

    /// A database at the older upstream 4.5.0 schema.
    pub async fn on_4_5_0() -> Self {
        use coolify_db::testing::{MIGRATIONS_4_5_0, SCHEMA_4_5_0};
        Self::build(TestDb::with_baseline(SCHEMA_4_5_0, MIGRATIONS_4_5_0, SEED).await).await
    }

    async fn build(tdb: TestDb) -> Self {
        Self::build_with(tdb, None).await
    }

    /// A harness that also serves the web UI from `dir`.
    pub async fn with_ui(dir: std::path::PathBuf) -> Self {
        Self::build_with(TestDb::new(SEED).await, Some(dir)).await
    }

    async fn build_with(tdb: TestDb, ui_dir: Option<std::path::PathBuf>) -> Self {
        let mut tokens = Vec::new();
        for (i, (name, user, team, abilities, expires)) in TOKENS.iter().enumerate() {
            let plain = format!("{name}-secret-{i:02}");
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, team_id, abilities, expires_at, created_at, updated_at) \
                 VALUES ('App\\Models\\User', $1, $2, $3, $4, $5, $6::timestamp, now(), now()) RETURNING id",
            )
            .bind(user)
            .bind(name)
            .bind(hash_token(&plain))
            .bind(team)
            .bind(abilities)
            .bind(expires)
            .fetch_one(tdb.db.pool())
            .await
            .unwrap();
            tokens.push((name.to_string(), format!("{id}|{plain}")));
        }
        let state = AppState::with_options(
            tdb.db.clone(),
            VERSION.into(),
            TrustedProxies::default(),
            Options {
                encrypter: Some(Encrypter::new(APP_KEY).unwrap()),
                ui_dir,
                ..Options::default()
            },
        );
        let app = coolify_api::router(state);
        Self { tdb, app, tokens }
    }

    pub fn token(&self, name: &str) -> &str {
        &self.tokens.iter().find(|(n, _)| n == name).unwrap().1
    }

    pub async fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Resp {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header("Authorization", format!("Bearer {}", self.token(t)));
        }
        let req = match body {
            Some(v) => b
                .header("Content-Type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let ctype = res
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let allow = res
            .headers()
            .get("allow")
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            ctype,
            allow,
            set_cookie: None,
            csp: None,
            body: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    pub async fn call_bearer(&self, method: &str, path: &str, bearer: &str) -> Resp {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            ctype: String::new(),
            allow: None,
            set_cookie: None,
            csp: None,
            body: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    /// Raw request with arbitrary headers; captures `Set-Cookie`.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<serde_json::Value>,
    ) -> Resp {
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = match body {
            Some(v) => b
                .header("Content-Type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let set_cookie = res
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_string());
        let csp = res
            .headers()
            .get("content-security-policy")
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            ctype: String::new(),
            allow: None,
            set_cookie,
            csp,
            body: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    /// Logs in through `/ui/session`; returns (cookie header, csrf token).
    pub async fn ui_login(&self, email: &str) -> (String, String) {
        let r = self
            .request(
                "POST",
                "/ui/session",
                &[],
                Some(serde_json::json!({"email": email, "password": PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        let cookie = r
            .set_cookie
            .clone()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf = r.json()["csrf_token"].as_str().unwrap().to_string();
        (cookie, csrf)
    }

    pub async fn get(&self, path: &str, token: &str) -> Resp {
        self.call("GET", path, Some(token), None).await
    }

    pub async fn finish(self) {
        self.tdb.drop_db().await;
    }
}

#[derive(Debug)]
pub struct Resp {
    pub status: StatusCode,
    pub ctype: String,
    pub allow: Option<String>,
    pub set_cookie: Option<String>,
    pub csp: Option<String>,
    pub body: String,
}

impl Resp {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }

    #[track_caller]
    pub fn assert(&self, status: u16, body: &str) {
        assert_eq!(self.status.as_u16(), status, "body: {}", self.body);
        assert_eq!(self.body, body);
    }
}
