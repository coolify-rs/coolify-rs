//! Session endpoints behind `Livewire\Analytics`: the `livewire:analytics` part of the server
//! Analytics page and the top-level `/analytics` page. Mounting and `loadData` run per request;
//! Sentinel answers come from the 60 second cache of [`client`].

mod aggregator;
mod client;
mod countries;
mod helpers;
mod resource;

use aggregator::{Aggregator, BreakdownRow, top_by_requests};
use axum::Extension;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use client::{Client, Scope};
use coolify_db::servers::ServerRow;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::Srv;
use md5::{Digest, Md5};
use resource::Resources;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::find_server;
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_general::SERVER_PAGE;

/// `MAX_LEADERBOARD_APPS`.
const MAX_LEADERBOARD_APPS: usize = 200;
/// `$breakdownDimensions`.
const DIMENSIONS: [&str; 12] = [
    "country",
    "city",
    "referer",
    "browser",
    "os",
    "device",
    "protocol",
    "cache",
    "status",
    "agent",
    "ip",
    "useragent",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/analytics", get(global))
        .route(&format!("{SERVER_PAGE}/analytics/data"), get(scoped))
}

#[derive(Debug, Deserialize)]
struct Filters {
    range: Option<String>,
    server: Option<String>,
    app: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppRowOut {
    uuid: String,
    name: String,
    domain: Option<String>,
    link: Option<String>,
    is_service: bool,
    requests: i64,
    bandwidth: i64,
}

#[derive(Debug, Serialize)]
struct HostRowOut {
    host: String,
    requests: i64,
    bandwidth: i64,
}

fn md5_hex(text: &str) -> String {
    Md5::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_enabled(server: &ServerRow) -> bool {
    server
        .settings
        .as_ref()
        .and_then(|s| s.get("is_traffic_analytics_enabled"))
        .is_some_and(php_truthy)
}

fn ip_mode_of(server: &ServerRow) -> String {
    server
        .settings
        .as_ref()
        .and_then(|s| s.get("traffic_ip_mode"))
        .and_then(Value::as_str)
        .unwrap_or("full")
        .to_string()
}

/// `TrafficIpMode::forServers`.
fn ip_mode_for_servers(modes: &[String]) -> &'static str {
    if modes.is_empty() {
        return "full";
    }
    let recording: Vec<&String> = modes.iter().filter(|m| *m != "off").collect();
    if recording.is_empty() {
        "off"
    } else if recording.iter().all(|m| *m == "anonymized") {
        "anonymized"
    } else {
        "full"
    }
}

/// `TrafficIpMode::breakdownDimensions`.
fn dimensions_for(mode: &str) -> Vec<&'static str> {
    DIMENSIONS
        .iter()
        .copied()
        .filter(|d| mode != "off" || !matches!(*d, "ip" | "city"))
        .collect()
}

/// `SentinelTrafficClient::rangeWindow`: ends at the next full minute.
fn range_window(range: &str, now: chrono::DateTime<chrono::Utc>) -> (String, String) {
    let seconds = now.timestamp();
    let whole_minute = seconds % 60 == 0 && now.timestamp_subsec_nanos() == 0;
    let to = seconds - seconds.rem_euclid(60) + if whole_minute { 0 } else { 60 };
    let to = chrono::DateTime::from_timestamp(to, 0).unwrap_or(now);
    let from = to
        - chrono::Duration::days(match range {
            "7d" => 7,
            "30d" => 30,
            _ => 1,
        });
    let format = "%Y-%m-%dT%H:%M:%SZ";
    (
        from.format(format).to_string(),
        to.format(format).to_string(),
    )
}

/// PHP's `round()` for the figures the views print.
fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10f64.powi(places);
    (value * factor).round() / factor
}

/// `errorRate`.
fn error_rate(overview: &aggregator::Overview) -> f64 {
    if overview.requests == 0 {
        return 0.0;
    }
    round_to(
        (overview.s4xx + overview.s5xx) as f64 / overview.requests as f64 * 100.0,
        2,
    )
}

