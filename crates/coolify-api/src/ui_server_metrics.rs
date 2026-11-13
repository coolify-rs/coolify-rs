//! Session endpoints behind `Server\Charts` (the Metrics page) with `HasMetrics`.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::servers::ServerRow;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::{Srv, sentinel};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{Setting, find_server, save_settings};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_fleet_metrics::round_to;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;
use crate::validation::{Field, Rule, validate};

const UNAUTHORIZED: &str = "This action is unauthorized.";

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/{s}");
    Router::new()
        .route(&p("metrics"), get(show).post(save))
        .route(&p("metrics/toggle"), post(toggle))
        .route(&p("metrics/data"), get(data))
        .route(
            "/ui/resources/{kind}/{uuid}/metrics/data",
            get(resource_data),
        )
        .route("/ui/dashboard/server-metrics/{uuid}", get(dashboard_chart))
}

fn setting(s: &ServerRow, k: &str) -> Value {
    s.settings
        .as_ref()
        .and_then(|r| r.get(k))
        .cloned()
        .unwrap_or(Value::Null)
}

fn metrics_enabled(s: &ServerRow) -> bool {
    php_truthy(&setting(s, "is_metrics_enabled"))
}

/// `Server\Charts::mount`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let info = ServerInfo::from_row(&s);
    Ok(ok(json!({
        "is_metrics_enabled": metrics_enabled(&s),
        "is_sentinel_enabled": !info.is_build_server()
            && !info.is_swarm()
            && info.setting_bool("force_disabled") != Some(true)
            && !info.transferred,
        "sentinel_metrics_refresh_rate_seconds": setting(&s, "sentinel_metrics_refresh_rate_seconds"),
        "sentinel_metrics_history_days": setting(&s, "sentinel_metrics_history_days"),
        "sentinel_push_interval_seconds": setting(&s, "sentinel_push_interval_seconds"),
    }))
    .into_response())
}

/// `Server\Charts::saveMetricsSettings`: saving the settings restarts Sentinel (`ServerSetting::updated`).
async fn save(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let input = Input(body);
    let rule = |name: &'static str, min: usize| Field {
        name,
        rules: vec![Rule::Required, Rule::Integer, Rule::Min(min)],
    };
    validate(
        &input,
        &[
            rule("sentinel_metrics_refresh_rate_seconds", 1),
            rule("sentinel_metrics_history_days", 1),
            rule("sentinel_push_interval_seconds", 10),
        ],
        &[],
        &[
            "sentinel_metrics_refresh_rate_seconds",
            "sentinel_metrics_history_days",
            "sentinel_push_interval_seconds",
        ],
    )?;
    let number = |k: &str| {
        input
            .0
            .get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .unwrap_or_default()
    };
    let changed: Vec<&str> = [
        "sentinel_metrics_refresh_rate_seconds",
        "sentinel_metrics_history_days",
        "sentinel_push_interval_seconds",
    ]
    .into_iter()
    .filter(|k| setting(&s, k).as_i64() != Some(number(k)))
    .collect();
    let changes = vec![
        (
            "sentinel_metrics_refresh_rate_seconds",
            Setting::Int(number("sentinel_metrics_refresh_rate_seconds")),
        ),
        (
            "sentinel_metrics_history_days",
            Setting::Int(number("sentinel_metrics_history_days")),
        ),
        (
            "sentinel_push_interval_seconds",
            Setting::Int(number("sentinel_push_interval_seconds")),
        ),
    ];
    save_settings(&state, &s, &changes).await?;
    audit_sentinel_update(&state, &ctx, &meta, &s, &changed).await;
    Ok(ok(json!({"message": "Metrics settings updated. Restarting Sentinel."})).into_response())
}

