//! The component's entry points: events are recorded, routes answer from storage, settings and resources.
//!
//! Routes, under `/api/v1/ext/coolify-rs/delivery-metrics` (and `/ui/ext/...` for the panels' bridge):
//! - `GET /metrics.json?window=30|90&app=<uuid>&top=<n>`
//! - `GET /metrics.csv?window=30|90`
//! - `GET /deployments.csv?window=30|90&app=<uuid>`
//! - `GET /inventory.csv`
//! - `GET /panels/dashboard`, `GET /panels/application?resource=<uuid>`

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use coolify_rs_plugin::resources::{self, Kind};
use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, config, log, storage, ui};
use serde_json::{Value, json};

use crate::record;
use crate::report::{self, Data, Inventory, Settings};
use crate::store::{self, Kv, Limits, Recorded};
use crate::time::DAY;

struct Storage;

impl Kv for Storage {
    fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
        storage::get(key)
    }
    fn set(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
        storage::set(key, value)
    }
    fn delete(&mut self, key: &str) -> Result<(), String> {
        storage::delete(key)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        storage::keys(prefix)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A random number, for the inbox key of a deployment without a uuid.
fn nonce() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(SystemTime::now())
}

fn settings() -> Settings {
    Settings::read(config::get)
}

struct DeliveryMetrics;

impl Plugin for DeliveryMetrics {
    fn on_event(event: Event) -> Result<(), String> {
        let now = now();
        let dep = record::from_event(&event.kind, &event.data(), now)?;
        let retention = settings().retention_days;
        // Storage trouble never fails the event: the deployment is dropped and the reason logged.
        match store::record(&mut Storage, &dep, now, retention, Limits::DEFAULT, nonce()) {
            Ok(Recorded::Dropped(e)) => {
                log::warn(&format!("deployment kept for the next event: {e}"))
            }
            Ok(_) => {}
            Err(e) => log::warn(&format!("deployment not recorded: {e}")),
        }
        Ok(())
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        let q = request.query.as_str();
        let result = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/metrics.json") => metrics_json(q),
            ("GET", "/metrics.csv") => with_data(q, |d, days| csv(d.metrics_csv(days))),
            ("GET", "/deployments.csv") => match app_param(q) {
                Ok(app) => with_data(q, |d, days| csv(d.deployments_csv(days, app.as_deref()))),
                Err(r) => Ok(r),
            },
            ("GET", "/inventory.csv") => Ok(inventory().map_or_else(
                |e| message(502, &format!("The inventory could not be read: {e}")),
                csv,
            )),
            ("GET", "/panels/dashboard") => panel(None),
            ("GET", "/panels/application") => match report::query(q, "resource") {
                Some(uuid) if report::valid_uuid(&uuid) => panel(Some(&uuid)),
                _ => Ok(message(400, "The application is missing or invalid.")),
            },
            (
                _,
                "/metrics.json"
                | "/metrics.csv"
                | "/deployments.csv"
                | "/inventory.csv"
                | "/panels/dashboard"
                | "/panels/application",
            ) => Ok(message(405, "Method not allowed.")),
            _ => Ok(message(404, "Not found.")),
        };
        result.unwrap_or_else(|e| {
            log::warn(&format!("{} {}: {e}", request.method, request.path));
            message(500, "The metrics could not be read.")
        })
    }
}

fn message(status: u16, text: &str) -> Response {
    Response::json(status, &json!({ "message": text }))
}

fn csv(body: String) -> Response {
    Response {
        status: 200,
        headers: vec![
            ("content-type".into(), "text/csv; charset=utf-8".into()),
            ("cache-control".into(), "no-store".into()),
        ],
        body: body.into_bytes(),
    }
}

/// `app=<uuid>`, if given; a 422 answer when it isn't a uuid.
fn app_param(q: &str) -> Result<Option<String>, Response> {
    match report::query(q, "app") {
        None => Ok(None),
        Some(a) if a.is_empty() => Ok(None),
        Some(a) if report::valid_uuid(&a) => Ok(Some(a)),
        Some(_) => Err(message(422, "The application uuid is invalid.")),
    }
}

/// Current application names; empty when `resources` can't answer (the recorded names are used then).
fn names() -> HashMap<String, String> {
    resources::applications()
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            Some((
                a.get("uuid")?.as_str()?.to_owned(),
                a.get("name")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Reads the deployments the window needs, and calls `f` with them and the window's days. A 422 answer for
/// an invalid window.
fn with_data(q: &str, f: impl FnOnce(&Data, u32) -> Response) -> Result<Response, String> {
    let settings = settings();
    let days = match report::window_days(q, &settings) {
        Ok(d) => d,
        Err(e) => return Ok(message(422, &e)),
    };
    let now = now();
    let since = (now - i64::from(days + report::LOOKBACK_DAYS) * DAY)
        .max(now - i64::from(settings.retention_days) * DAY);
    let (deps, index) = store::load_since(&mut Storage, since)?;
    let names = names();
    let data = Data {
        deps: &deps,
        index: &index,
        settings: &settings,
        names: &names,
        now,
    };
    Ok(f(&data, days))
}

fn metrics_json(q: &str) -> Result<Response, String> {
    let app = match app_param(q) {
        Ok(a) => a,
        Err(r) => return Ok(r),
    };
    let top = report::top(q);
    with_data(q, |d, days| {
        Response::json(200, &d.metrics_json(days, app.as_deref(), top))
    })
}

fn panel(app: Option<&str>) -> Result<Response, String> {
    with_data("", |d, days| {
        let initial: Value = d.metrics_json(days, app, report::TOP_DEFAULT);
        let body = match app {
            Some(uuid) => crate::panels::application(uuid, &initial),
            None => crate::panels::dashboard(&initial),
        };
        ui::panel(&body)
    })
}

fn inventory() -> Result<String, String> {
    let mut inv = Inventory::default();
    for project in resources::projects()? {
        let uuid = project
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let envs = match resources::environments(&uuid)? {
            Some(text) => match serde_json::from_str(&text) {
                Ok(Value::Array(items)) => items,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        inv.projects.push((project, envs));
    }
    inv.applications = resources::list(Kind::Applications)?;
    inv.services = resources::list(Kind::Services)?;
    inv.databases = resources::list(Kind::Databases)?;
    inv.servers = resources::list(Kind::Servers)?;
    Ok(report::inventory_csv(&inv))
}

coolify_rs_plugin::export!(DeliveryMetrics);