/// `refreshAppOptions`: the valid `?app=` values and the grouped listbox options.
fn app_options(
    resources: &Resources,
    enabled: &HashSet<&str>,
    server_filter: &str,
) -> (HashSet<String>, Vec<Value>) {
    let mut by_project: BTreeMap<String, Vec<&resource::TrafficResource>> = BTreeMap::new();
    let mut valid = HashSet::new();
    for item in &resources.items {
        let Some(server) = item.0.server_uuid.as_deref() else {
            continue;
        };
        if !enabled.contains(server) || (!server_filter.is_empty() && server != server_filter) {
            continue;
        }
        valid.insert(item.0.uuid.clone());
        by_project
            .entry(item.project_name())
            .or_default()
            .push(item);
    }
    let mut grouped = Vec::new();
    for (project, mut items) in by_project {
        grouped.push(json!({"value": format!("__group_{}", md5_hex(&project)), "label": project, "header": true}));
        items.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        for item in items {
            grouped.push(json!({
                "value": item.0.uuid,
                "label": if item.0.is_service { format!("{} (Service)", item.0.name) } else { item.0.name.clone() },
            }));
        }
    }
    (valid, grouped)
}

/// `Analytics::mount` with `loadData`.
async fn analytics(
    state: &AppState,
    ctx: &AuthCtx,
    scoped: Option<&str>,
    filters: Filters,
) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let db = state.db.clone();
    let all = coolify_db::servers::list_by_name(&db, ctx.team_id).await?;
    let range = filters.range.unwrap_or_else(|| "24h".to_string());
    let mut server_uuid = filters.server.unwrap_or_default();
    let mut app_uuid = filters.app.unwrap_or_default();

    let mut eligible_disabled: Vec<&ServerRow> = Vec::new();
    let servers: Vec<&ServerRow>;
    let chart_id;
    let mut server_options = Vec::new();
    if let Some(uuid) = scoped {
        // `abort_if($server === null, 404)` is `find_server` in the handler; deleted since then here.
        let server = all
            .iter()
            .find(|s| s.uuid == uuid)
            .ok_or(ApiError::Unauthorized)?;
        server_uuid.clone_from(&server.uuid);
        chart_id = format!("server-analytics-{}", server.uuid);
        servers = if is_enabled(server) {
            vec![server]
        } else {
            Vec::new()
        };
        server_options.push(json!({"value": server.uuid, "label": server_name(server)}));
    } else {
        chart_id = "global-analytics".to_string();
        servers = all.iter().filter(|s| is_enabled(s)).collect();
        for server in &servers {
            server_options.push(json!({"value": server.uuid, "label": server_name(server)}));
        }
        eligible_disabled = all
            .iter()
            .filter(|s| {
                let info = ServerInfo::from_row(s);
                !is_enabled(s) && !info.is_swarm() && !info.is_build_server()
            })
            .collect();
        if !server_uuid.is_empty() && !servers.iter().any(|s| s.uuid == server_uuid) {
            server_uuid.clear();
        }
    }
    let nudge_key = if scoped.is_some() {
        String::new()
    } else {
        let mut uuids: Vec<&str> = eligible_disabled.iter().map(|s| s.uuid.as_str()).collect();
        uuids.sort_unstable();
        md5_hex(&uuids.join(","))[..12].to_string()
    };

    let rows = coolify_db::traffic::team_resources(&db, ctx.team_id).await?;
    let mut resources = Resources::new(db, rows);
    let enabled: HashSet<&str> = servers.iter().map(|s| s.uuid.as_str()).collect();
    let (valid_apps, grouped) = app_options(&resources, &enabled, &server_uuid);
    if !app_uuid.is_empty() && !valid_apps.contains(&app_uuid) {
        app_uuid.clear();
    }

    let mut out = Map::new();
    out.insert("chartId".into(), json!(chart_id));
    out.insert("scopedServerUuid".into(), json!(scoped));
    out.insert("hasServers".into(), json!(!servers.is_empty()));
    out.insert("serverOptions".into(), json!(server_options));
    out.insert("appOptions".into(), json!(grouped));
    out.insert("range".into(), json!(range));
    out.insert("serverUuid".into(), json!(server_uuid));
    out.insert("appUuid".into(), json!(app_uuid));
    out.insert(
        "eligibleDisabledServers".into(),
        Value::Array(
            eligible_disabled
                .iter()
                .map(|s| json!({"uuid": s.uuid, "name": server_name(s)}))
                .collect(),
        ),
    );
    out.insert("nudgeKey".into(), json!(nudge_key));
    out.insert("overview".into(), Value::Null);
    out.insert("ipMode".into(), json!("full"));

    if !servers.is_empty() {
        load_data(
            jobs,
            &servers,
            scoped.is_some(),
            &range,
            &server_uuid,
            &app_uuid,
            &mut resources,
            &mut out,
        )
        .await;
    }
    Ok(ok(Value::Object(out)).into_response())
}

