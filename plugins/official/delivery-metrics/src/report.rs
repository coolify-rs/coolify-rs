//! What the routes answer, from recorded deployments and the team's settings: the metrics as JSON and CSV,
//! the deployments as CSV, and the team's inventory as CSV.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::csv::{Writer, opt};
use crate::metrics::{self, Band, Filter, Metrics, Window};
use crate::record::Deployment;
use crate::store::{self, Index};
use crate::time::rfc3339;

pub const WINDOWS: [u32; 2] = [30, 90];
pub const TOP_DEFAULT: usize = 10;
pub const TOP_MAX: usize = 100;
/// The deployments `recent` lists for the team and for one application.
const RECENT_TEAM: usize = 10;
const RECENT_APP: usize = 20;

/// The team's settings (plugin.toml), with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub default_window: u32,
    pub include_previews: bool,
    pub environments: Vec<String>,
    pub retention_days: u32,
}

impl Settings {
    /// From `config::get` in the component; any function in tests.
    pub fn read(get: impl Fn(&str) -> Option<String>) -> Self {
        let window = get("default_window")
            .and_then(|w| w.trim().parse().ok())
            .filter(|w| WINDOWS.contains(w))
            .unwrap_or(WINDOWS[0]);
        Settings {
            default_window: window,
            include_previews: get("include_previews").is_some_and(|v| v.trim() == "yes"),
            environments: Filter::environments(get("environments").as_deref()),
            retention_days: store::retention_days(get("retention_days").as_deref()),
        }
    }

    pub fn filter(&self, app: Option<&str>) -> Filter {
        Filter {
            include_previews: self.include_previews,
            environments: self.environments.clone(),
            app: app.map(str::to_owned),
        }
    }

    fn json(&self) -> Value {
        json!({
            "default_window": self.default_window,
            "include_previews": self.include_previews,
            "environments": self.environments,
            "retention_days": self.retention_days,
        })
    }
}

/// One value of a query string, percent-decoded (`+` is a space). `None` when absent or not UTF-8.
pub fn query(q: &str, key: &str) -> Option<String> {
    let raw = q
        .split('&')
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .find(|(k, _)| *k == key)?
        .1;
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The request's window: `window=30` or `window=90`, else the team's default.
pub fn window_days(q: &str, settings: &Settings) -> Result<u32, String> {
    match query(q, "window") {
        None => Ok(settings.default_window),
        Some(w) if w.is_empty() => Ok(settings.default_window),
        Some(w) => w
            .trim()
            .trim_end_matches('d')
            .parse()
            .ok()
            .filter(|w| WINDOWS.contains(w))
            .ok_or_else(|| "The window must be 30 or 90 (days).".to_owned()),
    }
}

/// The request's `top`: how many applications the team's table lists.
pub fn top(q: &str) -> usize {
    query(q, "top")
        .and_then(|t| t.parse().ok())
        .map_or(TOP_DEFAULT, |t: usize| t.clamp(1, TOP_MAX))
}

/// An application uuid as the API has them: letters, digits, `-` and `_`.
pub fn valid_uuid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// How far before the window the deployments are read: failures this long before it still tell whether a
/// failure in the window continues an older streak.
pub const LOOKBACK_DAYS: u32 = 30;

/// Everything the routes need, read once.
pub struct Data<'a> {
    pub deps: &'a [Deployment],
    pub index: &'a Index,
    pub settings: &'a Settings,
    /// Current application names from `resources`, by uuid; the recorded name is used otherwise.
    pub names: &'a HashMap<String, String>,
    pub now: i64,
}

