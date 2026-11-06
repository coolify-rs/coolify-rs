//! `HetznerService`, `VultrService`, `DigitalOceanService`, `HostingerService`, the
//! token check of `CloudProviderTokensController`, the `refresh*State` methods of `Server`
//! and the provider part of `DeleteServer`.
//!
//! `COOLIFY_RS_CLOUD_API_BASE` replaces the provider hosts (for a local fake): the
//! requests then go to `<base>/hetzner/v1/...`, `<base>/vultr/v2/...`,
//! `<base>/digitalocean/v2/...` and `<base>/hostinger/...`.

use std::time::Duration;

use coolify_db::cloud as db;
use coolify_db::servers::{self, ColumnValue, ServerRow};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::Jobs;

/// Replaces `COOLIFY_RS_CLOUD_API_BASE` for this process (tests aim it at a local fake).
static API_BASE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Sends every provider request of this process to `base` (a local fake), like
/// `COOLIFY_RS_CLOUD_API_BASE`.
pub fn set_api_base(base: &str) {
    if let Ok(mut b) = API_BASE.write() {
        *b = Some(base.trim_end_matches('/').to_string());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Hetzner,
    Vultr,
    DigitalOcean,
    Hostinger,
}

impl Provider {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "hetzner" => Some(Self::Hetzner),
            "vultr" => Some(Self::Vultr),
            "digitalocean" => Some(Self::DigitalOcean),
            "hostinger" => Some(Self::Hostinger),
            _ => None,
        }
    }

    fn base(self) -> String {
        let over = API_BASE
            .read()
            .ok()
            .and_then(|b| b.clone())
            .or_else(|| std::env::var("COOLIFY_RS_CLOUD_API_BASE").ok());
        match (self, over.as_deref()) {
            (Self::Hetzner, Some(b)) => format!("{b}/hetzner/v1"),
            (Self::Vultr, Some(b)) => format!("{b}/vultr/v2"),
            (Self::DigitalOcean, Some(b)) => format!("{b}/digitalocean/v2"),
            (Self::Hostinger, Some(b)) => format!("{b}/hostinger"),
            (Self::Hetzner, None) => "https://api.hetzner.cloud/v1".into(),
            (Self::Vultr, None) => "https://api.vultr.com/v2".into(),
            (Self::DigitalOcean, None) => "https://api.digitalocean.com/v2".into(),
            (Self::Hostinger, None) => "https://developers.hostinger.com".into(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hetzner => "Hetzner",
            Self::Vultr => "Vultr",
            Self::DigitalOcean => "DigitalOcean",
            Self::Hostinger => "Hostinger",
        }
    }
}

/// What a provider call can fail with.
#[derive(Debug, Clone)]
pub enum CloudError {
    /// `RateLimitException`.
    RateLimit(Option<i64>),
    /// `\Exception` with the message and code: a connection failure, or a Hostinger error.
    Api(String, u16),
    /// `CloudProviderApiException`: an error response from Hetzner, DigitalOcean or Vultr.
    Provider(ProviderApiError),
}

impl CloudError {
    /// The HTTP status of a rejected request (0 for a connection failure).
    pub fn status(&self) -> u16 {
        match self {
            Self::RateLimit(_) => 429,
            Self::Api(_, s) => *s,
            Self::Provider(p) => p.status,
        }
    }
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimit(_) => f.write_str("Rate limit exceeded. Please try again later."),
            Self::Api(m, _) => f.write_str(m),
            Self::Provider(p) => f.write_str(&p.message),
        }
    }
}

/// `CloudProviderApiException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderApiError {
    pub provider: &'static str,
    /// `"{provider} API error: {providerMessage}"`.
    pub message: String,
    pub status: u16,
    /// Only a value that looks like an identifier is kept.
    pub error_code: Option<String>,
    pub invalid_fields: Vec<String>,
}

impl ProviderApiError {
    pub fn new(
        provider: &'static str,
        provider_message: &str,
        status: u16,
        error_code: Option<&Value>,
        invalid_fields: &[Value],
    ) -> Self {
        // `preg_match('/^[a-z_]{1,64}$/')` after `strtolower`.
        let error_code = error_code
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .filter(|c| {
                (1..=64).contains(&c.len())
                    && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            });
        // `preg_match('/^[a-z0-9_.\[\]]{1,64}$/i')`.
        let invalid_fields = invalid_fields
            .iter()
            .filter_map(Value::as_str)
            .filter(|n| {
                (1..=64).contains(&n.len())
                    && n.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'[' | b']')
                    })
            })
            .map(str::to_string)
            .collect();
        Self {
            provider,
            message: format!("{provider} API error: {provider_message}"),
            status,
            error_code,
            invalid_fields,
        }
    }

    /// `safeMessage()`: built only from the status, the error code and field names, never from
    /// the provider's free-text message.
    pub fn safe_message(&self) -> String {
        let p = self.provider;
        match (self.error_code.as_deref(), self.status) {
            (Some("resource_limit_exceeded"), _) => format!(
                "Your {p} account has reached a resource limit, for example the server limit. Remove unused resources or ask {p} for a higher limit."
            ),
            (Some("resource_unavailable"), _) => "The selected server type, image, or location is not available now. Select a different location or server type.".to_string(),
            (Some("uniqueness_error"), _) => format!(
                "A resource with the same name already exists in your {p} account, for example a server or an SSH key. Use a different name."
            ),
            (Some("maintenance"), _) | (_, 423) | (_, 500..) => {
                format!("{p} cannot process the request now. Try again later.")
            }
            (_, 401) => format!(
                "The {p} API token is invalid or expired. Update the cloud provider token in Coolify."
            ),
            (_, 403) => format!(
                "The {p} API token does not have permission for this request. Use a token with read and write access."
            ),
            (_, 404) => format!(
                "{p} could not find a requested resource, for example the location, server type, image, or SSH key."
            ),
            (_, 409) => format!(
                "{p} rejected the request because of a conflict with an existing resource."
            ),
            (_, 422) => {
                let mut m = format!(
                    "{p} rejected the request as invalid. Check the selected values and your account limits."
                );
                if !self.invalid_fields.is_empty() {
                    m.push_str(&format!(
                        " Invalid fields: {}.",
                        self.invalid_fields.join(", ")
                    ));
                }
                m
            }
            (code, status) => format!(
                "{p} returned HTTP {status}{}.",
                code.map(|c| format!(" (error code: {c})"))
                    .unwrap_or_default()
            ),
        }
    }

    /// `toJsonResponse()`: the body of the 502 answer.
    pub fn json_body(&self, failure_message: &str) -> Value {
        json!({
            "message": format!("{failure_message} {}", self.safe_message()),
            "provider_status": self.status,
            "provider_error_code": self.error_code,
        })
    }
}

