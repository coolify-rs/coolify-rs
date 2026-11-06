//! `coolify-rs plugin check`: loads a component in the real sandbox, sends it every event kind it subscribes
//! to with a sample payload and with malformed input, calls its routes, and reports memory and time. It
//! needs no database, so the registry's CI can run it on every submitted version.
//!
//! A trap, a timeout or an invalid response is a failure. A plugin returning an error for input it cannot
//! use is fine, and is reported as a note.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::manifest::Manifest;
use crate::runtime::{CallError, Caller, Event, Host, Probe, Request, Settings};

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub id: Option<String>,
    pub version: Option<String>,
    pub sha256: String,
    pub size: usize,
    pub capabilities: Vec<String>,
    pub imports: Vec<String>,
    pub checks: Vec<Check>,
    pub limits: LimitsReport,
    /// The largest linear memory any call reached, in bytes.
    pub max_memory: usize,
    pub max_elapsed_ms: f64,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitsReport {
    pub memory: usize,
    pub event_deadline_ms: u128,
    pub request_deadline_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    /// `ok`, `status 200`, `plugin error: ...`, or why it failed.
    pub outcome: String,
    pub elapsed_ms: f64,
    pub peak_memory: usize,
}

/// A payload shaped like the notification the kind comes from.
pub fn sample_payload(kind: &str) -> Value {
    let base = json!({
        "team_id": 1,
        "message": format!("Sample {kind} event from coolify-rs plugin check."),
    });
    let extra = match kind.split('.').next().unwrap_or_default() {
        "deployment" | "application" | "container" => json!({
            "application_name": "web",
            "application_uuid": "a1b2c3d4e5f6g7h8i9j0k1l2",
            "deployment_uuid": "d1e2p3l4o5y6m7e8n9t0u1v2",
            "deployment_url": "https://coolify.example.com/project/p/environment/e/application/a/deployment/d",
            "fqdn": "https://web.example.com",
            "project_name": "Example",
            "environment_name": "production",
            "container_name": "web-a1b2c3",
        }),
        "backup" | "volume_backup" => json!({
            "database_name": "postgres",
            "database_type": "standalone-postgresql",
            "frequency": "0 0 * * *",
            "output": "backup finished",
            "s3_error": null,
        }),
        "scheduled_task" => json!({
            "task_name": "cleanup",
            "task_uuid": "t1a2s3k4u5u6i7d8",
            "output": "done",
        }),
        _ => json!({
            "server_name": "production",
            "server_uuid": "s1e2r3v4e5r6u7u8i9d0",
            "disk_usage": 91,
            "threshold": 80,
        }),
    };
    let mut out = base;
    if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    out
}

fn ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

struct Run {
    checks: Vec<Check>,
    notes: Vec<String>,
}

impl Run {
    fn record<T>(
        &mut self,
        name: String,
        probe: Probe<T>,
        judge: impl FnOnce(&T) -> Result<String, String>,
    ) {
        let (ok, outcome) = match &probe.result {
            Ok(v) => match judge(v) {
                Ok(o) => (true, o),
                Err(e) => (false, e),
            },
            Err(CallError::Plugin(_, e)) => {
                self.notes
                    .push(format!("{name}: the plugin returned an error: {e}"));
                (true, format!("plugin error: {e}"))
            }
            Err(e) => (false, e.to_string()),
        };
        self.checks.push(Check {
            name,
            ok,
            outcome,
            elapsed_ms: ms(probe.elapsed),
            peak_memory: probe.peak_memory,
        });
    }
}

fn event(kind: &str, team_id: i64, version: u32, payload: String) -> Event {
    Event {
        kind: kind.to_owned(),
        version,
        team_id,
        payload,
    }
}

fn request(method: &str, path: &str, query: &str, body: Vec<u8>) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body,
    }
}

