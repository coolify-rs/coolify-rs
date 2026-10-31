//! `Services\SentinelTrafficClient`: Sentinel's traffic API through `docker exec`, with the token
//! on stdin, and the 60 second response cache the whole page is served from.

use coolify_jobs::server_ops::{Srv, sentinel};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::aggregator::{Breakdown, Overview, PathRow, SeriesBucket};

const BASE: &str = "http://localhost:8888/api";
const RECORD_SEPARATOR: char = '\u{1e}';
const CACHE_TTL: Duration = Duration::from_secs(60);
/// `RESOURCE_SCOPE_ABSENCE_TTL`.
const RESOURCE_SCOPE_ABSENCE_TTL: Duration = Duration::from_secs(300);
/// `UNAVAILABLE_TTL`.
const UNAVAILABLE_TTL: Duration = Duration::from_secs(60);
/// `REMOTE_TIMEOUT`.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);
/// `CURL_TIMEOUT_OPTIONS`.
const CURL_TIMEOUT_OPTIONS: &str = "--connect-timeout 3 --max-time 10";
/// `ALLOWED_DIMENSIONS`.
const ALLOWED_DIMENSIONS: [&str; 16] = [
    "status",
    "method",
    "country",
    "city",
    "referer",
    "browser",
    "os",
    "device",
    "protocol",
    "scheme",
    "tls",
    "cache",
    "bot",
    "agent",
    "ip",
    "useragent",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    App,
    Resource,
}

impl Scope {
    fn segment(self) -> &'static str {
        match self {
            Scope::App => "app",
            Scope::Resource => "resource",
        }
    }
}

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(key: &str) -> Option<String> {
    let cache = CACHE.lock().ok()?;
    let (until, value) = cache.get(key)?;
    (*until > Instant::now()).then(|| value.clone())
}

fn cache_put(key: String, value: String, ttl: Duration) {
    if let Ok(mut cache) = CACHE.lock() {
        let now = Instant::now();
        cache.retain(|_, (until, _)| *until > now);
        cache.insert(key, (now + ttl, value));
    }
}

/// `keyBelongsTo`.
pub fn key_belongs_to(key: &str, resource_uuid: &str) -> bool {
    !resource_uuid.is_empty()
        && (key == resource_uuid
            || key
                .strip_prefix(resource_uuid)
                .is_some_and(|rest| rest.starts_with('-')))
}

/// `candidateOwnerUuids`.
pub fn candidate_owner_uuids(key: &str) -> Vec<&str> {
    let mut candidates = vec![key];
    let mut position = key.len();
    while let Some(found) = key[..position].rfind('-') {
        if found > 0 {
            candidates.push(&key[..found]);
        }
        position = found;
    }
    candidates
}

/// `isSafeKey`.
fn is_safe_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn assert_safe_key(value: &str) -> Result<(), String> {
    if is_safe_key(value) {
        Ok(())
    } else {
        Err("Invalid traffic analytics app key.".to_string())
    }
}

/// `guard`.
fn guard(response: &str) -> Result<(), String> {
    let payload: Value = serde_json::from_str(response)
        .map_err(|_| "Traffic analytics returned an invalid response.".to_string())?;
    match &payload {
        Value::Array(_) => Ok(()),
        Value::Object(map) => match map.get("error") {
            None => Ok(()),
            Some(Value::String(error)) => Err(error.clone()),
            Some(_) => Err("Traffic analytics request failed.".to_string()),
        },
        _ => Err("Traffic analytics returned an invalid response.".to_string()),
    }
}

fn normal_range(range: &str) -> &str {
    if matches!(range, "24h" | "7d" | "30d") {
        range
    } else {
        "24h"
    }
}

/// `appScopedPath`.
fn scoped_path(key: Option<&str>, suffix: &str, scope: Scope) -> Result<String, String> {
    match key {
        None => Ok(format!("/traffic/{suffix}")),
        Some(key) => {
            assert_safe_key(key)?;
            Ok(format!("/{}/{key}/traffic/{suffix}", scope.segment()))
        }
    }
}

fn overview_url(key: Option<&str>, from: &str, to: &str, scope: Scope) -> Result<String, String> {
    let path = scoped_path(key, "overview", scope)?;
    Ok(format!("{BASE}{path}?from={from}&to={to}"))
}

fn paths_url(
    key: Option<&str>,
    from: &str,
    to: &str,
    limit: i64,
    scope: Scope,
) -> Result<String, String> {
    let path = scoped_path(key, "paths", scope)?;
    Ok(format!("{BASE}{path}?from={from}&to={to}&limit={limit}"))
}

