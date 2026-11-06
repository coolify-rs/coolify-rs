//! Events to Healthchecks.io pings.
//!
//! Each job gets one check, addressed by slug under the project's ping key
//! (`https://hc-ping.com/<ping key>/<slug>`), so nothing has to be set up per job when "create checks
//! automatically" is on:
//!
//! | Job | Slug |
//! |---|---|
//! | Scheduled task | `<prefix>-task-<task uuid>` |
//! | Database backup | `<prefix>-backup-<database uuid>-<database name>` (one check per database dumped) |
//! | Storage backup | `<prefix>-volume-<resource uuid>-<volume or directory>` |
//!
//! A success pings the slug, a failure pings `<slug>/fail` with the end of the output. A database backup
//! whose S3 upload failed is a failure: the off-site copy is missing. `backup.missing` names the database by
//! name and the schedule by a uuid no other event carries, so it fails the checks this plugin pinged before
//! for a database of that name ([`Names`]); before any run was seen there is nothing to fail, and the check
//! itself (if it exists) raises the alarm when its period passes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const BASE: &str = "https://hc-ping.com";

/// Every kind the plugin subscribes to; the manifest lists the same.
pub const KINDS: &[&str] = &[
    "scheduled_task.succeeded",
    "scheduled_task.failed",
    "backup.succeeded",
    "backup.succeeded_with_s3_warning",
    "backup.failed",
    "backup.missing",
    "volume_backup.succeeded",
    "volume_backup.failed",
    "volume_backup.missing",
    "volume_backup.recovery_failed",
];

/// How much of a failure's output a ping carries: its end, where the error is.
pub const OUTPUT: usize = 1000;
/// Longest slug sent; longer ones are cut and end with a hash, so they stay unique.
pub const MAX_SLUG: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Fail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ping {
    pub slug: String,
    pub status: Status,
    /// What the job is, for the panel: "Task backup-files", "Backup of shop on pg".
    pub label: String,
    /// The ping's body: a line, and for failures the end of the output.
    pub body: String,
}

/// What an event asks for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub pings: Vec<Ping>,
    /// A database (by name) and the slug of one of its backups, remembered for `backup.missing`.
    pub learn: Option<(String, String)>,
}

/// Database names and the backup slugs pinged for them. Kept in storage, bounded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Names(pub BTreeMap<String, Vec<String>>);

pub const MAX_NAMES: usize = 200;
pub const MAX_SLUGS_PER_NAME: usize = 10;

impl Names {
    /// Remembers `slug` for `name`; returns whether anything changed.
    pub fn learn(&mut self, name: &str, slug: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        if !self.0.contains_key(name) && self.0.len() >= MAX_NAMES {
            // Forget an arbitrary (the first) name rather than grow without bound.
            if let Some(first) = self.0.keys().next().cloned() {
                self.0.remove(&first);
            }
        }
        let slugs = self.0.entry(name.to_owned()).or_default();
        if slugs.iter().any(|s| s == slug) {
            return false;
        }
        slugs.push(slug.to_owned());
        if slugs.len() > MAX_SLUGS_PER_NAME {
            slugs.remove(0);
        }
        true
    }
}