/// Runs the conformance check. `settings` are the limits to hold the plugin to (the defaults in CI).
pub async fn check(component: &[u8], manifest_text: &str, settings: Settings) -> Report {
    let limits = settings.limits.clone();
    let mut report = Report {
        ok: false,
        id: None,
        version: None,
        sha256: hex::encode(Sha256::digest(component)),
        size: component.len(),
        capabilities: vec![],
        imports: vec![],
        checks: vec![],
        limits: LimitsReport {
            memory: limits.memory,
            event_deadline_ms: limits.event_deadline.as_millis(),
            request_deadline_ms: limits.request_deadline.as_millis(),
        },
        max_memory: 0,
        max_elapsed_ms: 0.0,
        errors: vec![],
        warnings: vec![],
    };
    let manifest = match Manifest::parse(manifest_text) {
        Ok(m) => m,
        Err(e) => {
            report.errors.push(e.to_string());
            return report;
        }
    };
    report.id = Some(manifest.id.clone());
    report.version = Some(manifest.version.clone());
    report.capabilities = manifest.capability_summary();
    let host = match Host::detached(settings) {
        Ok(h) => h,
        Err(e) => {
            report.errors.push(e.to_string());
            return report;
        }
    };
    let id = manifest.id.clone();
    let caps = manifest.capabilities.clone();
    match host.load_detached(manifest, component) {
        Ok(imports) => report.imports = imports,
        Err(e) => {
            report.errors.push(e.to_string());
            return report;
        }
    }
    let imports = |name: &str| {
        report
            .imports
            .iter()
            .any(|i| i.starts_with(&format!("coolify-rs:plugin/{name}@")))
    };
    if caps.storage && !imports("storage") {
        report
            .warnings
            .push("the manifest asks for storage, but the component never uses it".into());
    }
    if !caps.http.is_empty() && !imports("http") {
        report
            .warnings
            .push("the manifest asks for http, but the component never uses it".into());
    }

    let mut run = Run {
        checks: vec![],
        notes: vec![],
    };
    for kind in &caps.events {
        let payload = sample_payload(kind).to_string();
        let probe = host.probe_event(&id, event(kind, 1, 1, payload)).await;
        run.record(format!("event {kind}"), probe, |_| Ok("ok".into()));
    }
    if let Some(kind) = caps.events.first() {
        let malformed: Vec<(&str, Event)> = vec![
            ("payload that is not JSON", event(kind, 2, 1, "{not json".into())),
            ("payload that is a JSON array", event(kind, 2, 1, "[1,2,3]".into())),
            ("empty payload object", event(kind, 2, 1, "{}".into())),
            ("empty payload", event(kind, 2, 1, String::new())),
            (
                "payload with wrong types",
                event(
                    kind,
                    2,
                    1,
                    json!({"application_name": 42, "deployment_uuid": [], "server_name": {"a": null}})
                        .to_string(),
                ),
            ),
            (
                "payload with a 256 KiB string",
                event(kind, 2, 1, json!({"message": "x".repeat(256 * 1024)}).to_string()),
            ),
            (
                "unknown payload version",
                event(kind, 2, 999, sample_payload(kind).to_string()),
            ),
            (
                "unknown event kind",
                event("check.unknown_kind", 2, 1, "{}".into()),
            ),
            ("negative team id", event(kind, -1, 1, "{}".into())),
        ];
        for (what, e) in malformed {
            let probe = host.probe_event(&id, e).await;
            run.record(format!("malformed event: {what}"), probe, |_| {
                Ok("ok".into())
            });
        }
    }
    if caps.routes {
        let caller = |team_id| Caller {
            team_id,
            user_id: 1,
            abilities: vec!["read".into()],
        };
        let long = format!("/{}", "a/".repeat(2000));
        let requests: Vec<(&str, i64, Request)> = vec![
            ("GET /", 1, request("GET", "/", "", vec![])),
            (
                "GET an unknown path",
                1,
                request("GET", "/coolify-rs-check/unknown", "", vec![]),
            ),
            (
                "POST with a body that is not JSON",
                1,
                request("POST", "/", "", vec![0xff, 0xfe, 0x00, b'{']),
            ),
            (
                "PUT with a 512 KiB body",
                2,
                request("PUT", "/", "", vec![b'a'; 512 * 1024]),
            ),
            (
                "odd path and query",
                2,
                request("GET", "/%00/../\u{1F600}", "a=%ZZ&&=b&c", vec![]),
            ),
            ("a very long path", 2, request("DELETE", &long, "", vec![])),
            ("unknown method", 2, request("BREW", "/", "", vec![])),
        ];
        for (what, team, r) in requests {
            let probe = host.probe_request(&id, caller(team), r).await;
            run.record(format!("request: {what}"), probe, |resp| {
                if (100..=599).contains(&resp.status) {
                    Ok(format!("status {}", resp.status))
                } else {
                    Err(format!("invalid status {}", resp.status))
                }
            });
        }
    }
    if run.checks.is_empty() {
        report.warnings.push(
            "the plugin subscribes to no events and has no routes, so nothing could be called"
                .into(),
        );
    }

    report.max_memory = run.checks.iter().map(|c| c.peak_memory).max().unwrap_or(0);
    report.max_elapsed_ms = run.checks.iter().map(|c| c.elapsed_ms).fold(0.0, f64::max);
    if report.max_memory > limits.memory / 2 {
        report.warnings.push(format!(
            "a call used {} of the {} bytes of memory a plugin may use",
            report.max_memory, limits.memory
        ));
    }
    let tightest = limits.event_deadline.min(limits.request_deadline);
    if report.max_elapsed_ms > ms(tightest) / 2.0 {
        report.warnings.push(format!(
            "a call took {} ms, more than half of a deadline",
            report.max_elapsed_ms
        ));
    }
    for c in run.checks.iter().filter(|c| !c.ok) {
        report.errors.push(format!("{}: {}", c.name, c.outcome));
    }
    report.warnings.extend(run.notes);
    report.checks = run.checks;
    report.ok = report.errors.is_empty();
    report
}
