//! `Fleet\Metrics` (the `/metrics` page) with `SentinelMetricsClient`, `FleetMetricsAggregator`,
//! `BuildsMetricsChartPayload` and `PressureLevel`.

use axum::Extension;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::servers::{ColumnValue, ServerRow};
use coolify_db::traffic::container_resources;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::{Srv, sentinel};
use futures_util::future::{BoxFuture, join_all};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_metrics::audit_sentinel_update;

const BASE: &str = "http://localhost:8888/api";
const CACHE_TTL: Duration = Duration::from_secs(60);
/// `SPARK_MAX_POINTS`.
const SPARK_MAX_POINTS: usize = 40;
/// How many metric reads of the page run at once. They read the answers of
/// [`prefetch_concurrently`], so they rarely reach a server.
const BATCH: usize = 8;
/// `SentinelMetricsClient::REMOTE_TIMEOUT` of the page prefetch.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);
/// `ROOT_MOUNTS`: mounts that can hold the root disk, in the order of `rootMountName()`.
const ROOT_MOUNTS: [&str; 4] = ["/", "/app/db", "/data/coolify", "/data"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/metrics", get(page))
        .route("/ui/metrics/enable", post(enable))
}

type Point = (i64, f64);

/// PHP 8.4's `round($value, $places)`: a value on or past the decimal edge rounds away from zero,
/// so 31.835 is 31.84 although the nearest double is below it.
pub(crate) fn round_to(value: f64, places: i32) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let factor = 10f64.powi(places);
    let scaled = value * factor;
    if scaled.abs() >= 1e16 {
        return value;
    }
    let integral = scaled.trunc();
    let edge = (integral + 0.5f64.copysign(integral)) / factor;
    let rounded = if value.abs() >= edge.abs() {
        integral + 1f64.copysign(integral)
    } else {
        integral
    };
    rounded / factor
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SentinelMetricsClient
// ---------------------------------------------------------------------------

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `rangeFrom`: rounded to the minute, so the history URL is stable within a minute.
fn range_from(range: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = now.timestamp();
    let minute = seconds - seconds.rem_euclid(60);
    let days = match range {
        "7d" => 7,
        "30d" => 30,
        _ => 1,
    };
    chrono::DateTime::from_timestamp(minute - days * 86_400, 0)
        .unwrap_or(now)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// `bucketMs`.
fn bucket_ms(range: &str) -> i64 {
    match range {
        "7d" => 30 * 60_000,
        "30d" => 2 * 60 * 60_000,
        _ => 5 * 60_000,
    }
}

/// `historyQuery`: Sentinel averages rows into `bucket_ms` windows. Older Sentinels ignore the
/// parameter and return raw rows, which `series_rows` buckets the same way.
fn history_query(range: &str, from: &str) -> String {
    format!("from={from}&bucket_ms={}", bucket_ms(range))
}

/// `guard`.
fn guard(response: &str) -> Result<(), String> {
    match serde_json::from_str::<Value>(response) {
        Err(_) | Ok(Value::Null) => Err("Metrics returned an invalid response.".to_string()),
        Ok(Value::Object(map)) if map.contains_key("error") => {
            Err("Metrics request failed.".to_string())
        }
        Ok(_) => Ok(()),
    }
}

/// `rootMountName`: `/`, else the disk that holds /data/coolify.
pub(crate) fn root_mount_name(rows: &[Value]) -> Option<&'static str> {
    ["/", "/app/db", "/data/coolify", "/data"]
        .into_iter()
        .find(|mount| {
            rows.iter()
                .any(|row| row.get("mount").and_then(Value::as_str) == Some(*mount))
        })
}

fn has_mount(rows: &[Value]) -> bool {
    rows.iter().any(|row| row.get("mount").is_some())
}

/// `rootMount`.
fn root_mount(mounts: &[Value]) -> (Option<i64>, Option<i64>, Option<f64>) {
    let root = if has_mount(mounts) {
        root_mount_name(mounts).and_then(|name| {
            mounts
                .iter()
                .find(|row| row.get("mount").and_then(Value::as_str) == Some(name))
        })
    } else {
        mounts.first()
    };
    let Some(root) = root else {
        return (None, None, None);
    };
    let value = |key: &str| root.get(key).and_then(number).unwrap_or(0.0);
    (
        Some(value("used") as i64),
        Some(value("total") as i64),
        Some(value("usedPercent")),
    )
}

#[derive(Debug, Clone, Default)]
struct Summary {
    cpu: Option<f64>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    disk_used: Option<i64>,
    disk_total: Option<i64>,
    disk_percent: Option<f64>,
    load1: Option<f64>,
    net_rx: Option<f64>,
    net_tx: Option<f64>,
}

