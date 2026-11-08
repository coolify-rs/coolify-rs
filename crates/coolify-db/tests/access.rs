//! `AccessResolver` (upstream 4.6.0): grant UUIDs resolved inside the team, inheritance from
//! project to environment to resource, `userHolds`, and the visibility of the finders.
#![cfg(feature = "testing")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;

use coolify_db::access;
use coolify_db::testing::TestDb;
use coolify_domain::grants::{AccessLevel, AccessScope, Grant, Target};

const SEED: &str = r#"
ALTER TABLE team_user ADD COLUMN access json;
INSERT INTO teams (id, name, personal_team, created_at, updated_at) VALUES
  (1, 'Acme', false, now(), now()), (2, 'Other', false, now(), now());
INSERT INTO users (id, name, email, password, created_at, updated_at) VALUES
  (1, 'Alice', 'alice@example.test', 'x', now(), now()), (2, 'Bob', 'bob@example.test', 'x', now(), now());
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner'), (1, 2, 'member');
INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES
  (11, 'p-one', 'One', 1, now(), now()), (12, 'p-two', 'Two', 1, now(), now()),
  (21, 'p-other', 'Other', 2, now(), now());
INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES
  (111, 'e-one', 'production', 11, now(), now()), (121, 'e-two', 'production', 12, now(), now()),
  (122, 'e-two-staging', 'staging', 12, now(), now()), (211, 'e-other', 'production', 21, now(), now());
INSERT INTO services (id, uuid, name, environment_id, docker_compose_raw) VALUES
  (1, 's-two', 's-two', 121, ''), (2, 's-other', 's-other', 211, ''), (3, 's-staging', 's-staging', 122, '');
"#;

fn grant(scope: AccessScope, uuid: &str, levels: &[AccessLevel]) -> Grant {
    Grant {
        scope,
        uuid: Some(uuid.into()),
        levels: levels.to_vec(),
    }
}

#[tokio::test]
async fn grants_resolve_inside_the_team_only() {
    let t = TestDb::new(SEED).await;
    let layer = access::resolve(
        &t.db,
        1,
        &[
            grant(AccessScope::Project, "p-one", &[AccessLevel::Write]),
            grant(AccessScope::Project, "p-other", &[AccessLevel::Root]),
            grant(AccessScope::Resource, "s-two", &[AccessLevel::Deploy]),
            grant(AccessScope::Resource, "s-other", &[AccessLevel::Root]),
        ],
    )
    .await
    .unwrap();
    // The other team's project and resource are dropped.
    assert_eq!(
        layer.project_levels.keys().copied().collect::<Vec<_>>(),
        vec![11]
    );
    assert_eq!(layer.resource_uuids(), vec!["s-two".to_string()]);
    assert_eq!(layer.project_environment_ids, vec![111]);
    // A granted resource makes its environment and project visible, not its siblings.
    assert!(layer.environment_visible(121));
    assert!(!layer.environment_visible(122));
    assert!(layer.project_visible(12));
    assert!(layer.resource_visible(Some(121), "s-two"));
    assert!(!layer.resource_visible(Some(122), "s-staging"));
    assert!(layer.allows(AccessLevel::Deploy, Some(12), Some(121), Some("s-two")));
    assert!(!layer.allows(AccessLevel::Write, Some(12), Some(121), Some("s-two")));
    assert!(layer.allows(AccessLevel::Deploy, Some(11), Some(111), Some("anything")));
    t.drop_db().await;
}