fn breakdown_url(
    key: Option<&str>,
    dimension: &str,
    from: &str,
    to: &str,
    limit: i64,
    scope: Scope,
) -> Result<String, String> {
    if !ALLOWED_DIMENSIONS.contains(&dimension) {
        return Err("Invalid traffic analytics dimension.".to_string());
    }
    let path = scoped_path(key, &format!("breakdown/{dimension}"), scope)?;
    Ok(format!("{BASE}{path}?from={from}&to={to}&limit={limit}"))
}

fn series_url(key: Option<&str>, range: &str, scope: Scope) -> Result<String, String> {
    let path = scoped_path(key, "series", scope)?;
    Ok(format!("{BASE}{path}?range={}", normal_range(range)))
}

#[allow(clippy::too_many_arguments)]
fn dashboard_url(
    key: Option<&str>,
    from: &str,
    to: &str,
    range: &str,
    path_limit: i64,
    breakdown_limit: i64,
    apps_limit: i64,
    scope: Scope,
) -> Result<String, String> {
    let query = format!(
        "from={from}&to={to}&range={}&paths_limit={path_limit}&breakdown_limit={breakdown_limit}",
        normal_range(range)
    );
    match key {
        None => Ok(format!(
            "{BASE}/traffic/dashboard?{query}&apps_limit={apps_limit}"
        )),
        Some(_) => Ok(format!(
            "{BASE}{}?{query}",
            scoped_path(key, "dashboard", scope)?
        )),
    }
}

fn apps_url() -> String {
    format!("{BASE}/traffic/apps")
}

fn attribution_url() -> String {
    format!("{BASE}/traffic/attribution")
}

/// `authorizationHeredoc`.
fn authorization_heredoc(token: &str) -> String {
    format!(" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer {token}\nCOOLIFY_SENTINEL_AUTH")
}

/// `buildFetchCommand`.
pub fn build_fetch_command(token: &str, url: &str) -> String {
    format!(
        "docker exec -i coolify-sentinel curl -sS {CURL_TIMEOUT_OPTIONS} -H @- \"{url}\"{}",
        authorization_heredoc(token)
    )
}

/// `buildBatchCommand`.
pub fn build_batch_command(token: &str, urls: &[String]) -> String {
    let script = urls
        .iter()
        .map(|url| {
            format!(
                "curl -s {CURL_TIMEOUT_OPTIONS} -H @- \"{url}\"{}\nprintf '\\036'",
                authorization_heredoc(token)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\n{script}\nCOOLIFY_SENTINEL_SCRIPT"
    )
}

fn rows_of(body: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(rows)) => rows,
        Ok(Value::Object(map)) => map.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    }
}

pub struct Client {
    srv: Srv,
}

impl Client {
    pub fn new(srv: Srv) -> Self {
        Self { srv }
    }

    pub fn server_uuid(&self) -> &str {
        &self.srv.row.uuid
    }

    fn cache_key(&self, url: &str) -> String {
        format!("traffic:{}:{url}", self.server_uuid())
    }

    fn dashboard_absence_key(&self) -> String {
        format!("traffic:dashboard-absent:{}", self.server_uuid())
    }

    fn resource_scope_absence_key(&self) -> String {
        format!("traffic:resource-scope-absent:{}", self.server_uuid())
    }

    fn unavailable_key(&self) -> String {
        format!("traffic:unavailable:{}", self.server_uuid())
    }

    /// `raw`.
    async fn raw(&self, url: &str) -> Result<String, String> {
        let key = self.cache_key(url);
        if let Some(body) = cache_get(&key) {
            return Ok(body);
        }
        let body = self.remote_fetch(url).await?;
        guard(&body)?;
        cache_put(key, body.clone(), CACHE_TTL);
        Ok(body)
    }

    /// `warm`: an optimization, so a failure only leaves the per-call fetch to report it.
    async fn warm(&self, urls: &[String]) {
        let misses: Vec<String> = urls
            .iter()
            .filter(|url| cache_get(&self.cache_key(url)).is_none())
            .cloned()
            .collect();
        if misses.is_empty() {
            return;
        }
        let Ok(output) = self.batch_remote_fetch(&misses).await else {
            return;
        };
        let bodies: Vec<&str> = output.split(RECORD_SEPARATOR).collect();
        for (index, url) in misses.iter().enumerate() {
            let body = bodies.get(index).copied().unwrap_or_default();
            if guard(body).is_ok() {
                cache_put(self.cache_key(url), body.to_string(), CACHE_TTL);
            }
        }
    }