/// `Server\Charts::auditSentinelUpdate`: metrics settings use the same event as the Sentinel page and API.
pub(crate) async fn audit_sentinel_update(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    s: &ServerRow,
    changed: &[&str],
) {
    if changed.is_empty() {
        return;
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        "ui.server.sentinel.updated",
        json!({
            "team_id": s.team_id,
            "server_uuid": s.uuid,
            "server_name": s.record.get("name"),
            "changed_fields": changed,
        }),
        "info",
    )
    .await;
}

/// `Server\Charts::toggleMetrics`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let enable = !metrics_enabled(&s);
    let srv = Srv::from_row(jobs, s.clone());
    // Saved quietly: Sentinel is started below, so the settings hook must not start it a second time.
    if let Err(e) = srv
        .settings_update(&[(
            "is_metrics_enabled",
            coolify_db::servers::ColumnValue::Bool(enable),
        )])
        .await
    {
        return toast_error(e);
    }
    audit_sentinel_update(&state, &ctx, &meta, &s, &["is_metrics_enabled"]).await;
    if enable {
        if let Err(e) = sentinel::start_sentinel(jobs, s.id, true, None).await {
            return toast_error(e);
        }
        return Ok(ok(json!({"message": "Metrics enabled. Starting Sentinel."})).into_response());
    }
    jobs.spawn_restart_sentinel(s.id);
    Ok(ok(json!({"message": "Metrics disabled. Restarting Sentinel."})).into_response())
}

pub(crate) const TYPES: [&str; 5] = ["cpu", "memory", "disk", "load", "network"];

/// `HasMetrics::getMetricsEndpoint`: `container` is the uuid of a resource. Ranges over an hour
/// ask Sentinel to average rows into whole-minute windows, at most 1000 per range; older
/// Sentinels ignore `bucket_ms` and the rows are still downsampled here.
fn endpoint(container: Option<&str>, kind: &str, from: &str, mins: i64) -> String {
    let base = "http://localhost:8888/api";
    let mut query = format!("from={from}");
    if mins > 60 {
        query.push_str(&format!("&bucket_ms={}", (mins + 999) / 1000 * 60_000));
    }
    match container {
        None => format!("{base}/{kind}/history?{query}"),
        Some(uuid) => format!("{base}/container/{uuid}/{kind}/history?{query}"),
    }
}

/// What `SentinelMetricsClient::fetchOverSsh()` keeps per server: the cooldown of an unavailable
/// server, the short response cache and the one read in flight.
#[derive(Default)]
struct SshFetches {
    unavailable_until: HashMap<i64, Instant>,
    responses: HashMap<String, (Instant, String)>,
    inflight_until: HashMap<i64, Instant>,
}

static SSH_FETCHES: LazyLock<Mutex<SshFetches>> =
    LazyLock::new(|| Mutex::new(SshFetches::default()));

/// How long a server stays unavailable after a failed metrics read.
const UNAVAILABLE_COOLDOWN: Duration = Duration::from_secs(60);
/// How long a metrics response is shared.
const RESPONSE_TTL: Duration = Duration::from_secs(10);
/// The expiry of the in-flight lock.
const INFLIGHT_LOCK: Duration = Duration::from_secs(15);
/// `instant_remote_process(..., timeout: 8, maxAttempts: 1)`.
const SSH_TIMEOUT: Duration = Duration::from_secs(8);

/// Releases the in-flight lock of a server.
struct Inflight(i64);

impl Drop for Inflight {
    fn drop(&mut self) {
        if let Ok(mut state) = SSH_FETCHES.lock() {
            state.inflight_until.remove(&self.0);
        }
    }
}