/// The mapping in `summary`.
fn summary_of(json: &Value) -> Summary {
    let field = |group: &str, key: &str| json.get(group).and_then(|g| g.get(key)).and_then(number);
    let mounts = json
        .get("disk")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (disk_used, disk_total, disk_percent) = root_mount(&mounts);
    Summary {
        cpu: field("cpu", "percent"),
        mem_used: field("memory", "used").map(|v| v as i64),
        mem_total: field("memory", "total").map(|v| v as i64),
        disk_used,
        disk_total,
        disk_percent,
        load1: field("load", "load1"),
        net_rx: field("network", "rxBytesPerSec"),
        net_tx: field("network", "txBytesPerSec"),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Container {
    id: String,
    cpu: Option<f64>,
    mem_used: Option<i64>,
    mem_percent: Option<f64>,
    disk_bytes: Option<i64>,
    net: Option<f64>,
    server: String,
    server_uuid: String,
    name: String,
    image: Option<String>,
    link: Option<String>,
}

/// The mapping in `containersCurrent`: only a container that Docker still lists has a status.
fn containers_of(rows: &[Value], server: &ServerRow) -> Vec<Container> {
    rows.iter()
        .filter(|c| {
            c.get("status")
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str)
                == Some("running")
        })
        .map(|c| {
            let field =
                |group: &str, key: &str| c.get(group).and_then(|g| g.get(key)).and_then(number);
            let present = |group: &str| c.get(group).is_some_and(|g| !g.is_null());
            let disk = field("disk", "writableLayer").unwrap_or(0.0)
                + field("disk", "volumesTotal").unwrap_or(0.0);
            let net = field("network", "rxBytesPerSec").unwrap_or(0.0)
                + field("network", "txBytesPerSec").unwrap_or(0.0);
            let id = c
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Container {
                name: id.clone(),
                id,
                cpu: field("cpu", "percent"),
                mem_used: field("memory", "used").map(|v| v as i64),
                mem_percent: field("memory", "usedPercent"),
                disk_bytes: present("disk").then_some(disk as i64),
                net: present("network").then_some(net),
                server: server_name(server),
                server_uuid: server.uuid.clone(),
                image: None,
                link: None,
            }
        })
        .collect()
}

/// `seriesRows`: raw samples averaged into epoch-aligned buckets, so every server shares timestamps.
fn series_rows(rows: &[Value], field: &str, range: &str, root_only: bool) -> Vec<Point> {
    let mount = (root_only && has_mount(rows)).then(|| root_mount_name(rows));
    let width = bucket_ms(range);
    let mut buckets: BTreeMap<i64, (f64, u32)> = BTreeMap::new();
    for row in rows {
        if let Some(name) = mount
            && row.get("mount").and_then(Value::as_str) != name
        {
            continue;
        }
        let time = row.get("time").and_then(number).unwrap_or(0.0) as i64;
        let entry = buckets.entry(time / width * width).or_default();
        entry.0 += row.get(field).and_then(number).unwrap_or(0.0);
        entry.1 += 1;
    }
    buckets
        .into_iter()
        .map(|(bucket, (sum, count))| (bucket, round_to(sum / f64::from(count), 2)))
        .collect()
}

/// `historyUrl`: Sentinel 1.1.4+ returns only the root mounts of the disk history; older
/// versions ignore `mount`.
fn history_url(metric: &str, range: &str, from: &str) -> String {
    let path = match metric {
        "memory" | "disk" | "load" | "network" => metric,
        _ => "cpu",
    };
    let mut query = history_query(range, from);
    if metric == "disk" {
        // `http_build_query` encodes `/` and `,`.
        let mounts = ROOT_MOUNTS
            .join(",")
            .replace('/', "%2F")
            .replace(',', "%2C");
        query.push_str(&format!("&mount={mounts}"));
    }
    format!("{BASE}/{path}/history?{query}")
}

/// `pageUrls`: every URL the page reads for one server.
fn page_urls(range: &str, from: &str) -> Vec<String> {
    let mut urls = vec![
        format!("{BASE}/summary"),
        format!("{BASE}/containers/current"),
    ];
    urls.extend(
        ["cpu", "memory", "disk", "load", "network"]
            .iter()
            .map(|m| history_url(m, range, from)),
    );
    urls
}

fn cache_key(srv: &Srv, url: &str) -> String {
    format!("metrics:{}:{url}", srv.row.uuid)
}

fn cached(key: &str) -> Option<String> {
    let cache = CACHE.lock().ok()?;
    cache
        .get(key)
        .filter(|(until, _)| *until > Instant::now())
        .map(|(_, body)| body.clone())
}

fn remember(key: String, body: &str) {
    if let Ok(mut cache) = CACHE.lock() {
        let now = Instant::now();
        cache.retain(|_, (until, _)| *until > now);
        cache.insert(key, (now + CACHE_TTL, body.to_owned()));
    }
}

/// `startBatch` and `awaitBatch`: one `docker exec` that curls every URL in order, the responses
/// separated by 0x1E. `true` last: a failed curl empties only its own response.
async fn fetch_batch(srv: &Srv, urls: &[String]) -> Result<String, String> {
    let token = sentinel::ensure_valid_token(srv).await?;
    let script = urls
        .iter()
        .map(|url| {
            format!(
                "curl -s --connect-timeout 3 --max-time 10 -H @- \"{url}\" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer {token}\nCOOLIFY_SENTINEL_AUTH"
            )
        })
        .collect::<Vec<_>>()
        .join("\nprintf '\\036'\n");
    let command = format!(
        "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\n{script}\ntrue\nCOOLIFY_SENTINEL_SCRIPT"
    );
    Ok(srv
        .run_once(&[command], "fleet.metrics", REMOTE_TIMEOUT)
        .await?
        .unwrap_or_default())
}

/// `SentinelMetricsClient::prefetchConcurrently`: the page responses of every server at the same
/// time, one SSH command per server, so the page waits for the slowest server only. URLs still
/// cached are not fetched again; a failed fetch is not tried again per metric.
async fn prefetch_concurrently(
    servers: Vec<std::sync::Arc<Srv>>,
    range: &str,
    from: &str,
) -> HashMap<String, HashMap<String, Option<String>>> {
    let tasks = servers.into_iter().map(|srv| {
        let urls: Vec<String> = page_urls(range, from)
            .into_iter()
            .filter(|url| cached(&cache_key(&srv, url)).is_none())
            .collect();
        async move {
            let mut prefetched = HashMap::new();
            if urls.is_empty() {
                return (srv.row.uuid.clone(), prefetched);
            }
            let output = fetch_batch(&srv, &urls).await.unwrap_or_default();
            let mut responses: Vec<&str> = output.split('\u{1e}').collect();
            if responses.len() != urls.len() {
                responses.clear();
            }
            for (index, url) in urls.iter().enumerate() {
                let body = responses.get(index).map_or("", |r| r.trim());
                let answer = guard(body).ok().map(|()| {
                    remember(cache_key(&srv, url), body);
                    body.to_owned()
                });
                prefetched.insert(url.clone(), answer);
            }
            (srv.row.uuid.clone(), prefetched)
        }
    });
    join_all(tasks).await.into_iter().collect()
}

#[derive(Clone)]
struct MetricsClient {
    srv: std::sync::Arc<Srv>,
    /// url => response of [`prefetch_concurrently`]; `None` when it failed.
    prefetched: std::sync::Arc<HashMap<String, Option<String>>>,
}

impl MetricsClient {
    /// `raw`: the prefetched answer, else one kept for a minute; a failure is not kept.
    async fn raw(&self, url: &str) -> Result<String, String> {
        if let Some(answer) = self.prefetched.get(url) {
            return answer
                .clone()
                .ok_or_else(|| "Metrics request failed.".to_string());
        }
        let key = cache_key(&self.srv, url);
        if let Some(body) = cached(&key) {
            return Ok(body);
        }
        let body = self.remote_fetch(url).await;
        guard(&body)?;
        remember(key, &body);
        Ok(body)
    }

    /// `remoteFetch`, through `fetchOverSsh`; no answer is an empty one.
    async fn remote_fetch(&self, url: &str) -> String {
        let Ok(token) = sentinel::ensure_valid_token(&self.srv).await else {
            return String::new();
        };
        let command = format!(
            "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\ncurl -sS --connect-timeout 2 --max-time 3 -H @- \"{url}\" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer {token}\nCOOLIFY_SENTINEL_AUTH\nCOOLIFY_SENTINEL_SCRIPT"
        );
        crate::ui_server_metrics::fetch_over_ssh(&self.srv, &[command], url)
            .await
            .unwrap_or_default()
    }

    async fn rows(&self, url: &str) -> Vec<Value> {
        self.raw(url)
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|json| json.as_array().cloned())
            .unwrap_or_default()
    }

    /// `summary`.
    async fn summary(&self) -> Option<Summary> {
        let body = self.raw(&format!("{BASE}/summary")).await.ok()?;
        let json = serde_json::from_str::<Value>(&body).ok()?;
        (json.is_object() || json.is_array()).then(|| summary_of(&json))
    }

    /// `history`.
    async fn history(&self, metric: &str, range: &str, from: &str) -> Vec<Point> {
        let field = match metric {
            "memory" => "used",
            "disk" => "usedPercent",
            "load" => "load1",
            _ => "percent",
        };
        let rows = self.rows(&history_url(metric, range, from)).await;
        series_rows(&rows, field, range, metric == "disk")
    }

    /// `networkHistory`.
    async fn network_history(&self, range: &str, from: &str) -> (Vec<Point>, Vec<Point>) {
        let rows = self.rows(&history_url("network", range, from)).await;
        (
            series_rows(&rows, "rxBytesPerSec", range, false),
            series_rows(&rows, "txBytesPerSec", range, false),
        )
    }
}

