//! The plugin's decisions as plain functions: what a valid rule is, whether rules loop, what a
//! `deployment.succeeded` event should deploy, and how the history is kept. Nothing here calls the plugin
//! interface, so `cargo test -p deploy-chains` runs it natively (README.md, "Development").

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Applications one rule may deploy. Every target is deployed in turn within the 5 s event deadline.
pub const MAX_TARGETS: usize = 10;
/// Rules per team.
pub const MAX_RULES: usize = 200;
/// Entries kept in the history, newest first.
pub const MAX_HISTORY: usize = 100;
/// Length of a rule's branch.
pub const MAX_BRANCH: usize = 255;

/// Storage key prefix of a rule; the rest is the source application's uuid.
pub const RULE_PREFIX: &str = "rule:";
/// Storage key of the history.
pub const HISTORY_KEY: &str = "history";

pub fn rule_key(source: &str) -> String {
    format!("{RULE_PREFIX}{source}")
}

/// An application to deploy after the source deployed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub uuid: String,
    /// Rebuild without the build cache, as the API's `force`.
    #[serde(default)]
    pub force: bool,
}

/// What happens after one source application deploys successfully. Stored at [`rule_key`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub targets: Vec<Target>,
    /// Only when the source deployed this branch (the event's `commit.branch`).
    #[serde(default)]
    pub branch: Option<String>,
}

impl Rule {
    pub fn target_uuids(&self) -> Vec<String> {
        self.targets.iter().map(|t| t.uuid.clone()).collect()
    }
}

/// The body of `PUT /rules/{source}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleInput {
    pub targets: Vec<Target>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// Validation errors by field, as the API answers them (`{"message": ..., "errors": {...}}`).
pub type Errors = BTreeMap<&'static str, Vec<String>>;

/// A string that could be a Coolify uuid. Anything else is not looked up and not stored.
pub fn plausible_uuid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether the caller's abilities allow changing rules: `write` or `root`, as for the API's writes.
pub fn can_write(abilities: &[String]) -> bool {
    abilities.iter().any(|a| a == "write" || a == "root")
}

/// Checks a rule for `source` against the team's applications (uuid → name). `rules` is how many rules the
/// team has, and `exists` whether `source` already has one. Loops are checked separately with
/// [`find_cycle`], which needs the stored rules.
pub fn check(
    source: &str,
    input: RuleInput,
    applications: &BTreeMap<String, String>,
    rules: usize,
    exists: bool,
) -> Result<Rule, Errors> {
    let mut errors = Errors::new();
    let mut add = |field: &'static str, message: String| {
        errors.entry(field).or_default().push(message);
    };
    if !plausible_uuid(source) || !applications.contains_key(source) {
        add(
            "source",
            "The source is not an application of this team.".into(),
        );
    }
    if !exists && rules >= MAX_RULES {
        add(
            "source",
            format!("A team can have at most {MAX_RULES} deploy chains."),
        );
    }
    if input.targets.len() > MAX_TARGETS {
        add(
            "targets",
            format!("A chain can deploy at most {MAX_TARGETS} applications."),
        );
    }
    let mut seen = BTreeSet::new();
    for (i, t) in input.targets.iter().enumerate() {
        let field = "targets";
        if !plausible_uuid(&t.uuid) || !applications.contains_key(&t.uuid) {
            add(
                field,
                format!("Target {} is not an application of this team.", i + 1),
            );
        } else if t.uuid == source {
            add(
                field,
                "An application can't deploy itself after it deploys.".into(),
            );
        } else if !seen.insert(t.uuid.as_str()) {
            add(
                field,
                format!(
                    "{} is listed more than once.",
                    name_of(applications, &t.uuid)
                ),
            );
        }
    }
    let branch = match input.branch.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(b) => {
            if b.len() > MAX_BRANCH || b.chars().any(|c| c.is_control() || c.is_whitespace()) {
                add(
                    "branch",
                    format!("The branch must be one word of at most {MAX_BRANCH} characters."),
                );
            }
            Some(b.to_owned())
        }
    };
    if errors.is_empty() {
        Ok(Rule {
            targets: input.targets,
            branch,
        })
    } else {
        Err(errors)
    }
}

/// The application's name, or its uuid when the team has no such application.
pub fn name_of<'a>(applications: &'a BTreeMap<String, String>, uuid: &'a str) -> &'a str {
    applications
        .get(uuid)
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .unwrap_or(uuid)
}