/// `SentinelMetricsClient::fetchOverSsh`: optional charts do not wait for the general SSH
/// timeout. One in-flight read per server, a short shared response cache and a cooldown keep
/// repeated dashboard refreshes from reconnecting to an unavailable server. `None`: no answer.
pub(crate) async fn fetch_over_ssh(
    srv: &Srv,
    commands: &[String],
    cache_key: &str,
) -> Option<String> {
    let id = srv.row.id;
    let response_key = format!("{id}:{cache_key}");
    let lock = {
        let mut state = SSH_FETCHES.lock().ok()?;
        let now = Instant::now();
        if state.unavailable_until.get(&id).is_some_and(|t| *t > now) {
            return None;
        }
        if let Some((until, body)) = state.responses.get(&response_key)
            && *until > now
        {
            return Some(body.clone());
        }
        if state.inflight_until.get(&id).is_some_and(|t| *t > now) {
            return None;
        }
        state.inflight_until.insert(id, now + INFLIGHT_LOCK);
        Inflight(id)
    };
    let response = srv
        .run_once(commands, "server.metrics", SSH_TIMEOUT)
        .await
        .ok()
        .flatten()
        .filter(|r| !r.trim().is_empty());
    let mut state = SSH_FETCHES.lock().ok()?;
    let now = Instant::now();
    let answer = match response {
        Some(body) => {
            state.responses.retain(|_, (until, _)| *until > now);
            state
                .responses
                .insert(response_key, (now + RESPONSE_TTL, body.clone()));
            Some(body)
        }
        None => {
            state
                .unavailable_until
                .insert(id, now + UNAVAILABLE_COOLDOWN);
            None
        }
    };
    drop(state);
    drop(lock);
    answer
}

/// `docker exec` of a Sentinel read: `curl` reads the token from stdin (`-H @-`), so it is not a
/// process argument. The token only has letters, digits and `._-+/=`, so it cannot end the heredoc.
fn sentinel_curl(url: &str, token: &str) -> String {
    format!(
        "curl -sS --connect-timeout 2 --max-time 3 -H @- \"{url}\" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer {token}\nCOOLIFY_SENTINEL_AUTH"
    )
}

fn sentinel_script(script: &str) -> String {
    format!(
        "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\n{script}\nCOOLIFY_SENTINEL_SCRIPT"
    )
}

/// The `static::class.':'.$this->uuid` part of the `fetchOverSsh` cache key.
fn owner_key(srv: &Srv, container: Option<&str>) -> String {
    match container {
        Some(uuid) => format!("container:{uuid}"),
        None => format!("App\\Models\\Server:{}", srv.row.uuid),
    }
}

/// `$prefetchedMetricResponses`: a type maps to `None` when its prefetch failed, so its getter
/// does not try the unavailable server again.
pub(crate) type Prefetched = HashMap<&'static str, Option<String>>;

/// `HasMetrics::prefetchMetrics`: several types in one SSH round trip, separated by 0x1E.
pub(crate) async fn prefetch(
    srv: &Srv,
    types: &[&'static str],
    container: Option<&str>,
    mins: i64,
) -> Prefetched {
    let mut answers: Prefetched = types.iter().map(|t| (*t, None)).collect();
    let from = (chrono::Utc::now() - chrono::Duration::minutes(mins))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let Ok(token) = sentinel::ensure_valid_token(srv).await else {
        return answers;
    };
    let script = types
        .iter()
        .map(|t| sentinel_curl(&endpoint(container, t, &from, mins), &token))
        .collect::<Vec<_>>()
        .join("\nprintf '\\036'\n");
    let key = format!("{}:{}:{mins}", owner_key(srv, container), types.join(","));
    let Some(output) = fetch_over_ssh(srv, &[sentinel_script(&script)], &key).await else {
        return answers;
    };
    let parts: Vec<&str> = output.split('\u{1e}').collect();
    if parts.len() != types.len() {
        return answers;
    }
    for (t, r) in types.iter().zip(parts) {
        answers.insert(*t, Some(r.trim().to_string()));
    }
    answers
}

/// `HasMetrics::fetchMetricRows`: the prefetched answer, or one fetch of its own. `Ok(None)` when
/// there is no answer (the getters then return `null`).
async fn fetch_rows(
    srv: &Srv,
    answers: &mut Prefetched,
    kind: &'static str,
    container: Option<&str>,
    mins: i64,
) -> Result<Option<Vec<Value>>, String> {
    let response = if let Some(prefetched) = answers.remove(kind) {
        prefetched
    } else {
        let from = (chrono::Utc::now() - chrono::Duration::minutes(mins))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let token = sentinel::ensure_valid_token(srv).await?;
        let url = endpoint(container, kind, &from, mins);
        let key = format!("{}:{kind}:{mins}", owner_key(srv, container));
        fetch_over_ssh(srv, &[sentinel_script(&sentinel_curl(&url, &token))], &key).await
    };
    match response {
        None => Ok(None),
        Some(r) => rows(Some(&r)).map(Some),
    }
}

/// `HasMetrics::fetchMetricRows`: an error in the answer is an error, an empty answer has no rows.
pub(crate) fn rows(response: Option<&String>) -> Result<Vec<Value>, String> {
    let response = response.map_or("", String::as_str);
    if response.contains("error") {
        let error = serde_json::from_str::<Value>(response)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| "Something is not okay, are you okay?".to_string());
        if error == "Unauthorized" {
            return Err("Unauthorized, please check your metrics token or restart Sentinel to set a new token.".to_string());
        }
        return Err(error);
    }
    Ok(serde_json::from_str::<Value>(response)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default())
}

