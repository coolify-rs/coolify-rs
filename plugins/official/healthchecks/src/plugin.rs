//! The component's glue: settings in, pings out, the log and learned names in storage, routes and the panel.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, config, http, storage, ui};
use serde_json::json;

use crate::ping::{self, Entry, Log, Names, Status, html, utc};

/// No ping starts after this much of an event's 5 s. Each request is also cut off by the host before the
/// call's deadline, so a slow answer fails its ping and never the whole call.
const BUDGET: Duration = Duration::from_millis(3500);
const LOG: &str = "log";
const NAMES: &str = "names";

struct Healthchecks;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Plugin for Healthchecks {
    fn on_event(event: Event) -> Result<(), String> {
        let Some(key) = config::get("ping_key") else {
            return Ok(());
        };
        let create = config::get_or("create", "yes") == "yes";
        let prefix = ping::prefix(config::get("slug_prefix").as_deref());
        let mut names: Names = storage::get_json(NAMES)?.unwrap_or_default();
        let plan = ping::plan(&event.kind, &event.data(), &prefix, &names);
        if plan.pings.is_empty() {
            return Ok(());
        }
        let mut log: Log = storage::get_json(LOG)?.unwrap_or_default();
        let started = Instant::now();
        let mut failed = Vec::new();
        for p in &plan.pings {
            let (delivered, result) = if started.elapsed() > BUDGET {
                (false, "skipped: no time left for this event".to_owned())
            } else {
                match ping::request(&key, create, p) {
                    Err(e) => (false, e),
                    Ok(r) => match http::fetch(&http::Outgoing {
                        method: "POST".into(),
                        url: r.url,
                        headers: r.headers,
                        body: r.body,
                    }) {
                        Ok(resp) => (
                            (200..300).contains(&resp.status),
                            format!("HTTP {}", resp.status),
                        ),
                        // The host's reasons never hold the URL, so the ping key stays out of them.
                        Err(e) => (false, e),
                    },
                }
            };
            if !delivered {
                failed.push(format!("{}: {result}", p.slug));
            }
            log.record(Entry {
                slug: p.slug.clone(),
                label: p.label.clone(),
                status: p.status,
                at: now(),
                delivered,
                result,
            });
        }
        storage::set_json(LOG, &log)?;
        if let Some((name, slug)) = &plan.learn
            && names.learn(name, slug)
        {
            storage::set_json(NAMES, &names)?;
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        let log: Log = storage::get_json(LOG).ok().flatten().unwrap_or_default();
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/recent") => Response::json(
                200,
                &json!({ "configured": config::get("ping_key").is_some(), "checks": log.0 }),
            ),
            ("GET", "/panel") => panel(&log),
            _ => Response::json(404, &json!({ "message": "Not found." })),
        }
    }
}

fn panel(log: &Log) -> Response {
    let configured = config::get("ping_key").is_some();
    let mut out = String::from(
        r#"<style>
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: .25rem .75rem .25rem 0; vertical-align: top; }
th { font-weight: 600; }
code { font-size: 12px; }
.fail { color: #dc2626; }
.ok { color: #16a34a; }
</style>"#,
    );
    if !configured {
        out.push_str(
            "<p class=\"muted\">Set the project's ping key under Team → Plugins → Configure to start pinging \
             Healthchecks.io.</p>",
        );
    }
    if log.0.is_empty() {
        out.push_str("<p class=\"muted\">No pings yet. Each scheduled task and backup run pings its check.</p>");
        return ui::panel(&out);
    }
    out.push_str("<table><thead><tr><th>Check</th><th>Last ping</th><th>When</th><th>Healthchecks</th></tr></thead><tbody>");
    for e in &log.0 {
        let (status, class) = match e.status {
            Status::Success => ("success", "ok"),
            Status::Fail => ("fail", "fail"),
        };
        out.push_str(&format!(
            "<tr><td>{}<br><code class=\"muted\">{}</code></td><td class=\"{class}\">{status}</td><td>{}</td><td class=\"{}\">{}</td></tr>",
            html(&e.label),
            html(&e.slug),
            utc(e.at),
            if e.delivered { "" } else { "fail" },
            html(&e.result),
        ));
    }
    out.push_str("</tbody></table>");
    ui::panel(&out)
}

coolify_rs_plugin::export!(Healthchecks);
