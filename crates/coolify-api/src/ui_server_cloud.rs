//! Session endpoints behind `Server\New\By{Hetzner,Vultr,DigitalOcean,Hostinger}`,
//! `Security\CloudProviderTokenForm` and `Server\CloudProviderToken\Show`.

use std::cmp::Ordering;
use std::net::SocketAddr;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coolify_db::cloud as db;
use coolify_db::private_keys::{self, KeyRow};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue as C, ServerRow};
use coolify_domain::new_public_id;
use coolify_jobs::cloud::{self, Client, CloudError, Provider};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::Input;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::cloud_providers::{
    Created, cloud_init_field, create_row, hostname_rule, integer_items, key_material, string_items,
};
use crate::routes::server_settings::{find_server, validation_errors};
use crate::routes::servers::as_php_string;
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;
use crate::validation::{Field, Rule, validate};

const UNAUTHORIZED: &str = "This action is unauthorized.";
const NOT_FOUND: ApiError = ApiError::Message(axum::http::StatusCode::NOT_FOUND, "Not found.");
/// `Server::PLACEHOLDER_IP`.
const PLACEHOLDER_IP: &str = "1.2.3.4";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/cloud/tokens", post(add_token))
        .route("/ui/cloud/{provider}/form", get(form))
        .route("/ui/cloud/{provider}/{token}/data", get(data))
        .route("/ui/cloud/{provider}/{token}/servers", post(submit))
        .route(
            &format!("{SERVER_PAGE}/cloud-provider-token"),
            get(show).post(set_token),
        )
        .route(
            &format!("{SERVER_PAGE}/cloud-provider-token/validate"),
            post(validate_token),
        )
}

fn parse_provider(name: &str) -> Result<Provider, ApiError> {
    Provider::from_name(name).ok_or(NOT_FOUND)
}

pub(crate) fn decrypt(state: &AppState, encrypted: Option<&str>) -> Result<String, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    encrypted
        .and_then(|c| enc.decrypt_string(c).ok())
        .ok_or(ApiError::Internal)
}

fn token_summary(r: &Record) -> Value {
    json!({
        "id": r.get("id"),
        "uuid": r.get("uuid"),
        "name": r.get("name"),
        "description": r.get("description"),
        "created_at": r.get("created_at"),
    })
}

/// `CloudProviderToken::ownedByCurrentTeam()->where('provider', ...)`.
async fn team_tokens(
    state: &AppState,
    team_id: i64,
    provider: &str,
) -> Result<Vec<Record>, ApiError> {
    Ok(db::list_tokens(&state.db, team_id)
        .await?
        .into_iter()
        .filter(|r| r.get("provider").and_then(Value::as_str) == Some(provider))
        .collect())
}

/// `selectTokenFromUrl`: the row and the decrypted API token.
async fn selected_token(
    state: &AppState,
    ctx: &AuthCtx,
    provider: &str,
    uuid: &str,
) -> Result<Option<(i64, String)>, ApiError> {
    let Some(row) = db::find_token(&state.db, ctx.team_id, uuid)
        .await?
        .filter(|r| r.get("provider").and_then(Value::as_str) == Some(provider))
    else {
        return Ok(None);
    };
    let plain = decrypt(state, row.get("token").and_then(Value::as_str))?;
    Ok(Some((
        row.get("id").and_then(Value::as_i64).unwrap_or_default(),
        plain,
    )))
}

/// `mount()` of the four forms.
async fn form(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(provider): Path<String>,
) -> ApiResult {
    parse_provider(&provider)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let tokens = team_tokens(&state, ctx.team_id, &provider).await?;
    let keys = servers::ssh_keys_for_form(&state.db, ctx.team_id).await?;
    let mut scripts = Vec::new();
    for r in db::list_scripts(&state.db, ctx.team_id).await? {
        scripts.push(json!({
            "id": r.get("id"),
            "name": r.get("name"),
            "script": decrypt(&state, r.get("script").and_then(Value::as_str))?,
        }));
    }
    Ok(ok(json!({
        "tokens": tokens.iter().map(token_summary).collect::<Vec<_>>(),
        "keys": keys.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "scripts": scripts,
        "name": crate::routes::private_keys::random_name(),
    }))
    .into_response())
}

// ── Provider data ───────────────────────────────────────────────────────

/// PHP's `<=>` with `SORT_REGULAR`, as far as provider data needs it.
fn php_cmp(a: &Value, b: &Value) -> Ordering {
    let number = |v: &Value| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    };
    if let (Some(x), Some(y)) = (number(a), number(b)) {
        return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    }
    let text = |v: &Value| match v {
        Value::Null => String::new(),
        o => as_php_string(o),
    };
    text(a).cmp(&text(b))
}

/// `collect($items)->sortBy($key)->values()`: PHP's sort is stable.
fn sort_by(mut items: Vec<Value>, key: &str) -> Vec<Value> {
    items.sort_by(|a, b| {
        php_cmp(
            a.get(key).unwrap_or(&Value::Null),
            b.get(key).unwrap_or(&Value::Null),
        )
    });
    items
}

fn as_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

fn data_response(
    r: Result<Value, CloudError>,
    error: impl FnOnce(CloudError) -> String,
) -> Response {
    match r {
        Ok(v) => ok(v).into_response(),
        Err(e) => ok(json!({"error": error(e)})).into_response(),
    }
}

/// `providerDataErrorMessage` of `ByVultr` and `ByDigitalOcean`.
fn prefixed_error(provider: &str, e: &CloudError) -> String {
    let details = e.to_string();
    let prefix = format!("{provider} API error: ");
    if details.starts_with(&prefix) {
        details
    } else {
        format!("{prefix}{details}")
    }
}

/// The `loadHetznerData` error: a `CloudProviderApiException` keeps its message, anything else
/// gets the `Hetzner API error: ` prefix.
fn hetzner_data_error(e: &CloudError) -> String {
    match e {
        CloudError::Provider(p) => p.message.clone(),
        other => format!("Hetzner API error: {other}"),
    }
}