impl Data<'_> {
    fn name(&self, d: &Deployment) -> String {
        self.names
            .get(&d.app)
            .cloned()
            .unwrap_or_else(|| d.app_name.clone())
    }

    fn app_name(&self, uuid: &str) -> Option<String> {
        self.names.get(uuid).cloned().or_else(|| {
            self.deps
                .iter()
                .rev()
                .find(|d| d.app == uuid)
                .map(|d| d.app_name.clone())
        })
    }

    fn deployment(&self, d: &Deployment) -> Value {
        json!({
            "at": rfc3339(d.at),
            "application_uuid": d.app,
            "application_name": self.name(d),
            "project": d.project,
            "environment": d.environment,
            "outcome": if d.succeeded { "succeeded" } else { "failed" },
            "pull_request": (d.pull_request != 0).then_some(d.pull_request),
            "deployment_uuid": (!d.deployment.is_empty()).then_some(&d.deployment),
            "commit": (!d.commit.is_empty()).then_some(&d.commit),
            "lead_time_seconds": d.lead_time(),
        })
    }

    fn window(&self, days: u32) -> Window {
        Window {
            days,
            end: self.now,
        }
    }

    /// `GET /metrics.json`: the team's metrics, its applications (top `top` by deployments) and its latest
    /// deployments; with `app`, that application's metrics and latest deployments.
    pub fn metrics_json(&self, days: u32, app: Option<&str>, top: usize) -> Value {
        let window = self.window(days);
        let filter = self.settings.filter(app);
        let m = metrics::compute(self.deps, window, &filter);
        let recent: Vec<Value> = self
            .deps
            .iter()
            .rev()
            .filter(|d| window.contains(d.at) && filter.counts(d))
            .take(if app.is_some() {
                RECENT_APP
            } else {
                RECENT_TEAM
            })
            .map(|d| self.deployment(d))
            .collect();
        let mut out = json!({
            "scope": if app.is_some() { "application" } else { "team" },
            "window_days": days,
            "from": rfc3339(window.start()),
            "to": rfc3339(window.end),
            "settings": self.settings.json(),
            "metrics": m,
            "recent": recent,
        });
        if let Some(uuid) = app {
            out["application"] = json!({"uuid": uuid, "name": self.app_name(uuid)});
        } else {
            let apps = metrics::per_application(self.deps, window, &filter);
            out["applications_total"] = json!(apps.len());
            out["applications"] = apps
                .into_iter()
                .take(top)
                .map(|mut a| {
                    if let Some(n) = self.names.get(&a.uuid) {
                        a.name.clone_from(n);
                    }
                    let mut v = json!(a);
                    v["last_deployment_at"] = json!(rfc3339(a.last_deployment_at));
                    v["deployment_frequency_band"] = json!(a.metrics.deployment_frequency.band);
                    v["change_failure_rate_band"] = json!(a.metrics.change_failure_rate.band);
                    v["time_to_restore_band"] = json!(a.metrics.time_to_restore.band);
                    v
                })
                .collect();
            out["storage"] = json!({
                "deployments": self.index.count(),
                "bytes": self.index.bytes(),
                "oldest": self.index.oldest().map(rfc3339),
            });
        }
        out
    }

    /// `GET /metrics.csv`: one line for the team, then one per application with deployments in the window.
    pub fn metrics_csv(&self, days: u32) -> String {
        let window = self.window(days);
        let filter = self.settings.filter(None);
        let mut w = Writer::new(&[
            "scope",
            "application_uuid",
            "application_name",
            "window_days",
            "from",
            "to",
            "deployments",
            "succeeded",
            "failed",
            "deployments_per_day",
            "deployments_per_week",
            "deployment_frequency_band",
            "change_failure_rate",
            "change_failure_rate_band",
            "restored",
            "unrestored",
            "time_to_restore_median_seconds",
            "time_to_restore_p90_seconds",
            "time_to_restore_band",
            "lead_time_samples",
            "lead_time_median_seconds",
            "lead_time_p90_seconds",
            "lead_time_band",
        ]);
        let line = |scope: &str, uuid: &str, name: &str, m: &Metrics| -> Vec<String> {
            let band = |b: Option<Band>| b.map(|b| b.as_str().to_owned()).unwrap_or_default();
            vec![
                scope.into(),
                uuid.into(),
                name.into(),
                days.to_string(),
                rfc3339(window.start()),
                rfc3339(window.end),
                m.deployments.to_string(),
                m.succeeded.to_string(),
                m.failed.to_string(),
                m.deployment_frequency.per_day.to_string(),
                m.deployment_frequency.per_week.to_string(),
                band(m.deployment_frequency.band),
                opt(m.change_failure_rate.rate),
                band(m.change_failure_rate.band),
                m.time_to_restore.restored.to_string(),
                m.time_to_restore.unrestored.to_string(),
                opt(m.time_to_restore.median_seconds),
                opt(m.time_to_restore.p90_seconds),
                band(m.time_to_restore.band),
                m.lead_time.samples.to_string(),
                opt(m.lead_time.median_seconds),
                opt(m.lead_time.p90_seconds),
                band(m.lead_time.band),
            ]
        };
        w.row(line(
            "team",
            "",
            "",
            &metrics::compute(self.deps, window, &filter),
        ));
        for a in metrics::per_application(self.deps, window, &filter) {
            let name = self.names.get(&a.uuid).unwrap_or(&a.name);
            w.row(line("application", &a.uuid, name, &a.metrics));
        }
        w.finish()
    }

    /// `GET /deployments.csv`: every recorded deployment in the window, oldest first, whether the team's
    /// settings count it or not (`counted` says).
    pub fn deployments_csv(&self, days: u32, app: Option<&str>) -> String {
        let window = self.window(days);
        let filter = self.settings.filter(None);
        let mut w = Writer::new(&[
            "finished_at",
            "application_uuid",
            "application_name",
            "project",
            "environment",
            "outcome",
            "pull_request",
            "deployment_uuid",
            "commit_sha",
            "commit_at",
            "lead_time_seconds",
            "counted",
        ]);
        for d in self
            .deps
            .iter()
            .filter(|d| window.contains(d.at) && app.is_none_or(|a| a == d.app))
        {
            w.row([
                rfc3339(d.at),
                d.app.clone(),
                self.name(d),
                d.project.clone(),
                d.environment.clone(),
                (if d.succeeded { "succeeded" } else { "failed" }).to_owned(),
                if d.pull_request == 0 {
                    String::new()
                } else {
                    d.pull_request.to_string()
                },
                d.deployment.clone(),
                d.commit.clone(),
                d.commit_at.map(rfc3339).unwrap_or_default(),
                opt(d.lead_time()),
                (if filter.counts(d) { "yes" } else { "no" }).to_owned(),
            ]);
        }
        w.finish()
    }
}

