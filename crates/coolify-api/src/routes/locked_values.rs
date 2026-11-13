//! `redactLockedApiResourceValues()` (v4.6.0): the locked (shown once) values a serialized
//! application or service still contains in its generated `docker_compose` are replaced by
//! `<locked>`, also for tokens that can read sensitive data
//! (`redactLockedValuesInGeneratedCompose()`).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::state::AppState;

/// `{{ team.KEY }}`, `{{ project.KEY }}`, `{{ environment.KEY }}`, `{{ server.KEY }}` in
/// `docker_compose_raw`.
static SHARED_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*(\w+)\.([^}\s]+)\s*\}\}").unwrap_or_else(|_| unreachable!())
});

/// The resource whose generated compose is redacted.
pub(crate) struct Owner<'a> {
    pub team_id: i64,
    pub project_id: Option<i64>,
    pub environment_id: Option<i64>,
    /// `data_get($resource, 'destination.server_id')`.
    pub server_id: Option<i64>,
    pub compose_raw: Option<&'a str>,
}

/// `(type, key, value, team_id, project_id, environment_id, server_id)` of a shared variable.
type SharedRow = (
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// The shown-once shared variables `docker_compose_raw` references directly.
async fn shared_references(state: &AppState, owner: &Owner<'_>) -> Vec<String> {
    let raw = owner.compose_raw.unwrap_or_default();
    let references: Vec<(String, String)> = SHARED_REFERENCE
        .captures_iter(raw)
        // `SHARED_VARIABLE_TYPES`.
        .filter(|c| matches!(&c[1], "team" | "project" | "environment" | "server"))
        .map(|c| (c[1].to_owned(), c[2].to_owned()))
        .collect();
    if references.is_empty() {
        return Vec::new();
    }
    let keys: Vec<String> = references.iter().map(|(_, k)| k.clone()).collect();
    let rows: Vec<SharedRow> = sqlx::query_as(
        "SELECT type, key, value, team_id, project_id, environment_id, server_id \
             FROM shared_environment_variables \
             WHERE team_id = $1 AND is_shown_once = true AND key = ANY($2) ORDER BY id",
    )
    .bind(owner.team_id)
    .bind(&keys)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let Some(enc) = state.encrypter.as_ref() else {
        return Vec::new();
    };
    rows.into_iter()
        .filter(|(kind, key, _, team, project, environment, server)| {
            let (scope, stored) = match kind.as_str() {
                "team" => (Some(owner.team_id), *team),
                "project" => (owner.project_id, *project),
                "environment" => (owner.environment_id, *environment),
                "server" => (owner.server_id, *server),
                _ => return false,
            };
            references.iter().any(|(t, k)| t == kind && k == key)
                && scope.is_some()
                && stored == scope
        })
        .filter_map(|(_, _, value, ..)| {
            let value = value?;
            enc.decrypt_string(&value)
                .or_else(|_| enc.decrypt_maybe_serialized(&value))
                .ok()
        })
        .collect()
}

/// The `strtr()` map of `redactLockedValuesInGeneratedCompose()`: each value, with `$` doubled,
/// with `'` doubled and JSON-encoded, at least 4 characters long, longest first.
fn variants(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let json = serde_json::to_string(value).unwrap_or_default();
        let inner = json
            .strip_prefix('"')
            .and_then(|j| j.strip_suffix('"'))
            .unwrap_or_default()
            .to_owned();
        for v in [
            value.clone(),
            value.replace('$', "$$"),
            value.replace('\'', "''"),
            inner,
        ] {
            if v.chars().count() >= 4 && !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.len()));
    out
}

/// PHP `strtr($text, [$variant => '<locked>', ...])`: the longest match first, and replaced text is
/// not searched again.
pub(crate) fn strtr_locked(text: &str, variants: &[String]) -> String {
    if variants.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(v) = variants.iter().find(|v| rest.starts_with(v.as_str())) {
            out.push_str("<locked>");
            rest = &rest[v.len()..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Replaces the locked values in `payload["docker_compose"]` when it is filled. `locked` are the
/// `locked` values of `logRedactionSecretsFor()`.
pub(crate) async fn redact_compose(
    state: &AppState,
    owner: &Owner<'_>,
    locked: Vec<String>,
    payload: &mut Map<String, Value>,
) {
    let Some(Value::String(compose)) = payload.get("docker_compose") else {
        return;
    };
    if compose.trim().is_empty() {
        return;
    }
    let mut values = shared_references(state, owner).await;
    values.extend(locked);
    let variants = variants(&values);
    if variants.is_empty() {
        return;
    }
    let redacted = strtr_locked(compose, &variants);
    payload.insert("docker_compose".into(), Value::String(redacted));
}

/// The generated compose of an application, redacted.
pub(crate) async fn redact_application(
    state: &AppState,
    a: &coolify_db::applications::ApplicationRow,
    payload: &mut Map<String, Value>,
) {
    if !payload
        .get("docker_compose")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.trim().is_empty())
    {
        return;
    }
    let locked = match &state.jobs {
        Some(jobs) => jobs.application_locked_values(a, a.server_id()).await,
        None => Vec::new(),
    };
    let owner = Owner {
        team_id: a.team_id,
        project_id: a.project.get("id").and_then(Value::as_i64),
        environment_id: a.int("environment_id"),
        server_id: a.server_id(),
        compose_raw: a.str("docker_compose_raw"),
    };
    redact_compose(state, &owner, locked, payload).await;
}

/// The generated compose of a service, redacted.
pub(crate) async fn redact_service(
    state: &AppState,
    s: &coolify_db::services::ServiceRow,
    payload: &mut Map<String, Value>,
) {
    if !payload
        .get("docker_compose")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.trim().is_empty())
    {
        return;
    }
    let locked = match &state.jobs {
        Some(jobs) => {
            jobs.service_locked_values(s.id, s.team_id, s.server_id())
                .await
        }
        None => Vec::new(),
    };
    let owner = Owner {
        team_id: s.team_id,
        project_id: s.project.get("id").and_then(Value::as_i64),
        environment_id: s.int("environment_id"),
        server_id: s.server_id(),
        compose_raw: s.str("docker_compose_raw"),
    };
    redact_compose(state, &owner, locked, payload).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtr_replaces_longest_first_once() {
        let v = variants(&["abcd".into(), "abcdef".into(), "p$ss".into()]);
        assert_eq!(
            strtr_locked("x abcdef abcd p$$ss p$ss", &v),
            "x <locked> <locked> <locked> <locked>"
        );
        // Values under 4 characters are kept.
        assert_eq!(variants(&["abc".into()]), Vec::<String>::new());
        assert_eq!(
            strtr_locked("ééabcdé", &variants(&["abcd".into()])),
            "éé<locked>é"
        );
    }
}