/// `ByHetzner::isServerTypeOutOfStock`: the type's entry for the location says unavailable.
fn server_type_out_of_stock(server_type: &Value, location: &str) -> bool {
    server_type
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|l| {
            l.iter()
                .find(|l| l.get("name").and_then(Value::as_str) == Some(location))
        })
        .and_then(|l| l.get("available"))
        == Some(&Value::Bool(false))
}

async fn hetzner_data(c: &Client) -> Result<Value, CloudError> {
    let locations = c.hetzner_locations().await?;
    // `getServerTypes(includeOutOfStock: true)`: the form shows them disabled.
    let server_types = c.hetzner_server_types(None, true).await?;
    let images: Vec<Value> = c
        .hetzner_images()
        .await?
        .into_iter()
        .filter(|i| {
            i.get("type").and_then(Value::as_str) == Some("system")
                && i.get("deprecated") != Some(&Value::Bool(true))
        })
        .collect();
    let ssh_keys = c.hetzner_ssh_keys().await?;
    let firewalls = c.hetzner_firewalls().await?;
    let networks = c.hetzner_networks().await?;
    Ok(json!({
        "locations": locations,
        "server_types": server_types,
        "images": sort_by(images, "name"),
        "ssh_keys": ssh_keys,
        "firewalls": sort_by(firewalls, "name"),
        "networks": sort_by(networks, "name"),
    }))
}

async fn vultr_data(c: &Client) -> Result<Value, CloudError> {
    let regions = c.vultr_regions().await?;
    let plans = c.vultr_plans().await?;
    let os = c.vultr_os().await?;
    let ssh_keys = c.vultr_ssh_keys().await?;
    Ok(json!({
        "regions": sort_by(regions, "id"),
        "plans": sort_by(plans, "monthly_cost"),
        "operating_systems": sort_by(os, "name"),
        "ssh_keys": ssh_keys,
    }))
}

async fn digitalocean_data(c: &Client) -> Result<Value, CloudError> {
    let regions = c.do_regions().await?;
    let sizes = c.do_sizes().await?;
    let mut images = c.do_images().await?;
    let sort_key = |i: &Value| {
        let part = |k: &str| i.get(k).map(as_php_string).unwrap_or_default();
        Value::String(format!("{} {}", part("distribution"), part("name")))
    };
    images.sort_by(|a, b| php_cmp(&sort_key(a), &sort_key(b)));
    let ssh_keys = c.do_ssh_keys().await?;
    Ok(json!({
        "regions": regions,
        "sizes": sizes,
        "images": images,
        "ssh_keys": ssh_keys,
    }))
}

async fn hostinger_data(c: &Client) -> Result<Value, CloudError> {
    let data_centers = c.hostinger_data_centers().await?;
    let templates = as_list(c.hostinger_templates().await?);
    let catalog = c.hostinger_catalog().await?;
    // `optionalProviderData`: some tokens cannot read these lists.
    let public_keys = c
        .hostinger_public_keys()
        .await
        .unwrap_or_else(|_| json!([]));
    let scripts = c
        .hostinger_post_install_scripts()
        .await
        .unwrap_or_else(|_| json!([]));
    Ok(json!({
        "data_centers": data_centers,
        "templates": sort_by(templates, "name"),
        "catalog_items": catalog,
        "public_keys": public_keys,
        "post_install_scripts": scripts,
    }))
}

/// `loadHetznerData`, `loadVultrData`, `loadDigitalOceanData` and `loadHostingerData`.
async fn data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((provider, token)): Path<(String, String)>,
) -> ApiResult {
    let p = parse_provider(&provider)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let Some((_, plain)) = selected_token(&state, &ctx, &provider, &token).await? else {
        return Err(NOT_FOUND);
    };
    let client = Client::new(p, &plain);
    Ok(match p {
        Provider::Hetzner => data_response(hetzner_data(&client).await, |e| hetzner_data_error(&e)),
        Provider::Vultr => {
            data_response(vultr_data(&client).await, |e| prefixed_error("Vultr", &e))
        }
        Provider::DigitalOcean => data_response(digitalocean_data(&client).await, |e| {
            prefixed_error("DigitalOcean", &e)
        }),
        Provider::Hostinger => data_response(hostinger_data(&client).await, |e| e.to_string()),
    })
}

// ── Creating the server ─────────────────────────────────────────────────

/// A blank string is `null` to `required` (`validateRequired` trims) and `nullable`.
fn normalized(body: Map<String, Value>) -> Input {
    Input(
        body.into_iter()
            .map(|(k, v)| match v {
                Value::String(s) if s.trim().is_empty() => (k, Value::Null),
                v => (k, v),
            })
            .collect(),
    )
}

fn server_name_field() -> Field {
    Field {
        name: "server_name",
        rules: vec![
            Rule::Required,
            Rule::String,
            Rule::Max(253),
            Rule::CustomOwned(hostname_rule),
        ],
    }
}

fn field(name: &'static str, rules: Vec<Rule>) -> Field {
    Field { name, rules }
}

fn required_string(name: &'static str) -> Field {
    field(name, vec![Rule::Required, Rule::String])
}

fn required_int(name: &'static str) -> Field {
    field(name, vec![Rule::Required, Rule::Integer])
}

fn required_bool(name: &'static str) -> Field {
    field(name, vec![Rule::Required, Rule::Boolean])
}

fn array(name: &'static str) -> Field {
    field(name, vec![Rule::Nullable, Rule::Array])
}

/// The cloud-init rules shared by Hetzner, Vultr and DigitalOcean.
fn cloud_init_fields() -> Vec<Field> {
    vec![
        cloud_init_field(),
        field("save_cloud_init_script", vec![Rule::Boolean]),
        field(
            "cloud_init_script_name",
            vec![Rule::Nullable, Rule::String, Rule::Max(255)],
        ),
        field(
            "selected_cloud_init_script_id",
            vec![Rule::Nullable, Rule::Integer],
        ),
    ]
}