/// The team's resources, as `resources` returns them.
#[derive(Debug, Default)]
pub struct Inventory {
    /// Each project with its environments.
    pub projects: Vec<(Value, Vec<Value>)>,
    pub applications: Vec<Value>,
    pub services: Vec<Value>,
    pub databases: Vec<Value>,
    pub servers: Vec<Value>,
}

/// A scalar field as text; objects, arrays and null are empty.
fn scalar(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// `GET /inventory.csv`: the team's projects, environments, applications, services, databases and servers,
/// with only the fields named here. `resources` already leaves out everything that needs `read:sensitive`.
pub fn inventory_csv(inv: &Inventory) -> String {
    let mut w = Writer::new(&[
        "kind",
        "name",
        "uuid",
        "type",
        "status",
        "address",
        "project",
        "environment",
    ]);
    // Environment id → (environment, project).
    let mut envs: HashMap<String, (String, String)> = HashMap::new();
    for (project, environments) in &inv.projects {
        let pname = scalar(project, "name");
        w.row([
            "project".into(),
            pname.clone(),
            scalar(project, "uuid"),
            String::new(),
            String::new(),
            String::new(),
            pname.clone(),
            String::new(),
        ]);
        for e in environments {
            let ename = scalar(e, "name");
            envs.insert(scalar(e, "id"), (ename.clone(), pname.clone()));
            w.row([
                "environment".into(),
                ename.clone(),
                scalar(e, "uuid"),
                String::new(),
                String::new(),
                String::new(),
                pname.clone(),
                ename,
            ]);
        }
    }
    let placed = |v: &Value| -> (String, String) {
        envs.get(&scalar(v, "environment_id"))
            .map(|(e, p)| (p.clone(), e.clone()))
            .unwrap_or_default()
    };
    for (kind, list, kind_field, address_field) in [
        ("application", &inv.applications, "build_pack", "fqdn"),
        ("service", &inv.services, "service_type", "fqdn"),
        ("database", &inv.databases, "database_type", ""),
    ] {
        for v in list {
            let (project, environment) = placed(v);
            w.row([
                kind.into(),
                scalar(v, "name"),
                scalar(v, "uuid"),
                scalar(v, kind_field),
                scalar(v, "status"),
                if address_field.is_empty() {
                    String::new()
                } else {
                    scalar(v, address_field)
                },
                project,
                environment,
            ]);
        }
    }
    for s in &inv.servers {
        let status = match s.get("is_reachable") {
            Some(Value::Bool(true)) => "reachable",
            Some(Value::Bool(false)) => "unreachable",
            _ => "",
        };
        w.row([
            "server".into(),
            scalar(s, "name"),
            scalar(s, "uuid"),
            String::new(),
            status.into(),
            scalar(s, "ip"),
            String::new(),
            String::new(),
        ]);
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{NOW, dep};
    use crate::time::DAY;

    fn settings() -> Settings {
        Settings::read(|_| None)
    }

    #[test]
    fn settings_and_queries() {
        assert_eq!(
            settings(),
            Settings {
                default_window: 30,
                include_previews: false,
                environments: vec![],
                retention_days: 180
            }
        );
        let s = Settings::read(|k| {
            Some(
                match k {
                    "default_window" => "90",
                    "include_previews" => "yes",
                    "environments" => "Production",
                    _ => "45",
                }
                .into(),
            )
        });
        assert_eq!(
            (s.default_window, s.include_previews, s.retention_days),
            (90, true, 45)
        );
        assert_eq!(s.environments, ["production"]);
        assert_eq!(Settings::read(|_| Some("7".into())).default_window, 30);

        assert_eq!(query("a=1&window=90", "window").as_deref(), Some("90"));
        assert_eq!(query("app=a%2Db+c", "app").as_deref(), Some("a-b c"));
        assert_eq!(query("app=%zz", "app"), None);
        assert_eq!(query("app=%2", "app"), None);
        assert_eq!(query("app=%+1", "app"), None);
        assert_eq!(query("flag", "flag").as_deref(), Some(""));
        assert_eq!(query("", "app"), None);
        assert_eq!(window_days("", &settings()), Ok(30));
        assert_eq!(window_days("window=90", &settings()), Ok(90));
        assert_eq!(window_days("window=90d", &settings()), Ok(90));
        assert!(window_days("window=7", &settings()).is_err());
        assert!(window_days("window=x", &settings()).is_err());
        assert_eq!(top(""), 10);
        assert_eq!(top("top=500"), 100);
        assert_eq!(top("top=0"), 1);
        assert!(valid_uuid("abc-123_x"));
        assert!(!valid_uuid("a/b"));
        assert!(!valid_uuid(""));
    }

    fn data_with<'a>(
        deps: &'a [Deployment],
        index: &'a Index,
        settings: &'a Settings,
        names: &'a HashMap<String, String>,
    ) -> Data<'a> {
        Data {
            deps,
            index,
            settings,
            names,
            now: NOW,
        }
    }

    #[test]
    fn metrics_json_for_the_team_and_an_application() {
        let mut preview = dep(NOW - 3 * DAY, "web", false);
        preview.pull_request = 12;
        let deps = vec![
            dep(NOW - 40 * DAY, "web", true),
            dep(NOW - 10 * DAY, "web", false),
            dep(NOW - 10 * DAY + 600, "web", true),
            dep(NOW - 5 * DAY, "api", true),
            preview,
            dep(NOW - DAY, "web", true),
        ];
        let names = HashMap::from([("web".to_owned(), "Web (renamed)".to_owned())]);
        let (index, s) = (Index::default(), settings());
        let data = data_with(&deps, &index, &s, &names);
        let v = data.metrics_json(30, None, 10);
        assert_eq!(v["scope"], "team");
        assert_eq!(v["from"], "2026-10-07T12:00:00Z");
        assert_eq!(v["to"], "2026-11-06T12:00:00Z");
        let m = &v["metrics"];
        assert_eq!(m["deployments"], 4);
        assert_eq!(m["deployment_frequency"]["deployments"], 3);
        assert_eq!(m["deployment_frequency"]["per_day"], 0.1);
        assert_eq!(m["deployment_frequency"]["band"], "high");
        assert_eq!(m["change_failure_rate"]["rate"], 0.25);
        assert_eq!(m["change_failure_rate"]["band"], "high");
        assert_eq!(m["time_to_restore"]["median_seconds"], 600);
        assert_eq!(m["time_to_restore"]["band"], "elite");
        assert_eq!(m["lead_time"]["available"], false);
        assert_eq!(m["lead_time"]["reason"], metrics::NO_COMMIT_TIMES);
        assert_eq!(v["applications_total"], 2);
        assert_eq!(v["applications"][0]["uuid"], "web");
        assert_eq!(v["applications"][0]["name"], "Web (renamed)");
        assert_eq!(v["applications"][0]["deployments"], 3);
        assert_eq!(
            v["applications"][0]["last_deployment_at"],
            "2026-11-05T12:00:00Z"
        );
        assert_eq!(v["applications"][1]["name"], "api-name");
        assert_eq!(v["recent"].as_array().unwrap().len(), 4, "no previews");
        assert_eq!(v["recent"][0]["at"], "2026-11-05T12:00:00Z");
        assert_eq!(
            data.metrics_json(30, None, 1)["applications"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let a = data.metrics_json(90, Some("web"), 10);
        assert_eq!(a["scope"], "application");
        assert_eq!(
            a["application"],
            json!({"uuid": "web", "name": "Web (renamed)"})
        );
        assert_eq!(a["metrics"]["deployments"], 4);
        assert_eq!(a["recent"].as_array().unwrap().len(), 4);
        assert!(a.get("applications").is_none());
        let none = data.metrics_json(30, Some("gone"), 10);
        assert_eq!(none["application"]["name"], Value::Null);
        assert_eq!(none["metrics"]["deployments"], 0);
    }

    #[test]
    fn csv_exports() {
        let mut evil = dep(NOW - 2 * DAY, "evil", false);
        evil.app_name = "=HYPERLINK(\"http://x\")".into();
        evil.environment = "prod, eu".into();
        let mut preview = dep(NOW - DAY, "web", true);
        preview.pull_request = 3;
        preview.commit = "abcdef1".into();
        preview.commit_at = Some(NOW - DAY - 90);
        let deps = vec![dep(NOW - 50 * DAY, "web", true), evil, preview];
        let (index, s, names) = (Index::default(), settings(), HashMap::new());
        let data = data_with(&deps, &index, &s, &names);

        let csv = data.deployments_csv(30, None);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "finished_at,application_uuid,application_name,project,environment,outcome,pull_request,deployment_uuid,commit_sha,commit_at,lead_time_seconds,counted"
        );
        assert_eq!(
            lines[1],
            format!(
                "2026-11-04T12:00:00Z,evil,\"'=HYPERLINK(\"\"http://x\"\")\",Shop,\"prod, eu\",failed,,dep-{}-evil-false,,,,yes",
                NOW - 2 * DAY
            )
        );
        assert_eq!(
            lines[2],
            format!(
                "2026-11-05T12:00:00Z,web,web-name,Shop,production,succeeded,3,dep-{}-web-true,abcdef1,2026-11-05T11:58:30Z,90,no",
                NOW - DAY
            )
        );
        assert_eq!(
            lines.len(),
            4,
            "the 50-day-old one is outside; a final CRLF"
        );
        assert_eq!(data.deployments_csv(90, Some("web")).lines().count(), 3);

        let csv = data.metrics_csv(30);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert!(
            lines[0].starts_with("scope,application_uuid,application_name,window_days,from,to,")
        );
        assert_eq!(
            lines[1],
            "team,,,30,2026-10-07T12:00:00Z,2026-11-06T12:00:00Z,1,0,1,0,0,low,1,low,0,1,,,,0,,,"
        );
        assert!(
            lines[2].starts_with("application,evil,\"'=HYPERLINK("),
            "{}",
            lines[2]
        );
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn inventory_has_only_the_named_fields() {
        let inv = Inventory {
            projects: vec![(
                json!({"id": 1, "uuid": "p1", "name": "Shop", "description": "=cmd"}),
                vec![json!({"id": 7, "uuid": "e1", "name": "production", "project_id": 1})],
            )],
            applications: vec![json!({
                "uuid": "a1", "name": "web", "status": "running:healthy", "fqdn": "https://web.example.com",
                "build_pack": "dockerimage", "environment_id": 7, "git_repository": "x", "manual_webhook_secret_github": "nope"
            })],
            services: vec![json!({"uuid": "s1", "name": "-svc", "environment_id": 99})],
            databases: vec![json!({
                "uuid": "d1", "name": "db", "status": "exited", "database_type": "standalone-postgresql",
                "environment_id": 7, "postgres_password": "hunter2", "internal_db_url": "postgres://u:hunter2@db"
            })],
            servers: vec![
                json!({"uuid": "v1", "name": "box", "ip": "203.0.113.5", "is_reachable": true, "user": "root"}),
                json!({"uuid": "v2", "name": "gone", "ip": "203.0.113.6", "is_reachable": false}),
            ],
        };
        let csv = inventory_csv(&inv);
        assert_eq!(
            csv,
            "kind,name,uuid,type,status,address,project,environment\r\n\
             project,Shop,p1,,,,Shop,\r\n\
             environment,production,e1,,,,Shop,production\r\n\
             application,web,a1,dockerimage,running:healthy,https://web.example.com,Shop,production\r\n\
             service,'-svc,s1,,,,,\r\n\
             database,db,d1,standalone-postgresql,exited,,Shop,production\r\n\
             server,box,v1,,reachable,203.0.113.5,,\r\n\
             server,gone,v2,,unreachable,203.0.113.6,,\r\n"
        );
        assert!(!csv.contains("hunter2") && !csv.contains("nope") && !csv.contains("=cmd"));
        assert_eq!(
            inventory_csv(&Inventory::default()),
            "kind,name,uuid,type,status,address,project,environment\r\n"
        );
    }
}
