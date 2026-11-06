//! One recorded deployment, from a `deployment.succeeded` or `deployment.failed` event.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The longest name kept; longer names are cut, so every record stays small.
const MAX_NAME: usize = 120;
/// The longest uuid or commit SHA kept.
const MAX_ID: usize = 64;

/// A deployment as stored. Short field names keep the shards small: a record is about 200 bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    /// When the event arrived (Unix seconds): the deployment finished then.
    #[serde(rename = "t")]
    pub at: i64,
    #[serde(rename = "a")]
    pub app: String,
    #[serde(rename = "n", default, skip_serializing_if = "String::is_empty")]
    pub app_name: String,
    #[serde(rename = "p", default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(rename = "e", default, skip_serializing_if = "String::is_empty")]
    pub environment: String,
    #[serde(rename = "ok")]
    pub succeeded: bool,
    /// The pull request of a preview deployment; 0 for the application itself.
    #[serde(rename = "pr", default, skip_serializing_if = "is_zero")]
    pub pull_request: i64,
    #[serde(rename = "d", default, skip_serializing_if = "String::is_empty")]
    pub deployment: String,
    #[serde(rename = "c", default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// When the commit was made, if the event said so (it currently never does; see the README).
    #[serde(rename = "ct", default, skip_serializing_if = "Option::is_none")]
    pub commit_at: Option<i64>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Deployment {
    pub fn is_preview(&self) -> bool {
        self.pull_request != 0
    }

    /// Seconds from commit to this deployment, for a successful deployment whose commit time is known.
    pub fn lead_time(&self) -> Option<i64> {
        let c = self.commit_at?;
        (self.succeeded && self.at >= c).then(|| self.at - c)
    }
}

/// Cuts `s` to at most `max` bytes, on a character boundary, without control characters.
fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.trim().chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// An id: letters, digits, `-` and `_` only, or nothing.
fn id(v: Option<&Value>) -> String {
    let s = v.and_then(Value::as_str).unwrap_or_default().trim();
    if s.is_empty()
        || s.len() > MAX_ID
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return String::new();
    }
    s.to_owned()
}

fn text(v: Option<&Value>) -> String {
    clip(v.and_then(Value::as_str).unwrap_or_default(), MAX_NAME)
}

/// The deployment an event describes, finished at `now`. `Err` for events this plugin can't use: another
/// kind, a payload that isn't an object, or one without an application uuid.
pub fn from_event(kind: &str, payload: &Value, now: i64) -> Result<Deployment, String> {
    let succeeded = match kind {
        "deployment.succeeded" => true,
        "deployment.failed" => false,
        other => return Err(format!("not a deployment event: {other}")),
    };
    let Some(p) = payload.as_object() else {
        return Err("the payload is not a JSON object".into());
    };
    let app = id(p.get("application_uuid"));
    if app.is_empty() {
        return Err("the payload has no valid application_uuid".into());
    }
    let pull_request = p
        .get("preview")
        .and_then(|v| v.get("pull_request_id"))
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(0);
    let commit = p.get("commit").filter(|c| c.is_object());
    let sha = commit
        .and_then(|c| c.get("sha"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| (7..=MAX_ID).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Coolify's events carry no commit time; these names are what a future payload would use.
    let commit_at = commit.and_then(|c| {
        ["committed_at", "timestamp"]
            .iter()
            .find_map(|k| c.get(*k).and_then(crate::time::parse))
    });
    Ok(Deployment {
        at: now,
        app,
        app_name: text(p.get("application_name")),
        project: text(p.get("project_name")),
        environment: text(p.get("environment_name")),
        succeeded,
        pull_request,
        deployment: id(p.get("deployment_uuid")),
        commit: sha,
        commit_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_793_966_400;

    #[test]
    fn reads_the_deployment_payload() {
        let payload = json!({
            "application_name": "web",
            "application_uuid": "app-1",
            "deployment_uuid": "dep-1",
            "project_name": "Shop",
            "project_uuid": "p1",
            "environment_name": "production",
            "environment_uuid": "e1",
            "fqdn": "https://web.example.com",
            "preview": null,
            "commit": {"sha": "ABCDEF1234567", "message": "fix", "url": null, "branch": "main"}
        });
        let d = from_event("deployment.succeeded", &payload, NOW).unwrap();
        assert_eq!(
            d,
            Deployment {
                at: NOW,
                app: "app-1".into(),
                app_name: "web".into(),
                project: "Shop".into(),
                environment: "production".into(),
                succeeded: true,
                pull_request: 0,
                deployment: "dep-1".into(),
                commit: "abcdef1234567".into(),
                commit_at: None,
            }
        );
        assert!(!d.is_preview());
        assert_eq!(d.lead_time(), None);
        let stored = serde_json::to_string(&d).unwrap();
        assert_eq!(
            stored,
            r#"{"t":1793966400,"a":"app-1","n":"web","p":"Shop","e":"production","ok":true,"d":"dep-1","c":"abcdef1234567"}"#
        );
        assert_eq!(serde_json::from_str::<Deployment>(&stored).unwrap(), d);
    }

    #[test]
    fn previews_and_commit_times() {
        let payload = json!({
            "application_uuid": "app-1",
            "preview": {"pull_request_id": 42, "fqdn": null},
            "commit": {"sha": "abcdef1", "committed_at": "2026-11-06T11:00:00Z"}
        });
        let d = from_event("deployment.succeeded", &payload, NOW).unwrap();
        assert_eq!(d.pull_request, 42);
        assert!(d.is_preview());
        assert_eq!(d.lead_time(), Some(3600));
        let failed = from_event("deployment.failed", &payload, NOW).unwrap();
        assert!(!failed.succeeded);
        assert_eq!(failed.lead_time(), None, "only successful deployments");
        let future = json!({"application_uuid": "a", "commit": {"timestamp": NOW + 10}});
        assert_eq!(
            from_event("deployment.succeeded", &future, NOW)
                .unwrap()
                .lead_time(),
            None
        );
    }

    #[test]
    fn refuses_what_it_cannot_use_and_cleans_what_it_keeps() {
        assert!(from_event("backup.failed", &json!({"application_uuid": "a"}), NOW).is_err());
        assert!(from_event("deployment.failed", &json!("text"), NOW).is_err());
        assert!(from_event("deployment.failed", &json!({}), NOW).is_err());
        assert!(
            from_event(
                "deployment.failed",
                &json!({"application_uuid": "../x"}),
                NOW
            )
            .is_err()
        );
        let long = "é".repeat(500);
        let d = from_event(
            "deployment.failed",
            &json!({
                "application_uuid": "a",
                "application_name": format!("a\u{7}b{long}"),
                "deployment_uuid": 12,
                "preview": {"pull_request_id": "7"},
                "commit": {"sha": "not-a-sha"}
            }),
            NOW,
        )
        .unwrap();
        assert!(d.app_name.starts_with("ab"));
        assert!(d.app_name.len() <= MAX_NAME);
        assert_eq!(d.deployment, "");
        assert_eq!(d.pull_request, 0);
        assert_eq!(d.commit, "");
    }
}