fn number(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `HasMetrics::averagePerMinute`.
fn average_per_minute(metrics: &[(i64, f64)]) -> Vec<(i64, f64)> {
    let mut buckets: BTreeMap<i64, (f64, u32)> = BTreeMap::new();
    for (time, value) in metrics {
        let bucket = buckets.entry(time / 60_000 * 60_000).or_default();
        bucket.0 += value;
        bucket.1 += 1;
    }
    buckets
        .into_iter()
        .map(|(b, (sum, count))| (b, sum / f64::from(count)))
        .collect()
}

/// `downsampleLTTB`.
fn downsample_lttb(data: &[(i64, f64)], threshold: usize) -> Vec<(i64, f64)> {
    let length = data.len();
    if threshold >= length || threshold <= 2 {
        return data.to_vec();
    }
    let mut sampled = vec![data[0]];
    let bucket_size = (length - 2) as f64 / (threshold - 2) as f64;
    let mut a = 0usize;
    for i in 0..threshold - 2 {
        let start = (((i + 1) as f64 * bucket_size).floor() as usize) + 1;
        let end = (((i + 2) as f64 * bucket_size).floor() as usize + 1).min(length - 1);
        let next_start = (((i + 2) as f64 * bucket_size).floor() as usize) + 1;
        let next_end = (((i + 3) as f64 * bucket_size).floor() as usize + 1).min(length - 1);
        let (mut avg_x, mut avg_y) = (0.0, 0.0);
        if next_end + 1 > next_start {
            let count = (next_end + 1 - next_start) as f64;
            for point in &data[next_start..=next_end] {
                avg_x += point.0 as f64;
                avg_y += point.1;
            }
            avg_x /= count;
            avg_y /= count;
        }
        let (mut max_area, mut max_index) = (-1.0, start);
        let (ax, ay) = (data[a].0 as f64, data[a].1);
        for (j, point) in data.iter().enumerate().take(end + 1).skip(start) {
            let area =
                ((ax - avg_x) * (point.1 - ay) - (ax - point.0 as f64) * (avg_y - ay)).abs() * 0.5;
            if area > max_area {
                max_area = area;
                max_index = j;
            }
        }
        sampled.push(data[max_index]);
        a = max_index;
    }
    sampled.push(data[length - 1]);
    sampled
}

/// `HasMetrics::mapRows`.
pub(crate) fn map_rows(rows: &[Value], field: &str, mins: i64) -> Vec<(i64, f64)> {
    let mut metrics: Vec<(i64, f64)> = rows
        .iter()
        .map(|m| (number(m.get("time")) as i64, number(m.get(field))))
        .collect();
    if mins > 24 * 60 {
        metrics = average_per_minute(&metrics);
    }
    if mins > 60 && metrics.len() > 1000 {
        metrics = downsample_lttb(&metrics, 1000);
    }
    metrics
}

/// `Server\Charts::loadData`.
async fn data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !metrics_enabled(&s) {
        return Ok(ok(json!({"cpuSeries": null, "memorySeries": null, "diskSeries": null, "loadSeries": null, "networkRxSeries": [], "networkTxSeries": []})).into_response());
    }
    let mins: i64 = query
        .get("interval")
        .and_then(|i| i.parse().ok())
        .unwrap_or(5);
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, s);
    let mut answers = prefetch(&srv, &TYPES, None, mins).await;
    let series = |f: &str, rows_: &[Value]| series_json(&map_rows(rows_, f, mins));
    let cpu = match fetch_rows(&srv, &mut answers, "cpu", None, mins).await {
        Ok(r) => r.map_or(Value::Null, |r| series("percent", &r)),
        Err(e) => return toast_error(e),
    };
    let memory = match fetch_rows(&srv, &mut answers, "memory", None, mins).await {
        Ok(r) => r.map_or(Value::Null, |r| series("usedPercent", &r)),
        Err(e) => return toast_error(e),
    };
    // Disk, network and load arrived in a later Sentinel: an older one answers 404 and still shows CPU and memory.
    let disk = fetch_rows(&srv, &mut answers, "disk", None, mins)
        .await
        .ok()
        .flatten()
        .map(|r| {
            // `SentinelMetricsClient::rootMountName`, as the Fleet page picks it.
            let root: Vec<Value> = if r.iter().any(|row| row.get("mount").is_some()) {
                let mount = crate::ui_fleet_metrics::root_mount_name(&r).map(str::to_owned);
                r.into_iter()
                    .filter(|row| row.get("mount").and_then(Value::as_str) == mount.as_deref())
                    .collect()
            } else {
                r
            };
            series("usedPercent", &root)
        });
    let load = fetch_rows(&srv, &mut answers, "load", None, mins)
        .await
        .ok()
        .flatten()
        .map(|r| series("load1", &r));
    let network = fetch_rows(&srv, &mut answers, "network", None, mins)
        .await
        .ok()
        .flatten();
    let (rx, tx) = network.map_or((json!([]), json!([])), |r| {
        (series("rxBytesPerSec", &r), series("txBytesPerSec", &r))
    });
    Ok(ok(json!({
        "cpuSeries": cpu,
        "memorySeries": memory,
        "diskSeries": disk,
        "loadSeries": load,
        "networkRxSeries": rx,
        "networkTxSeries": tx,
    }))
    .into_response())
}