/// The string at `path` in `v`, or `""`.
fn field(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for key in path {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return String::new(),
        }
    }
    match cur {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The last `max` characters of `text`, with `…` in front when something was cut.
pub fn tail(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let clean = clean.trim();
    let n = clean.chars().count();
    if n <= max {
        return clean.to_owned();
    }
    let mut out = String::from("…");
    out.extend(clean.chars().skip(n - max.saturating_sub(1)));
    out
}

/// `a-z`, `0-9`, `_` and `-`, the characters of a Healthchecks slug; anything else becomes one `-`.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// The team's slug prefix, made safe; `coolify` when it is empty.
pub fn prefix(setting: Option<&str>) -> String {
    let p = slugify(setting.unwrap_or_default());
    if p.is_empty() {
        "coolify".into()
    } else {
        p.chars().take(40).collect()
    }
}

/// A slug from parts, each slugified, at most [`MAX_SLUG`] characters. `None` when an identifying part is
/// empty: a job without an id gets no check rather than a shared one.
fn slug(prefix: &str, kind: &str, parts: &[&str]) -> Option<String> {
    let parts: Vec<String> = parts.iter().map(|p| slugify(p)).collect();
    if parts.first().is_none_or(|p| p.is_empty()) {
        return None;
    }
    let mut s = format!("{prefix}-{kind}");
    for p in parts.iter().filter(|p| !p.is_empty()) {
        s.push('-');
        s.push_str(p);
    }
    if s.len() <= MAX_SLUG {
        return Some(s);
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let keep = MAX_SLUG - 13;
    Some(format!(
        "{}-{:012x}",
        s[..keep].trim_end_matches('-'),
        h & 0xffff_ffff_ffff
    ))
}

fn or<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() { fallback } else { s }
}

fn with_output(line: String, output: &str) -> String {
    let output = tail(output, OUTPUT);
    if output.is_empty() {
        line
    } else {
        format!("{line}\n\n{output}")
    }
}

/// The pings for an event. `names` is what was learned for `backup.missing`.
pub fn plan(kind: &str, p: &Value, prefix: &str, names: &Names) -> Plan {
    let mut plan = Plan::default();
    match kind {
        "scheduled_task.succeeded" | "scheduled_task.failed" => {
            let name = field(p, &["name"]);
            let Some(slug) = slug(prefix, "task", &[&field(p, &["uuid"])]) else {
                return plan;
            };
            let label = format!("Task {}", or(&name, "(unnamed)"));
            let (status, body) = if kind == "scheduled_task.succeeded" {
                (Status::Success, format!("{label} succeeded."))
            } else {
                (
                    Status::Fail,
                    with_output(format!("{label} failed."), &field(p, &["output"])),
                )
            };
            plan.pings.push(Ping {
                slug,
                status,
                label,
                body,
            });
        }
        "backup.succeeded" | "backup.failed" | "backup.succeeded_with_s3_warning" => {
            let empty = Value::Null;
            let b = if kind == "backup.succeeded" {
                p
            } else {
                p.get("backup").unwrap_or(&empty)
            };
            let name = field(b, &["name"]);
            let dbname = field(b, &["database_name"]);
            let Some(slug) = slug(prefix, "backup", &[&field(b, &["uuid"]), &dbname]) else {
                return plan;
            };
            let label = if dbname.is_empty() || dbname == name {
                format!("Backup of {}", or(&name, "a database"))
            } else {
                format!("Backup of {dbname} on {}", or(&name, "a database"))
            };
            let (status, body) = match kind {
                "backup.succeeded" => (Status::Success, format!("{label} succeeded.")),
                "backup.failed" => (
                    Status::Fail,
                    with_output(format!("{label} failed."), &field(p, &["output"])),
                ),
                _ => (
                    Status::Fail,
                    with_output(
                        format!("{label} succeeded, but uploading it to S3 failed."),
                        &field(p, &["s3_error"]),
                    ),
                ),
            };
            plan.learn = Some((name, slug.clone()));
            plan.pings.push(Ping {
                slug,
                status,
                label,
                body,
            });
        }
        "backup.missing" => {
            let name = field(p, &["database_name"]);
            let days = field(p, &["days"]);
            let mut body = format!("No backup of {} has run", or(&name, "a database"));
            if !days.is_empty() {
                body.push_str(&format!(" in the last {days} days"));
            }
            body.push('.');
            for slug in names.0.get(&name).into_iter().flatten() {
                plan.pings.push(Ping {
                    slug: slug.clone(),
                    status: Status::Fail,
                    label: format!("Backup of {}", or(&name, "a database")),
                    body: body.clone(),
                });
            }
        }
        "volume_backup.succeeded"
        | "volume_backup.failed"
        | "volume_backup.missing"
        | "volume_backup.recovery_failed" => {
            let Some(b) = p.get("backup").filter(|b| b.is_object()) else {
                // A recovery whose backup is gone: no check to fail.
                return plan;
            };
            let target = field(b, &["target"]);
            let resource = field(b, &["resource_name"]);
            let url = field(b, &["url"]);
            // The payload names no schedule: the resource (its page's uuid, else its name) and the target are
            // what stays the same from one run to the next.
            let resource_id = url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty() && url.contains('/'))
                .map_or_else(|| resource.clone(), str::to_owned);
            let Some(slug) = slug(prefix, "volume", &[&resource_id, &target]) else {
                return plan;
            };
            let label = format!(
                "Storage backup of {} ({})",
                or(&target, "storage"),
                or(&resource, "unknown resource")
            );
            let (status, body) = match kind {
                "volume_backup.succeeded" => (Status::Success, format!("{label} succeeded.")),
                "volume_backup.failed" => (
                    Status::Fail,
                    with_output(format!("{label} failed."), &field(p, &["output"])),
                ),
                "volume_backup.missing" => {
                    let days = field(p, &["days"]);
                    let mut body = format!("No {} has run", label.to_lowercase());
                    if !days.is_empty() {
                        body.push_str(&format!(" in the last {days} days"));
                    }
                    body.push('.');
                    (Status::Fail, body)
                }
                _ => (
                    Status::Fail,
                    format!(
                        "Recovery after the {} failed ({}).",
                        label.to_lowercase(),
                        or(&field(p, &["recovery_error"]), "unknown error")
                    ),
                ),
            };
            plan.pings.push(Ping {
                slug,
                status,
                label,
                body,
            });
        }
        _ => {}
    }
    plan
}

