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
        let over = std::env::var("COOLIFY_RS_CLOUD_API_BASE").ok();
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
    /// `\Exception` with the message and code.
    Api(String, u16),
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimit(_) => f.write_str("Rate limit exceeded. Please try again later."),
            Self::Api(m, _) => f.write_str(m),
        }
    }
}

/// One provider API client.
#[derive(Debug, Clone)]
pub struct Client {
    provider: Provider,
    token: String,
    http: reqwest::Client,
    /// Status and body of the last rejected response, as a `RequestException` carries them.
    failure: std::sync::Arc<std::sync::Mutex<Option<(u16, String)>>>,
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
            failure: Default::default(),
        }
    }

    /// Status and body of the last rejected response.
    pub fn last_failure(&self) -> Option<(u16, String)> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }

    fn attempts(&self, method: &Method) -> u32 {
        // `Http::retry(3, ...)`; Hostinger retries only GET.
        if self.provider == Provider::Hostinger && *method != Method::GET {
            1
        } else {
            3
        }
    }

    /// `request($method, $endpoint, $data)`: query string for GET, JSON otherwise.
    async fn request(
        &self,
        method: Method,
        endpoint: &str,
        data: Option<&Value>,
    ) -> Result<Value, CloudError> {
        let url = format!("{}{endpoint}", self.provider.base());
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
                    if !status.is_success()
                        && let Ok(mut f) = self.failure.lock()
                    {
                        *f = Some((
                            status.as_u16(),
                            String::from_utf8_lossy(&bytes).into_owned(),
                        ));
                    }
                    let retryable = !status.is_success()
                        && (self.provider != Provider::Hostinger || status.is_server_error());
                    last = Some((status, body, headers));
                    transport = None;
                    if !retryable {
                        break;
                    }
                }
                Err(e) => {
                    transport = Some(e.to_string());
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
        let message = match self.provider {
            Provider::Hetzner => text("/error/message"),
            Provider::Vultr => text("/error"),
            Provider::DigitalOcean => text("/message"),
            Provider::Hostinger => text("/message").or_else(|| text("/error")),
        }
        .unwrap_or_else(|| "Unknown error".to_string());
        Err(CloudError::Api(
            format!("{} API error: {message}", self.provider.label()),
            status.as_u16(),
        ))
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

    pub async fn hetzner_server_types(&self) -> Result<Vec<Value>, CloudError> {
        let types = self.paged("/server_types", "server_types", &[]).await?;
        Ok(types
            .into_iter()
            .filter(|t| t.get("deprecated") != Some(&Value::Bool(true)))
            .collect())
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

    pub async fn hostinger_get_vm(&self, id: i64) -> Result<Value, CloudError> {
        self.get(&format!("/api/vps/v1/virtual-machines/{id}"), None)
            .await
    }

    async fn hostinger_vms(&self) -> Result<Vec<Value>, CloudError> {
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
            Err(CloudError::Api(_, 404)) => {
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
            Err(CloudError::Api(_, 404)) => {
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

/// `ValidateServer`'s provider checks: the stopping error, stored by the caller in `validation_logs`.
pub async fn validation_block(jobs: &Jobs, row: &ServerRow) -> Result<Option<String>, String> {
    let r = &row.record;
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

/// `DeleteServer`'s deletion at the provider; failures are logged, the server is removed anyway.
pub async fn delete_from_provider(jobs: &Jobs, row: &ServerRow) {
    let r = &row.record;
    let token = match server_token(jobs, row).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "cloud token unavailable");
            None
        }
    };
    let Some((provider, t)) = token else {
        return;
    };
    let result = match Provider::from_name(&provider) {
        Some(Provider::Hetzner) => match r.get("hetzner_server_id").and_then(Value::as_i64) {
            Some(id) => {
                Client::new(Provider::Hetzner, &t)
                    .hetzner_delete_server(id)
                    .await
            }
            None => Ok(()),
        },
        Some(Provider::Vultr) => match r.get("vultr_instance_id").and_then(Value::as_str) {
            Some(id) => {
                Client::new(Provider::Vultr, &t)
                    .vultr_delete_instance(id)
                    .await
            }
            None => Ok(()),
        },
        Some(Provider::DigitalOcean) => {
            match r.get("digitalocean_droplet_id").and_then(Value::as_i64) {
                Some(id) => {
                    Client::new(Provider::DigitalOcean, &t)
                        .do_delete_droplet(id)
                        .await
                }
                None => Ok(()),
            }
        }
        _ => Ok(()),
    };
    if let Err(e) = result {
        tracing::error!(server_uuid = %row.uuid, error = %e, "failed to delete the server at its provider");
        // `DeleteServer::deleteFromHetzner`: the other providers only log.
        if provider == "hetzner"
            && let (Some(id), Some(team_id)) = (
                r.get("hetzner_server_id").and_then(Value::as_i64),
                r.get("team_id").and_then(Value::as_i64),
            )
        {
            crate::notifications::notify(
                &jobs.0.db,
                jobs.0.encrypter.as_ref(),
                team_id,
                crate::notifications::Notification::HetznerDeletionFailed {
                    hetzner_server_id: id,
                    error: coolify_compat::redact::redact_text(&e.to_string(), &[], &[]),
                },
            )
            .await;
        }
    }
}
