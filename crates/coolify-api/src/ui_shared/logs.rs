//! `Project\Shared\Logs`, `App\Support\RuntimeLogTargets` and `Project\Shared\GetLogs` for a resource.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::servers;
use coolify_remote::shell::quote;
use regex::Regex;
use serde_json::{Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::unauthorized;
use crate::ui_server_proxy::{
    MAX_DISPLAY_SIZE_BYTES, MAX_DOWNLOAD_SIZE_BYTES, is_docker_timestamp, log_tail,
    remove_ansi_colors,
};

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/logs";
    Router::new()
        .route(b, get(targets))
        .route(&format!("{b}/read"), get(read))
}

static CONTAINER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z").unwrap_or_else(|_| unreachable!())
});

/// `dockerPsByOwnerCommands`.
fn owner_commands(res: &Res) -> Vec<String> {
    let kind = match res.class {
        Class::Application => "application",
        Class::Service => "service",
        _ => "database",
    };
    let uuid = &res.uuid;
    let format = "--format '{{json .}}'";
    let mut out = vec![
        format!("docker ps -a --filter 'label=coolify.{kind}Uuid={uuid}' {format}"),
        format!(
            "docker ps -a --filter 'label=coolify.{kind}Id' --filter 'label=com.docker.compose.project={uuid}' {format}"
        ),
    ];
    if res.is_application() {
        out.push(format!(
            "docker ps -a --filter 'label=coolify.applicationId' --filter 'name={uuid}' {format}"
        ));
    }
    out
}

/// `format_docker_labels_to_json`.
fn labels_of(c: &Value) -> HashMap<String, String> {
    let raw = c.get("Labels").and_then(Value::as_str).unwrap_or_default();
    raw.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[derive(Debug, Clone)]
struct Target {
    name: String,
    key: String,
    state: String,
    deployment: String,
}

/// `Str::headline`.
fn headline(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '-' || c == '_' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .iter()
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_deployment(d: &str) -> String {
    match d.parse::<u64>() {
        Ok(n) if n != 0 && d.bytes().all(|b| b.is_ascii_digit()) => n.to_string(),
        _ => "0".to_owned(),
    }
}

fn target_of(c: &Value, group_deployments: bool) -> Option<Target> {
    let name = c.get("Names").and_then(Value::as_str)?.to_owned();
    if !CONTAINER_NAME.is_match(&name) {
        return None;
    }
    let labels = labels_of(c);
    let key = labels
        .get("com.docker.compose.service")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            labels
                .get("coolify.name")
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| {
            let before = name.rsplit_once('-').map_or(name.as_str(), |(a, _)| a);
            let h = headline(before);
            if h.is_empty() { name.clone() } else { h }
        });
    let state = c
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or("running")
        .trim()
        .to_lowercase();
    Some(Target {
        name,
        key,
        state: if state.is_empty() {
            "running".to_owned()
        } else {
            state
        },
        deployment: if group_deployments {
            normalize_deployment(
                labels
                    .get("coolify.pullRequestId")
                    .map_or("", String::as_str),
            )
        } else {
            "0".to_owned()
        },
    })
}

const LIVE: [&str; 4] = ["running", "starting", "restarting", "degraded"];

fn service_dot(states: &[&str]) -> &'static str {
    if states.iter().all(|s| *s == "running") {
        "success"
    } else if !states.iter().any(|s| LIVE.contains(s)) {
        "error"
    } else {
        "warning"
    }
}

fn service_state(states: &[&str]) -> Option<&'static str> {
    let mut problems: Vec<&str> = states.iter().copied().filter(|s| *s != "running").collect();
    problems.dedup();
    if problems.is_empty() {
        return None;
    }
    let any_live = states.iter().any(|s| LIVE.contains(s));
    Some(match problems.as_slice() {
        ["starting"] => "Starting",
        ["restarting"] => "Restarting",
        ["degraded"] => "Degraded",
        _ if !any_live => "Stopped",
        _ => "Degraded",
    })
}

fn status_option(value: &str, label: &str, states: &[&str]) -> Value {
    let mut o = json!({"value": value, "label": label, "dot": service_dot(states)});
    if let (Some(state), Value::Object(m)) = (service_state(states), &mut o) {
        m.insert("state".into(), json!(state));
    }
    o
}