fn server_name(server: &ServerRow) -> String {
    server
        .record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `targetServers`.
fn target_servers<'a>(
    servers: &[&'a ServerRow],
    is_scoped: bool,
    server_uuid: &str,
    app_uuid: &str,
    resources: &Resources,
) -> Vec<&'a ServerRow> {
    if is_scoped {
        return servers.to_vec();
    }
    if !app_uuid.is_empty() {
        let owner = resources
            .get(app_uuid)
            .and_then(|r| r.0.server_uuid.as_deref());
        return servers
            .iter()
            .filter(|s| Some(s.uuid.as_str()) == owner)
            .copied()
            .collect();
    }
    if !server_uuid.is_empty() {
        return servers
            .iter()
            .filter(|s| s.uuid == server_uuid)
            .copied()
            .collect();
    }
    servers.to_vec()
}

/// `loadData`.
#[allow(clippy::too_many_arguments)]
async fn load_data(
    jobs: &coolify_jobs::Jobs,
    servers: &[&ServerRow],
    is_scoped: bool,
    range: &str,
    server_uuid: &str,
    app_uuid: &str,
    resources: &mut Resources,
    out: &mut Map<String, Value>,
) {
    let (from, to) = range_window(range, chrono::Utc::now());
    let selected = (!app_uuid.is_empty() && resources.get(app_uuid).is_some()).then_some(app_uuid);
    let targets = target_servers(servers, is_scoped, server_uuid, app_uuid, resources);
    let modes: Vec<String> = targets.iter().map(|s| ip_mode_of(s)).collect();
    let ip_mode = ip_mode_for_servers(&modes);
    out.insert("ipMode".into(), json!(ip_mode));
    let dimensions = dimensions_for(ip_mode);

    let mut aggregate = Aggregator::new(dimensions.clone());
    let mut app_rows: Vec<(String, AppRowOut)> = Vec::new();
    let mut host_totals: Vec<(String, HostRowOut)> = Vec::new();
    for server in targets {
        let client = Client::new(Srv::from_row(jobs, (*server).clone()));
        // Skip unreachable/failed servers so one bad server doesn't break the whole view.
        let _ = collect_server(
            &client,
            selected,
            &from,
            &to,
            range,
            &dimensions,
            &mut aggregate,
            resources,
            &mut app_rows,
            &mut host_totals,
        )
        .await;
    }
    if !aggregate.has_overview() {
        return;
    }
    let summed = aggregate.overview();
    let breakdowns = aggregate.breakdowns();
    let rows_of = |dimension: &str| -> &[BreakdownRow] {
        breakdowns
            .iter()
            .find(|(d, _)| *d == dimension)
            .map_or(&[], |(_, rows)| rows.as_slice())
    };
    let cities = helpers::traffic_city_rows(rows_of("city"));
    let (countries, known) = helpers::country_rows(rows_of("country"));
    let mut breakdowns_out = Map::new();
    for (dimension, rows) in &breakdowns {
        breakdowns_out.insert(
            (*dimension).to_string(),
            json!(
                rows.iter()
                    .map(|r| helpers::breakdown_out(dimension, r))
                    .collect::<Vec<_>>()
            ),
        );
    }
    out.insert("errorRate".into(), json!(error_rate(&summed.overview)));
    out.insert(
        "bandwidthBytes".into(),
        json!(summed.overview.bytes_in + summed.overview.bytes_out),
    );
    out.insert("overview".into(), json!(summed.overview));
    out.insert(
        "latencyApproximate".into(),
        json!(summed.latency_approximate),
    );
    out.insert(
        "uniquesApproximate".into(),
        json!(summed.uniques_approximate),
    );
    out.insert(
        "topApps".into(),
        json!(top_by_requests(
            app_rows.into_iter().map(|(_, r)| r).collect(),
            |r| r.requests,
            50
        )),
    );
    out.insert(
        "topHosts".into(),
        json!(top_by_requests(
            host_totals.into_iter().map(|(_, r)| r).collect(),
            |r| r.requests,
            50
        )),
    );
    out.insert("topPaths".into(), json!(aggregate.top_paths()));
    out.insert("breakdowns".into(), Value::Object(breakdowns_out));
    out.insert("attribution".into(), json!(aggregate.attribution()));
    out.insert("series".into(), json!(aggregate.series()));
    out.insert(
        "geoInit".into(),
        json!(helpers::traffic_geo_markers(&known, &cities)),
    );
    out.insert(
        "geo".into(),
        json!(helpers::traffic_geo_markers(rows_of("country"), &cities)),
    );
    out.insert("countries".into(), json!(countries));
    out.insert("cities".into(), json!(cities));
    out.insert(
        "device".into(),
        json!(helpers::device_chart_data(rows_of("device"))),
    );
}

