//! A password reset (by email link or a forced reset) revokes the user's API tokens with the new
//! password, and only that user's.
#![cfg(feature = "testing")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use coolify_db::auth_pages;
use coolify_db::testing::TestDb;

const SEED: &str = r#"
INSERT INTO users (id, name, email, password, force_password_reset) VALUES
  (1, 'a', 'a@example.test', 'x', false), (2, 'b', 'b@example.test', 'x', true),
  (3, 'c', 'c@example.test', 'x', false);
INSERT INTO teams (id, name, personal_team) VALUES (1, 'acme', false);
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner'), (1, 2, 'member'), (1, 3, 'member');
INSERT INTO personal_access_tokens (id, tokenable_type, tokenable_id, name, token, team_id, abilities) VALUES
  (1, 'App\Models\User', 1, 'a1', 't1', '1', '["root"]'),
  (2, 'App\Models\User', 1, 'a2', 't2', '1', '["read"]'),
  (3, 'App\Models\User', 2, 'b1', 't3', '1', '["write"]'),
  (4, 'App\Models\User', 3, 'c1', 't4', '1', '["read"]');
"#;

async fn tokens_of(t: &TestDb, user: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM personal_access_tokens WHERE tokenable_id = $1")
        .bind(user)
        .fetch_one(t.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_password_reset_revokes_the_users_api_tokens() {
    let t = TestDb::new(SEED).await;
    let now = chrono::Utc::now().naive_utc();
    auth_pages::reset_password(&t.db, 1, "$2y$new", "remember", now)
        .await
        .unwrap();
    assert_eq!(tokens_of(&t, 1).await, 0);
    // Other users keep theirs.
    assert_eq!(tokens_of(&t, 2).await, 1);
    assert_eq!(tokens_of(&t, 3).await, 1);
    let pass: String = sqlx::query_scalar("SELECT password FROM users WHERE id = 1")
        .fetch_one(t.db.pool())
        .await
        .unwrap();
    assert_eq!(pass, "$2y$new");
}

#[tokio::test]
async fn a_forced_reset_revokes_the_users_api_tokens() {
    let t = TestDb::new(SEED).await;
    let now = chrono::Utc::now().naive_utc();
    auth_pages::finish_forced_reset(&t.db, 2, "$2y$new", now)
        .await
        .unwrap();
    assert_eq!(tokens_of(&t, 2).await, 0);
    assert_eq!(tokens_of(&t, 1).await, 2);
    assert_eq!(tokens_of(&t, 3).await, 1);
    let forced: bool = sqlx::query_scalar("SELECT force_password_reset FROM users WHERE id = 2")
        .fetch_one(t.db.pool())
        .await
        .unwrap();
    assert!(!forced);
}
