//! `EnvironmentVariable::authorizeAddedSharedVariableReferencesIn` (upstream 4.6.0, 6f1905b7b): a
//! user can only add a reference to a shared variable whose value they can read, because the
//! resource gets the resolved value. References the stored value already has are kept, so the
//! other fields stay editable.

use std::sync::LazyLock;

use axum::http::StatusCode;
use coolify_domain::grants::{AccessLevel, Target};
use regex::Regex;
use serde_json::json;

use crate::auth::AuthCtx;
use crate::response::ApiError;

/// `SHARED_VARIABLE_TYPES`.
const TYPES: [&str; 4] = ["team", "project", "environment", "server"];

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*(\w+)\.([^}\s]+)\s*\}\}").unwrap_or_else(|_| unreachable!())
});

/// `sharedVariableReferences`: `(type, key)` pairs.
pub(crate) fn references(value: Option<&str>) -> Vec<(String, String)> {
    REFERENCE
        .captures_iter(value.unwrap_or_default())
        .map(|c| (c[1].to_owned(), c[2].to_owned()))
        .collect()
}

/// `SharedEnvironmentVariablePolicy::viewSensitive` on an unsaved variable of that type for the
/// resource: team and server variables need an owner or admin, and (4.7.5) a token that reads
/// secrets on the whole team (`allowsTeamWide`); project and environment variables
/// read-secrets access on the resource's project or environment.
fn can_read(ctx: &AuthCtx, kind: &str, environment_id: Option<i64>) -> bool {
    match kind {
        "team" | "server" => {
            ctx.is_team_admin() && ctx.allows_team_wide(AccessLevel::ReadSensitive)
        }
        "project" | "environment" => {
            let Some(env) = environment_id else {
                return false;
            };
            if kind == "environment" {
                return ctx.can(AccessLevel::ReadSensitive, &Target::environment(env));
            }
            match ctx.access.environment_projects.get(&env) {
                Some(p) => ctx.can(AccessLevel::ReadSensitive, &Target::project(*p)),
                // Team-wide layers only: the project's grants are the environment's.
                None => ctx.can(AccessLevel::ReadSensitive, &Target::environment(env)),
            }
        }
        _ => true,
    }
}

/// Rejects references that are new in `value` and that the user cannot read, as the model's
/// `ValidationException` on `field`.
pub(crate) fn authorize_added(
    ctx: &AuthCtx,
    environment_id: Option<i64>,
    stored: Option<&str>,
    value: Option<&str>,
    field: &str,
) -> Result<(), ApiError> {
    if !value.unwrap_or_default().contains("{{") {
        return Ok(());
    }
    let existing = references(stored);
    for (kind, key) in references(value) {
        if !TYPES.contains(&kind.as_str()) || existing.contains(&(kind.clone(), key.clone())) {
            continue;
        }
        if !can_read(ctx, &kind, environment_id) {
            let message = format!(
                "You cannot reference the shared variable {kind}.{key}, because you cannot read its value."
            );
            let mut errors = serde_json::Map::new();
            errors.insert(field.to_owned(), json!([message]));
            return Err(ApiError::Status(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": message, "errors": errors}),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolify_domain::grants::{Access, AccessGrants, expand};
    use coolify_domain::{Abilities, Role};
    use std::sync::Arc;

    fn member_with_project_write() -> AuthCtx {
        let mut layer = AccessGrants::default();
        layer
            .project_levels
            .insert(7, expand(&[AccessLevel::Write]));
        layer.project_environment_ids = vec![70];
        AuthCtx {
            token_id: 1,
            user_id: 2,
            team_id: 1,
            team_id_raw: "1".into(),
            role: Role::Member,
            abilities: Abilities::default(),
            can_read_sensitive: false,
            access: Arc::new(Access {
                layers: vec![layer],
                environment_projects: [(70, 7), (80, 8)].into_iter().collect(),
            }),
        }
    }

    #[test]
    fn parses_references() {
        assert_eq!(
            references(Some("a {{ team.DB_URL }} b {{project.X}}")),
            vec![
                ("team".to_string(), "DB_URL".to_string()),
                ("project".to_string(), "X".to_string())
            ]
        );
    }

    #[test]
    fn members_cannot_add_references_they_cannot_read() {
        let ctx = member_with_project_write();
        // Project and environment variables of the granted project: write includes read secrets.
        assert!(authorize_added(&ctx, Some(70), None, Some("{{project.A}}"), "value").is_ok());
        assert!(authorize_added(&ctx, Some(70), None, Some("{{environment.A}}"), "value").is_ok());
        // Team variables need an owner or admin; another project's are out of reach.
        assert!(authorize_added(&ctx, Some(70), None, Some("{{team.SECRET}}"), "value").is_err());
        assert!(authorize_added(&ctx, Some(80), None, Some("{{project.A}}"), "value").is_err());
        // A reference that is already stored stays.
        assert!(
            authorize_added(
                &ctx,
                Some(70),
                Some("{{team.SECRET}}"),
                Some("x {{team.SECRET}}"),
                "value"
            )
            .is_ok()
        );
        // Unknown types are not references to shared variables.
        assert!(authorize_added(&ctx, Some(70), None, Some("{{service.X}}"), "value").is_ok());
    }

    /// 4.7.5 (`SharedEnvironmentVariablePolicy::allows`): the API token of an owner must also
    /// cover the whole team to read team and server variables.
    #[test]
    fn a_token_limited_to_a_project_cannot_reference_team_or_server_variables() {
        let mut ctx = member_with_project_write();
        ctx.role = Role::Owner;
        assert!(authorize_added(&ctx, Some(70), None, Some("{{team.SECRET}}"), "value").is_err());
        assert!(authorize_added(&ctx, Some(70), None, Some("{{server.SECRET}}"), "value").is_err());
        assert!(authorize_added(&ctx, Some(70), None, Some("{{project.A}}"), "value").is_ok());
        // The same owner with a team-wide token (no layers) can.
        ctx.access = Arc::new(Access::unrestricted());
        assert!(authorize_added(&ctx, Some(70), None, Some("{{team.SECRET}}"), "value").is_ok());
        // A team-wide read token of an owner does not read secrets.
        let read_only = AccessGrants {
            team_levels: expand(&[AccessLevel::Read]),
            ..Default::default()
        };
        ctx.access = Arc::new(Access {
            layers: vec![read_only],
            environment_projects: Default::default(),
        });
        assert!(authorize_added(&ctx, Some(70), None, Some("{{team.SECRET}}"), "value").is_err());
    }
}