/// One HTTP request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The ping as a request: `POST https://hc-ping.com/<key>/<slug>[/fail][?create=1]` with the body as text.
/// The key must be a Healthchecks ping key (letters, digits, `-` and `_`), so it can't change the path.
pub fn request(ping_key: &str, create: bool, ping: &Ping) -> Result<Request, String> {
    let ok = !ping_key.is_empty()
        && ping_key.len() <= 128
        && ping_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err(
            "the ping key may only contain letters, digits, - and _; copy it from the project's settings"
                .into(),
        );
    }
    let mut url = format!("{BASE}/{ping_key}/{}", ping.slug);
    if ping.status == Status::Fail {
        url.push_str("/fail");
    }
    if create {
        url.push_str("?create=1");
    }
    Ok(Request {
        url,
        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
        body: ping.body.as_bytes().to_vec(),
    })
}

/// One line of the log: the last ping of a check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub slug: String,
    pub label: String,
    pub status: Status,
    /// Unix seconds.
    pub at: u64,
    /// Whether Healthchecks accepted the ping.
    pub delivered: bool,
    /// `HTTP 200`, or why the ping could not be sent.
    pub result: String,
}

/// The checks pinged recently, newest first, one entry per slug. Kept in storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Log(pub Vec<Entry>);

pub const MAX_LOG: usize = 50;

impl Log {
    pub fn record(&mut self, entry: Entry) {
        self.0.retain(|e| e.slug != entry.slug);
        self.0.insert(0, entry);
        self.0.truncate(MAX_LOG);
    }
}