/// A loop reachable from `start`, as the path that closes it (`[a, b, a]`), or `None`. `start` deploys
/// `start_targets` (the rule being saved, or the stored one); `targets_of` reads the stored rule of any
/// other application, empty when it has none. Each application is looked up at most once.
pub fn find_cycle<E>(
    start: &str,
    start_targets: &[String],
    mut targets_of: impl FnMut(&str) -> Result<Vec<String>, E>,
) -> Result<Option<Vec<String>>, E> {
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut path: Vec<String> = vec![start.to_owned()];
    let mut stack: Vec<std::vec::IntoIter<String>> = vec![Vec::from(start_targets).into_iter()];
    while let Some(next) = stack.last_mut().map(Iterator::next) {
        match next {
            Some(next) => {
                if let Some(at) = path.iter().position(|p| *p == next) {
                    let mut cycle = path[at..].to_vec();
                    cycle.push(next);
                    return Ok(Some(cycle));
                }
                if done.contains(&next) {
                    continue;
                }
                let targets = targets_of(&next)?;
                path.push(next);
                stack.push(targets.into_iter());
            }
            None => {
                stack.pop();
                if let Some(finished) = path.pop() {
                    done.insert(finished);
                }
            }
        }
    }
    Ok(None)
}

/// The validation error for a loop, with the applications' names.
pub fn cycle_errors(cycle: &[String], applications: &BTreeMap<String, String>) -> Errors {
    let mut errors = Errors::new();
    errors.insert(
        "targets",
        vec![format!(
            "This would deploy in a loop: {}.",
            describe_cycle(cycle, applications)
        )],
    );
    errors
}

pub fn describe_cycle(cycle: &[String], applications: &BTreeMap<String, String>) -> String {
    cycle
        .iter()
        .map(|u| name_of(applications, u))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// What a `deployment.succeeded` event says about the deployment that finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub application: String,
    pub deployment: String,
    pub preview: bool,
    pub branch: Option<String>,
}

/// Reads an event's payload (`Deployment` of the notifications). `None` when it names no plausible
/// application. A `preview` that is present and not `null` makes it a preview, whatever its shape.
pub fn trigger(payload: &Value) -> Option<Trigger> {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let application = text(payload.get("application_uuid")).filter(|u| plausible_uuid(u))?;
    Some(Trigger {
        application,
        deployment: text(payload.get("deployment_uuid")).unwrap_or_default(),
        preview: !matches!(payload.get("preview"), None | Some(Value::Null)),
        branch: text(payload.get("commit").and_then(|c| c.get("branch"))),
    })
}

/// The targets to deploy for `trigger` under the source's `rule`, or why nothing is deployed. Loops are
/// checked by the caller with [`find_cycle`] before deploying.
pub fn wanted<'a>(trigger: &Trigger, rule: Option<&'a Rule>) -> Result<&'a [Target], &'static str> {
    let Some(rule) = rule else {
        return Err("the application has no deploy chain");
    };
    if trigger.preview {
        return Err("preview deployments never trigger deploy chains");
    }
    if let Some(branch) = &rule.branch
        && trigger.branch.as_ref() != Some(branch)
    {
        return Err("the deployment is not of the chain's branch");
    }
    if rule.targets.is_empty() {
        return Err("the deploy chain has no targets");
    }
    Ok(&rule.targets)
}

/// The result of one triggered deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// coolify-rs accepted it (queued, or already queued for the commit).
    Queued,
    /// coolify-rs refused it: consent, rate limit, permissions, an application that is gone.
    Failed,
    /// Not attempted: the chain loops, or the event's time ran out.
    Skipped,
}

/// One entry of the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub source: String,
    pub source_deployment: String,
    pub target: String,
    pub force: bool,
    pub status: Status,
    /// The target's deployment, when one was queued.
    pub deployment: Option<String>,
    pub message: String,
}

/// Reads the answer of `deploy::queue`: the API's `{"deployments": [{"message", "deployment_uuid"}]}`, or
/// the reason coolify-rs refused it.
pub fn outcome(result: Result<Value, String>) -> (Status, Option<String>, String) {
    match result {
        Ok(answer) => {
            let first = answer.get("deployments").and_then(|d| d.get(0));
            let text = |k: &str| {
                first
                    .and_then(|f| f.get(k))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            (
                Status::Queued,
                text("deployment_uuid"),
                text("message").unwrap_or_else(|| "Deployment queued.".into()),
            )
        }
        Err(e) => (Status::Failed, None, e),
    }
}

/// Adds `entries` (oldest first) to the front of `history`, keeping [`MAX_HISTORY`].
pub fn record(history: &mut Vec<Entry>, entries: Vec<Entry>) {
    for e in entries {
        history.insert(0, e);
    }
    history.truncate(MAX_HISTORY);
}

/// The history entries that involve `application`, as source or target.
pub fn involving<'a>(history: &'a [Entry], application: &str) -> Vec<&'a Entry> {
    history
        .iter()
        .filter(|e| e.source == application || e.target == application)
        .collect()
}