/// One provider API client.
#[derive(Debug, Clone)]
pub struct Client {
    provider: Provider,
    token: String,
    http: reqwest::Client,
    /// Replaces the provider's base URL (tests).
    base: Option<String>,
}

impl Client {
    pub fn new(provider: Provider, token: &str) -> Self {
        Self {
            provider,
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            base: None,
        }
    }

    /// A client that sends its requests to `base` instead of the provider (a local fake).
    pub fn with_base(provider: Provider, token: &str, base: &str) -> Self {
        Self {
            base: Some(base.trim_end_matches('/').to_string()),
            ..Self::new(provider, token)
        }
    }

    fn attempts(&self, method: &Method) -> u32 {
        // `Http::retry(3, ...)`; Hostinger retries only GET.
        if self.provider == Provider::Hostinger && *method != Method::GET {
            1
        } else {
            3
        }
    }

    /// The `when` callback of `Http::retry`: whether a failed attempt is sent again.
    /// `status` is None for a connection failure.
    fn retries(&self, method: &Method, status: Option<StatusCode>) -> bool {
        match self.provider {
            // Only GET is retried, and only on a server error or a connection failure.
            Provider::Hostinger => status.is_none_or(|s| s.is_server_error()),
            // A POST that failed with a 5xx or a timeout can still have created the resource,
            // so only idempotent requests are sent again; client errors are not retried.
            Provider::Hetzner => {
                status == Some(StatusCode::TOO_MANY_REQUESTS)
                    || (status.is_none_or(|s| s.as_u16() >= 500)
                        && (*method == Method::GET || *method == Method::DELETE))
            }
            Provider::Vultr | Provider::DigitalOcean => true,
        }
    }

