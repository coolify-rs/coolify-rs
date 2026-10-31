//! `Services\TrafficAnalyticsAggregator` with the `Data\Traffic` shapes.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use super::client::{Client, Scope};
use super::helpers::referer_host;
use super::resource::Resources;

fn data_get<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(row, |value, key| value.get(key))
}

/// `(int) data_get(...)`.
fn int(row: &Value, path: &str) -> i64 {
    match data_get(row, path) {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => s
            .parse::<i64>()
            .unwrap_or_else(|_| s.parse::<f64>().unwrap_or(0.0) as i64),
        Some(Value::Bool(b)) => i64::from(*b),
        _ => 0,
    }
}

/// `(float) data_get(...)`.
fn float(row: &Value, path: &str) -> f64 {
    match data_get(row, path) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

fn text(row: &Value, path: &str) -> String {
    match data_get(row, path) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(true)) => "1".to_string(),
        _ => String::new(),
    }
}

/// `is_numeric`.
fn numeric(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `TrafficOverviewData`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub requests: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub s2xx: i64,
    pub s3xx: i64,
    pub s4xx: i64,
    pub s5xx: i64,
    pub latency_p50: f64,
    pub latency_p95: f64,
    pub latency_p99: f64,
    pub unique_visitors: i64,
}

impl Overview {
    pub fn from_sentinel(json: &Value) -> Self {
        Self {
            requests: int(json, "requests"),
            bytes_in: int(json, "bytes_in"),
            bytes_out: int(json, "bytes_out"),
            s2xx: int(json, "status.s2xx"),
            s3xx: int(json, "status.s3xx"),
            s4xx: int(json, "status.s4xx"),
            s5xx: int(json, "status.s5xx"),
            latency_p50: float(json, "latency.p50"),
            latency_p95: float(json, "latency.p95"),
            latency_p99: float(json, "latency.p99"),
            unique_visitors: int(json, "unique_visitors"),
        }
    }
}

/// `TrafficPathData`.
#[derive(Debug, Clone)]
pub struct PathRow {
    pub path: String,
    pub requests: i64,
    pub bytes_out: i64,
    pub s4xx: i64,
    pub s5xx: i64,
    pub p95: f64,
    pub app: String,
}

impl PathRow {
    pub fn from_sentinel(row: &Value) -> Self {
        Self {
            path: text(row, "path"),
            requests: int(row, "requests"),
            bytes_out: int(row, "bytes_out"),
            s4xx: int(row, "s4xx"),
            s5xx: int(row, "s5xx"),
            p95: float(row, "p95"),
            app: text(row, "app"),
        }
    }
}