#[tokio::test]
async fn members_default_to_team_read_and_owners_have_no_limits() {
    let t = TestDb::new(SEED).await;
    assert_eq!(access::member_grants(&t.db, 1, 1).await.unwrap(), None);
    let bob = access::member_grants(&t.db, 1, 2).await.unwrap().unwrap();
    assert_eq!(bob, vec![Grant::team(vec![AccessLevel::Read])]);
    // Not a member: no grants at all.
    assert_eq!(
        access::member_grants(&t.db, 2, 2).await.unwrap(),
        Some(vec![])
    );
    sqlx::query(
        r#"UPDATE team_user SET access = '[{"scope":"environment","uuid":"e-two","levels":["write"]}]' WHERE user_id = 2"#,
    )
    .execute(t.db.pool())
    .await
    .unwrap();
    let bob = access::member_grants(&t.db, 1, 2).await.unwrap();
    let a = access::actor(&t.db, 1, bob.clone(), None).await.unwrap();
    assert!(!a.covers_whole_team());
    assert!(a.allows(AccessLevel::Write, &Target::resource(Some(121), "s-two")));
    assert!(!a.allows(AccessLevel::Read, &Target::resource(Some(122), "s-staging")));
    assert!(!a.allows(AccessLevel::Write, &Target::project(12)));
    assert!(a.can_open_project(12) && !a.can_open_project(11));
    // `userHolds`: a token may not get more than the member.
    let holds = |grants: Vec<Grant>| {
        let db = t.db.clone();
        let bob = bob.clone();
        async move { access::user_holds(&db, 1, bob, &grants).await.unwrap() }
    };
    assert!(
        holds(vec![grant(
            AccessScope::Resource,
            "s-two",
            &[AccessLevel::Deploy]
        )])
        .await
    );
    assert!(
        holds(vec![grant(
            AccessScope::Environment,
            "e-two",
            &[AccessLevel::Write]
        )])
        .await
    );
    assert!(
        !holds(vec![grant(
            AccessScope::Environment,
            "e-two",
            &[AccessLevel::Delete]
        )])
        .await
    );
    assert!(
        !holds(vec![grant(
            AccessScope::Project,
            "p-two",
            &[AccessLevel::Read]
        )])
        .await
    );
    assert!(!holds(vec![Grant::team(vec![AccessLevel::Read])]).await);
    assert!(!holds(vec![grant(AccessScope::Project, "missing", &[])]).await);
    t.drop_db().await;
}

#[tokio::test]
async fn finders_hide_what_the_request_cannot_see() {
    let t = TestDb::new(SEED).await;
    let a = access::actor(
        &t.db,
        1,
        Some(vec![grant(
            AccessScope::Environment,
            "e-two",
            &[AccessLevel::Read],
        )]),
        None,
    )
    .await
    .unwrap();
    let db = t.db.clone();
    access::scope(Arc::new(a), async move {
        let projects = coolify_db::projects::list(&db, 1).await.unwrap();
        let ids: Vec<i64> = projects
            .iter()
            .filter_map(|p| p.get("id").and_then(serde_json::Value::as_i64))
            .collect();
        assert_eq!(ids, vec![12]);
        assert!(
            coolify_db::projects::find(&db, 1, "p-one")
                .await
                .unwrap()
                .is_none()
        );
        let p = coolify_db::projects::find(&db, 1, "p-two")
            .await
            .unwrap()
            .unwrap();
        assert!(
            coolify_db::projects::find_environment(&db, p.id, "staging")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            coolify_db::services::find_in_team(&db, 1, "s-two")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            coolify_db::services::find_in_team(&db, 1, "s-staging")
                .await
                .unwrap()
                .is_none()
        );
        let all = coolify_db::services::list_for_team(&db, 1).await.unwrap();
        assert_eq!(
            all.iter().map(|s| s.uuid.as_str()).collect::<Vec<_>>(),
            vec!["s-two"]
        );
    })
    .await;
    // Outside a request (jobs) every row is visible.
    assert!(
        coolify_db::services::find_in_team(&t.db, 1, "s-staging")
            .await
            .unwrap()
            .is_some()
    );
    t.drop_db().await;
}

#[test]
fn token_grants_take_the_token_abilities() {
    let g = access::token_grants(
        Some(r#"[{"scope":"project","uuid":"p-one"}]"#),
        &[
            "read".to_string(),
            "deploy".to_string(),
            "write:sensitive".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        g,
        vec![grant(
            AccessScope::Project,
            "p-one",
            &[AccessLevel::Read, AccessLevel::Deploy]
        )]
    );
    assert_eq!(access::token_grants(None, &[]), None);
    assert_eq!(access::token_grants(Some("null"), &[]), None);
}