    /// `remoteFetch`.
    async fn remote_fetch(&self, url: &str) -> Result<String, String> {
        self.ensure_remote_available()?;
        let token = sentinel::ensure_valid_token(&self.srv).await?;
        self.run_remote(build_fetch_command(&token, url)).await
    }

    /// `batchRemoteFetch`.
    async fn batch_remote_fetch(&self, urls: &[String]) -> Result<String, String> {
        self.ensure_remote_available()?;
        let token = sentinel::ensure_valid_token(&self.srv).await?;
        self.run_remote(build_batch_command(&token, urls)).await
    }

    /// `ensureRemoteAvailable`.
    fn ensure_remote_available(&self) -> Result<(), String> {
        if cache_get(&self.unavailable_key()).is_some() {
            return Err("Traffic analytics is temporarily unavailable on this server.".to_string());
        }
        if !self.srv.info.is_functional() {
            return Err("Server is not reachable.".to_string());
        }
        Ok(())
    }

    /// `runRemote`: no SSH retries; a failure marks the server unavailable.
    async fn run_remote(&self, command: String) -> Result<String, String> {
        let output = self
            .srv
            .run_once(&[command], "server.traffic", REMOTE_TIMEOUT)
            .await
            .inspect_err(|_| {
                cache_put(self.unavailable_key(), "1".to_string(), UNAVAILABLE_TTL);
            })?
            .unwrap_or_default();
        let output = output.trim_matches([' ', '\t', '\n', '\r', '\0', '\u{b}']);
        Ok(if output == "null" {
            String::new()
        } else {
            output.to_string()
        })
    }

    /// `apps`.
    async fn apps(&self) -> Result<Vec<Value>, String> {
        Ok(rows_of(&self.raw(&apps_url()).await?))
    }

    /// `overview` and `resourceOverview`.
    pub async fn overview(
        &self,
        key: Option<&str>,
        from: &str,
        to: &str,
        scope: Scope,
    ) -> Result<Overview, String> {
        let body = self.raw(&overview_url(key, from, to, scope)?).await?;
        Ok(Overview::from_sentinel(
            &serde_json::from_str(&body).unwrap_or(Value::Null),
        ))
    }

    /// `paths` and `resourcePaths`.
    pub async fn paths(
        &self,
        key: Option<&str>,
        from: &str,
        to: &str,
        limit: i64,
        scope: Scope,
    ) -> Result<Vec<PathRow>, String> {
        let body = self.raw(&paths_url(key, from, to, limit, scope)?).await?;
        Ok(rows_of(&body).iter().map(PathRow::from_sentinel).collect())
    }

    /// `breakdown` and `resourceBreakdown`.
    pub async fn breakdown(
        &self,
        key: Option<&str>,
        dimension: &str,
        from: &str,
        to: &str,
        limit: i64,
        scope: Scope,
    ) -> Result<Vec<Breakdown>, String> {
        let body = self
            .raw(&breakdown_url(key, dimension, from, to, limit, scope)?)
            .await?;
        Ok(rows_of(&body)
            .iter()
            .map(Breakdown::from_sentinel)
            .collect())
    }

    /// `series` and `resourceSeries`.
    pub async fn series(
        &self,
        key: Option<&str>,
        range: &str,
        scope: Scope,
    ) -> Result<Vec<SeriesBucket>, String> {
        let body = self.raw(&series_url(key, range, scope)?).await?;
        Ok(rows_of(&body)
            .iter()
            .map(SeriesBucket::from_sentinel)
            .collect())
    }

    /// `attribution`.
    pub async fn attribution(&self) -> Result<Option<String>, String> {
        let body = self.raw(&attribution_url()).await?;
        Ok(serde_json::from_str::<Value>(&body).ok().and_then(|v| {
            v.get("attribution")
                .and_then(Value::as_str)
                .map(str::to_string)
        }))
    }

    /// `supportsResourceScope`.
    fn supports_resource_scope(&self) -> bool {
        cache_get(&self.resource_scope_absence_key()).is_none()
    }

    /// `fetchBundle`.
    async fn fetch_bundle(&self, url: &str) -> Option<Value> {
        let body = self.raw(url).await.ok()?;
        let decoded: Value = serde_json::from_str(&body).ok()?;
        decoded.get("overview").is_some().then_some(decoded)
    }

