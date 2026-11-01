//! The "Link provider" menu of `Server\Show`: `load*Tokens`, `search*`, `search*ById`, `linkTo*`.
use std::net::{IpAddr, Ipv6Addr};

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coolify_db::cloud as db;
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue as C, ServerRow};
use coolify_jobs::cloud::{Client, CloudError, Provider};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::find_server;
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;

const UNAUTHORIZED: &str = "This action is unauthorized.";
/// `Server::assertNotTransferredAway()`
const TRANSFERRED: &str = "This server is managed by another coolify-rs instance.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(&format!("{SERVER_PAGE}/provider-links"), get(show))
        .route(
            &format!("{SERVER_PAGE}/provider-links/{{provider}}/search"),
            post(search),
        )
        .route(
            &format!("{SERVER_PAGE}/provider-links/{{provider}}"),
            post(link),
        )
}

/// What differs between the four providers in `Server\Show`.
struct Spec {
    provider: Provider,
    name: &'static str,
    label: &'static str,
    /// `search*ById` without an id: "Please enter a {this}."
    id_label: &'static str,
    /// "Failed to search {this}: ", "No {link_noun} selected."
    plural: &'static str,
    /// "Failed to fetch {this}: ", "Could not find {this} with ID: "
    singular: &'static str,
    success: &'static str,
    id_column: &'static str,
    status_column: &'static str,
    status_key: &'static str,
    /// `linkToDigitalOcean` and `linkToHostinger` also store the public address.
    updates_ip: bool,
}

const SPECS: [Spec; 4] = [
    Spec {
        provider: Provider::Hetzner,
        name: "hetzner",
        label: "Hetzner",
        id_label: "Hetzner Server ID",
        plural: "Hetzner servers",
        singular: "Hetzner server",
        success: "Server successfully linked to Hetzner Cloud!",
        id_column: "hetzner_server_id",
        status_column: "hetzner_server_status",
        status_key: "status",
        updates_ip: false,
    },
    Spec {
        provider: Provider::DigitalOcean,
        name: "digitalocean",
        label: "DigitalOcean",
        id_label: "DigitalOcean Droplet ID",
        plural: "DigitalOcean droplets",
        singular: "DigitalOcean droplet",
        success: "Server successfully linked to DigitalOcean!",
        id_column: "digitalocean_droplet_id",
        status_column: "digitalocean_droplet_status",
        status_key: "status",
        updates_ip: true,
    },
    Spec {
        provider: Provider::Vultr,
        name: "vultr",
        label: "Vultr",
        id_label: "Vultr Instance ID",
        plural: "Vultr instances",
        singular: "Vultr instance",
        success: "Server successfully linked to Vultr!",
        id_column: "vultr_instance_id",
        status_column: "vultr_instance_status",
        status_key: "status",
        updates_ip: false,
    },
    Spec {
        provider: Provider::Hostinger,
        name: "hostinger",
        label: "Hostinger",
        id_label: "Hostinger Virtual Machine ID",
        plural: "Hostinger virtual machines",
        singular: "Hostinger virtual machine",
        success: "Server successfully linked to Hostinger!",
        id_column: "hostinger_virtual_machine_id",
        status_column: "hostinger_virtual_machine_status",
        status_key: "state",
        updates_ip: true,
    },
];

fn spec(name: &str) -> Result<&'static Spec, ApiError> {
    SPECS
        .iter()
        .find(|s| s.name == name)
        .ok_or(ApiError::Abort(StatusCode::NOT_FOUND, "Not found."))
}

fn linked(s: &ServerRow, spec: &Spec) -> bool {
    s.record.get(spec.id_column).is_some_and(php_truthy)
}

/// `$hetznerMatch`, `$digitalOceanMatch`, `$vultrMatch` and `$hostingerMatch` of show.blade.php.
fn matched_summary(spec: &Spec, m: &Value) -> Value {
    let text = |k: &str| m.get(k).filter(|v| !v.is_null()).cloned();
    let name = match spec.name {
        "hetzner" => text("name").unwrap_or_else(|| json!("Hetzner server")),
        "digitalocean" => text("name").unwrap_or_else(|| json!("DigitalOcean Droplet")),
        "vultr" => text("label")
            .or_else(|| text("hostname"))
            .unwrap_or_else(|| json!("Vultr instance")),
        _ => text("hostname").unwrap_or_else(|| json!("Hostinger VPS")),
    };
    json!({"name": name, "id": m.get("id"), "status": m.get(spec.status_key)})
}