fn series_json(points: &[(i64, f64)]) -> Value {
    json!(
        points
            .iter()
            .map(|(a, b)| json!([a, b]))
            .collect::<Vec<_>>()
    )
}

/// `convertContainerMemoryBytesToMegabytes`.
fn bytes_to_megabytes(points: &[(i64, f64)]) -> Vec<(i64, f64)> {
    points
        .iter()
        .map(|(time, value)| (*time, round_to(value / 1024.0 / 1024.0, 2)))
        .collect()
}

/// `Project\Shared\Metrics::loadData`: an application or database container.
async fn resource_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let res = crate::ui_shared::find(&state, &ctx, &kind, &uuid).await?;
    let server = match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let Some(server) = server.filter(metrics_enabled) else {
        return Ok(ok(json!({"cpu": null, "memory": null})).into_response());
    };
    let mins: i64 = query
        .get("interval")
        .and_then(|i| i.parse().ok())
        .unwrap_or(5);
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, server);
    let mut answers = prefetch(&srv, &["cpu", "memory"], Some(&res.uuid), mins).await;
    let cpu = match fetch_rows(&srv, &mut answers, "cpu", Some(&res.uuid), mins).await {
        Ok(r) => r.map_or(Value::Null, |r| series_json(&map_rows(&r, "percent", mins))),
        Err(e) => return toast_error(e),
    };
    let memory = match fetch_rows(&srv, &mut answers, "memory", Some(&res.uuid), mins).await {
        Ok(r) => r.map_or(Value::Null, |r| {
            series_json(&bytes_to_megabytes(&map_rows(&r, "used", mins)))
        }),
        Err(e) => return toast_error(e),
    };
    Ok(ok(json!({"cpu": cpu, "memory": memory})).into_response())
}