    /// `prefetchResourceScope`.
    #[allow(clippy::too_many_arguments)]
    pub async fn prefetch_resource_scope(
        &self,
        uuid: &str,
        from: &str,
        to: &str,
        dimensions: &[&'static str],
        range: &str,
        path_limit: i64,
        breakdown_limit: i64,
    ) -> Result<bool, String> {
        assert_safe_key(uuid)?;
        if !self.supports_resource_scope() {
            return Ok(false);
        }
        let url = dashboard_url(
            Some(uuid),
            from,
            to,
            range,
            path_limit,
            breakdown_limit,
            0,
            Scope::Resource,
        )?;
        let Some(bundle) = self.fetch_bundle(&url).await else {
            cache_put(
                self.resource_scope_absence_key(),
                "1".to_string(),
                RESOURCE_SCOPE_ABSENCE_TTL,
            );
            return Ok(false);
        };
        self.seed_from_dashboard(
            Some(uuid),
            from,
            to,
            range,
            dimensions,
            path_limit,
            breakdown_limit,
            &bundle,
            Scope::Resource,
        )?;
        Ok(true)
    }

    /// `resourceKeys`.
    async fn resource_keys(&self, uuid: &str) -> Result<Vec<String>, String> {
        assert_safe_key(uuid)?;
        let recorded = self.apps().await.unwrap_or_default();
        let mut keys: Vec<String> = recorded
            .iter()
            .filter_map(Value::as_str)
            .filter(|key| is_safe_key(key) && key_belongs_to(key, uuid))
            .map(str::to_string)
            .collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            keys.push(uuid.to_string());
        }
        Ok(keys)
    }