/// `normalizeIpAddress()`
fn normalize_ip(ip: &str) -> String {
    let bare = ip.trim().trim_start_matches('[').trim_end_matches(']');
    bare.parse::<IpAddr>()
        .map_or_else(|_| bare.to_string(), |a| a.to_string())
}

/// `ipMatch()` for one IPv6 network.
fn in_v6_network(ip: &str, network: &str) -> bool {
    let Some((net, bits)) = network.split_once('/') else {
        return false;
    };
    let (Ok(addr), Ok(net), Ok(bits)) = (
        ip.parse::<Ipv6Addr>(),
        net.parse::<Ipv6Addr>(),
        bits.parse::<u32>(),
    ) else {
        return false;
    };
    if bits > 128 {
        return false;
    }
    let mask = if bits == 0 {
        0
    } else {
        u128::MAX << (128 - bits)
    };
    u128::from(addr) & mask == u128::from(net) & mask
}

/// `findServerByIp`, `findDropletByIp`, `findInstanceByIp`, `findVirtualMachineByIp`.
fn find_by_ip(spec: &Spec, items: Vec<Value>, ip: &str) -> Option<Value> {
    let pointer = |v: &Value, p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    items.into_iter().find(|item| match spec.name {
        "hetzner" => {
            let wanted = normalize_ip(ip);
            pointer(item, "/public_net/ipv4/ip").is_some_and(|a| normalize_ip(&a) == wanted)
                || pointer(item, "/public_net/ipv6/ip").is_some_and(|n| in_v6_network(&wanted, &n))
        }
        "digitalocean" => ["v4", "v6"].iter().any(|version| {
            item.pointer(&format!("/networks/{version}"))
                .and_then(Value::as_array)
                .is_some_and(|nets| {
                    nets.iter()
                        .any(|n| n.get("ip_address").and_then(Value::as_str) == Some(ip))
                })
        }),
        "vultr" => {
            item.get("main_ip").and_then(Value::as_str) == Some(ip)
                || item.get("v6_main_ip").and_then(Value::as_str) == Some(ip)
        }
        _ => ["ipv4", "ipv6"].iter().any(|version| {
            item.get(*version)
                .and_then(Value::as_array)
                .is_some_and(|ips| {
                    ips.iter()
                        .any(|a| a.get("address").and_then(Value::as_str) == Some(ip))
                })
        }),
    })
}

async fn list(spec: &Spec, c: &Client) -> Result<Vec<Value>, CloudError> {
    match spec.name {
        "hetzner" => c.hetzner_servers().await,
        "digitalocean" => c.do_droplets().await,
        "vultr" => c.vultr_instances().await,
        _ => c.hostinger_vms().await,
    }
}

/// `(int) $id` for the three providers with numeric ids.
fn php_int(id: &str) -> i64 {
    id.trim().parse().unwrap_or(0)
}

async fn fetch(spec: &Spec, c: &Client, id: &str) -> Result<Value, CloudError> {
    match spec.name {
        "hetzner" => c.hetzner_get_server(php_int(id)).await,
        "digitalocean" => c.do_get_droplet(php_int(id)).await,
        "vultr" => c.vultr_get_instance(id).await,
        _ => c.hostinger_get_vm(php_int(id)).await,
    }
}

/// PHP's `empty()` on a decoded response.
fn is_empty(v: &Value) -> bool {
    !php_truthy(v) || v.as_object().is_some_and(|o| o.is_empty())
}

fn plain_token(state: &AppState, row: &Record) -> Result<String, ApiError> {
    crate::ui_server_cloud::decrypt(state, row.get("token").and_then(Value::as_str))
}

/// `availableHetznerTokens->firstWhere('id', ...)`
async fn token_in_list(
    state: &AppState,
    ctx: &AuthCtx,
    spec: &Spec,
    id: i64,
) -> Result<Option<Record>, ApiError> {
    let rows = db::list_tokens(&state.db, ctx.team_id).await?;
    Ok(rows.into_iter().find(|r| {
        r.get("provider").and_then(Value::as_str) == Some(spec.name)
            && r.get("id").and_then(Value::as_i64) == Some(id)
    }))
}

/// What the menu needs: `load*Tokens` and `$hasLinkableCloudProviders`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let rows = db::list_tokens(&state.db, ctx.team_id).await?;
    let mut providers = Vec::new();
    for spec in &SPECS {
        if linked(&s, spec) {
            continue;
        }
        let tokens: Vec<Value> = rows
            .iter()
            .filter(|r| r.get("provider").and_then(Value::as_str) == Some(spec.name))
            .map(|r| json!({"value": r.get("id").map(crate::routes::servers::as_php_string), "label": r.get("name")}))
            .collect();
        if !tokens.is_empty() {
            providers.push(json!({"provider": spec.name, "label": spec.label, "tokens": tokens}));
        }
    }
    Ok(ok(json!({"providers": providers})).into_response())
}