/// The `try` block of `loadData`, for one server.
#[allow(clippy::too_many_arguments)]
async fn collect_server(
    client: &Client,
    selected: Option<&str>,
    from: &str,
    to: &str,
    range: &str,
    dimensions: &[&'static str],
    aggregate: &mut Aggregator,
    resources: &mut Resources,
    app_rows: &mut Vec<(String, AppRowOut)>,
    host_totals: &mut Vec<(String, HostRowOut)>,
) -> Result<(), String> {
    if let Some(uuid) = selected {
        return aggregate
            .collect_resource(client, uuid, from, to, range, resources, 50)
            .await;
    }
    let mut keys = client
        .prefetch_server_wide(
            None,
            from,
            to,
            dimensions,
            range,
            50,
            50,
            MAX_LEADERBOARD_APPS as i64,
        )
        .await?;
    if keys.len() > MAX_LEADERBOARD_APPS {
        tracing::warn!(
            server = client.server_uuid(),
            total = keys.len(),
            shown = MAX_LEADERBOARD_APPS,
            "Traffic analytics leaderboard truncated"
        );
        keys.truncate(MAX_LEADERBOARD_APPS);
    }
    aggregate
        .collect(client, None, from, to, range, resources, 50)
        .await?;
    for key in keys {
        let overview = client.overview(Some(&key), from, to, Scope::App).await?;
        let bandwidth = overview.bytes_in + overview.bytes_out;
        let meta = resources.app_meta(&key).await;
        let row = aggregator::entry(app_rows, &meta.uuid, || AppRowOut {
            uuid: meta.uuid.clone(),
            name: meta.name.clone(),
            domain: meta.row_domain.clone(),
            link: meta.link.clone(),
            is_service: meta.is_service,
            requests: 0,
            bandwidth: 0,
        });
        row.requests += overview.requests;
        row.bandwidth += bandwidth;
        let host = meta.domain.clone().unwrap_or_default();
        let total = aggregator::entry(host_totals, &host, || HostRowOut {
            host: host.clone(),
            requests: 0,
            bandwidth: 0,
        });
        total.requests += overview.requests;
        total.bandwidth += bandwidth;
    }
    Ok(())
}

/// The top-level `/analytics` page.
async fn global(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(filters): Query<Filters>,
) -> ApiResult {
    analytics(&state, &ctx, None, filters).await
}

/// `<livewire:analytics :scoped-server-uuid="$server->uuid" />`.
async fn scoped(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(filters): Query<Filters>,
) -> ApiResult {
    let server = find_server(&state, &ctx, &uuid).await?;
    analytics(&state, &ctx, Some(&server.uuid), filters).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn window_ends_at_the_next_full_minute() {
        let at = chrono::Utc
            .with_ymd_and_hms(2026, 10, 31, 12, 30, 15)
            .unwrap();
        assert_eq!(
            range_window("24h", at),
            (
                "2026-10-30T12:31:00Z".to_string(),
                "2026-10-31T12:31:00Z".to_string()
            )
        );
        assert_eq!(range_window("7d", at).0, "2026-10-24T12:31:00Z");
        assert_eq!(range_window("30d", at).0, "2026-10-01T12:31:00Z");
        let whole = chrono::Utc
            .with_ymd_and_hms(2026, 10, 31, 12, 30, 0)
            .unwrap();
        assert_eq!(range_window("24h", whole).1, "2026-10-31T12:30:00Z");
    }

    #[test]
    fn ip_modes_merge_like_upstream() {
        let m = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(ip_mode_for_servers(&[]), "full");
        assert_eq!(ip_mode_for_servers(&m(&["off", "off"])), "off");
        assert_eq!(
            ip_mode_for_servers(&m(&["off", "anonymized"])),
            "anonymized"
        );
        assert_eq!(ip_mode_for_servers(&m(&["full", "anonymized"])), "full");
        assert!(!dimensions_for("off").contains(&"ip"));
        assert!(!dimensions_for("off").contains(&"city"));
        assert_eq!(dimensions_for("full").len(), 12);
    }

    #[test]
    fn error_rate_rounds_to_two_places() {
        let overview = aggregator::Overview {
            requests: 3,
            s4xx: 1,
            ..Default::default()
        };
        assert_eq!(error_rate(&overview), 33.33);
        assert_eq!(error_rate(&aggregator::Overview::default()), 0.0);
    }
}