    /// `prefetchResource`.
    #[allow(clippy::too_many_arguments)]
    pub async fn prefetch_resource(
        &self,
        uuid: &str,
        from: &str,
        to: &str,
        dimensions: &[&'static str],
        range: &str,
        path_limit: i64,
        breakdown_limit: i64,
    ) -> Result<Vec<String>, String> {
        assert_safe_key(uuid)?;
        let mut urls = vec![apps_url()];
        if cache_get(&self.dashboard_absence_key()).is_none() {
            urls.push(dashboard_url(
                Some(uuid),
                from,
                to,
                range,
                path_limit,
                breakdown_limit,
                0,
                Scope::App,
            )?);
        }
        self.warm(&urls).await;
        let keys = self.resource_keys(uuid).await?;
        self.prefetch_keys(
            &keys,
            from,
            to,
            dimensions,
            range,
            path_limit,
            breakdown_limit,
        )
        .await?;
        Ok(keys)
    }

    /// `prefetchKeys`.
    #[allow(clippy::too_many_arguments)]
    async fn prefetch_keys(
        &self,
        keys: &[String],
        from: &str,
        to: &str,
        dimensions: &[&'static str],
        range: &str,
        path_limit: i64,
        breakdown_limit: i64,
    ) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        if keys.len() > 1 {
            if cache_get(&self.dashboard_absence_key()).is_some() {
                return Ok(());
            }
            let urls = keys
                .iter()
                .map(|key| {
                    dashboard_url(
                        Some(key),
                        from,
                        to,
                        range,
                        path_limit,
                        breakdown_limit,
                        0,
                        Scope::App,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.warm(&urls).await;
            if cache_get(&self.cache_key(&urls[0])).is_none() {
                cache_put(self.dashboard_absence_key(), "1".to_string(), CACHE_TTL);
                return Ok(());
            }
        }
        for key in keys {
            self.prefetch_server_wide(
                Some(key),
                from,
                to,
                dimensions,
                range,
                path_limit,
                breakdown_limit,
                200,
            )
            .await?;
        }
        Ok(())
    }

    /// `prefetchServerWide` with `fetchDashboard`: the leaderboard keys of the server-wide view.
    #[allow(clippy::too_many_arguments)]
    pub async fn prefetch_server_wide(
        &self,
        key: Option<&str>,
        from: &str,
        to: &str,
        dimensions: &[&'static str],
        range: &str,
        path_limit: i64,
        breakdown_limit: i64,
        apps_limit: i64,
    ) -> Result<Vec<String>, String> {
        let absence = self.dashboard_absence_key();
        if cache_get(&absence).is_some() {
            return Ok(Vec::new());
        }
        let url = dashboard_url(
            key,
            from,
            to,
            range,
            path_limit,
            breakdown_limit,
            apps_limit,
            Scope::App,
        )?;
        let Some(bundle) = self.fetch_bundle(&url).await else {
            cache_put(absence, "1".to_string(), CACHE_TTL);
            return Ok(Vec::new());
        };
        self.seed_from_dashboard(
            key,
            from,
            to,
            range,
            dimensions,
            path_limit,
            breakdown_limit,
            &bundle,
            Scope::App,
        )?;
        if key.is_some() {
            return Ok(Vec::new());
        }
        // `safeReportedKeys`: a key that would be unsafe in a url is dropped on its own.
        Ok(bundle
            .get("apps")
            .and_then(Value::as_array)
            .map(|apps| {
                apps.iter()
                    .filter_map(|app| app.get("uuid").and_then(Value::as_str))
                    .filter(|key| is_safe_key(key))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `seedFromDashboard`.
    #[allow(clippy::too_many_arguments)]
    fn seed_from_dashboard(
        &self,
        key: Option<&str>,
        from: &str,
        to: &str,
        range: &str,
        dimensions: &[&'static str],
        path_limit: i64,
        breakdown_limit: i64,
        bundle: &Value,
        scope: Scope,
    ) -> Result<(), String> {
        let put = |url: String, member: &Value| {
            cache_put(self.cache_key(&url), member.to_string(), CACHE_TTL);
        };
        let member = |name: &str| bundle.get(name).cloned().unwrap_or_else(|| json!([]));
        put(overview_url(key, from, to, scope)?, &member("overview"));
        put(
            paths_url(key, from, to, path_limit, scope)?,
            &member("paths"),
        );
        put(series_url(key, range, scope)?, &member("series"));
        put(
            attribution_url(),
            &json!({"attribution": bundle.get("attribution").cloned().unwrap_or(Value::Null)}),
        );
        let breakdowns = bundle.get("breakdowns");
        for dimension in dimensions {
            put(
                breakdown_url(key, dimension, from, to, breakdown_limit, scope)?,
                breakdowns
                    .and_then(|b| b.get(*dimension))
                    .unwrap_or(&json!([])),
            );
        }
        for app in bundle
            .get("apps")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let (Some(uuid), Some(overview)) = (
                app.get("uuid").and_then(Value::as_str),
                app.get("overview").filter(|o| !o.is_null()),
            ) && is_safe_key(uuid)
            {
                put(overview_url(Some(uuid), from, to, Scope::App)?, overview);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_belong_to_their_resource() {
        assert!(key_belongs_to("abc", "abc"));
        assert!(key_belongs_to("abc-web", "abc"));
        assert!(!key_belongs_to("abcd", "abc"));
        assert!(!key_belongs_to("abc", ""));
    }

    #[test]
    fn owner_candidates_go_from_specific_to_general() {
        assert_eq!(
            candidate_owner_uuids("abc-12-web"),
            vec!["abc-12-web", "abc-12", "abc"]
        );
        assert_eq!(candidate_owner_uuids("-x"), vec!["-x"]);
    }

    #[test]
    fn urls_match_upstream() {
        assert_eq!(
            dashboard_url(None, "F", "T", "bad", 50, 50, 200, Scope::App).unwrap(),
            "http://localhost:8888/api/traffic/dashboard?from=F&to=T&range=24h&paths_limit=50&breakdown_limit=50&apps_limit=200"
        );
        assert_eq!(
            dashboard_url(Some("u"), "F", "T", "7d", 50, 50, 0, Scope::Resource).unwrap(),
            "http://localhost:8888/api/resource/u/traffic/dashboard?from=F&to=T&range=7d&paths_limit=50&breakdown_limit=50"
        );
        assert!(overview_url(Some("a b"), "F", "T", Scope::App).is_err());
        assert!(breakdown_url(None, "nope", "F", "T", 50, Scope::App).is_err());
    }

    #[test]
    fn commands_keep_the_token_on_stdin() {
        assert_eq!(
            build_fetch_command("tok", "http://u?a=1&b=2"),
            "docker exec -i coolify-sentinel curl -sS --connect-timeout 3 --max-time 10 -H @- \"http://u?a=1&b=2\" <<'COOLIFY_SENTINEL_AUTH'\nAuthorization: Bearer tok\nCOOLIFY_SENTINEL_AUTH"
        );
        let batch = build_batch_command("tok", &["http://u".to_string()]);
        assert!(batch.starts_with(
            "docker exec -i coolify-sentinel sh -s <<'COOLIFY_SENTINEL_SCRIPT'\ncurl -s "
        ));
        assert!(batch.ends_with("printf '\\036'\nCOOLIFY_SENTINEL_SCRIPT"));
    }

    #[test]
    fn guard_rejects_errors_and_non_arrays() {
        assert!(guard("{\"a\":1}").is_ok());
        assert!(guard("[]").is_ok());
        assert_eq!(guard("{\"error\":\"nope\"}").unwrap_err(), "nope");
        assert!(guard("404 page not found").is_err());
        assert!(guard("").is_err());
    }
}