/// `TrafficBreakdownData`.
#[derive(Debug, Clone)]
pub struct Breakdown {
    pub value: String,
    pub requests: i64,
    pub bytes_out: i64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl Breakdown {
    pub fn from_sentinel(row: &Value) -> Self {
        let coordinates = match (numeric(row.get("lat")), numeric(row.get("lon"))) {
            (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => Some((lat, lon)),
            _ => None,
        };
        Self {
            value: text(row, "value"),
            requests: int(row, "requests"),
            bytes_out: int(row, "bytes_out"),
            lat: coordinates.map(|c| c.0),
            lon: coordinates.map(|c| c.1),
        }
    }
}

/// `TrafficSeriesBucketData`.
#[derive(Debug, Clone, Default)]
pub struct SeriesBucket {
    pub bucket: i64,
    pub s2xx: i64,
    pub s3xx: i64,
    pub s4xx: i64,
    pub s5xx: i64,
    pub requests: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub unique_visitors: i64,
    pub p95: f64,
}

impl SeriesBucket {
    pub fn from_sentinel(row: &Value) -> Self {
        Self {
            bucket: int(row, "bucket"),
            s2xx: int(row, "s2xx"),
            s3xx: int(row, "s3xx"),
            s4xx: int(row, "s4xx"),
            s5xx: int(row, "s5xx"),
            requests: int(row, "requests"),
            bytes_in: int(row, "bytes_in"),
            bytes_out: int(row, "bytes_out"),
            unique_visitors: int(row, "unique_visitors"),
            p95: float(row, "p95"),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SeriesOut {
    pub bucket: i64,
    pub s2xx: i64,
    pub s3xx: i64,
    pub s4xx: i64,
    pub s5xx: i64,
    pub requests: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub unique_visitors: i64,
    pub p95: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PathOut {
    pub path: String,
    pub domain: Option<String>,
    pub requests: i64,
    pub bytes_out: i64,
    pub s4xx: i64,
    pub s5xx: i64,
    pub p95: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BreakdownRow {
    pub value: String,
    pub requests: i64,
    pub bytes_out: i64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// A PHP array keyed by string: insertion ordered, small enough to search.
pub fn entry<'a, T>(
    rows: &'a mut Vec<(String, T)>,
    key: &str,
    make: impl FnOnce() -> T,
) -> &'a mut T {
    let index = match rows.iter().position(|(k, _)| k == key) {
        Some(index) => index,
        None => {
            rows.push((key.to_string(), make()));
            rows.len() - 1
        }
    };
    &mut rows[index].1
}

/// `topByRequests`: a stable sort, largest first.
pub fn top_by_requests<T>(mut rows: Vec<T>, requests: impl Fn(&T) -> i64, limit: usize) -> Vec<T> {
    rows.sort_by_key(|row| std::cmp::Reverse(requests(row)));
    rows.truncate(limit);
    rows
}

/// `groupRefererBreakdownRows`.
fn group_referer_rows(rows: Vec<BreakdownRow>) -> Vec<BreakdownRow> {
    let mut grouped: Vec<(String, BreakdownRow)> = Vec::new();
    for row in rows {
        let host = if row.value == "__other__" {
            row.value.clone()
        } else {
            referer_host(&row.value).unwrap_or_else(|| row.value.clone())
        };
        let group = entry(&mut grouped, &host, || BreakdownRow {
            value: host.clone(),
            requests: 0,
            bytes_out: 0,
            lat: None,
            lon: None,
        });
        group.requests += row.requests;
        group.bytes_out += row.bytes_out;
    }
    top_by_requests(
        grouped.into_iter().map(|(_, row)| row).collect(),
        |r| r.requests,
        usize::MAX,
    )
}

pub struct Summed {
    pub overview: Overview,
    pub latency_approximate: bool,
    pub uniques_approximate: bool,
}

/// `sumOverviews`.
pub fn sum_overviews(overviews: &[Overview]) -> Summed {
    let multi = overviews.len() > 1;
    let mut sum = Overview::default();
    for o in overviews {
        sum.requests += o.requests;
        sum.bytes_in += o.bytes_in;
        sum.bytes_out += o.bytes_out;
        sum.s2xx += o.s2xx;
        sum.s3xx += o.s3xx;
        sum.s4xx += o.s4xx;
        sum.s5xx += o.s5xx;
        sum.latency_p50 = sum.latency_p50.max(o.latency_p50);
        sum.latency_p95 = sum.latency_p95.max(o.latency_p95);
        sum.latency_p99 = sum.latency_p99.max(o.latency_p99);
        sum.unique_visitors += o.unique_visitors;
    }
    Summed {
        overview: sum,
        latency_approximate: multi,
        uniques_approximate: multi,
    }
}

pub struct Aggregator {
    dimensions: Vec<&'static str>,
    overviews: Vec<Overview>,
    paths: Vec<(String, PathOut)>,
    breakdowns: Vec<(&'static str, Vec<(String, BreakdownRow)>)>,
    series: BTreeMap<i64, SeriesOut>,
    attribution: Option<String>,
}

impl Aggregator {
    pub fn new(dimensions: Vec<&'static str>) -> Self {
        Self {
            breakdowns: dimensions.iter().map(|d| (*d, Vec::new())).collect(),
            dimensions,
            overviews: Vec::new(),
            paths: Vec::new(),
            series: BTreeMap::new(),
            attribution: None,
        }
    }

    /// `collect`.
    #[allow(clippy::too_many_arguments)]
    pub async fn collect(
        &mut self,
        client: &Client,
        key: Option<&str>,
        from: &str,
        to: &str,
        range: &str,
        resources: &mut Resources,
        limit: i64,
    ) -> Result<(), String> {
        self.collect_scope(client, key, Scope::App, from, to, range, resources, limit)
            .await
    }

    /// `collectResource`.
    #[allow(clippy::too_many_arguments)]
    pub async fn collect_resource(
        &mut self,
        client: &Client,
        uuid: &str,
        from: &str,
        to: &str,
        range: &str,
        resources: &mut Resources,
        limit: i64,
    ) -> Result<(), String> {
        if client
            .prefetch_resource_scope(uuid, from, to, &self.dimensions, range, limit, limit)
            .await?
        {
            return self
                .collect_scope(
                    client,
                    Some(uuid),
                    Scope::Resource,
                    from,
                    to,
                    range,
                    resources,
                    limit,
                )
                .await;
        }
        let keys = client
            .prefetch_resource(uuid, from, to, &self.dimensions, range, limit, limit)
            .await?;
        for key in keys {
            self.collect(client, Some(&key), from, to, range, resources, limit)
                .await?;
        }
        Ok(())
    }

    /// `collectScope`.
    #[allow(clippy::too_many_arguments)]
    async fn collect_scope(
        &mut self,
        client: &Client,
        key: Option<&str>,
        scope: Scope,
        from: &str,
        to: &str,
        range: &str,
        resources: &mut Resources,
        limit: i64,
    ) -> Result<(), String> {
        self.overviews
            .push(client.overview(key, from, to, scope).await?);
        let paths = client.paths(key, from, to, limit, scope).await?;
        self.add_paths(paths, resources).await;
        for dimension in self.dimensions.clone() {
            let rows = client
                .breakdown(key, dimension, from, to, limit, scope)
                .await?;
            self.add_breakdown(dimension, rows);
        }
        if self.attribution.is_none() {
            self.attribution = client.attribution().await?;
        }
        self.add_series(client.series(key, range, scope).await?);
        Ok(())
    }

    /// `addPaths`.
    async fn add_paths(&mut self, paths: Vec<PathRow>, resources: &mut Resources) {
        for data in paths {
            let app = if data.path == "__other__" {
                ""
            } else {
                data.app.as_str()
            };
            let key = format!("{app}\n{}", data.path);
            if !self.paths.iter().any(|(k, _)| *k == key) {
                let domain = if app.is_empty() {
                    None
                } else {
                    resources.app_meta(app).await.domain
                };
                self.paths.push((
                    key.clone(),
                    PathOut {
                        path: data.path.clone(),
                        domain,
                        requests: 0,
                        bytes_out: 0,
                        s4xx: 0,
                        s5xx: 0,
                        p95: 0.0,
                    },
                ));
            }
            if let Some((_, row)) = self.paths.iter_mut().find(|(k, _)| *k == key) {
                row.requests += data.requests;
                row.bytes_out += data.bytes_out;
                row.s4xx += data.s4xx;
                row.s5xx += data.s5xx;
                row.p95 = row.p95.max(data.p95);
            }
        }
    }

    /// `addBreakdown`.
    fn add_breakdown(&mut self, dimension: &str, rows: Vec<Breakdown>) {
        let Some((_, merged)) = self.breakdowns.iter_mut().find(|(d, _)| *d == dimension) else {
            return;
        };
        for data in rows {
            let row = entry(merged, &data.value, || BreakdownRow {
                value: data.value.clone(),
                requests: 0,
                bytes_out: 0,
                lat: None,
                lon: None,
            });
            row.requests += data.requests;
            row.bytes_out += data.bytes_out;
            if let (Some(lat), Some(lon)) = (data.lat, data.lon) {
                row.lat.get_or_insert(lat);
                row.lon.get_or_insert(lon);
            }
        }
    }

    /// `addSeries`.
    fn add_series(&mut self, buckets: Vec<SeriesBucket>) {
        for data in buckets {
            let row = self.series.entry(data.bucket).or_insert(SeriesOut {
                bucket: data.bucket,
                s2xx: 0,
                s3xx: 0,
                s4xx: 0,
                s5xx: 0,
                requests: 0,
                bytes_in: 0,
                bytes_out: 0,
                unique_visitors: 0,
                p95: 0.0,
            });
            row.s2xx += data.s2xx;
            row.s3xx += data.s3xx;
            row.s4xx += data.s4xx;
            row.s5xx += data.s5xx;
            row.requests += data.requests;
            row.bytes_in += data.bytes_in;
            row.bytes_out += data.bytes_out;
            row.unique_visitors += data.unique_visitors;
            row.p95 = row.p95.max(data.p95);
        }
    }

    pub fn has_overview(&self) -> bool {
        !self.overviews.is_empty()
    }

    pub fn overview(&self) -> Summed {
        sum_overviews(&self.overviews)
    }

    pub fn top_paths(&self) -> Vec<PathOut> {
        top_by_requests(
            self.paths.iter().map(|(_, row)| row.clone()).collect(),
            |r| r.requests,
            50,
        )
    }

    /// `breakdowns`.
    pub fn breakdowns(&self) -> Vec<(&'static str, Vec<BreakdownRow>)> {
        self.breakdowns
            .iter()
            .map(|(dimension, rows)| {
                let mut rows = top_by_requests(
                    rows.iter().map(|(_, row)| row.clone()).collect(),
                    |r| r.requests,
                    usize::MAX,
                );
                if *dimension == "referer" {
                    rows = group_referer_rows(rows);
                }
                rows.truncate(50);
                (*dimension, rows)
            })
            .collect()
    }

    pub fn series(&self) -> Vec<SeriesOut> {
        self.series.values().cloned().collect()
    }

    pub fn attribution(&self) -> Option<String> {
        self.attribution.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overview_reads_sentinel_shape() {
        let o = Overview::from_sentinel(&json!({
            "requests": 5, "bytes_in": 1, "bytes_out": "2",
            "status": {"s2xx": 3, "s4xx": 2},
            "latency": {"p95": 12.5},
            "unique_visitors": 1,
        }));
        assert_eq!(o.requests, 5);
        assert_eq!(o.bytes_out, 2);
        assert_eq!(o.s2xx, 3);
        assert_eq!(o.s4xx, 2);
        assert_eq!(o.latency_p95, 12.5);
        assert_eq!(o.unique_visitors, 1);
    }

    #[test]
    fn overviews_sum_and_flag_approximate_once_merged() {
        let a = Overview {
            requests: 1,
            latency_p95: 10.0,
            unique_visitors: 2,
            ..Default::default()
        };
        let b = Overview {
            requests: 2,
            latency_p95: 7.0,
            unique_visitors: 3,
            ..Default::default()
        };
        let one = sum_overviews(std::slice::from_ref(&a));
        assert!(!one.latency_approximate && !one.uniques_approximate);
        let both = sum_overviews(&[a, b]);
        assert_eq!(both.overview.requests, 3);
        assert_eq!(both.overview.latency_p95, 10.0);
        assert_eq!(both.overview.unique_visitors, 5);
        assert!(both.latency_approximate && both.uniques_approximate);
    }

    #[test]
    fn breakdowns_merge_and_keep_first_coordinates() {
        let mut a = Aggregator::new(vec!["city", "referer"]);
        a.add_breakdown(
            "city",
            vec![Breakdown::from_sentinel(
                &json!({"value": "DE|BE|Berlin", "requests": 1, "bytes_out": 5, "lat": 52.5, "lon": 13.4}),
            )],
        );
        a.add_breakdown(
            "city",
            vec![
                Breakdown::from_sentinel(
                    &json!({"value": "DE|BE|Berlin", "requests": 2, "bytes_out": 5, "lat": 1, "lon": 2}),
                ),
                Breakdown::from_sentinel(&json!({"value": "X", "requests": 9, "lat": 200, "lon": 2})),
            ],
        );
        a.add_breakdown(
            "referer",
            vec![
                Breakdown::from_sentinel(&json!({"value": "https://www.a.com/x", "requests": 1})),
                Breakdown::from_sentinel(&json!({"value": "http://a.com/y", "requests": 1})),
                Breakdown::from_sentinel(&json!({"value": "", "requests": 5})),
            ],
        );
        let result = a.breakdowns();
        let city = &result[0].1;
        assert_eq!(city[0].value, "X");
        assert_eq!(city[0].lat, None);
        assert_eq!(city[1].requests, 3);
        assert_eq!(city[1].lat, Some(52.5));
        let referer = &result[1].1;
        assert_eq!(referer[0].value, "");
        assert_eq!(referer[1].value, "a.com");
        assert_eq!(referer[1].requests, 2);
    }

    #[test]
    fn series_sums_by_bucket_in_order() {
        let mut a = Aggregator::new(vec![]);
        a.add_series(vec![
            SeriesBucket {
                bucket: 20,
                requests: 1,
                p95: 3.0,
                ..Default::default()
            },
            SeriesBucket {
                bucket: 10,
                requests: 1,
                ..Default::default()
            },
        ]);
        a.add_series(vec![SeriesBucket {
            bucket: 20,
            requests: 2,
            p95: 1.0,
            ..Default::default()
        }]);
        let series = a.series();
        assert_eq!(series[0].bucket, 10);
        assert_eq!(series[1].requests, 3);
        assert_eq!(series[1].p95, 3.0);
    }

    #[test]
    fn top_by_requests_is_stable() {
        let rows = vec![("a", 1), ("b", 2), ("c", 1)];
        assert_eq!(top_by_requests(rows, |r| r.1, 2), vec![("b", 2), ("a", 1)]);
    }
}