/// `RuntimeLogTargets::select`.
fn select(mut all: Vec<Target>, deployment: &str, service: &str, group_deployments: bool) -> Value {
    let empty = json!({"deployments": [], "services": [], "deployment": "0", "service": "", "sources": [], "stream_key": ""});
    if all.is_empty() {
        return empty;
    }
    let mut ids: Vec<String> = Vec::new();
    for t in &all {
        if !ids.contains(&t.deployment) {
            ids.push(t.deployment.clone());
        }
    }
    ids.sort_by_key(|i| {
        if i == "0" {
            (0, 0)
        } else {
            (1, -i.parse::<i64>().unwrap_or(0))
        }
    });
    let preferred = normalize_deployment(deployment);
    let chosen = if !group_deployments {
        "0".to_owned()
    } else if ids.contains(&preferred) {
        preferred
    } else if ids.iter().any(|i| i == "0") {
        "0".to_owned()
    } else {
        ids.first().cloned().unwrap_or_else(|| "0".to_owned())
    };
    let deployments: Vec<Value> = if group_deployments {
        ids.iter()
            .map(|id| {
                let states: Vec<&str> = all
                    .iter()
                    .filter(|t| &t.deployment == id)
                    .map(|t| t.state.as_str())
                    .collect();
                let label = if id == "0" {
                    "Production".to_owned()
                } else {
                    format!("PR #{id}")
                };
                status_option(id, &label, &states)
            })
            .collect()
    } else {
        Vec::new()
    };
    all.retain(|t| t.deployment == chosen);
    if all.is_empty() {
        return empty;
    }
    // `uksort(strcasecmp)`.
    let mut groups: BTreeMap<String, (String, Vec<&Target>)> = BTreeMap::new();
    for t in &all {
        groups
            .entry(t.key.to_lowercase())
            .or_insert_with(|| (t.key.clone(), Vec::new()))
            .1
            .push(t);
    }
    let chosen_key = groups
        .values()
        .find(|(k, _)| !service.is_empty() && k == service)
        .or_else(|| {
            groups
                .values()
                .find(|(_, v)| v.iter().any(|t| t.state == "running"))
        })
        .or_else(|| groups.values().next())
        .map(|(k, _)| k.clone())
        .unwrap_or_default();
    let services: Vec<Value> = groups
        .values()
        .map(|(k, v)| {
            let states: Vec<&str> = v.iter().map(|t| t.state.as_str()).collect();
            let mut o = status_option(k, k, &states);
            if v.len() > 1
                && let Value::Object(m) = &mut o
            {
                m.insert(
                    "description".into(),
                    json!(format!("{} containers", v.len())),
                );
            }
            o
        })
        .collect();
    let mut chosen_containers: Vec<&Target> = groups
        .values()
        .find(|(k, _)| *k == chosen_key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    chosen_containers.sort_by(|a, b| a.name.cmp(&b.name));
    let sources: Vec<Value> = chosen_containers
        .iter()
        .map(|t| json!({"container": t.name, "label": t.name}))
        .collect();
    let stream_key = chosen_containers
        .iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    json!({
        "deployments": deployments,
        "services": services,
        "deployment": chosen,
        "service": if groups.len() > 1 { chosen_key } else { String::new() },
        "sources": sources,
        "stream_key": stream_key,
    })
}

/// `Logs::loadAllContainers` and `selectionTargets`.
async fn targets(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let id = res.server_id.ok_or(ApiError::Internal)?;
    let row = servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    let info = coolify_jobs::server::ServerInfo::from_row(&row);
    let functional = info.is_functional();
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut server_error: Option<String> = None;
    let mut containers: Vec<Value> = Vec::new();
    if functional {
        match jobs
            .instant_remote(
                &row,
                &owner_commands(&res),
                true,
                "resource.logs.containers",
            )
            .await
        {
            Ok(out) => {
                let mut seen: Vec<String> = Vec::new();
                for line in out.unwrap_or_default().lines() {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        let id = v
                            .get("ID")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        if !seen.contains(&id) {
                            seen.push(id);
                            containers.push(v);
                        }
                    }
                }
            }
            Err(e) => server_error = Some(e),
        }
    }
    let group = res.is_application();
    let list: Vec<Target> = containers
        .iter()
        .filter_map(|c| target_of(c, group))
        .collect();
    let selection = select(
        list,
        q.get("pull_request_id").map_or("0", String::as_str),
        q.get("service").map_or("", String::as_str),
        group,
    );
    Ok(ok(json!({
        "can_view": ctx.is_team_admin(),
        "server_name": row.record.get("name"),
        "functional": functional,
        "server_error": server_error,
        "total_containers": containers.len(),
        "targets": selection,
        "timezone": row
            .settings
            .as_ref()
            .and_then(|s| s.get("server_timezone"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or("UTC"),
    }))
    .into_response())
}

/// `GetLogs::getLogs`, `copyLogs`, `downloadAllLogs`, and `mergedLogs` for several sources.
async fn read(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    // Container logs can contain secrets.
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let id = res.server_id.ok_or(ApiError::Internal)?;
    let row = servers::find_by_id(&state.db, id)
        .await?
        .ok_or(ApiError::Internal)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let requested: Vec<&str> = query
        .get("containers")
        .map_or("", String::as_str)
        .split(',')
        .filter(|c| !c.is_empty())
        .collect();
    // `authorizedSources`: only containers this resource owns.
    let owned = jobs
        .instant_remote(
            &row,
            &owner_commands(&res),
            true,
            "resource.logs.containers",
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .unwrap_or_default();
    let owned: Vec<String> = owned
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("Names").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let containers: Vec<&str> = requested
        .into_iter()
        .filter(|c| CONTAINER_NAME.is_match(c) && owned.iter().any(|o| o == c))
        .collect();
    if containers.is_empty() {
        return Ok(ok(json!({"logs": ""})).into_response());
    }
    let mode = query.get("mode").map_or("get", String::as_str);
    let lines = query.get("lines").and_then(|l| l.parse::<i64>().ok());
    let timestamps = query.get("timestamps").is_none_or(|t| t == "1");
    let tail = log_tail(lines);
    let merged = containers.len() > 1;
    let (options, max, limit, timestamped) = match mode {
        "copy" => (
            format!("-n {tail}{}", if timestamps || merged { " -t" } else { "" }),
            MAX_DISPLAY_SIZE_BYTES,
            "5MB",
            timestamps || merged,
        ),
        "all" => (
            if timestamps || merged {
                "-t".to_string()
            } else {
                String::new()
            },
            MAX_DOWNLOAD_SIZE_BYTES,
            "50MB",
            timestamps || merged,
        ),
        _ => match query.get("since").filter(|v| is_docker_timestamp(v)) {
            Some(since) => (
                format!("--since {since} -t"),
                MAX_DISPLAY_SIZE_BYTES,
                "5MB",
                true,
            ),
            None => (format!("-n {tail} -t"), MAX_DISPLAY_SIZE_BYTES, "5MB", true),
        },
    };
    let options = if options.is_empty() {
        String::new()
    } else {
        format!("{options} ")
    };
    let mut truncated = false;
    let mut rows: Vec<(String, String, String)> = Vec::new();
    let mut plain = String::new();
    for container in &containers {
        let out = jobs
            .instant_remote(
                &row,
                &[format!(
                    "(docker logs {options}{}) 2>&1 | head -c {}",
                    quote(container),
                    max + 1
                )],
                false,
                "resource.logs",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let cut = if out.len() > max {
            truncated = true;
            let mut end = max;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            &out[..end]
        } else {
            out.as_str()
        };
        let cleaned = remove_ansi_colors(cut);
        let cleaned = cleaned.trim_end_matches('\n');
        if !merged && !timestamped {
            plain = cleaned.to_owned();
        }
        if timestamped {
            for l in cleaned
                .split('\n')
                .filter(|l| merged || !l.is_empty() || cleaned.is_empty())
            {
                let ts = l.split(' ').next().unwrap_or_default().to_owned();
                rows.push((ts, (*container).to_owned(), l.to_owned()));
            }
        }
    }
    let mut text = if timestamped {
        rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let export = mode != "get";
        rows.iter()
            .map(|(_, label, line)| {
                let shown = if merged && export && !timestamps {
                    line.split_once(' ').map_or(line.as_str(), |(_, rest)| rest)
                } else {
                    line.as_str()
                };
                if !merged {
                    shown.to_owned()
                } else if export {
                    format!("{label} | {shown}")
                } else {
                    format!("{label}\t{shown}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        plain
    };
    // `mergedLogs()`: the joined stream of all containers is capped too, cut at a character
    // boundary (`mb_strcut`) so the viewer never gets half a character.
    if merged && text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.truncate(text.trim_end_matches('\n').len());
        truncated = true;
    }
    if truncated {
        text.push_str(&format!("\n\n[... Output truncated at {limit} limit ...]"));
    }
    Ok(ok(json!({"logs": text})).into_response())
}