/// The sources whose rules deploy `application`.
pub fn sources_of(rules: &BTreeMap<String, Rule>, application: &str) -> Vec<String> {
    rules
        .iter()
        .filter(|(_, r)| r.targets.iter().any(|t| t.uuid == application))
        .map(|(s, _)| s.clone())
        .collect()
}

/// One value of a query string, percent-decoded; `None` when absent or not valid UTF-8.
pub fn query_value(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// JSON for a `<script type="application/json">` element: `<`, `>` and `&` are escaped, so the text can't
/// close the element or open a comment, and `JSON.parse` reads it unchanged.
pub fn json_for_html(v: &Value) -> String {
    v.to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apps() -> BTreeMap<String, String> {
        [
            ("api", "API"),
            ("worker", "Worker"),
            ("scheduler", "Scheduler"),
            ("web", "Web"),
        ]
        .into_iter()
        .map(|(u, n)| (u.to_owned(), n.to_owned()))
        .collect()
    }

    fn input(targets: &[&str]) -> RuleInput {
        RuleInput {
            targets: targets
                .iter()
                .map(|u| Target {
                    uuid: (*u).to_owned(),
                    force: false,
                })
                .collect(),
            branch: None,
        }
    }

    fn graph(edges: &[(&str, &[&str])]) -> BTreeMap<String, Rule> {
        edges
            .iter()
            .map(|(s, ts)| {
                (
                    (*s).to_owned(),
                    Rule {
                        targets: input(ts).targets,
                        branch: None,
                    },
                )
            })
            .collect()
    }

    /// `find_cycle` for saving `start → targets` into the stored `rules`.
    fn cycle(rules: &BTreeMap<String, Rule>, start: &str, targets: &[&str]) -> Option<Vec<String>> {
        let targets: Vec<String> = targets.iter().map(|t| (*t).to_owned()).collect();
        find_cycle::<()>(start, &targets, |u| {
            Ok(rules.get(u).map(Rule::target_uuids).unwrap_or_default())
        })
        .unwrap()
    }

    #[test]
    fn a_valid_rule() {
        let mut i = input(&["worker", "scheduler"]);
        i.targets[1].force = true;
        i.branch = Some("  main ".into());
        let rule = check("api", i, &apps(), 0, false).unwrap();
        assert_eq!(rule.target_uuids(), ["worker", "scheduler"]);
        assert!(rule.targets[1].force && !rule.targets[0].force);
        assert_eq!(rule.branch.as_deref(), Some("main"));
        let empty = RuleInput {
            targets: vec![],
            branch: Some(" ".into()),
        };
        assert_eq!(
            check("api", empty, &apps(), 0, false).unwrap(),
            Rule::default()
        );
    }

    #[test]
    fn rules_name_only_the_teams_applications() {
        let e = check("other-team", input(&["worker"]), &apps(), 0, false).unwrap_err();
        assert_eq!(
            e["source"],
            ["The source is not an application of this team."]
        );
        let e = check(
            "api",
            input(&["worker", "foreign", "x'; drop"]),
            &apps(),
            0,
            false,
        )
        .unwrap_err();
        assert_eq!(
            e["targets"],
            [
                "Target 2 is not an application of this team.",
                "Target 3 is not an application of this team."
            ]
        );
    }

    #[test]
    fn self_loops_duplicates_branches_and_limits() {
        let e = check("api", input(&["api"]), &apps(), 0, false).unwrap_err();
        assert_eq!(
            e["targets"],
            ["An application can't deploy itself after it deploys."]
        );
        let e = check("api", input(&["worker", "worker"]), &apps(), 0, false).unwrap_err();
        assert_eq!(e["targets"], ["Worker is listed more than once."]);
        let mut i = input(&["worker"]);
        i.branch = Some("feature x".into());
        assert!(check("api", i, &apps(), 0, false).unwrap_err()["branch"][0].contains("one word"));
        let mut i = input(&["worker"]);
        i.branch = Some("b".repeat(MAX_BRANCH + 1));
        assert!(check("api", i, &apps(), 0, false).is_err());

        let many: Vec<String> = (0..=MAX_TARGETS).map(|n| format!("app-{n}")).collect();
        let mut team = apps();
        for m in &many {
            team.insert(m.clone(), m.clone());
        }
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let e = check("api", input(&refs), &team, 0, false).unwrap_err();
        assert!(e["targets"][0].contains("at most 10"), "{e:?}");

        // The rule count limits new rules only; an existing rule can still change.
        assert!(check("api", input(&["worker"]), &apps(), MAX_RULES, false).is_err());
        assert!(check("api", input(&["worker"]), &apps(), MAX_RULES, true).is_ok());
    }

    #[test]
    fn rule_bodies_are_strict() {
        let ok: RuleInput = serde_json::from_value(
            json!({"targets": [{"uuid": "w", "force": true}], "branch": null}),
        )
        .unwrap();
        assert!(ok.targets[0].force);
        for bad in [
            json!({}),
            json!({"targets": "worker"}),
            json!({"targets": [{"uuid": 1}]}),
            json!({"targets": [], "extra": 1}),
            json!({"targets": [{"uuid": "w", "force": "yes"}]}),
        ] {
            assert!(
                serde_json::from_value::<RuleInput>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn loops_are_found_through_stored_rules() {
        let none = graph(&[]);
        assert_eq!(cycle(&none, "api", &["worker", "scheduler"]), None);

        // A → B → A.
        let rules = graph(&[("worker", &["api"])]);
        assert_eq!(
            cycle(&rules, "api", &["worker"]),
            Some(vec!["api".into(), "worker".into(), "api".into()])
        );

        // A longer one: api → worker → scheduler → web → api.
        let rules = graph(&[
            ("worker", &["scheduler"]),
            ("scheduler", &["web"]),
            ("web", &["api"]),
        ]);
        let c = cycle(&rules, "api", &["worker"]).unwrap();
        assert_eq!(c, ["api", "worker", "scheduler", "web", "api"]);
        assert_eq!(
            describe_cycle(&c, &apps()),
            "API → Worker → Scheduler → Web → API"
        );
        assert_eq!(
            cycle_errors(&c, &apps())["targets"],
            ["This would deploy in a loop: API → Worker → Scheduler → Web → API."]
        );

        // A diamond is not a loop: api → worker, api → scheduler, both → web.
        let rules = graph(&[("worker", &["web"]), ("scheduler", &["web"])]);
        assert_eq!(cycle(&rules, "api", &["worker", "scheduler"]), None);

        // A self-loop, and a loop downstream that was stored by a race, are found too.
        assert!(cycle(&none, "api", &["api"]).is_some());
        let rules = graph(&[("worker", &["scheduler"]), ("scheduler", &["worker"])]);
        assert_eq!(
            cycle(&rules, "api", &["worker"]),
            Some(vec!["worker".into(), "scheduler".into(), "worker".into()])
        );

        // Replacing api's rule ignores what was stored for api: the new targets count.
        let rules = graph(&[("api", &["worker"]), ("worker", &["scheduler"])]);
        assert_eq!(
            cycle(&rules, "scheduler", &["api"]).map(|c| c.len()),
            Some(4)
        );
        assert_eq!(cycle(&rules, "api", &["scheduler"]), None);
    }

    #[test]
    fn each_application_is_looked_up_once() {
        let rules = graph(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"]), ("d", &[])]);
        let mut looked = vec![];
        let found = find_cycle::<()>("s", &["a".into(), "d".into()], |u| {
            looked.push(u.to_owned());
            Ok(rules.get(u).map(Rule::target_uuids).unwrap_or_default())
        })
        .unwrap();
        assert_eq!(found, None);
        looked.sort();
        assert_eq!(looked, ["a", "b", "c", "d"]);
        let err = find_cycle("s", &["a".into()], |_| Err("storage failed"));
        assert_eq!(err, Err("storage failed"));
    }

    fn payload() -> Value {
        json!({
            "application_name": "API",
            "application_uuid": "api",
            "deployment_uuid": "dep-1",
            "project_name": "p", "project_uuid": "p", "environment_name": "production", "environment_uuid": "e",
            "fqdn": null,
            "preview": null,
            "commit": {"sha": "abc", "message": "m", "url": null, "branch": "main"},
        })
    }

    #[test]
    fn events_are_read_defensively() {
        let t = trigger(&payload()).unwrap();
        assert_eq!(
            t,
            Trigger {
                application: "api".into(),
                deployment: "dep-1".into(),
                preview: false,
                branch: Some("main".into()),
            }
        );
        let mut p = payload();
        p["preview"] = json!({"pull_request_id": 7, "fqdn": null});
        assert!(trigger(&p).unwrap().preview);
        // Anything in `preview` but null counts as a preview: chains never run for one by mistake.
        p["preview"] = json!(0);
        assert!(trigger(&p).unwrap().preview);
        let mut p = payload();
        p.as_object_mut().unwrap().remove("preview");
        p.as_object_mut().unwrap().remove("commit");
        let t = trigger(&p).unwrap();
        assert!(!t.preview && t.branch.is_none());
        for bad in [
            json!({}),
            json!([1]),
            json!({"application_uuid": 42}),
            json!({"application_uuid": ""}),
            json!({"application_uuid": "a/../b"}),
        ] {
            assert_eq!(trigger(&bad), None, "{bad}");
        }
    }

    #[test]
    fn what_an_event_deploys() {
        let rule = Rule {
            targets: input(&["worker", "scheduler"]).targets,
            branch: None,
        };
        let t = trigger(&payload()).unwrap();
        assert_eq!(wanted(&t, Some(&rule)).unwrap().len(), 2);
        assert!(wanted(&t, None).is_err());

        let mut preview = t.clone();
        preview.preview = true;
        assert_eq!(
            wanted(&preview, Some(&rule)),
            Err("preview deployments never trigger deploy chains")
        );

        let on_main = Rule {
            branch: Some("main".into()),
            ..rule.clone()
        };
        assert!(wanted(&t, Some(&on_main)).is_ok());
        let mut other = t.clone();
        other.branch = Some("develop".into());
        assert!(wanted(&other, Some(&on_main)).is_err());
        other.branch = None;
        assert!(
            wanted(&other, Some(&on_main)).is_err(),
            "no branch in the event"
        );
        assert!(wanted(&t, Some(&Rule::default())).is_err());
    }

    #[test]
    fn outcomes_and_history() {
        let ok = outcome(Ok(json!({"deployments": [
            {"message": "Application Worker deployment queued.", "resource_uuid": "worker", "deployment_uuid": "d-9"}
        ]})));
        assert_eq!(
            ok,
            (
                Status::Queued,
                Some("d-9".into()),
                "Application Worker deployment queued.".into()
            )
        );
        let refused = outcome(Err("the team has not granted deploy to this plugin".into()));
        assert_eq!(refused.0, Status::Failed);
        assert_eq!(outcome(Ok(json!({}))).0, Status::Queued);

        let entry = |n: u64| Entry {
            at: n,
            source: "api".into(),
            source_deployment: "d".into(),
            target: if n.is_multiple_of(2) { "worker" } else { "web" }.into(),
            force: false,
            status: Status::Queued,
            deployment: None,
            message: String::new(),
        };
        let mut history = vec![];
        record(&mut history, (0..3).map(entry).collect());
        assert_eq!(history.iter().map(|e| e.at).collect::<Vec<_>>(), [2, 1, 0]);
        record(
            &mut history,
            (3..MAX_HISTORY as u64 + 10).map(entry).collect(),
        );
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0].at, MAX_HISTORY as u64 + 9);
        assert_eq!(involving(&history, "web").len(), MAX_HISTORY / 2);
        assert_eq!(involving(&history, "api").len(), MAX_HISTORY);
        assert!(involving(&history, "scheduler").is_empty());
        assert_eq!(
            serde_json::to_value(Status::Skipped).unwrap(),
            json!("skipped")
        );
    }

    #[test]
    fn callers_and_queries() {
        let ab = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(can_write(&ab(&["read", "write"])));
        assert!(can_write(&ab(&["root"])));
        assert!(!can_write(&ab(&["read", "deploy", "read:sensitive"])));
        assert!(!can_write(&[]));

        let rules = graph(&[("api", &["worker"]), ("web", &["worker", "api"])]);
        assert_eq!(sources_of(&rules, "worker"), ["api", "web"]);
        assert!(sources_of(&rules, "web").is_empty());

        assert_eq!(
            query_value("slot=application&resource=ab-1", "resource").as_deref(),
            Some("ab-1")
        );
        assert_eq!(
            query_value("source=a%2Db", "source").as_deref(),
            Some("a-b")
        );
        assert_eq!(query_value("a=%ZZ&&=b&c", "a"), None);
        assert_eq!(query_value("a=%E", "a"), None);
        assert_eq!(query_value("", "a"), None);

        let html = json_for_html(&json!({"name": "</script><!--&"}));
        assert!(!html.contains('<') && !html.contains('&'));
        let back: Value = serde_json::from_str(&html).unwrap();
        assert_eq!(back["name"], "</script><!--&");
    }
}
