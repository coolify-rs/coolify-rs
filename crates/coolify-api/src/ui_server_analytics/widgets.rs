//! `Dashboard\TrafficAnalytics` and `Project\Shared\ResourceTrafficAnalytics` (the analytics tab of
//! applications and services). Mounting and `loadData` run per request; Sentinel answers come from
//! the 60 second cache of [`Client`].

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use coolify_jobs::server_ops::Srv;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::aggregator::{Aggregator, sum_overviews};
use super::client::{Client, Scope};
use super::resource::Resources;
use super::{
    dimensions_for, error_rate, ip_mode_for_servers, ip_mode_of, is_enabled, range_window,
    shape_common,
};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    range: Option<String>,
}

/// `setRange`.
fn valid_range(range: Option<String>) -> String {
    range
        .filter(|r| matches!(r.as_str(), "24h" | "7d" | "30d"))
        .unwrap_or_else(|| "24h".to_string())
}

/// `Dashboard\TrafficAnalytics::mount` with `loadData`.
pub async fn dashboard(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(query): Query<RangeQuery>,
) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let range = valid_range(query.range);
    let all = coolify_db::servers::list_by_name(&state.db, ctx.team_id).await?;
    let servers: Vec<_> = all.iter().filter(|s| is_enabled(s)).collect();
    let (from, to) = range_window(&range, chrono::Utc::now());
    let mut overviews = Vec::new();
    let mut aggregate = Aggregator::new(Vec::new());
    for server in &servers {
        let client = Client::new(Srv::from_row(jobs, (*server).clone()));
        // Skip unreachable/failed servers so one bad server doesn't break the whole summary.
        let Ok(overview) = client.overview(None, &from, &to, Scope::App).await else {
            continue;
        };
        let Ok(series) = client.series(None, &range, Scope::App).await else {
            continue;
        };
        overviews.push(overview);
        aggregate.add_series(series);
    }
    let mut out = Map::new();
    out.insert("serverCount".into(), json!(servers.len()));
    out.insert("range".into(), json!(range));
    if overviews.is_empty() {
        // Every server's fetch failed; don't present an all-zero KPI panel as if it were real data.
        out.insert("overview".into(), Value::Null);
        return Ok(ok(Value::Object(out)).into_response());
    }
    let summed = sum_overviews(&overviews);
    out.insert("errorRate".into(), json!(error_rate(&summed.overview)));
    out.insert(
        "bandwidthBytes".into(),
        json!(summed.overview.bytes_in + summed.overview.bytes_out),
    );
    out.insert("overview".into(), json!(summed.overview));
    out.insert(
        "uniquesApproximate".into(),
        json!(summed.uniques_approximate),
    );
    out.insert("series".into(), json!(aggregate.series()));
    Ok(ok(Value::Object(out)).into_response())
}

/// `ResourceTrafficAnalytics::mount` with `loadData`.
pub async fn resource(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Query(query): Query<RangeQuery>,
) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let res = crate::ui_shared::find(&state, &ctx, &kind, &uuid).await?;
    if !res.is_application() && !res.is_service() {
        return Err(ApiError::Unauthorized);
    }
    let range = valid_range(query.range);
    let server = match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let mut out = Map::new();
    let enabled = server.as_ref().is_some_and(is_enabled);
    out.insert("enabled".into(), json!(enabled));
    out.insert(
        "analyticsServerUuid".into(),
        json!(server.as_ref().map(|s| s.uuid.clone())),
    );
    out.insert("range".into(), json!(range));
    out.insert(
        "chartId".into(),
        json!(if res.is_service() {
            "service-analytics"
        } else {
            "application-analytics"
        }),
    );
    // The public properties before `loadData`.
    out.insert("overview".into(), Value::Null);
    out.insert("ipMode".into(), json!("full"));
    out.insert("latencyApproximate".into(), json!(false));
    out.insert("uniquesApproximate".into(), json!(false));
    out.insert("topPaths".into(), json!([]));
    out.insert("breakdowns".into(), json!([]));
    out.insert("attribution".into(), Value::Null);
    out.insert("series".into(), json!([]));
    out.insert("errorRate".into(), json!(0.0));
    out.insert("bandwidthBytes".into(), json!(0));
    out.insert("device".into(), json!({"labels": [], "series": []}));
    let Some(server) = server.filter(|_| enabled) else {
        return Ok(ok(Value::Object(out)).into_response());
    };
    let mode = ip_mode_for_servers(&[ip_mode_of(&server)]);
    out.insert("ipMode".into(), json!(mode));
    let (from, to) = range_window(&range, chrono::Utc::now());
    let client = Client::new(Srv::from_row(jobs, server));
    let rows = coolify_db::traffic::team_resources(&state.db, ctx.team_id).await?;
    let mut resources = Resources::new(state.db.clone(), rows);
    let mut aggregate = Aggregator::new(dimensions_for(mode));
    if let Err(e) = aggregate
        .collect_resource(&client, &res.uuid, &from, &to, &range, &mut resources, 50)
        .await
    {
        // `handleError($e, $this)`: the toast, and no data on the page.
        out.insert("error".into(), json!(e));
        return Ok(ok(Value::Object(out)).into_response());
    }
    shape_common(&aggregate, &mut out);
    Ok(ok(Value::Object(out)).into_response())
}