// ---------------------------------------------------------------------------
// FleetMetricsAggregator
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    uuid: String,
    name: String,
    online: bool,
    reporting: bool,
    cpu: Option<f64>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    disk_used: Option<i64>,
    disk_total: Option<i64>,
    disk_percent: Option<f64>,
    load1: Option<f64>,
    net_rx: Option<f64>,
    net_tx: Option<f64>,
    containers: Option<i64>,
}

/// `PressureLevel::for`.
fn pressure_is_ok(percent: Option<f64>, kind: &str) -> bool {
    let warn = if kind == "disk" { 80.0 } else { 70.0 };
    percent.is_none_or(|p| p < warn)
}

/// `percent`.
fn percent(used: i64, total: i64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    round_to(used as f64 / total as f64 * 100.0, 1)
}

/// `fleetKpis`.
fn fleet_kpis(rows: &[Row]) -> Value {
    let online: Vec<&Row> = rows.iter().filter(|r| r.online).collect();
    let reporting: Vec<&Row> = online.iter().copied().filter(|r| r.reporting).collect();
    let cpu_values: Vec<f64> = reporting.iter().filter_map(|r| r.cpu).collect();
    let cpu_avg = if cpu_values.is_empty() {
        0.0
    } else {
        round_to(cpu_values.iter().sum::<f64>() / cpu_values.len() as f64, 1)
    };
    let mut busiest: Option<(&str, f64)> = None;
    for r in &reporting {
        if let Some(cpu) = r.cpu
            && busiest.is_none_or(|(_, p)| cpu > p)
        {
            busiest = Some((&r.name, cpu));
        }
    }
    let sum = |f: fn(&Row) -> Option<i64>| reporting.iter().filter_map(|r| f(r)).sum::<i64>();
    let mem_used = sum(|r| r.mem_used);
    let mem_total = sum(|r| r.mem_total);
    let disk_used = sum(|r| r.disk_used);
    let disk_total = sum(|r| r.disk_total);
    let load1: Vec<f64> = reporting.iter().filter_map(|r| r.load1).collect();
    let needs_attention = reporting
        .iter()
        .filter(|r| {
            !pressure_is_ok(r.cpu, "cpu")
                || !pressure_is_ok(
                    Some(percent(r.mem_used.unwrap_or(0), r.mem_total.unwrap_or(0))),
                    "memory",
                )
                || !pressure_is_ok(r.disk_percent, "disk")
        })
        .count();
    json!({
        "serversOnline": online.len(),
        "serversReporting": reporting.len(),
        "serversTotal": rows.len(),
        "needsAttention": needs_attention,
        "cpuAvg": cpu_avg,
        "cpuApproximate": cpu_values.len() > 1,
        "cpuBusiest": busiest.map(|(name, percent)| json!({"name": name, "percent": percent})),
        "memUsed": mem_used,
        "memTotal": mem_total,
        "memPercent": percent(mem_used, mem_total),
        "diskUsed": disk_used,
        "diskTotal": disk_total,
        "diskPercent": percent(disk_used, disk_total),
        "netRx": reporting.iter().filter_map(|r| r.net_rx).fold(0.0, |a, b| a + b),
        "netTx": reporting.iter().filter_map(|r| r.net_tx).fold(0.0, |a, b| a + b),
        "loadBusiest": load1.iter().copied().fold(0.0, f64::max),
        "loadAvg": if load1.is_empty() { 0.0 } else { round_to(load1.iter().sum::<f64>() / load1.len() as f64, 2) },
        "containers": reporting.iter().filter_map(|r| r.containers).sum::<i64>(),
    })
}