/// `Dashboard\ServerMetricsChart::loadData`: `null` when nothing is dispatched.
async fn dashboard_chart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ServerInfo::from_row(&s).is_functional() {
        return Ok(ok(Value::Null).into_response());
    }
    if !metrics_enabled(&s) {
        return Ok(ok(json!({"cpu": null, "memory": null})).into_response());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let srv = Srv::from_row(jobs, s);
    let mut answers = prefetch(&srv, &["cpu", "memory"], None, 10).await;
    let cpu = fetch_rows(&srv, &mut answers, "cpu", None, 10).await;
    let memory = fetch_rows(&srv, &mut answers, "memory", None, 10).await;
    match (cpu, memory) {
        (Ok(cpu), Ok(memory)) => Ok(ok(json!({
            "cpu": cpu.map_or(Value::Null, |r| series_json(&map_rows(&r, "percent", 10))),
            "memory": memory.map_or(Value::Null, |r| series_json(&map_rows(&r, "usedPercent", 10))),
        }))
        .into_response()),
        // `report($e)`: the chart keeps what it shows.
        _ => Ok(ok(Value::Null).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_reads_have_short_curl_timeouts_and_the_token_on_stdin() {
        assert_eq!(
            sentinel_script(&sentinel_curl(
                "http://localhost:8888/api/cpu/history?from=x",
                "tok"
            )),
            "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\ncurl -sS --connect-timeout 2 --max-time 3 -H @- \"http://localhost:8888/api/cpu/history?from=x\" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer tok\nCOOLIFY_SENTINEL_AUTH\nCOOLIFY_SENTINEL_SCRIPT"
        );
    }

    #[test]
    fn container_memory_is_in_megabytes() {
        assert_eq!(
            bytes_to_megabytes(&[(1, 10_485_760.0), (2, 1_572_864.0)]),
            vec![(1, 10.0), (2, 1.5)]
        );
    }

    #[test]
    fn endpoints_of_servers_and_containers() {
        assert_eq!(
            endpoint(None, "cpu", "T", 60),
            "http://localhost:8888/api/cpu/history?from=T"
        );
        assert_eq!(
            endpoint(Some("abc"), "memory", "T", 5),
            "http://localhost:8888/api/container/abc/memory/history?from=T"
        );
        // Over an hour: whole-minute buckets, at most 1000 per range.
        assert_eq!(
            endpoint(None, "cpu", "T", 1440),
            "http://localhost:8888/api/cpu/history?from=T&bucket_ms=120000"
        );
        assert_eq!(
            endpoint(Some("abc"), "disk", "T", 43200),
            "http://localhost:8888/api/container/abc/disk/history?from=T&bucket_ms=2640000"
        );
    }

    #[test]
    fn lttb_keeps_ends_and_length() {
        let data: Vec<(i64, f64)> = (0..5000).map(|i| (i, (i % 17) as f64)).collect();
        let out = downsample_lttb(&data, 1000);
        assert_eq!(out.len(), 1000);
        assert_eq!(out[0], data[0]);
        assert_eq!(out[999], data[4999]);
    }

    #[test]
    fn averages_per_minute() {
        let out = average_per_minute(&[(60_000, 1.0), (61_000, 3.0), (120_000, 5.0)]);
        assert_eq!(out, vec![(60_000, 2.0), (120_000, 5.0)]);
    }
}