    /// `request($method, $endpoint, $data)`: query string for GET, JSON otherwise.
    async fn request(
        &self,
        method: Method,
        endpoint: &str,
        data: Option<&Value>,
    ) -> Result<Value, CloudError> {
        let base = self.base.clone().unwrap_or_else(|| self.provider.base());
        let url = format!("{base}{endpoint}");
        let attempts = self.attempts(&method);
        let mut last: Option<(StatusCode, Value, reqwest::header::HeaderMap)> = None;
        let mut transport: Option<String> = None;
        for attempt in 1..=attempts {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("User-Agent", "GuzzleHttp/7");
            if matches!(self.provider, Provider::DigitalOcean | Provider::Hostinger) {
                req = req.header("Accept", "application/json");
            }
            if let Some(d) = data {
                if method == Method::GET || method == Method::DELETE {
                    if let Some(m) = d.as_object() {
                        let pairs: Vec<(String, String)> = m
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    match v {
                                        Value::String(s) => s.clone(),
                                        o => o.to_string(),
                                    },
                                )
                            })
                            .collect();
                        req = req.query(&pairs);
                    }
                } else {
                    req = req.json(d);
                }
            }
            match req.send().await {
                Ok(r) => {
                    let status = r.status();
                    let headers = r.headers().clone();
                    let bytes = r.bytes().await.unwrap_or_default();
                    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                    let retryable = !status.is_success() && self.retries(&method, Some(status));
                    last = Some((status, body, headers));
                    transport = None;
                    if !retryable {
                        break;
                    }
                }
                Err(e) => {
                    transport = Some(e.to_string());
                    if !self.retries(&method, None) {
                        break;
                    }
                }
            }
            if attempt < attempts {
                tokio::time::sleep(Duration::from_millis(u64::from(attempt) * 100)).await;
            }
        }
        if let Some(e) = transport {
            return Err(CloudError::Api(e, 0));
        }
        let Some((status, body, headers)) = last else {
            return Err(CloudError::Api("no response".to_string(), 0));
        };
        if status.is_success() {
            return Ok(if body.is_null() { json!({}) } else { body });
        }
        let header_int = |k: &str| {
            headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
        };
        if status == StatusCode::TOO_MANY_REQUESTS {
            let mut retry = header_int("Retry-After");
            if retry.is_none()
                && matches!(self.provider, Provider::Hetzner | Provider::DigitalOcean)
            {
                retry = header_int("RateLimit-Reset")
                    .map(|r| (r - chrono::Utc::now().timestamp()).max(0));
            }
            return Err(CloudError::RateLimit(retry));
        }
        let text = |p: &str| body.pointer(p).and_then(Value::as_str).map(str::to_string);
        let unknown = || "Unknown error".to_string();
        let label = self.provider.label();
        let status = status.as_u16();
        Err(match self.provider {
            Provider::Hetzner => CloudError::Provider(ProviderApiError::new(
                label,
                &text("/error/message").unwrap_or_else(unknown),
                status,
                body.pointer("/error/code"),
                &body
                    .pointer("/error/details/fields")
                    .and_then(Value::as_array)
                    .map(|fields| {
                        fields
                            .iter()
                            .map(|f| f.get("name").cloned().unwrap_or(Value::Null))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )),
            Provider::Vultr => CloudError::Provider(ProviderApiError::new(
                label,
                &text("/error").unwrap_or_else(unknown),
                status,
                None,
                &[],
            )),
            Provider::DigitalOcean => CloudError::Provider(ProviderApiError::new(
                label,
                &text("/message").unwrap_or_else(unknown),
                status,
                body.get("id"),
                &[],
            )),
            Provider::Hostinger => CloudError::Api(
                format!(
                    "{label} API error: {}",
                    text("/message")
                        .or_else(|| text("/error"))
                        .unwrap_or_else(unknown)
                ),
                status,
            ),
        })
    }

    async fn get(&self, endpoint: &str, data: Option<Value>) -> Result<Value, CloudError> {
        self.request(Method::GET, endpoint, data.as_ref()).await
    }

    async fn post(&self, endpoint: &str, data: &Value) -> Result<Value, CloudError> {
        self.request(Method::POST, endpoint, Some(data)).await
    }

    async fn delete(&self, endpoint: &str) -> Result<Value, CloudError> {
        self.request(Method::DELETE, endpoint, None).await
    }

    /// Hetzner's and DigitalOcean's page paging.
    async fn paged(
        &self,
        endpoint: &str,
        key: &str,
        extra: &[(&str, &str)],
    ) -> Result<Vec<Value>, CloudError> {
        let mut out = Vec::new();
        let mut page = 1i64;
        loop {
            let mut q = Map::new();
            for (k, v) in extra {
                q.insert((*k).to_string(), json!(v));
            }
            q.insert("page".into(), json!(page));
            q.insert("per_page".into(), json!(50));
            let r = self.get(endpoint, Some(Value::Object(q))).await?;
            if let Some(items) = r.get(key).and_then(Value::as_array) {
                out.extend(items.iter().cloned());
            }
            match self.provider {
                Provider::Hetzner => {
                    match r
                        .pointer("/meta/pagination/next_page")
                        .and_then(Value::as_i64)
                    {
                        Some(n) => page = n,
                        None => break,
                    }
                }
                _ => {
                    let has_next = r
                        .pointer("/links/pages/next")
                        .is_some_and(|v| !matches!(v, Value::Null) && v.as_str() != Some(""));
                    page += 1;
                    if !has_next {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Vultr's cursor paging.
    async fn cursored(&self, endpoint: &str, key: &str) -> Result<Vec<Value>, CloudError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = Map::new();
            q.insert("per_page".into(), json!(100));
            if let Some(c) = &cursor {
                q.insert("cursor".into(), json!(c));
            }
            let r = self.get(endpoint, Some(Value::Object(q))).await?;
            if let Some(items) = r.get(key).and_then(Value::as_array) {
                out.extend(items.iter().cloned());
            }
            let next = r
                .pointer("/meta/links/next")
                .and_then(Value::as_str)
                .filter(|n| !n.trim().is_empty());
            cursor = next.and_then(|n| {
                let query = n.split_once('?').map_or(n, |(_, q)| q);
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("cursor=").map(str::to_string))
            });
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    // ── Hetzner ─────────────────────────────────────────────────────────

    pub async fn hetzner_locations(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/locations", "locations", &[]).await
    }

    pub async fn hetzner_images(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/images", "images", &[("type", "system")]).await
    }

    /// `getServerTypes($location, $includeOutOfStock)`: the types that can be ordered now.
    pub async fn hetzner_server_types(
        &self,
        location: Option<&str>,
        include_out_of_stock: bool,
    ) -> Result<Vec<Value>, CloudError> {
        let types = self.paged("/server_types", "server_types", &[]).await?;
        Ok(filter_hetzner_server_types(
            types,
            location,
            include_out_of_stock,
        ))
    }

    pub async fn hetzner_ssh_keys(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/ssh_keys", "ssh_keys", &[]).await
    }

    pub async fn hetzner_firewalls(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/firewalls", "firewalls", &[]).await
    }

    pub async fn hetzner_networks(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/networks", "networks", &[]).await
    }

    pub async fn hetzner_upload_ssh_key(
        &self,
        name: &str,
        public_key: &str,
    ) -> Result<Value, CloudError> {
        let r = self
            .post(
                "/ssh_keys",
                &json!({"name": name, "public_key": public_key}),
            )
            .await?;
        Ok(r.get("ssh_key").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn hetzner_create_server(&self, params: &Value) -> Result<Value, CloudError> {
        let r = self.post("/servers", params).await?;
        Ok(r.get("server").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn hetzner_enable_backup(&self, id: i64) -> Result<(), CloudError> {
        self.request(
            Method::POST,
            &format!("/servers/{id}/actions/enable_backup"),
            // `Http::post($url, [])` sends an empty JSON array.
            Some(&json!([])),
        )
        .await
        .map(|_| ())
    }

    /// `HetznerService::getServers`
    pub async fn hetzner_servers(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/servers", "servers", &[]).await
    }

    pub async fn hetzner_get_server(&self, id: i64) -> Result<Value, CloudError> {
        let r = self.get(&format!("/servers/{id}"), None).await?;
        Ok(r.get("server").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn hetzner_delete_server(&self, id: i64) -> Result<(), CloudError> {
        self.delete(&format!("/servers/{id}")).await.map(|_| ())
    }

    // ── Vultr ───────────────────────────────────────────────────────────

    pub async fn vultr_regions(&self) -> Result<Vec<Value>, CloudError> {
        self.cursored("/regions", "regions").await
    }

    pub async fn vultr_plans(&self) -> Result<Vec<Value>, CloudError> {
        self.cursored("/plans", "plans").await
    }

    pub async fn vultr_os(&self) -> Result<Vec<Value>, CloudError> {
        self.cursored("/os", "os").await
    }

    pub async fn vultr_ssh_keys(&self) -> Result<Vec<Value>, CloudError> {
        self.cursored("/ssh-keys", "ssh_keys").await
    }

    pub async fn vultr_upload_ssh_key(&self, name: &str, key: &str) -> Result<Value, CloudError> {
        let r = self
            .post("/ssh-keys", &json!({"name": name, "ssh_key": key}))
            .await?;
        Ok(r.get("ssh_key").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn vultr_create_instance(&self, params: &Value) -> Result<Value, CloudError> {
        use base64::Engine as _;
        let mut p = params.clone();
        if let Some(u) = p
            .get("user_data")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
        {
            let e = base64::engine::general_purpose::STANDARD.encode(u);
            p["user_data"] = json!(e);
        }
        let r = self.post("/instances", &p).await?;
        Ok(r.get("instance").cloned().unwrap_or_else(|| json!({})))
    }

    /// `VultrService::getInstances`
    pub async fn vultr_instances(&self) -> Result<Vec<Value>, CloudError> {
        self.cursored("/instances", "instances").await
    }

    pub async fn vultr_get_instance(&self, id: &str) -> Result<Value, CloudError> {
        let r = self
            .get(&format!("/instances/{}", rawurlencode(id)), None)
            .await?;
        Ok(r.get("instance").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn vultr_delete_instance(&self, id: &str) -> Result<(), CloudError> {
        self.delete(&format!("/instances/{}", rawurlencode(id)))
            .await
            .map(|_| ())
    }

    /// `VultrService::getPublicIp`.
    pub fn vultr_public_ip(instance: &Value, disable_v4: bool, enable_v6: bool) -> Option<String> {
        let usable = |k: &str| {
            instance
                .get(k)
                .and_then(Value::as_str)
                .filter(|i| !i.trim().is_empty() && !["0.0.0.0", "::"].contains(i))
                .map(str::to_string)
        };
        if !disable_v4 && let Some(i) = usable("main_ip") {
            return Some(i);
        }
        if enable_v6 {
            return usable("v6_main_ip");
        }
        None
    }

    /// `VultrService::waitForPublicIp`.
    pub async fn vultr_wait_for_ip(
        &self,
        mut instance: Value,
        disable_v4: bool,
        enable_v6: bool,
    ) -> Result<Value, CloudError> {
        let Some(id) = instance
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Ok(instance);
        };
        if Self::vultr_public_ip(&instance, disable_v4, enable_v6).is_some() {
            return Ok(instance);
        }
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            instance = self.vultr_get_instance(&id).await?;
            if Self::vultr_public_ip(&instance, disable_v4, enable_v6).is_some() {
                break;
            }
        }
        Ok(instance)
    }

    // ── DigitalOcean ────────────────────────────────────────────────────

    pub async fn do_regions(&self) -> Result<Vec<Value>, CloudError> {
        Ok(self
            .paged("/regions", "regions", &[])
            .await?
            .into_iter()
            .filter(|r| {
                r.get("available")
                    .is_none_or(|a| a.is_null() || *a == Value::Bool(true))
            })
            .collect())
    }

    pub async fn do_sizes(&self) -> Result<Vec<Value>, CloudError> {
        Ok(self
            .paged("/sizes", "sizes", &[])
            .await?
            .into_iter()
            .filter(|r| {
                r.get("available")
                    .is_none_or(|a| a.is_null() || *a == Value::Bool(true))
            })
            .collect())
    }

    pub async fn do_images(&self) -> Result<Vec<Value>, CloudError> {
        Ok(self
            .paged("/images", "images", &[("type", "distribution")])
            .await?
            .into_iter()
            .filter(|r| {
                r.get("public")
                    .is_none_or(|a| a.is_null() || *a == Value::Bool(true))
            })
            .collect())
    }

    pub async fn do_ssh_keys(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/account/keys", "ssh_keys", &[]).await
    }

    pub async fn do_upload_ssh_key(&self, name: &str, key: &str) -> Result<Value, CloudError> {
        let r = self
            .post("/account/keys", &json!({"name": name, "public_key": key}))
            .await?;
        Ok(r.get("ssh_key").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn do_create_droplet(&self, params: &Value) -> Result<Value, CloudError> {
        let r = self.post("/droplets", params).await?;
        Ok(r.get("droplet").cloned().unwrap_or_else(|| json!({})))
    }

    /// `DigitalOceanService::getDroplets`
    pub async fn do_droplets(&self) -> Result<Vec<Value>, CloudError> {
        self.paged("/droplets", "droplets", &[]).await
    }

    pub async fn do_get_droplet(&self, id: i64) -> Result<Value, CloudError> {
        let r = self.get(&format!("/droplets/{id}"), None).await?;
        Ok(r.get("droplet").cloned().unwrap_or_else(|| json!({})))
    }

    pub async fn do_delete_droplet(&self, id: i64) -> Result<(), CloudError> {
        self.delete(&format!("/droplets/{id}")).await.map(|_| ())
    }

    /// `DigitalOceanService::getPublicIpAddress`.
    pub fn do_public_ip(droplet: &Value, enable_v4: bool, enable_v6: bool) -> Option<String> {
        let find = |p: &str| {
            droplet
                .pointer(p)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|n| {
                    n.get("type").and_then(Value::as_str) == Some("public")
                        && n.get("ip_address")
                            .and_then(Value::as_str)
                            .is_some_and(|i| !i.trim().is_empty())
                })
                .and_then(|n| n.get("ip_address").and_then(Value::as_str))
                .map(str::to_string)
        };
        if enable_v4 && let Some(i) = find("/networks/v4") {
            return Some(i);
        }
        if enable_v6 {
            return find("/networks/v6");
        }
        None
    }

    /// `DigitalOceanService::waitForPublicIp`.
    pub async fn do_wait_for_ip(
        &self,
        mut droplet: Value,
        enable_v6: bool,
    ) -> Result<Value, CloudError> {
        let Some(id) = droplet.get("id").and_then(Value::as_i64) else {
            return Ok(droplet);
        };
        if Self::do_public_ip(&droplet, true, enable_v6).is_some() {
            return Ok(droplet);
        }
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            droplet = self.do_get_droplet(id).await?;
            if Self::do_public_ip(&droplet, true, enable_v6).is_some() {
                break;
            }
        }
        Ok(droplet)
    }

    // ── Hostinger ───────────────────────────────────────────────────────

    pub async fn hostinger_data_centers(&self) -> Result<Value, CloudError> {
        self.get("/api/vps/v1/data-centers", None).await
    }

    pub async fn hostinger_templates(&self) -> Result<Value, CloudError> {
        self.get("/api/vps/v1/templates", None).await
    }

    pub async fn hostinger_catalog(&self) -> Result<Value, CloudError> {
        let r = self
            .get("/api/billing/v1/catalog", Some(json!({"category": "VPS"})))
            .await?;
        let re = regex::Regex::new(r"-vps-kvm\d+\n?\z")
            .map_err(|e| CloudError::Api(e.to_string(), 0))?;
        let keep = |item: &Value| {
            item.is_object()
                && item
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|i| re.is_match(i))
        };
        Ok(Value::Array(match r {
            Value::Array(items) => items.into_iter().filter(keep).collect(),
            Value::Object(m) => m.into_iter().map(|(_, v)| v).filter(keep).collect(),
            _ => Vec::new(),
        }))
    }

    pub async fn hostinger_public_keys(&self) -> Result<Value, CloudError> {
        let r = self.get("/api/vps/v1/public-keys", None).await?;
        Ok(r.get("data").cloned().unwrap_or_else(|| json!([])))
    }

    pub async fn hostinger_post_install_scripts(&self) -> Result<Value, CloudError> {
        let r = self.get("/api/vps/v1/post-install-scripts", None).await?;
        Ok(r.get("data").cloned().unwrap_or_else(|| json!([])))
    }

    pub async fn hostinger_attach_keys(&self, id: i64, keys: &[i64]) -> Result<(), CloudError> {
        self.post(
            &format!("/api/vps/v1/public-keys/attach/{id}"),
            &json!({"ids": keys}),
        )
        .await
        .map(|_| ())
    }

    /// `HostingerService::disableAutoRenewal`.
    pub async fn hostinger_disable_auto_renewal(
        &self,
        subscription_id: &str,
    ) -> Result<(), CloudError> {
        self.delete(&format!(
            "/api/billing/v1/subscriptions/{subscription_id}/auto-renewal/disable"
        ))
        .await
        .map(|_| ())
    }

    pub async fn hostinger_get_vm(&self, id: i64) -> Result<Value, CloudError> {
        self.get(&format!("/api/vps/v1/virtual-machines/{id}"), None)
            .await
    }

    pub async fn hostinger_vms(&self) -> Result<Vec<Value>, CloudError> {
        let r = self.get("/api/vps/v1/virtual-machines", None).await?;
        Ok(match r {
            Value::Array(a) => a,
            _ => Vec::new(),
        })
    }

    /// `HostingerService::purchaseVirtualMachine`.
    pub async fn hostinger_purchase(&self, params: &Value) -> Result<Value, CloudError> {
        let mut p = params.clone();
        let hostname = p
            .pointer("/setup/hostname")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !hostname.contains('.')
            && let Some(s) = p.get_mut("setup").and_then(Value::as_object_mut)
        {
            s.shift_remove("hostname");
        }
        let existing: Vec<Value> = self
            .hostinger_vms()
            .await?
            .iter()
            .filter_map(|v| v.get("id").cloned())
            .collect();
        let response = match self.post("/api/vps/v1/virtual-machines", &p).await {
            Ok(r) => r,
            Err(e) => {
                // Hostinger can charge for the VPS before its setup fails, so keep the VPS if one was created.
                let found = self.hostinger_vms().await.ok().and_then(|vms| {
                    vms.into_iter()
                        .find(|v| v.get("id").is_none_or(|i| !existing.contains(i)))
                });
                let Some(vm) = found else {
                    return Err(e);
                };
                if vm.get("state").and_then(Value::as_str) != Some("initial") {
                    return Ok(vm);
                }
                let id = vm.get("id").and_then(Value::as_i64).unwrap_or_default();
                let setup = p.get("setup").cloned().unwrap_or_else(|| json!({}));
                return Ok(self
                    .post(&format!("/api/vps/v1/virtual-machines/{id}/setup"), &setup)
                    .await
                    .unwrap_or(vm));
            }
        };
        match response.pointer("/virtual_machine/id") {
            Some(v) if !v.is_null() => Ok(response["virtual_machine"].clone()),
            _ => Err(CloudError::Api(
                format!(
                    "Hostinger order {} is waiting for payment. Finish the VPS setup in hPanel.",
                    response
                        .get("id")
                        .map_or("unknown".to_string(), |i| match i {
                            Value::String(s) => s.clone(),
                            o => o.to_string(),
                        })
                ),
                202,
            )),
        }
    }

    /// `HostingerService::getPublicIpAddress`.
    pub fn hostinger_public_ip(vm: &Value) -> Option<String> {
        for version in ["ipv4", "ipv6"] {
            for a in vm
                .get(version)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(ip) = a
                    .get("address")
                    .and_then(Value::as_str)
                    .filter(|i| !i.trim().is_empty())
                {
                    return Some(ip.to_string());
                }
            }
        }
        None
    }

    /// `HostingerService::waitForPublicIp`.
    pub async fn hostinger_wait_for_ip(&self, mut vm: Value) -> Result<Value, CloudError> {
        let Some(id) = vm.get("id").and_then(Value::as_i64) else {
            return Ok(vm);
        };
        if Self::hostinger_public_ip(&vm).is_some() {
            return Ok(vm);
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            vm = self.hostinger_get_vm(id).await?;
            if Self::hostinger_public_ip(&vm).is_some() {
                break;
            }
        }
        Ok(vm)
    }
}

/// The filter of `HetznerService::getServerTypes`: deprecated types are removed, each type keeps
/// only the locations where it is not deprecated and (unless `include_out_of_stock`) in stock,
/// and a type is kept when it has such a location (or that location is `location`).
pub fn filter_hetzner_server_types(
    types: Vec<Value>,
    location: Option<&str>,
    include_out_of_stock: bool,
) -> Vec<Value> {
    // PHP `empty()`.
    let empty = |v: Option<&Value>| match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => true,
        Some(Value::String(s)) => s.is_empty() || s == "0",
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(Value::Bool(true)) => false,
    };
    types
        .into_iter()
        .filter(|t| t.get("deprecated") != Some(&Value::Bool(true)))
        .filter_map(|mut t| {
            let locations: Vec<Value> = t
                .get("locations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|l| {
                    empty(l.get("deprecation"))
                        && (include_out_of_stock
                            || l.get("available").is_none_or(Value::is_null)
                            || l.get("available") == Some(&Value::Bool(true)))
                })
                .collect();
            let keep = match location {
                None => !locations.is_empty(),
                Some(name) => locations
                    .iter()
                    .any(|l| l.get("name").and_then(Value::as_str) == Some(name)),
            };
            let obj = t.as_object_mut()?;
            obj.insert("locations".into(), Value::Array(locations));
            keep.then_some(t)
        })
        .collect()
}

/// PHP `rawurlencode`.
fn rawurlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `hetznerServerIpv6`: the first address of the network.
pub fn hetzner_server_ipv6(network: Option<&str>) -> Option<String> {
    let network = network.filter(|n| !n.trim().is_empty())?;
    let address = network.split('/').next().unwrap_or_default();
    let parsed: std::net::Ipv6Addr = address.parse().ok()?;
    if network.contains('/') {
        let bits = u128::from(parsed) | 1;
        return Some(std::net::Ipv6Addr::from(bits).to_string());
    }
    Some(parsed.to_string())
}

/// `Http::withToken($token)->timeout(10)->get($endpoint)->successful()`: one request without retries.
pub async fn probe(
    provider: Provider,
    token: &str,
    endpoint: &str,
    accept_json: bool,
) -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let mut req = client
        .get(format!("{}{endpoint}", provider.base()))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "GuzzleHttp/7");
    if accept_json {
        req = req.header("Accept", "application/json");
    }
    match req.send().await {
        Ok(r) => Ok(r.status().is_success()),
        Err(e) => Err(e.to_string()),
    }
}

impl Provider {
    /// The endpoint every token of the provider can read.
    pub fn validation_endpoint(self) -> &'static str {
        match self {
            Self::Hetzner => "/servers",
            Self::DigitalOcean | Self::Vultr => "/account",
            Self::Hostinger => "/api/vps/v1/virtual-machines",
        }
    }
}

/// `CloudProviderTokensController::validateProviderToken`: the error message when invalid.
pub async fn validate_provider_token(provider: &str, token: &str) -> Result<(), String> {
    let Some(p) = Provider::from_name(provider) else {
        return Err("Unsupported provider.".to_string());
    };
    match probe(p, token, p.validation_endpoint(), p == Provider::Hostinger).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "Invalid {provider} token. Please check your API token."
        )),
        Err(_) => Err("Failed to validate token with provider API.".to_string()),
    }
}

fn str_of(r: &coolify_db::records::Record, k: &str) -> Option<String> {
    r.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The decrypted API token of a server's `cloudProviderToken`, with its provider.
async fn server_token(jobs: &Jobs, row: &ServerRow) -> Result<Option<(String, String)>, String> {
    let Some(id) = row
        .record
        .get("cloud_provider_token_id")
        .and_then(Value::as_i64)
    else {
        return Ok(None);
    };
    let Some(t) = db::find_token_by_id(&jobs.0.db, id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let enc = jobs
        .0
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    let plain = str_of(&t, "token").ok_or("token missing")?;
    let plain = enc.decrypt_string(&plain).map_err(|e| e.to_string())?;
    Ok(Some((str_of(&t, "provider").unwrap_or_default(), plain)))
}

/// `Server::backfillPlaceholderIp`.
async fn backfill_placeholder_ip(
    jobs: &Jobs,
    row: &ServerRow,
    ip: Option<String>,
) -> Result<(), String> {
    let info = crate::server::ServerInfo::from_row(row);
    if let Some(ip) = ip.filter(|i| !i.is_empty())
        && info.has_placeholder_ip()
        && !servers::ip_in_use(&jobs.0.db, &ip, Some(row.id))
            .await
            .map_err(|e| e.to_string())?
    {
        servers::update_columns(
            &jobs.0.db,
            row.id,
            &[("ip", ColumnValue::Text(Some(ip)))],
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn persist(
    jobs: &Jobs,
    row: &ServerRow,
    col: &'static str,
    status: Option<String>,
) -> Result<(), String> {
    servers::update_if_changed(
        &jobs.0.db,
        servers::Row::Server(row.id),
        &[(col, ColumnValue::Text(status))],
        Jobs::now(),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// `refreshVultrState`, `refreshDigitalOceanState`, `refreshHostingerState`, `refreshHetznerState`:
/// the provider's status of the server.
pub async fn refresh_state(jobs: &Jobs, row: &ServerRow) -> Result<Option<String>, String> {
    let r = &row.record;
    let stored = |k: &str| str_of(r, k);
    let transferred = crate::server::ServerInfo::from_row(row).transferred;
    let token = server_token(jobs, row).await?;
    if let Some(id) = r.get("vultr_instance_id").and_then(Value::as_str) {
        if transferred {
            return Ok(stored("vultr_instance_status"));
        }
        let Some((_, t)) = token else {
            return Ok(None);
        };
        let c = Client::new(Provider::Vultr, &t);
        return match c.vultr_get_instance(id).await {
            Err(e) if e.status() == 404 => {
                persist(
                    jobs,
                    row,
                    "vultr_instance_status",
                    Some("deleted".to_string()),
                )
                .await?;
                Ok(Some("deleted".to_string()))
            }
            Err(e) => Err(e.to_string()),
            Ok(i) => {
                let status = if i.get("power_status").and_then(Value::as_str) == Some("stopped") {
                    Some("stopped".to_string())
                } else {
                    i.get("status").and_then(Value::as_str).map(str::to_string)
                };
                persist(jobs, row, "vultr_instance_status", status.clone()).await?;
                backfill_placeholder_ip(jobs, row, Client::vultr_public_ip(&i, false, true))
                    .await?;
                Ok(status)
            }
        };
    }
    if let Some(id) = r.get("digitalocean_droplet_id").and_then(Value::as_i64) {
        let Some((provider, t)) = token
            .filter(|(p, _)| p == "digitalocean")
            .filter(|_| !transferred)
        else {
            return Ok(stored("digitalocean_droplet_status"));
        };
        let _ = provider;
        let c = Client::new(Provider::DigitalOcean, &t);
        return match c.do_get_droplet(id).await {
            Err(e) if e.status() == 404 => {
                persist(
                    jobs,
                    row,
                    "digitalocean_droplet_status",
                    Some("deleted".to_string()),
                )
                .await?;
                Ok(Some("deleted".to_string()))
            }
            Err(e) => Err(e.to_string()),
            Ok(d) => {
                if d.as_object().is_none_or(Map::is_empty) {
                    return Ok(stored("digitalocean_droplet_status"));
                }
                let status = d.get("status").and_then(Value::as_str).map(str::to_string);
                persist(jobs, row, "digitalocean_droplet_status", status.clone()).await?;
                backfill_placeholder_ip(jobs, row, Client::do_public_ip(&d, true, true)).await?;
                Ok(status)
            }
        };
    }
    if let Some(id) = r
        .get("hostinger_virtual_machine_id")
        .and_then(Value::as_i64)
    {
        let Some((_, t)) = token
            .filter(|(p, _)| p == "hostinger")
            .filter(|_| !transferred)
        else {
            return Ok(stored("hostinger_virtual_machine_status"));
        };
        let c = Client::new(Provider::Hostinger, &t);
        let vm = c.hostinger_get_vm(id).await.map_err(|e| e.to_string())?;
        if vm.as_object().is_none_or(Map::is_empty) {
            return Ok(stored("hostinger_virtual_machine_status"));
        }
        let status = vm.get("state").and_then(Value::as_str).map(str::to_string);
        persist(
            jobs,
            row,
            "hostinger_virtual_machine_status",
            status.clone(),
        )
        .await?;
        backfill_placeholder_ip(jobs, row, Client::hostinger_public_ip(&vm)).await?;
        return Ok(status);
    }
    if let Some(id) = r.get("hetzner_server_id").and_then(Value::as_i64) {
        let Some((_, t)) = token
            .filter(|(p, _)| p == "hetzner")
            .filter(|_| !transferred)
        else {
            return Ok(stored("hetzner_server_status"));
        };
        let c = Client::new(Provider::Hetzner, &t);
        let s = c.hetzner_get_server(id).await.map_err(|e| e.to_string())?;
        let status = s.get("status").and_then(Value::as_str).map(str::to_string);
        persist(jobs, row, "hetzner_server_status", status.clone()).await?;
        let ip = s
            .pointer("/public_net/ipv4/ip")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                hetzner_server_ipv6(s.pointer("/public_net/ipv6/ip").and_then(Value::as_str))
            });
        backfill_placeholder_ip(jobs, row, ip).await?;
        return Ok(status);
    }
    Ok(None)
}

/// `Server::refreshCloudProviderState`: the status of the linked cloud server, chosen by the
/// provider of the server's token.
pub async fn refresh_cloud_provider_state(
    jobs: &Jobs,
    row: &ServerRow,
) -> Result<Option<String>, String> {
    let Some((provider, _)) = server_token(jobs, row).await? else {
        return Ok(None);
    };
    let column = match provider.as_str() {
        "hetzner" => "hetzner_server_id",
        "vultr" => "vultr_instance_id",
        "digitalocean" => "digitalocean_droplet_id",
        "hostinger" => "hostinger_virtual_machine_id",
        _ => return Ok(None),
    };
    let linked = match row.record.get(column) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(Value::Number(n)) => n.as_i64() != Some(0),
        Some(_) => true,
    };
    if !linked {
        return Ok(None);
    }
    refresh_state(jobs, row).await
}

/// `ValidateServer`'s provider checks: the stopping error, stored by the caller in `validation_logs`.
pub async fn validation_block(jobs: &Jobs, row: &ServerRow) -> Result<Option<String>, String> {
    let r = &row.record;
    if r.get("hetzner_server_id").is_some_and(|v| !v.is_null()) {
        // The provider API can be down or the token revoked; the SSH checks still decide.
        let status = match refresh_state(jobs, row).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(server_id = row.id, error = %e, "Could not refresh the Hetzner server status before validation");
                None
            }
        };
        match status.as_deref() {
            Some("off") => {
                return Ok(Some(
                    "Hetzner server is off. Power it on before validating.".to_string(),
                ));
            }
            Some(
                s @ ("initializing" | "starting" | "stopping" | "deleting" | "rebuilding"
                | "migrating"),
            ) => {
                return Ok(Some(format!(
                    "Hetzner server is {s}. Wait until it is running, then validate."
                )));
            }
            _ => {}
        }
    }
    if r.get("vultr_instance_id").is_some_and(|v| !v.is_null()) {
        let status = refresh_state(jobs, row).await?;
        return Ok(match status.as_deref() {
            Some("deleted") => Some("Vultr instance is deleted or no longer accessible. Relink this server before validating.".to_string()),
            Some(s @ ("stopped" | "suspended")) => Some(format!("Vultr instance is {s}. Power it on before validating.")),
            _ => None,
        });
    }
    if r.get("digitalocean_droplet_id")
        .is_some_and(|v| !v.is_null())
    {
        let status = refresh_state(jobs, row).await?;
        return Ok(match status.as_deref() {
            Some("deleted") => Some("DigitalOcean droplet is deleted or no longer accessible. Relink this server before validating.".to_string()),
            Some(s @ ("off" | "archive")) => Some(format!("DigitalOcean droplet is {s}. Power it on before validating.")),
            _ => None,
        });
    }
    if r.get("hostinger_virtual_machine_id")
        .is_some_and(|v| !v.is_null())
    {
        let status = refresh_state(jobs, row).await?;
        return Ok(match status.as_deref() {
            Some(
                s @ ("stopping" | "stopped" | "suspending" | "suspended" | "destroying"
                | "destroyed" | "error"),
            ) => Some(format!(
                "Hostinger VPS is {s}. Power it on before validating."
            )),
            _ => None,
        });
    }
    Ok(None)
}

/// Which providers `DeleteServer` deletes from.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeleteFrom {
    pub hetzner: bool,
    pub vultr: bool,
    pub digitalocean: bool,
    pub hostinger: bool,
}

impl DeleteFrom {
    pub fn any(self) -> bool {
        self.hetzner || self.vultr || self.digitalocean || self.hostinger
    }
}

/// The server's own token when it belongs to this provider and team, else the team's first one.
async fn delete_token(
    jobs: &Jobs,
    row: &ServerRow,
    provider: &str,
) -> Result<Option<String>, String> {
    let team_id = row
        .record
        .get("team_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let own = match row
        .record
        .get("cloud_provider_token_id")
        .and_then(Value::as_i64)
    {
        Some(id) => db::find_token_by_id(&jobs.0.db, id)
            .await
            .map_err(|e| e.to_string())?
            .filter(|t| {
                t.get("team_id").and_then(Value::as_i64) == Some(team_id)
                    && str_of(t, "provider").as_deref() == Some(provider)
            }),
        None => None,
    };
    let token = match own {
        Some(t) => Some(t),
        None => db::first_token_of_provider(&jobs.0.db, team_id, provider)
            .await
            .map_err(|e| e.to_string())?,
    };
    let Some(token) = token else {
        return Ok(None);
    };
    let enc = jobs
        .0
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    let plain = str_of(&token, "token").ok_or("token missing")?;
    enc.decrypt_string(&plain)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// `DeleteServer::deleteFromHetznerById`: failures are logged and notified, the server still goes.
async fn delete_from_hetzner(jobs: &Jobs, row: &ServerRow, id: i64) {
    let team_id = row.record.get("team_id").and_then(Value::as_i64);
    let result = match delete_token(jobs, row, "hetzner").await {
        Ok(Some(t)) => Client::new(Provider::Hetzner, &t)
            .hetzner_delete_server(id)
            .await
            .map_err(|e| e.to_string()),
        // `No Hetzner token found`: logged, nothing else.
        Ok(None) => {
            tracing::error!(server_uuid = %row.uuid, "no hetzner token found for the server team");
            return;
        }
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        tracing::error!(server_uuid = %row.uuid, error = %e, "failed to delete the server from hetzner");
        if let Some(team_id) = team_id {
            crate::notifications::notify(
                &jobs.0.db,
                jobs.0.encrypter.as_ref(),
                team_id,
                crate::notifications::Notification::HetznerDeletionFailed {
                    hetzner_server_id: id,
                    error: coolify_compat::redact::redact_text(&e, &[], &[]),
                },
            )
            .await;
        }
    }
}

/// `DeleteServer::handle`, up to its `forceDelete()`. An error from Vultr, DigitalOcean or Hostinger is
/// rethrown upstream, which ends the job and leaves the server soft deleted.
pub async fn delete_from_providers(
    jobs: &Jobs,
    row: &ServerRow,
    from: DeleteFrom,
) -> Result<(), String> {
    let r = &row.record;
    if from.hetzner
        && let Some(id) = r.get("hetzner_server_id").and_then(Value::as_i64)
    {
        delete_from_hetzner(jobs, row, id).await;
    }
    if from.vultr
        && let Some(id) = r.get("vultr_instance_id").and_then(Value::as_str)
    {
        let t = delete_token(jobs, row, "vultr")
            .await?
            .ok_or("No Vultr token found for the server team.")?;
        Client::new(Provider::Vultr, &t)
            .vultr_delete_instance(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    if from.digitalocean
        && let Some(id) = r.get("digitalocean_droplet_id").and_then(Value::as_i64)
    {
        let t = delete_token(jobs, row, "digitalocean")
            .await?
            .ok_or("No DigitalOcean token found for the server team.")?;
        Client::new(Provider::DigitalOcean, &t)
            .do_delete_droplet(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    if from.hostinger
        && let Some(id) = r
            .get("hostinger_virtual_machine_id")
            .and_then(Value::as_i64)
    {
        let t = delete_token(jobs, row, "hostinger")
            .await?
            .ok_or("No Hostinger token found for the server team.")?;
        let client = Client::new(Provider::Hostinger, &t);
        let vm = client
            .hostinger_get_vm(id)
            .await
            .map_err(|e| e.to_string())?;
        let subscription = vm
            .get("subscription_id")
            .and_then(|v| match v {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .ok_or("The Hostinger VPS has no subscription.")?;
        client
            .hostinger_disable_auto_renewal(&subscription)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider stand-in that answers every request with `status` and `body`, counting requests.
    async fn fake(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn a_rejected_hetzner_post_is_a_provider_error_and_not_sent_again() {
        let (base, hits) = fake(
            "422 Unprocessable Entity",
            r#"{"error":{"code":"INVALID_INPUT","message":"invalid input in field 'name'","details":{"fields":[{"name":"name"},{"name":"bad field!"}]}}}"#,
        )
        .await;
        let c = Client::with_base(Provider::Hetzner, "t", &base);
        let Err(CloudError::Provider(e)) = c.hetzner_create_server(&json!({})).await else {
            panic!("expected a provider error");
        };
        assert_eq!(
            e.message,
            "Hetzner API error: invalid input in field 'name'"
        );
        assert_eq!(e.status, 422);
        assert_eq!(e.error_code.as_deref(), Some("invalid_input"));
        assert_eq!(e.invalid_fields, vec!["name".to_string()]);
        assert_eq!(
            e.safe_message(),
            "Hetzner rejected the request as invalid. Check the selected values and your account limits. Invalid fields: name."
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn hetzner_retries_only_server_errors_of_idempotent_requests() {
        let (base, hits) = fake("503 Service Unavailable", r#"{"error":{"message":"down"}}"#).await;
        let c = Client::with_base(Provider::Hetzner, "t", &base);
        let e = c.hetzner_get_server(1).await.unwrap_err();
        assert_eq!(e.status(), 503);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        let e = c.hetzner_create_server(&json!({})).await.unwrap_err();
        assert_eq!(e.status(), 503);
        assert_eq!(hits.load(Ordering::SeqCst), 4);

        let (base, hits) = fake(
            "403 Forbidden",
            r#"{"error":{"code":"forbidden","message":"no"}}"#,
        )
        .await;
        let c = Client::with_base(Provider::Hetzner, "t", &base);
        let Err(CloudError::Provider(e)) = c.hetzner_get_server(1).await else {
            panic!("expected a provider error");
        };
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            e.json_body("Failed to fetch Hetzner locations."),
            json!({
                "message": "Failed to fetch Hetzner locations. The Hetzner API token does not have permission for this request. Use a token with read and write access.",
                "provider_status": 403,
                "provider_error_code": "forbidden",
            })
        );
    }

    #[tokio::test]
    async fn droplet_and_vultr_errors_keep_their_codes_and_retries() {
        let (base, hits) = fake(
            "422 Unprocessable Entity",
            r#"{"id":"unprocessable_entity","message":"You specified an invalid size."}"#,
        )
        .await;
        let c = Client::with_base(Provider::DigitalOcean, "t", &base);
        let Err(CloudError::Provider(e)) = c.do_create_droplet(&json!({})).await else {
            panic!("expected a provider error");
        };
        assert_eq!(
            e.message,
            "DigitalOcean API error: You specified an invalid size."
        );
        assert_eq!(e.error_code.as_deref(), Some("unprocessable_entity"));
        // `Http::retry(3)` without a `when` callback sends every failed request again.
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        let (base, _) = fake("401 Unauthorized", r#"{"error":"Invalid API token."}"#).await;
        let c = Client::with_base(Provider::Vultr, "t", &base);
        let Err(CloudError::Provider(e)) = c.vultr_regions().await else {
            panic!("expected a provider error");
        };
        assert_eq!(e.message, "Vultr API error: Invalid API token.");
        assert_eq!(e.error_code, None);
        assert_eq!(
            e.safe_message(),
            "The Vultr API token is invalid or expired. Update the cloud provider token in Coolify."
        );
    }

    #[test]
    fn safe_messages_follow_the_code_then_the_status() {
        let m = |code: Option<&str>, status: u16| {
            ProviderApiError::new("Hetzner", "x", status, code.map(|c| json!(c)).as_ref(), &[])
                .safe_message()
        };
        assert_eq!(
            m(Some("resource_limit_exceeded"), 403),
            "Your Hetzner account has reached a resource limit, for example the server limit. Remove unused resources or ask Hetzner for a higher limit."
        );
        assert_eq!(
            m(Some("resource_unavailable"), 412),
            "The selected server type, image, or location is not available now. Select a different location or server type."
        );
        assert_eq!(
            m(Some("uniqueness_error"), 409),
            "A resource with the same name already exists in your Hetzner account, for example a server or an SSH key. Use a different name."
        );
        assert_eq!(
            m(None, 502),
            "Hetzner cannot process the request now. Try again later."
        );
        assert_eq!(
            m(Some("maintenance"), 400),
            "Hetzner cannot process the request now. Try again later."
        );
        assert_eq!(
            m(None, 404),
            "Hetzner could not find a requested resource, for example the location, server type, image, or SSH key."
        );
        assert_eq!(
            m(None, 409),
            "Hetzner rejected the request because of a conflict with an existing resource."
        );
        assert_eq!(
            m(Some("conflict"), 412),
            "Hetzner returned HTTP 412 (error code: conflict)."
        );
        assert_eq!(m(None, 418), "Hetzner returned HTTP 418.");
        // Codes that do not look like identifiers are dropped.
        assert_eq!(m(Some("<script>"), 418), "Hetzner returned HTTP 418.");
    }

    #[test]
    fn server_types_keep_orderable_locations() {
        let types = vec![
            json!({"name": "old", "deprecated": true, "locations": [{"name": "nbg1"}]}),
            json!({"name": "cx22", "locations": [
                {"name": "nbg1", "available": true},
                {"name": "fsn1", "available": false},
                {"name": "hel1", "deprecation": {"announced": "2026-01-01"}},
            ]}),
            json!({"name": "gone", "locations": [{"name": "fsn1", "available": false}]}),
        ];
        let names = |v: &[Value]| {
            v.iter()
                .map(|t| {
                    let locations: Vec<&str> = t["locations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| l["name"].as_str().unwrap())
                        .collect();
                    format!("{}:{}", t["name"].as_str().unwrap(), locations.join(","))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&filter_hetzner_server_types(types.clone(), None, false)),
            vec!["cx22:nbg1"]
        );
        assert_eq!(
            names(&filter_hetzner_server_types(
                types.clone(),
                Some("fsn1"),
                false
            )),
            Vec::<String>::new()
        );
        assert_eq!(
            names(&filter_hetzner_server_types(types.clone(), None, true)),
            vec!["cx22:nbg1,fsn1", "gone:fsn1"]
        );
        assert_eq!(
            names(&filter_hetzner_server_types(types, Some("nbg1"), false)),
            vec!["cx22:nbg1"]
        );
    }
}