/// `$this->validate()` with the two `exists` rules the validator cannot do.
async fn validate_form(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
    fields: &[Field],
    extra: impl FnOnce(&mut Map<String, Value>),
) -> Result<(), ApiError> {
    let allowed: Vec<&str> = input.keys().map(String::as_str).collect();
    let mut errors = match validate(input, fields, &[], &allowed) {
        Ok(()) => Map::new(),
        Err(ApiError::Validation(m)) => m,
        Err(e) => return Err(e),
    };
    extra(&mut errors);
    if let Some(id) = input.get("private_key_id").and_then(Value::as_i64)
        && !private_keys::exists_in_team(&state.db, ctx.team_id, id).await?
    {
        errors.insert(
            "private_key_id".into(),
            json!(["The selected private key id is invalid."]),
        );
    }
    if let Some(id) = input
        .get("selected_cloud_init_script_id")
        .and_then(Value::as_i64)
    {
        let scripts = db::list_scripts(&state.db, ctx.team_id).await?;
        if !scripts
            .iter()
            .any(|s| s.get("id").and_then(Value::as_i64) == Some(id))
        {
            errors.insert(
                "selected_cloud_init_script_id".into(),
                json!(["The selected selected cloud init script id is invalid."]),
            );
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// Saves the script when asked (`submit()` does so before it calls the provider).
async fn save_script(state: &AppState, ctx: &AuthCtx, input: &Input) -> Result<(), ApiError> {
    let wanted = input.get("save_cloud_init_script").is_some_and(php_truthy);
    let filled = |k: &str| input.get(k).is_some_and(php_truthy);
    if !wanted || !filled("cloud_init_script") || !filled("cloud_init_script_name") {
        return Ok(());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let name = input.str("cloud_init_script_name").unwrap_or_default();
    let id = db::create_script(
        &state.db,
        ctx.team_id,
        &new_public_id(),
        name,
        &enc.encrypt_string(input.str("cloud_init_script").unwrap_or_default()),
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "ui.cloud_init_script.created", team_id = ctx.team_id, cloud_init_script_id = id, cloud_init_script_name = %name, "cloud-init script created");
    Ok(())
}

fn unique(values: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn list(input: &Input, k: &str) -> Vec<Value> {
    input
        .get(k)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn flag(input: &Input, k: &str) -> bool {
    input.get(k).is_some_and(php_truthy)
}

fn user_data(input: &Input, params: &mut Map<String, Value>) {
    if flag(input, "cloud_init_script")
        && let Some(v) = input.get("cloud_init_script")
    {
        params.insert("user_data".into(), v.clone());
    }
}

/// `strtolower(trim($this->server_name))`.
fn normalized_name(input: &Input) -> String {
    coolify_remote::text::php_trim(input.str("server_name").unwrap_or_default()).to_lowercase()
}

fn status_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

struct Submit<'a> {
    state: &'a AppState,
    ctx: &'a AuthCtx,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &'a HeaderMap,
    token_id: i64,
    token: String,
    input: Input,
}

impl Submit<'_> {
    /// `PrivateKey::ownedByCurrentTeam()->findOrFail($this->private_key_id)`.
    async fn key(&self) -> Result<(KeyRow, String, String), ApiError> {
        let id = self
            .input
            .get("private_key_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let key = private_keys::find_by_id(&self.state.db, self.ctx.team_id, id)
            .await?
            .ok_or(NOT_FOUND)?;
        let (public, fingerprint) = key_material(self.state, &key)?;
        Ok((key, public, fingerprint))
    }

    /// `Team::createServerWithinLimit` with the provider columns.
    async fn row(
        &mut self,
        name: &str,
        ip: &str,
        key: &KeyRow,
        columns: Vec<(&'static str, C)>,
    ) -> Result<Created, ApiError> {
        // `Team::createServerWithinLimit` is not given a description.
        let mut all = vec![
            ("cloud_provider_token_id", C::Int(self.token_id)),
            ("description", C::Text(None)),
        ];
        all.extend(columns);
        create_row(
            self.state,
            self.ctx,
            self.peer.take(),
            self.headers,
            name,
            ip,
            key,
            &all,
        )
        .await
    }

    /// `ValidateNewCloudServerJob::dispatch($server)`, then the redirect to the server.
    fn created(&self, c: &Created) -> ApiResult {
        if let Some(jobs) = &self.state.jobs {
            jobs.spawn_validate_new_cloud_server(c.id);
        }
        Ok(ok(json!({"uuid": c.uuid})).into_response())
    }

    /// `ByHetzner::submit`.
    async fn hetzner(mut self) -> ApiResult {
        let mut fields = vec![
            server_name_field(),
            required_string("selected_location"),
            required_int("selected_image"),
            required_string("selected_server_type"),
            required_int("private_key_id"),
            array("selectedHetznerSshKeyIds"),
            array("selectedHetznerFirewallIds"),
            array("selectedHetznerNetworkIds"),
            required_bool("enable_ipv4"),
            required_bool("enable_ipv6"),
            required_bool("enable_backups"),
            field("show_cloud_init_script", vec![Rule::Boolean]),
        ];
        fields.extend(cloud_init_fields());
        let input = &self.input;
        validate_form(self.state, self.ctx, input, &fields, |errors| {
            for f in [
                "selectedHetznerSshKeyIds",
                "selectedHetznerFirewallIds",
                "selectedHetznerNetworkIds",
            ] {
                integer_items(input, f, errors);
            }
        })
        .await?;
        // `isServerTypeOutOfStock()` of the selected type, from the types the form loaded.
        let selected_type = input.str("selected_server_type").unwrap_or_default();
        let selected_location = input.str("selected_location").unwrap_or_default();
        if let Ok(types) = Client::new(Provider::Hetzner, &self.token)
            .hetzner_server_types(None, true)
            .await
            && types.iter().any(|t| {
                t.get("name").and_then(Value::as_str) == Some(selected_type)
                    && server_type_out_of_stock(t, selected_location)
            })
        {
            return Err(validation_errors(&[(
                "selected_server_type",
                "This server type is out of stock in the selected location. Select a different server type or location.",
            )]));
        }
        let v4 = flag(input, "enable_ipv4");
        let v6 = flag(input, "enable_ipv6");
        if !v4 && !v6 {
            return Err(validation_errors(&[
                ("enable_ipv4", "Enable at least one public IP protocol."),
                ("enable_ipv6", "Enable at least one public IP protocol."),
            ]));
        }
        if !self.ctx.is_team_admin() {
            return toast_error(UNAUTHORIZED);
        }
        save_script(self.state, self.ctx, input).await?;
        let (key, public, fingerprint) = self.key().await?;
        let client = Client::new(Provider::Hetzner, &self.token);
        let created = match self
            .create_hetzner(&client, &key, &public, &fingerprint)
            .await
        {
            Ok(c) => c,
            // `handleError($e)`: the exception's message.
            Err(e) => return toast_error(e.to_string()),
        };
        let v4_ip = created
            .pointer("/public_net/ipv4/ip")
            .and_then(Value::as_str);
        let v6_ip = created
            .pointer("/public_net/ipv6/ip")
            .and_then(Value::as_str);
        let ip = if v4 && v4_ip.is_some() {
            v4_ip.map(str::to_string)
        } else if v6 && v6_ip.is_some() {
            cloud::hetzner_server_ipv6(v6_ip)
        } else {
            None
        };
        let server_id = created
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        // Hetzner keeps `$this->server_name` as typed; the other three store it normalized.
        let name = self
            .input
            .str("server_name")
            .unwrap_or_default()
            .to_string();
        let c = match self
            .row(
                &name,
                ip.as_deref().unwrap_or(PLACEHOLDER_IP),
                &key,
                vec![
                    ("hetzner_server_id", C::Int(server_id)),
                    (
                        "hetzner_server_status",
                        C::Text(status_of(&created, "status")),
                    ),
                ],
            )
            .await
        {
            Ok(c) => c,
            Err(e) => {
                // `deleteUntrackedHetznerServer`.
                if server_id != 0
                    && let Err(d) = client.hetzner_delete_server(server_id).await
                {
                    tracing::error!(error = %d, "deleting the untracked Hetzner server failed");
                }
                return Err(e);
            }
        };
        if flag(&self.input, "enable_backups")
            && let Err(e) = client.hetzner_enable_backup(server_id).await
        {
            tracing::warn!(error = %e, "enabling Hetzner backups failed");
        }
        self.created(&c)
    }

    /// `createHetznerServer`.
    async fn create_hetzner(
        &self,
        client: &Client,
        key: &KeyRow,
        public: &str,
        fingerprint: &str,
    ) -> Result<Value, CloudError> {
        let input = &self.input;
        let existing = client
            .hetzner_ssh_keys()
            .await?
            .iter()
            .find(|k| k.get("fingerprint").and_then(Value::as_str) == Some(fingerprint))
            .and_then(|k| k.get("id").cloned());
        let ssh_key = match existing {
            Some(id) => id,
            None => client
                .hetzner_upload_ssh_key(&key.name, public)
                .await?
                .get("id")
                .cloned()
                .unwrap_or(Value::Null),
        };
        let mut keys = vec![ssh_key];
        keys.extend(list(input, "selectedHetznerSshKeyIds"));
        let mut params = Map::new();
        params.insert("name".into(), json!(normalized_name(input)));
        params.insert(
            "server_type".into(),
            input
                .get("selected_server_type")
                .cloned()
                .unwrap_or(Value::Null),
        );
        params.insert(
            "image".into(),
            input.get("selected_image").cloned().unwrap_or(Value::Null),
        );
        params.insert(
            "location".into(),
            input
                .get("selected_location")
                .cloned()
                .unwrap_or(Value::Null),
        );
        params.insert("start_after_create".into(), json!(true));
        params.insert("ssh_keys".into(), Value::Array(unique(keys)));
        params.insert(
            "public_net".into(),
            json!({
                "enable_ipv4": flag(input, "enable_ipv4"),
                "enable_ipv6": flag(input, "enable_ipv6"),
            }),
        );
        let firewalls = unique(list(input, "selectedHetznerFirewallIds"));
        if !firewalls.is_empty() {
            params.insert(
                "firewalls".into(),
                Value::Array(
                    firewalls
                        .into_iter()
                        .map(|f| json!({"firewall": f}))
                        .collect(),
                ),
            );
        }
        let networks = unique(list(input, "selectedHetznerNetworkIds"));
        if !networks.is_empty() {
            params.insert("networks".into(), Value::Array(networks));
        }
        user_data(input, &mut params);
        client.hetzner_create_server(&Value::Object(params)).await
    }

    /// `ByVultr::submit`.
    async fn vultr(mut self) -> ApiResult {
        let mut fields = vec![
            server_name_field(),
            required_string("selected_region"),
            required_string("selected_plan"),
            required_int("selected_os_id"),
            required_int("private_key_id"),
            array("selectedVultrSshKeyIds"),
            required_bool("enable_ipv6"),
            required_bool("disable_public_ipv4"),
        ];
        fields.extend(cloud_init_fields());
        let input = &self.input;
        validate_form(self.state, self.ctx, input, &fields, |errors| {
            string_items(input, "selectedVultrSshKeyIds", errors);
        })
        .await?;
        let v6 = flag(input, "enable_ipv6");
        let no_v4 = flag(input, "disable_public_ipv4");
        if no_v4 && !v6 {
            return Err(validation_errors(&[(
                "enable_ipv6",
                "Enable IPv6 when disabling public IPv4.",
            )]));
        }
        if !self.ctx.is_team_admin() {
            return toast_error(UNAUTHORIZED);
        }
        save_script(self.state, self.ctx, input).await?;
        let (key, public, _) = self.key().await?;
        let client = Client::new(Provider::Vultr, &self.token);
        let instance = match self.create_vultr(&client, &key, &public).await {
            Ok(i) => i,
            Err(e) => return toast_error(e.to_string()),
        };
        let instance_id = instance.get("id").map(as_php_string).unwrap_or_default();
        let ip = Client::vultr_public_ip(&instance, no_v4, v6)
            .unwrap_or_else(|| PLACEHOLDER_IP.to_string());
        let status = status_of(&instance, "status");
        let name = normalized_name(&self.input);
        let created = self
            .row(
                &name,
                &ip,
                &key,
                vec![
                    ("vultr_instance_id", C::Text(Some(instance_id.clone()))),
                    ("vultr_instance_status", C::Text(status.clone())),
                ],
            )
            .await;
        let c = match created {
            Ok(c) => c,
            Err(e) => {
                // `deleteUntrackedInstance`.
                let _ = client.vultr_delete_instance(&instance_id).await;
                return Err(e);
            }
        };
        // Non-fatal: the server page polling backfills the IP later.
        if let Ok(i) = client.vultr_wait_for_ip(instance, no_v4, v6).await
            && let Some(assigned) = Client::vultr_public_ip(&i, no_v4, v6).filter(|a| *a != ip)
        {
            servers::update_columns(
                &self.state.db,
                c.id,
                &[
                    ("ip", C::Text(Some(assigned))),
                    // `Server::saving`: the replaced address.
                    ("ip_previous", C::Text(Some(ip.clone()))),
                    (
                        "vultr_instance_status",
                        C::Text(status_of(&i, "status").or(status)),
                    ),
                ],
                self.state.now(),
            )
            .await?;
        }
        self.created(&c)
    }

    /// `createVultrServer`.
    async fn create_vultr(
        &self,
        client: &Client,
        key: &KeyRow,
        public: &str,
    ) -> Result<Value, CloudError> {
        let input = &self.input;
        // `normalizePublicKey`: the type and the key, without the comment.
        let normalize = |k: &str| k.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        let wanted = normalize(public);
        let existing = client
            .vultr_ssh_keys()
            .await?
            .iter()
            .find(|k| {
                normalize(k.get("ssh_key").and_then(Value::as_str).unwrap_or_default()) == wanted
            })
            .and_then(|k| k.get("id").cloned());
        let ssh_key = match existing {
            Some(id) => id,
            None => client
                .vultr_upload_ssh_key(&key.name, public)
                .await?
                .get("id")
                .cloned()
                .unwrap_or(Value::Null),
        };
        let mut keys = vec![ssh_key];
        keys.extend(list(input, "selectedVultrSshKeyIds"));
        let name = normalized_name(input);
        let mut params = Map::new();
        params.insert(
            "region".into(),
            input.get("selected_region").cloned().unwrap_or(Value::Null),
        );
        params.insert(
            "plan".into(),
            input.get("selected_plan").cloned().unwrap_or(Value::Null),
        );
        params.insert(
            "os_id".into(),
            input.get("selected_os_id").cloned().unwrap_or(Value::Null),
        );
        params.insert("label".into(), json!(name));
        params.insert("hostname".into(), json!(name));
        params.insert("sshkey_id".into(), Value::Array(unique(keys)));
        params.insert("enable_ipv6".into(), json!(flag(input, "enable_ipv6")));
        params.insert(
            "disable_public_ipv4".into(),
            json!(flag(input, "disable_public_ipv4")),
        );
        user_data(input, &mut params);
        client.vultr_create_instance(&Value::Object(params)).await
    }

    /// `ByDigitalOcean::submit`.
    async fn digitalocean(mut self) -> ApiResult {
        let mut fields = vec![
            server_name_field(),
            required_string("selected_region"),
            field("selected_image", vec![Rule::Required]),
            required_string("selected_size"),
            required_int("private_key_id"),
            array("selectedDigitalOceanSshKeyIds"),
            required_bool("enable_ipv6"),
            required_bool("monitoring"),
            field("show_cloud_init_script", vec![Rule::Boolean]),
        ];
        fields.extend(cloud_init_fields());
        let input = &self.input;
        validate_form(self.state, self.ctx, input, &fields, |errors| {
            integer_items(input, "selectedDigitalOceanSshKeyIds", errors);
        })
        .await?;
        let v6 = flag(input, "enable_ipv6");
        if !self.ctx.is_team_admin() {
            return toast_error(UNAUTHORIZED);
        }
        save_script(self.state, self.ctx, input).await?;
        let (key, public, fingerprint) = self.key().await?;
        let client = Client::new(Provider::DigitalOcean, &self.token);
        let droplet = match self
            .create_digitalocean(&client, &key, &public, &fingerprint)
            .await
        {
            Ok(d) => d,
            Err(e) => return toast_error(e.to_string()),
        };
        let droplet_id = droplet
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let status = status_of(&droplet, "status");
        let name = normalized_name(&self.input);
        let created = self
            .row(
                &name,
                PLACEHOLDER_IP,
                &key,
                vec![
                    ("digitalocean_droplet_id", C::Int(droplet_id)),
                    ("digitalocean_droplet_status", C::Text(status.clone())),
                ],
            )
            .await;
        let c = match created {
            Ok(c) => c,
            Err(e) => {
                // `deleteUntrackedDroplet`.
                let _ = client.do_delete_droplet(droplet_id).await;
                return Err(e);
            }
        };
        // Non-fatal: the server page polling backfills the IP later.
        if let Ok(d) = client.do_wait_for_ip(droplet, v6).await
            && let Some(ip) = Client::do_public_ip(&d, true, v6)
        {
            servers::update_columns(
                &self.state.db,
                c.id,
                &[
                    ("ip", C::Text(Some(ip))),
                    // `Server::saving`: the replaced address.
                    ("ip_previous", C::Text(Some(PLACEHOLDER_IP.to_string()))),
                    (
                        "digitalocean_droplet_status",
                        C::Text(status_of(&d, "status").or(status)),
                    ),
                ],
                self.state.now(),
            )
            .await?;
        }
        self.created(&c)
    }

    /// `createDigitalOceanDroplet`.
    async fn create_digitalocean(
        &self,
        client: &Client,
        key: &KeyRow,
        public: &str,
        fingerprint: &str,
    ) -> Result<Value, CloudError> {
        let input = &self.input;
        let existing = client
            .do_ssh_keys()
            .await?
            .iter()
            .find(|k| k.get("fingerprint").and_then(Value::as_str) == Some(fingerprint))
            .and_then(|k| k.get("id").cloned());
        let ssh_key = match existing {
            Some(id) => id,
            None => client
                .do_upload_ssh_key(&key.name, public)
                .await?
                .get("id")
                .cloned()
                .unwrap_or(Value::Null),
        };
        let mut keys = vec![ssh_key];
        keys.extend(list(input, "selectedDigitalOceanSshKeyIds"));
        let mut params = Map::new();
        params.insert("name".into(), json!(normalized_name(input)));
        params.insert(
            "region".into(),
            input.get("selected_region").cloned().unwrap_or(Value::Null),
        );
        params.insert(
            "size".into(),
            input.get("selected_size").cloned().unwrap_or(Value::Null),
        );
        params.insert(
            "image".into(),
            input.get("selected_image").cloned().unwrap_or(Value::Null),
        );
        params.insert("ssh_keys".into(), Value::Array(unique(keys)));
        params.insert("ipv6".into(), json!(flag(input, "enable_ipv6")));
        params.insert("monitoring".into(), json!(flag(input, "monitoring")));
        user_data(input, &mut params);
        client.do_create_droplet(&Value::Object(params)).await
    }

    /// `ByHostinger::providerSelectionsAreValid`.
    async fn hostinger_selection_errors(
        &self,
        client: &Client,
    ) -> Result<Vec<(&'static str, &'static str)>, CloudError> {
        let input = &self.input;
        let ids = |v: Value| -> Vec<Value> {
            as_list(v)
                .iter()
                .filter_map(|i| i.get("id").cloned())
                .collect()
        };
        let ints = |v: Value| -> Vec<i64> {
            ids(v)
                .iter()
                .map(crate::routes::servers::int_value)
                .collect()
        };
        let mut price_ids: Vec<String> = Vec::new();
        for item in as_list(client.hostinger_catalog().await?) {
            for price in item
                .get("prices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(id) = price.get("id") {
                    price_ids.push(as_php_string(id));
                }
            }
        }
        let data_centers = ints(client.hostinger_data_centers().await?);
        let templates = ints(client.hostinger_templates().await?);
        let selected_keys = list(input, "selected_public_key_ids");
        let public_keys = if selected_keys.is_empty() {
            Vec::new()
        } else {
            ints(client.hostinger_public_keys().await?)
        };
        let script = input
            .get("selected_post_install_script_id")
            .and_then(Value::as_i64)
            .filter(|i| *i != 0);
        let scripts = if script.is_some() {
            ints(client.hostinger_post_install_scripts().await?)
        } else {
            Vec::new()
        };
        let int_of = |k: &str| input.get(k).and_then(Value::as_i64).unwrap_or_default();
        let mut errors = Vec::new();
        if !price_ids
            .iter()
            .any(|p| Some(p.as_str()) == input.str("selected_price_id"))
        {
            errors.push((
                "selected_price_id",
                "The selected Hostinger plan or billing period is no longer available.",
            ));
        }
        if !data_centers.contains(&int_of("selected_data_center_id")) {
            errors.push((
                "selected_data_center_id",
                "The selected Hostinger data center is no longer available.",
            ));
        }
        if !templates.contains(&int_of("selected_template_id")) {
            errors.push((
                "selected_template_id",
                "The selected Hostinger operating system is no longer available.",
            ));
        }
        if selected_keys
            .iter()
            .any(|k| !public_keys.contains(&crate::routes::servers::int_value(k)))
        {
            errors.push((
                "selected_public_key_ids",
                "One or more selected Hostinger SSH keys are no longer available.",
            ));
        }
        if script.is_some_and(|s| !scripts.contains(&s)) {
            errors.push((
                "selected_post_install_script_id",
                "The selected Hostinger post-install script is no longer available.",
            ));
        }
        Ok(errors)
    }

    /// `ByHostinger::submit`.
    async fn hostinger(mut self) -> ApiResult {
        let fields = vec![
            server_name_field(),
            required_int("selected_data_center_id"),
            required_int("selected_template_id"),
            required_string("selected_price_id"),
            required_int("private_key_id"),
            required_bool("enable_backups"),
            field("selected_public_key_ids", vec![Rule::Array]),
            field(
                "selected_post_install_script_id",
                vec![Rule::Nullable, Rule::Integer],
            ),
        ];
        let input = &self.input;
        validate_form(self.state, self.ctx, input, &fields, |errors| {
            integer_items(input, "selected_public_key_ids", errors);
        })
        .await?;
        if !self.ctx.is_team_admin() {
            return toast_error(UNAUTHORIZED);
        }
        let (key, public, _) = self.key().await?;
        let client = Client::new(Provider::Hostinger, &self.token);
        match self.hostinger_selection_errors(&client).await {
            Ok(errors) if errors.is_empty() => {}
            Ok(errors) => return Err(validation_errors(&errors)),
            Err(e) => return toast_error(e.to_string()),
        }
        let input = &self.input;
        let name = normalized_name(input);
        let mut setup = Map::new();
        setup.insert(
            "data_center_id".into(),
            input
                .get("selected_data_center_id")
                .cloned()
                .unwrap_or(Value::Null),
        );
        setup.insert(
            "template_id".into(),
            input
                .get("selected_template_id")
                .cloned()
                .unwrap_or(Value::Null),
        );
        setup.insert("hostname".into(), json!(name));
        setup.insert(
            "enable_backups".into(),
            json!(flag(input, "enable_backups")),
        );
        setup.insert(
            "public_key".into(),
            json!({"name": key.name, "key": public}),
        );
        if flag(input, "selected_post_install_script_id")
            && let Some(id) = input.get("selected_post_install_script_id")
        {
            setup.insert("post_install_script_id".into(), id.clone());
        }
        let vm = match client
            .hostinger_purchase(&json!({
                "item_id": input.get("selected_price_id").cloned().unwrap_or(Value::Null),
                "setup": Value::Object(setup),
            }))
            .await
        {
            Ok(v) => v,
            Err(e) => return toast_error(e.to_string()),
        };
        let vm_id = vm.get("id").and_then(Value::as_i64).unwrap_or_default();
        let status = status_of(&vm, "state");
        let public_keys: Vec<i64> = list(input, "selected_public_key_ids")
            .iter()
            .map(crate::routes::servers::int_value)
            .collect();
        let created = self
            .row(
                &name,
                PLACEHOLDER_IP,
                &key,
                vec![
                    ("hostinger_virtual_machine_id", C::Int(vm_id)),
                    ("hostinger_virtual_machine_status", C::Text(status.clone())),
                ],
            )
            .await;
        let Ok(c) = created else {
            tracing::warn!(
                team_id = self.ctx.team_id,
                hostinger_virtual_machine_id = vm_id,
                "Hostinger VPS was purchased but could not be saved in Coolify"
            );
            return toast_error(format!(
                "Hostinger VPS {vm_id} was purchased but could not be saved in Coolify. Manage it in hPanel."
            ));
        };
        if !public_keys.is_empty()
            && let Err(e) = client.hostinger_attach_keys(vm_id, &public_keys).await
        {
            tracing::warn!(error = %e, "attaching Hostinger public keys failed");
        }
        if let Ok(v) = client.hostinger_wait_for_ip(vm).await
            && let Some(ip) = Client::hostinger_public_ip(&v)
        {
            servers::update_columns(
                &self.state.db,
                c.id,
                &[
                    ("ip", C::Text(Some(ip))),
                    // `Server::saving`: the replaced address.
                    ("ip_previous", C::Text(Some(PLACEHOLDER_IP.to_string()))),
                    (
                        "hostinger_virtual_machine_status",
                        C::Text(status_of(&v, "state").or(status)),
                    ),
                ],
                self.state.now(),
            )
            .await?;
        }
        self.created(&c)
    }
}

async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Path((provider, token)): Path<(String, String)>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let p = parse_provider(&provider)?;
    let Some((token_id, plain)) = selected_token(&state, &ctx, &provider, &token).await? else {
        let label = match p {
            Provider::Hetzner => "Hetzner",
            Provider::Vultr => "Vultr",
            Provider::DigitalOcean => "DigitalOcean",
            Provider::Hostinger => "Hostinger",
        };
        return toast_error(format!("Please select a {label} token."));
    };
    let form = Submit {
        state: &state,
        ctx: &ctx,
        peer,
        headers: &headers,
        token_id,
        token: plain,
        input: normalized(body),
    };
    match p {
        Provider::Hetzner => form.hetzner().await,
        Provider::Vultr => form.vultr().await,
        Provider::DigitalOcean => form.digitalocean().await,
        Provider::Hostinger => form.hostinger().await,
    }
}

// ── CloudProviderTokenForm ──────────────────────────────────────────────

const TOKEN_MESSAGES: crate::validation::Messages = &[
    ("provider.required", "Please select a cloud provider."),
    ("provider.in", "Invalid cloud provider selected."),
    ("token.required", "API token is required."),
    ("name.required", "Token name is required."),
];

/// `CloudProviderTokenForm::addToken`.
async fn add_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = normalized(body);
    let allowed: Vec<&str> = input.keys().map(String::as_str).collect();
    validate(
        &input,
        &[
            field(
                "provider",
                vec![
                    Rule::Required,
                    Rule::String,
                    Rule::In(&["hetzner", "digitalocean", "vultr", "hostinger"]),
                ],
            ),
            required_string("token"),
            field("name", vec![Rule::Required, Rule::String, Rule::Max(255)]),
            field(
                "description",
                vec![Rule::Nullable, Rule::String, Rule::Max(1000)],
            ),
        ],
        TOKEN_MESSAGES,
        &allowed,
    )?;
    let provider = input.str("provider").unwrap_or_default();
    let p = parse_provider(provider)?;
    let token = input.str("token").unwrap_or_default();
    // `validateToken`: DigitalOcean and Hostinger ask for JSON; an exception counts as invalid.
    let accept_json = matches!(p, Provider::DigitalOcean | Provider::Hostinger);
    if cloud::probe(p, token, p.validation_endpoint(), accept_json).await != Ok(true) {
        return toast_error("Invalid API token. Please check your token and try again.");
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = new_public_id();
    let name = input.str("name").unwrap_or_default();
    let id = db::create_token(
        &state.db,
        ctx.team_id,
        &uuid,
        provider,
        &enc.encrypt_string(token),
        name,
        state.now(),
    )
    .await?;
    let description = coolify_remote::text::php_trim(input.str("description").unwrap_or_default());
    if !description.is_empty() {
        db::set_token_description(&state.db, id, description).await?;
    }
    tracing::info!(target: "audit", event = "ui.cloud_token.created", team_id = ctx.team_id, cloud_token_uuid = %uuid, cloud_token_name = %name, provider = %provider, "cloud token created");
    Ok(ok(json!({
        "id": id,
        "uuid": uuid,
        "message": "Cloud provider token added successfully.",
    }))
    .into_response())
}

// ── Server\CloudProviderToken\Show ──────────────────────────────────────

fn filled(s: &ServerRow, k: &str) -> bool {
    s.record.get(k).is_some_and(php_truthy)
}

/// `loadTokens`: which provider the server belongs to.
fn server_provider(s: &ServerRow) -> (&'static str, &'static str, Provider) {
    if filled(s, "vultr_instance_id") {
        ("vultr", "Vultr", Provider::Vultr)
    } else if filled(s, "digitalocean_droplet_id") {
        ("digitalocean", "DigitalOcean", Provider::DigitalOcean)
    } else if filled(s, "hostinger_virtual_machine_id") {
        ("hostinger", "Hostinger", Provider::Hostinger)
    } else {
        ("hetzner", "Hetzner", Provider::Hetzner)
    }
}

fn current_token(s: &ServerRow) -> Option<i64> {
    s.record
        .get("cloud_provider_token_id")
        .and_then(Value::as_i64)
}

/// `Server\CloudProviderToken\Show::mount`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let (provider, label, _) = server_provider(&s);
    let tokens = team_tokens(&state, ctx.team_id, provider).await?;
    let managed = filled(&s, "hetzner_server_id")
        || filled(&s, "vultr_instance_id")
        || filled(&s, "digitalocean_droplet_id")
        || filled(&s, "hostinger_virtual_machine_id");
    Ok(ok(json!({
        "managed": managed,
        "provider": provider,
        "provider_name": label,
        "current_token_id": current_token(&s),
        "tokens": tokens.iter().map(token_summary).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `validateTokenForServer`: the error, if the token cannot manage this server.
async fn token_error(s: &ServerRow, provider: &str, token: &str) -> Option<String> {
    let failed = |e: String| Some(format!("Failed to validate token: {e}"));
    let p = Provider::from_name(provider)?;
    match cloud::probe(p, token, p.validation_endpoint(), false).await {
        Ok(true) => {}
        Ok(false) => {
            return Some("This token is invalid or has insufficient permissions.".to_string());
        }
        Err(e) => return failed(e),
    }
    let id = |k: &str| s.record.get(k).filter(|v| php_truthy(v)).map(as_php_string);
    let checks = [
        (
            id("hetzner_server_id").map(|i| format!("/servers/{i}")),
            Provider::Hetzner,
            false,
            "This token cannot access this server. It may belong to a different Hetzner project.",
        ),
        (
            id("vultr_instance_id").map(|i| format!("/instances/{i}")),
            Provider::Vultr,
            false,
            "This token cannot access this instance. It may belong to a different Vultr account.",
        ),
        (
            id("digitalocean_droplet_id").map(|i| format!("/droplets/{i}")),
            Provider::DigitalOcean,
            false,
            "This token cannot access this droplet. It may belong to a different DigitalOcean account.",
        ),
        (
            id("hostinger_virtual_machine_id").map(|i| format!("/api/vps/v1/virtual-machines/{i}")),
            Provider::Hostinger,
            true,
            "This token cannot access this VPS. It may belong to a different Hostinger account.",
        ),
    ];
    for (endpoint, target, accept_json, message) in checks {
        let Some(endpoint) = endpoint else { continue };
        match cloud::probe(target, token, &endpoint, accept_json).await {
            Ok(true) => {}
            Ok(false) => return Some(message.to_string()),
            Err(e) => return failed(e),
        }
    }
    None
}

/// `Server\CloudProviderToken\Show::setCloudProviderToken`.
async fn set_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let wanted = body.get("token_id").and_then(Value::as_i64);
    let token = db::list_tokens(&state.db, ctx.team_id)
        .await?
        .into_iter()
        .find(|r| r.get("id").and_then(Value::as_i64) == wanted);
    let Some(token) = token else {
        return toast_error("You are not allowed to use this token.");
    };
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    let provider = token
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plain = decrypt(&state, token.get("token").and_then(Value::as_str))?;
    if let Some(e) = token_error(&s, provider, &plain).await {
        return toast_error(e);
    }
    let id = token.get("id").and_then(Value::as_i64).unwrap_or_default();
    servers::update_columns(
        &state.db,
        s.id,
        &[("cloud_provider_token_id", C::Int(id))],
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "ui.server.cloud_token_assigned", team_id = ctx.team_id, server_uuid = %uuid, cloud_token_id = id, provider = %provider, "cloud token assigned");
    let (_, label, _) = server_provider(&s);
    Ok(ok(json!({"message": format!("{label} token updated successfully.")})).into_response())
}

/// `Server\CloudProviderToken\Show::validateToken`.
async fn validate_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let (_, label, _) = server_provider(&s);
    let token = match current_token(&s) {
        Some(id) => db::find_token_by_id(&state.db, id).await?,
        None => None,
    };
    let Some(token) = token else {
        return toast_error(format!("No {label} token is associated with this server."));
    };
    let provider = token
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let p = parse_provider(provider)?;
    let plain = decrypt(&state, token.get("token").and_then(Value::as_str))?;
    let valid = match cloud::probe(p, &plain, p.validation_endpoint(), false).await {
        Ok(v) => v,
        Err(e) => return toast_error(e),
    };
    tracing::info!(target: "audit", event = "ui.server.cloud_token_validated", team_id = ctx.team_id, server_uuid = %uuid, provider = %provider, valid, "cloud token validated");
    if valid {
        Ok(ok(json!({"message": format!("{label} token is valid and working.")})).into_response())
    } else {
        toast_error(format!(
            "{label} token is invalid or has insufficient permissions."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_sorting_is_numeric_for_numbers_and_bytewise_for_text() {
        let items = vec![
            json!({"monthly_cost": 10}),
            json!({"monthly_cost": "2.5"}),
            json!({"monthly_cost": 5}),
        ];
        let sorted = sort_by(items, "monthly_cost");
        assert_eq!(sorted[0]["monthly_cost"], json!("2.5"));
        assert_eq!(sorted[2]["monthly_cost"], json!(10));
        let names = sort_by(
            vec![json!({"name": "b"}), json!({}), json!({"name": "a"})],
            "name",
        );
        assert_eq!(names[0], json!({}));
        assert_eq!(names[1]["name"], json!("a"));
    }

    #[test]
    fn vultr_and_digitalocean_add_the_prefix_once() {
        let e = CloudError::Api("Vultr API error: nope".to_string(), 401);
        assert_eq!(prefixed_error("Vultr", &e), "Vultr API error: nope");
        let e = CloudError::RateLimit(None);
        assert_eq!(
            prefixed_error("DigitalOcean", &e),
            "DigitalOcean API error: Rate limit exceeded. Please try again later."
        );
    }

    #[test]
    fn blank_strings_are_null() {
        let mut m = Map::new();
        m.insert("a".into(), json!("  "));
        m.insert("b".into(), json!(" x "));
        let i = normalized(m);
        assert_eq!(i.get("a"), Some(&Value::Null));
        assert_eq!(i.get("b"), Some(&json!(" x ")));
    }
}