/// `searchHetznerServer`, `searchHetznerServerById` and the same for the other three.
async fn search(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, provider)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let spec = spec(&provider)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    let by_id = body.get("by").and_then(Value::as_str) == Some("id");
    let reply = |v: Value| -> ApiResult { Ok(ok(v).into_response()) };
    let token_id = body.get("token_id").and_then(Value::as_i64);
    let Some(token_id) = token_id else {
        let suffix = if by_id { " first." } else { "." };
        return reply(json!({"error": format!("Please select a {} token{suffix}", spec.label)}));
    };
    let manual = body
        .get("id")
        .map(crate::routes::servers::as_php_string)
        .unwrap_or_default();
    if by_id && !php_truthy(&json!(manual)) {
        return reply(json!({"error": format!("Please enter a {}.", spec.id_label)}));
    }
    let failure = |message: &str| {
        if by_id {
            format!("Failed to fetch {}: {message}", spec.singular)
        } else {
            format!("Failed to search {}: {message}", spec.plural)
        }
    };
    if !ctx.is_team_admin() {
        return reply(json!({"error": failure(UNAUTHORIZED)}));
    }
    if crate::routes::servers::is_transferred(&s) {
        return reply(json!({"error": failure(TRANSFERRED)}));
    }
    let Some(token) = token_in_list(&state, &ctx, spec, token_id).await? else {
        return reply(json!({"error": "Invalid token selected."}));
    };
    let client = Client::new(spec.provider, &plain_token(&state, &token)?);
    let found = if by_id {
        fetch(spec, &client, &manual)
            .await
            .map(|v| Some(v).filter(|v| !is_empty(v)))
    } else {
        let ip = s
            .record
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        list(spec, &client)
            .await
            .map(|items| find_by_ip(spec, items, &ip))
    };
    match found {
        Ok(Some(m)) => reply(json!({"matched": matched_summary(spec, &m)})),
        Ok(None) => reply(json!({"no_match": true})),
        Err(e) => reply(json!({"error": failure(&e.to_string())})),
    }
}

/// `linkToHetzner`, `linkToDigitalOcean`, `linkToVultr` and `linkToHostinger`.
async fn link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, provider)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let spec = spec(&provider)?;
    let s = find_server(&state, &ctx, &uuid).await?;
    let Some(id) = body.get("id").filter(|v| php_truthy(v)) else {
        let noun = spec.singular;
        return toast_error(format!("No {noun} selected."));
    };
    let id = crate::routes::servers::as_php_string(id);
    if !ctx.is_team_admin() {
        return toast_error(UNAUTHORIZED);
    }
    if crate::routes::servers::is_transferred(&s) {
        return toast_error(TRANSFERRED);
    }
    let token_id = body
        .get("token_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let Some(token) = token_in_list(&state, &ctx, spec, token_id).await? else {
        return toast_error("Invalid token selected.");
    };
    let client = Client::new(spec.provider, &plain_token(&state, &token)?);
    let data = match fetch(spec, &client, &id).await {
        Ok(data) => data,
        Err(e) => return toast_error(e.to_string()),
    };
    if is_empty(&data) {
        return toast_error(format!("Could not find {} with ID: {id}", spec.singular));
    }
    let status = data
        .get(spec.status_key)
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut updates = vec![
        ("cloud_provider_token_id", C::Int(token_id)),
        (
            spec.id_column,
            if spec.name == "vultr" {
                C::Text(Some(id.clone()))
            } else {
                C::Int(php_int(&id))
            },
        ),
        (spec.status_column, C::Text(status)),
    ];
    if spec.updates_ip {
        let ip = match spec.name {
            "digitalocean" => Client::do_public_ip(&data, true, true),
            _ => Client::hostinger_public_ip(&data),
        };
        if let Some(ip) = ip.filter(|ip| !ip.is_empty()) {
            updates.push(("ip", C::Text(Some(ip))));
        }
    }
    servers::update_columns(&state.db, s.id, &updates, state.now()).await?;
    tracing::info!(target: "audit", event = "ui.server.provider_linked", team_id = ctx.team_id, server = %uuid, provider = spec.name);
    Ok(ok(json!({"message": spec.success})).into_response())
}