/// `YYYY-MM-DD HH:MM UTC` for Unix seconds.
pub fn utc(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs = ts % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// `text` escaped for HTML.
pub fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(kind: &str, p: Value) -> Ping {
        let plan = plan(kind, &p, "coolify", &Names::default());
        assert_eq!(plan.pings.len(), 1, "{kind}: {plan:?}");
        plan.pings[0].clone()
    }

    #[test]
    fn tasks() {
        let task = json!({"name": "nightly report", "uuid": "t1", "output": "line\r\nTraceback: boom", "resource": null});
        let ok = one("scheduled_task.succeeded", task.clone());
        assert_eq!(
            ok,
            Ping {
                slug: "coolify-task-t1".into(),
                status: Status::Success,
                label: "Task nightly report".into(),
                body: "Task nightly report succeeded.".into(),
            }
        );
        let failed = one("scheduled_task.failed", task);
        assert_eq!(failed.slug, "coolify-task-t1");
        assert_eq!(failed.status, Status::Fail);
        assert_eq!(
            failed.body,
            "Task nightly report failed.\n\nline\nTraceback: boom"
        );
        // No uuid, no check.
        assert!(
            plan(
                "scheduled_task.failed",
                &json!({"name": "x"}),
                "coolify",
                &Names::default()
            )
            .pings
            .is_empty()
        );
    }

    #[test]
    fn database_backups_and_missing_ones() {
        let backup = json!({"name": "pg", "uuid": "db1", "project_uuid": "p", "environment_uuid": "e",
                            "database_name": "shop", "frequency": "daily"});
        let ok = plan("backup.succeeded", &backup, "acme", &Names::default());
        assert_eq!(ok.pings[0].slug, "acme-backup-db1-shop");
        assert_eq!(ok.pings[0].label, "Backup of shop on pg");
        assert_eq!(ok.learn, Some(("pg".into(), "acme-backup-db1-shop".into())));

        let output = format!("{}\npg_dump: error: connection refused", "x".repeat(4000));
        let failed = plan(
            "backup.failed",
            &json!({"backup": backup, "output": output}),
            "acme",
            &Names::default(),
        );
        let f = &failed.pings[0];
        assert_eq!(
            (f.slug.as_str(), f.status),
            ("acme-backup-db1-shop", Status::Fail)
        );
        assert!(f.body.starts_with("Backup of shop on pg failed.\n\n…"));
        assert!(f.body.ends_with("connection refused"));
        assert!(f.body.chars().count() <= 1000 + 40, "{}", f.body.len());

        let s3 = plan(
            "backup.succeeded_with_s3_warning",
            &json!({"backup": backup, "s3_error": "AccessDenied", "s3_uuid": "s"}),
            "acme",
            &Names::default(),
        );
        assert_eq!(s3.pings[0].status, Status::Fail);
        assert_eq!(
            s3.pings[0].body,
            "Backup of shop on pg succeeded, but uploading it to S3 failed.\n\nAccessDenied"
        );

        // `backup.missing` fails the checks learned for that database name, and nothing before any was.
        let missing = json!({"database_name": "pg", "backup_uuid": "sched1", "days": 2, "last_execution_at": null});
        assert!(
            plan("backup.missing", &missing, "acme", &Names::default())
                .pings
                .is_empty()
        );
        let mut names = Names::default();
        assert!(names.learn("pg", "acme-backup-db1-shop"));
        assert!(!names.learn("pg", "acme-backup-db1-shop"));
        names.learn("pg", "acme-backup-db1-analytics");
        let m = plan("backup.missing", &missing, "acme", &names);
        let slugs: Vec<&str> = m.pings.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, ["acme-backup-db1-shop", "acme-backup-db1-analytics"]);
        assert_eq!(
            m.pings[0].body,
            "No backup of pg has run in the last 2 days."
        );
        assert_eq!(m.pings[0].status, Status::Fail);
    }

    #[test]
    fn storage_backups() {
        let backup = json!({"resource_name": "files", "target": "Volume app_data", "frequency": "daily",
                            "url": "https://coolify.example.com/project/p/environment/e/application/app9"});
        let ok = one(
            "volume_backup.succeeded",
            json!({"backup": backup, "warning": null}),
        );
        assert_eq!(ok.slug, "coolify-volume-app9-volume-app_data");
        assert_eq!(ok.label, "Storage backup of Volume app_data (files)");
        let missing = one(
            "volume_backup.missing",
            json!({"backup": backup, "backup_uuid": "s", "days": 1}),
        );
        assert_eq!(
            (missing.slug.as_str(), missing.status),
            (ok.slug.as_str(), Status::Fail)
        );
        assert_eq!(
            missing.body,
            "No storage backup of volume app_data (files) has run in the last 1 days."
        );
        let recovery = one(
            "volume_backup.recovery_failed",
            json!({"backup": backup, "execution_uuid": "x", "recovery_error": "s3_auth", "containers_stopped": false}),
        );
        assert_eq!(recovery.slug, ok.slug);
        // Without a link the resource's name identifies it.
        let unlinked = one(
            "volume_backup.failed",
            json!({"backup": {"resource_name": "Files", "target": "Directory /srv/data"}, "output": ""}),
        );
        assert_eq!(unlinked.slug, "coolify-volume-files-directory-srv-data");
        assert_eq!(
            unlinked.body,
            "Storage backup of Directory /srv/data (Files) failed."
        );
        assert!(
            plan(
                "volume_backup.recovery_failed",
                &json!({"backup": null}),
                "coolify",
                &Names::default()
            )
            .pings
            .is_empty()
        );
    }

    #[test]
    fn slugs_prefixes_and_requests() {
        assert_eq!(prefix(None), "coolify");
        assert_eq!(prefix(Some("  ")), "coolify");
        assert_eq!(prefix(Some("My Team/Prod")), "my-team-prod");
        let long = slug("coolify", "volume", &[&"a".repeat(80), &"b".repeat(80)]).unwrap();
        assert_eq!(long.len(), MAX_SLUG);
        assert_ne!(
            long,
            slug("coolify", "volume", &[&"a".repeat(80), &"c".repeat(80)]).unwrap()
        );

        let ping = Ping {
            slug: "coolify-task-t1".into(),
            status: Status::Fail,
            label: "Task x".into(),
            body: "Task x failed.".into(),
        };
        let r = request("Ab_c-9", true, &ping).unwrap();
        assert_eq!(
            r.url,
            "https://hc-ping.com/Ab_c-9/coolify-task-t1/fail?create=1"
        );
        assert_eq!(r.body, b"Task x failed.");
        let ok = Ping {
            status: Status::Success,
            ..ping.clone()
        };
        assert_eq!(
            request("k", false, &ok).unwrap().url,
            "https://hc-ping.com/k/coolify-task-t1"
        );
        for bad in ["", "a/b", "a?create=0", "k#x", "a b", "ä"] {
            let err = request(bad, true, &ping).unwrap_err();
            assert!(!err.contains(bad) || bad.is_empty(), "{err}");
        }
    }

    #[test]
    fn log_and_names_are_bounded() {
        let mut log = Log::default();
        for i in 0..60 {
            log.record(Entry {
                slug: format!("s{}", i % 55),
                label: String::new(),
                status: Status::Success,
                at: i,
                delivered: true,
                result: "HTTP 200".into(),
            });
        }
        assert_eq!(log.0.len(), MAX_LOG);
        assert_eq!(log.0[0].slug, "s4");
        assert_eq!(log.0[0].at, 59);
        assert_eq!(log.0.iter().filter(|e| e.slug == "s4").count(), 1);

        let mut names = Names::default();
        for i in 0..(MAX_NAMES + 3) {
            names.learn(&format!("n{i:04}"), "s");
        }
        assert_eq!(names.0.len(), MAX_NAMES);
        for i in 0..15 {
            names.learn("many", &format!("s{i}"));
        }
        assert_eq!(names.0["many"].len(), MAX_SLUGS_PER_NAME);
        assert_eq!(names.0["many"][0], "s5");
    }

    #[test]
    fn malformed_payloads_and_helpers() {
        for kind in KINDS {
            for p in [json!(null), json!({}), json!({"backup": "x", "uuid": 5})] {
                let _ = plan(kind, &p, "coolify", &Names::default());
            }
        }
        assert!(
            plan(
                "deployment.failed",
                &json!({"uuid": "x"}),
                "c",
                &Names::default()
            )
            .pings
            .is_empty()
        );
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_793_966_400), "2026-11-06 12:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(
            html("<a href='x'>&</a>"),
            "&lt;a href=&#39;x&#39;&gt;&amp;&lt;/a&gt;"
        );
    }
}