/// `sumSeriesByBucket`.
fn sum_series_by_bucket(per_server: &[Vec<Point>], average: bool) -> Vec<Point> {
    let mut totals: BTreeMap<i64, (f64, u32)> = BTreeMap::new();
    for series in per_server {
        for (time, value) in series {
            let entry = totals.entry(*time).or_default();
            entry.0 += value;
            entry.1 += 1;
        }
    }
    totals
        .into_iter()
        .map(|(time, (sum, count))| {
            (
                time,
                round_to(if average { sum / f64::from(count) } else { sum }, 2),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// BuildsMetricsChartPayload
// ---------------------------------------------------------------------------

/// `combineSeries`.
fn combine_series(a: &[Point], b: &[Point]) -> Vec<Point> {
    let mut map: BTreeMap<i64, f64> = BTreeMap::new();
    for (time, value) in a.iter().chain(b) {
        *map.entry(*time).or_default() += value;
    }
    map.into_iter().collect()
}

/// `alignSpark`.
fn align_spark(series: &[Point], categories: &[i64]) -> Vec<Option<f64>> {
    let map: HashMap<i64, f64> = series.iter().copied().collect();
    categories.iter().map(|t| map.get(t).copied()).collect()
}

/// The evenly spaced indices of `capSparks`, first and last always kept.
fn cap_indices(count: usize, max: usize) -> Option<Vec<usize>> {
    if count <= max || max < 2 {
        return None;
    }
    let mut indices: Vec<usize> = (0..max)
        .map(|i| (i as f64 * (count - 1) as f64 / (max - 1) as f64).round() as usize)
        .collect();
    indices.dedup();
    Some(indices)
}

fn points_json(points: &[Point]) -> Value {
    json!(
        points
            .iter()
            .map(|(t, v)| json!([t, v]))
            .collect::<Vec<_>>()
    )
}

#[derive(Default)]
struct SeriesByServer {
    cpu: Vec<Vec<Point>>,
    memory: Vec<Vec<Point>>,
    disk: Vec<Vec<Point>>,
    load: Vec<Vec<Point>>,
    network_rx: Vec<Vec<Point>>,
    network_tx: Vec<Vec<Point>>,
}

/// `buildChartPayload`.
fn build_chart_payload(series: &SeriesByServer, range: &str) -> Value {
    let cpu = sum_series_by_bucket(&series.cpu, true);
    let memory = sum_series_by_bucket(&series.memory, false);
    let disk = sum_series_by_bucket(&series.disk, true);
    let load = sum_series_by_bucket(&series.load, true);
    let rx = sum_series_by_bucket(&series.network_rx, false);
    let tx = sum_series_by_bucket(&series.network_tx, false);
    let network = combine_series(&rx, &tx);
    let categories: Vec<i64> = cpu
        .iter()
        .chain(&memory)
        .chain(&disk)
        .chain(&network)
        .map(|(t, _)| *t)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let indices = cap_indices(categories.len(), SPARK_MAX_POINTS);
    let spark = |values: &[Point]| -> Value {
        let aligned = align_spark(values, &categories);
        match &indices {
            Some(indices) => json!(indices.iter().map(|i| aligned[*i]).collect::<Vec<_>>()),
            None => json!(aligned),
        }
    };
    let spark_categories: Vec<i64> = match &indices {
        Some(indices) => indices.iter().map(|i| categories[*i]).collect(),
        None => categories.clone(),
    };
    json!({
        "range": range,
        "cpu": points_json(&cpu),
        "memory": points_json(&memory),
        "disk": points_json(&disk),
        "load": points_json(&load),
        "networkRx": points_json(&rx),
        "networkTx": points_json(&tx),
        "sparkCategories": spark_categories,
        "cpuSpark": spark(&cpu),
        "memSpark": spark(&memory),
        "diskSpark": spark(&disk),
        "netSpark": spark(&network),
    })
}

// ---------------------------------------------------------------------------
// Fleet\Metrics
// ---------------------------------------------------------------------------

fn server_name(server: &ServerRow) -> String {
    server
        .record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn metrics_enabled(server: &ServerRow) -> bool {
    server
        .settings
        .as_ref()
        .and_then(|s| s.get("is_metrics_enabled"))
        .is_some_and(php_truthy)
}

fn is_functional(server: &ServerRow) -> bool {
    ServerInfo::from_row(server).is_functional()
}

/// `isSentinelEnabled`.
fn sentinel_enabled(server: &ServerRow) -> bool {
    let info = ServerInfo::from_row(server);
    !info.is_build_server()
        && !info.is_swarm()
        && info.setting_bool("force_disabled") != Some(true)
        && !info.transferred
}

/// `serversWithoutMetrics`.
fn servers_without_metrics<'a>(servers: &'a [ServerRow], ctx: &AuthCtx) -> Vec<&'a ServerRow> {
    servers
        .iter()
        .filter(|s| {
            !metrics_enabled(s) && sentinel_enabled(s) && ctx.is_team_admin() && is_functional(s)
        })
        .collect()
}

/// `rowWithoutMetrics`.
fn row_without_metrics(server: &ServerRow) -> Row {
    Row {
        uuid: server.uuid.clone(),
        name: server_name(server),
        online: is_functional(server),
        reporting: false,
        cpu: None,
        mem_used: None,
        mem_total: None,
        disk_used: None,
        disk_total: None,
        disk_percent: None,
        load1: None,
        net_rx: None,
        net_tx: None,
        containers: None,
    }
}

/// `rankContainers`: the sort key.
fn rank_key(container: &Container, metric: &str) -> f64 {
    match metric {
        "memory" => container.mem_used.unwrap_or(0) as f64,
        "disk" => container.disk_bytes.unwrap_or(0) as f64,
        "network" => container.net.unwrap_or(0.0),
        _ => container.cpu.unwrap_or(0.0),
    }
}

/// `rankContainers`: the indices of the top `limit`; ties keep their order, as PHP's stable `usort`.
fn ranked(containers: &[Container], metric: &str, limit: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..containers.len()).collect();
    order.sort_by(|a, b| {
        rank_key(&containers[*b], metric)
            .partial_cmp(&rank_key(&containers[*a], metric))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order.truncate(limit);
    order
}

enum Answer {
    Series(Vec<Point>),
    Containers(Vec<Value>),
    Network(Vec<Point>, Vec<Point>),
}

/// The page's metric reads, a few at a time; one failure only misses its own value (`attempt`).
async fn run_batched<T>(tasks: Vec<BoxFuture<'static, T>>) -> Vec<T> {
    let mut results = Vec::with_capacity(tasks.len());
    let mut tasks = tasks.into_iter().peekable();
    while tasks.peek().is_some() {
        let batch: Vec<_> = tasks.by_ref().take(BATCH).collect();
        results.extend(join_all(batch).await);
    }
    results
}

#[derive(Debug, Deserialize)]
struct Filters {
    range: Option<String>,
    server: Option<String>,
    cmetric: Option<String>,
}

/// `mount` with `loadData`.
async fn page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(filters): Query<Filters>,
) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let all = coolify_db::servers::list_by_name(&state.db, ctx.team_id).await?;
    let servers: Vec<&ServerRow> = all.iter().filter(|s| metrics_enabled(s)).collect();
    let enableable: Vec<String> = if servers.is_empty() {
        servers_without_metrics(&all, &ctx)
            .into_iter()
            .map(server_name)
            .collect()
    } else {
        Vec::new()
    };
    // `#[Url]` keeps what the address says; `rangeFrom` and `bucketMs` read anything else as a day.
    let range = filters.range.unwrap_or_else(|| "24h".to_string());
    let mut server_uuid = filters.server.unwrap_or_default();
    if !servers.iter().any(|s| s.uuid == server_uuid) {
        server_uuid.clear();
    }
    let metric = filters
        .cmetric
        .filter(|m| matches!(m.as_str(), "cpu" | "memory" | "disk" | "network"))
        .unwrap_or_else(|| "cpu".to_string());
    let server_options: Vec<Value> = servers
        .iter()
        .map(|s| json!({"value": s.uuid, "label": server_name(s)}))
        .collect();
    let base = json!({
        "hasServers": !servers.is_empty(),
        "enableableServers": enableable,
        "serverOptions": server_options,
        "range": range,
        "serverUuid": server_uuid,
        "containerMetric": metric,
    });
    if servers.is_empty() {
        return Ok(ok(base).into_response());
    }

    let targets: Vec<&ServerRow> = servers
        .iter()
        .copied()
        .filter(|s| server_uuid.is_empty() || s.uuid == server_uuid)
        .collect();
    // Coolify does not connect to an unreachable server.
    let functional: Vec<std::sync::Arc<Srv>> = targets
        .iter()
        .filter(|s| is_functional(s))
        .map(|s| std::sync::Arc::new(Srv::from_row(jobs, (*s).clone())))
        .collect();
    let from = range_from(&range, chrono::Utc::now());
    let mut prefetched = prefetch_concurrently(functional.clone(), &range, &from).await;
    let clients: HashMap<String, MetricsClient> = functional
        .into_iter()
        .map(|srv| {
            let answers = prefetched.remove(&srv.row.uuid).unwrap_or_default();
            (
                srv.row.uuid.clone(),
                MetricsClient {
                    srv,
                    prefetched: std::sync::Arc::new(answers),
                },
            )
        })
        .collect();
    let keys: Vec<String> = clients.keys().cloned().collect();
    let summaries: HashMap<String, Option<Summary>> = {
        let tasks = keys
            .iter()
            .map(|uuid| {
                let (uuid, client) = (uuid.clone(), clients[uuid].clone());
                Box::pin(async move {
                    let summary = client.summary().await;
                    (uuid, summary)
                }) as BoxFuture<'static, _>
            })
            .collect();
        run_batched(tasks).await.into_iter().collect()
    };

    let mut tasks: Vec<BoxFuture<'static, (String, Answer)>> = Vec::new();
    for uuid in &keys {
        let has_summary = summaries.get(uuid).is_some_and(Option::is_some);
        let metrics: &[&'static str] = if has_summary {
            &["cpu", "memory", "disk", "load"]
        } else {
            &["cpu", "memory"]
        };
        for metric in metrics {
            let (client, range, from, key) = (
                clients[uuid].clone(),
                range.clone(),
                from.clone(),
                format!("{uuid}:{metric}"),
            );
            let metric = *metric;
            tasks.push(Box::pin(async move {
                let series = client.history(metric, &range, &from).await;
                (key, Answer::Series(series))
            }));
        }
        if has_summary {
            let client = clients[uuid].clone();
            let key = format!("{uuid}:containers");
            tasks.push(Box::pin(async move {
                // `containersCurrent`: an invalid answer is no containers.
                let rows = client.rows(&format!("{BASE}/containers/current")).await;
                (key, Answer::Containers(rows))
            }));
            let (client, range, from) = (clients[uuid].clone(), range.clone(), from.clone());
            let key = format!("{uuid}:network");
            tasks.push(Box::pin(async move {
                let (rx, tx) = client.network_history(&range, &from).await;
                (key, Answer::Network(rx, tx))
            }));
        }
    }
    let mut results: HashMap<String, Answer> = run_batched(tasks).await.into_iter().collect();

    let mut rows = Vec::with_capacity(targets.len());
    let mut containers: Vec<Container> = Vec::new();
    let mut series = SeriesByServer::default();
    for server in &targets {
        let mut take = |metric: &str| match results.remove(&format!("{}:{metric}", server.uuid)) {
            Some(Answer::Series(points)) => points,
            _ => Vec::new(),
        };
        series.cpu.push(take("cpu"));
        series.memory.push(take("memory"));
        series.disk.push(take("disk"));
        series.load.push(take("load"));
        let Some(Some(summary)) = summaries.get(&server.uuid) else {
            rows.push(row_without_metrics(server));
            continue;
        };
        let current = match results.remove(&format!("{}:containers", server.uuid)) {
            Some(Answer::Containers(raw)) => containers_of(&raw, server),
            _ => Vec::new(),
        };
        rows.push(Row {
            uuid: server.uuid.clone(),
            name: server_name(server),
            online: true,
            reporting: true,
            cpu: summary.cpu,
            mem_used: summary.mem_used,
            mem_total: summary.mem_total,
            disk_used: summary.disk_used,
            disk_total: summary.disk_total,
            disk_percent: summary.disk_percent,
            load1: summary.load1,
            net_rx: summary.net_rx,
            net_tx: summary.net_tx,
            containers: Some(current.len() as i64),
        });
        containers.extend(current);
        if let Some(Answer::Network(rx, tx)) = results.remove(&format!("{}:network", server.uuid)) {
            series.network_rx.push(rx);
            series.network_tx.push(tx);
        }
    }

    // The toggle re-ranks in the browser without re-fetching, so it gets every container that can
    // be in the top 20 of a metric, in their fetched order. Names are resolved for those only.
    let shown: HashSet<usize> = ["cpu", "memory", "disk", "network"]
        .iter()
        .flat_map(|m| ranked(&containers, m, 20))
        .collect();
    let mut kept: Vec<Container> = containers
        .into_iter()
        .enumerate()
        .filter(|(i, _)| shown.contains(i))
        .map(|(_, c)| c)
        .collect();
    let ids: Vec<String> = kept.iter().map(|c| c.id.clone()).collect();
    let metas = container_resources(&state.db, &ids).await?;
    for container in &mut kept {
        // `containerMeta`: a Sentinel id never discloses another team's resource.
        let Some(meta) = metas
            .iter()
            .find(|m| m.uuid == container.id && m.team_id == Some(ctx.team_id))
        else {
            continue;
        };
        if let Some(name) = &meta.name {
            container.name.clone_from(name);
        }
        container.image = meta.image.clone().filter(|i| !i.is_empty() && i != "0");
        if meta.is_application
            && let (Some(project), Some(environment)) = (&meta.project_uuid, &meta.environment_uuid)
        {
            container.link = Some(format!(
                "/project/{project}/environment/{environment}/application/{}/metrics",
                meta.uuid
            ));
        }
    }

    let mut out = base;
    out["kpis"] = fleet_kpis(&rows);
    out["serverRows"] = json!(rows);
    out["containersRaw"] = json!(kept);
    out["chartData"] = build_chart_payload(&series, &range);
    Ok(ok(out).into_response())
}

/// `enableMetrics`: one failed server must not stop the others.
async fn enable(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let all = coolify_db::servers::list_by_name(&state.db, ctx.team_id).await?;
    let servers = servers_without_metrics(&all, &ctx);
    let mut failed = Vec::new();
    for server in &servers {
        let srv = Srv::from_row(jobs, (*server).clone());
        // Saved quietly: Sentinel is started below, so the settings hook must not start it a second time.
        match srv
            .settings_update(&[("is_metrics_enabled", ColumnValue::Bool(true))])
            .await
        {
            Ok(_) => {
                audit_sentinel_update(&state, &ctx, &meta, server, &["is_metrics_enabled"]).await;
                jobs.spawn_restart_sentinel(server.id);
            }
            Err(_) => failed.push(server_name(server)),
        }
    }
    let enabled = servers.len() - failed.len();
    let success = (enabled > 0).then(|| {
        format!(
            "Metrics enabled on {enabled} {}. Sentinel is starting, data shows in about one minute.",
            if enabled == 1 { "server" } else { "servers" }
        )
    });
    let error = (!failed.is_empty()).then(|| {
        format!(
            "Could not enable metrics on {}. Check the server settings and try again.",
            failed.join(", ")
        )
    });
    Ok(ok(json!({"success": success, "error": error})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, cpu: Option<f64>, mem: (i64, i64), disk: Option<f64>) -> Row {
        Row {
            uuid: name.into(),
            name: name.into(),
            online: true,
            reporting: true,
            cpu,
            mem_used: Some(mem.0),
            mem_total: Some(mem.1),
            disk_used: Some(10),
            disk_total: Some(100),
            disk_percent: disk,
            load1: Some(1.0),
            net_rx: Some(1.0),
            net_tx: Some(2.0),
            containers: Some(3),
        }
    }

    #[test]
    fn kpis_count_pressure_and_average() {
        let kpis = fleet_kpis(&[
            row("a", Some(10.0), (10, 100), Some(10.0)),
            row("b", Some(71.0), (10, 100), Some(10.0)),
            row("c", None, (10, 100), Some(80.0)),
        ]);
        assert_eq!(kpis["needsAttention"], 2);
        assert_eq!(kpis["cpuAvg"], 40.5);
        assert_eq!(kpis["cpuApproximate"], true);
        assert_eq!(kpis["cpuBusiest"]["name"], "b");
        assert_eq!(kpis["memPercent"], 10.0);
        assert_eq!(kpis["netTx"], 6.0);
        assert_eq!(kpis["containers"], 9);
    }

    #[test]
    fn buckets_are_epoch_aligned_and_keep_the_root_mount() {
        let rows = vec![
            json!({"time": "300000", "mount": "/", "usedPercent": 10.0}),
            json!({"time": 310000, "mount": "/", "usedPercent": 20.0}),
            json!({"time": 310000, "mount": "/data", "usedPercent": 99.0}),
            json!({"time": 600000, "mount": "/", "usedPercent": 30.0}),
        ];
        assert_eq!(
            series_rows(&rows, "usedPercent", "24h", true),
            vec![(300_000, 15.0), (600_000, 30.0)]
        );
    }

    #[test]
    fn rounds_as_php() {
        assert_eq!(round_to(31.835, 2), 31.84);
        assert_eq!(round_to(1.005, 2), 1.01);
        assert_eq!(round_to(-1.005, 2), -1.01);
        assert_eq!(round_to(2.384_999, 2), 2.38);
        assert_eq!(round_to(2.0, 2), 2.0);
        assert_eq!(round_to(0.0, 2), 0.0);
    }

    #[test]
    fn range_starts_on_a_minute() {
        let now = chrono::DateTime::from_timestamp(1_793_474_021, 0).unwrap();
        assert_eq!(range_from("24h", now), "2026-10-30T19:13:00Z");
        assert_eq!(range_from("7d", now), "2026-10-24T19:13:00Z");
    }

    #[test]
    fn sparks_keep_first_and_last() {
        let indices = cap_indices(100, 40).unwrap();
        assert_eq!(indices.len(), 40);
        assert_eq!(indices[0], 0);
        assert_eq!(indices[39], 99);
        assert!(cap_indices(40, 40).is_none());
    }

    #[test]
    fn guard_rejects_errors_and_garbage() {
        assert!(guard("[]").is_ok());
        assert!(guard("{\"cpu\":null}").is_ok());
        assert!(guard("{\"error\":\"x\"}").is_err());
        assert!(guard("").is_err());
        assert!(guard("null").is_err());
    }

    #[test]
    fn page_urls_ask_disk_history_for_the_root_mounts() {
        let from = "2026-11-08T10:00:00Z";
        assert_eq!(
            history_url("disk", "24h", from),
            "http://localhost:8888/api/disk/history?from=2026-11-08T10:00:00Z&bucket_ms=300000&mount=%2F%2C%2Fapp%2Fdb%2C%2Fdata%2Fcoolify%2C%2Fdata"
        );
        assert_eq!(
            history_url("bogus", "7d", from),
            "http://localhost:8888/api/cpu/history?from=2026-11-08T10:00:00Z&bucket_ms=1800000"
        );
        let urls = page_urls("24h", from);
        assert_eq!(urls.len(), 7);
        assert_eq!(urls[0], "http://localhost:8888/api/summary");
        assert_eq!(urls[1], "http://localhost:8888/api/containers/current");
        assert!(urls[6].starts_with("http://localhost:8888/api/network/history?"));
    }

    #[test]
    fn root_mount_prefers_slash_and_falls_back() {
        let rows = vec![
            json!({"mount": "/data", "used": 1, "total": 2, "usedPercent": 50.0}),
            json!({"mount": "/app/db", "used": 3, "total": 4, "usedPercent": 75.0}),
        ];
        assert_eq!(root_mount(&rows), (Some(3), Some(4), Some(75.0)));
        assert_eq!(root_mount(&[]), (None, None, None));
    }
}
