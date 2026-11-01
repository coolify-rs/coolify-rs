//! `App\Services\SharedVariableCanvas`, `App\Services\TagCanvas` and their controllers: the
//! connect canvases. Variable values are never sent.

use std::collections::{HashMap, HashSet};

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use coolify_db::canvas::{self as db, Resource, SERVICE_MORPH};
use coolify_db::records::Record;
use coolify_db::resource_envs::NewEnv;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::graph::{self, Loaded, environment_page, int, text};
use super::{add_error, base, check_end, input, message, validation};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_shared::envs::{
    ADDED_FIELDS, audit_deleted, compose_use, create_model, is_shared_reference,
};

const SOURCES_ZONE: &str = "sources";
const CONNECTED_ZONE: &str = "connected";
const RESOURCES_ZONE: &str = "resources";
const BUILT_IN_SERVER_KEYS: [&str; 2] = ["COOLIFY_SERVER_UUID", "COOLIFY_SERVER_NAME"];

fn headline(scope: &str) -> String {
    coolify_db::audit::headline(scope)
}

fn sort_by_name(l: &mut Loaded) {
    l.resources.sort_by_key(|r| r.name().to_lowercase());
}

/// `TagCanvas::resources`, `SharedVariableCanvas::resources`.
async fn load_scope(
    state: &AppState,
    scope: &str,
    owner: &Owner,
    team_id: i64,
) -> Result<Loaded, ApiError> {
    let environments = match scope {
        "team" | "server" => db::team_environments(&state.db, team_id).await?,
        "project" => db::project_environments(&state.db, &[int(&owner.row, "id")]).await?,
        _ => vec![owner.row.clone()],
    };
    let mut l = graph::load(state, &environments).await?;
    if scope == "server" {
        let owner_id = int(&owner.row, "id");
        let keep: Vec<bool> = l
            .resources
            .iter()
            .map(|r| l.main_server(r).is_some_and(|s| int(s, "id") == owner_id))
            .collect();
        let mut flags = keep.into_iter();
        l.resources.retain(|_| flags.next().unwrap_or(false));
    }
    sort_by_name(&mut l);
    Ok(l)
}

/// `SharedVariableCanvas::resourceNodes`.
async fn resource_nodes(state: &AppState, l: &Loaded, base: &str) -> Result<Vec<Value>, ApiError> {
    let g = graph::graph(state, l, &[], false, base).await?;
    Ok(g.nodes
        .into_iter()
        .zip(&l.resources)
        .map(|(mut node, r)| {
            let p = l.project(r);
            let e = l.environment(r);
            graph::spread(
                &mut node,
                json!({
                    "zone": RESOURCES_ZONE,
                    "extra_zones": [],
                    "context": format!(
                        "{} / {}",
                        p.map_or("", |p| text(p, "name")),
                        e.map_or("", |e| text(e, "name")),
                    ),
                    "context_link": environment_page(
                        base,
                        p.map_or("", |p| text(p, "uuid")),
                        e.map_or("", |e| text(e, "uuid")),
                    ),
                    "project": {"uuid": p.and_then(|p| p.get("uuid")), "name": p.and_then(|p| p.get("name"))},
                    "environment": {"uuid": e.and_then(|e| e.get("uuid")), "name": e.and_then(|e| e.get("name"))},
                }),
            );
            Value::Object(node)
        })
        .collect())
}

fn reference_regex(scope: &str) -> Result<Regex, ApiError> {
    Regex::new(&format!(
        r"\{{\{{\s*{}\.(.+?)\s*\}}\}}",
        regex::escape(scope)
    ))
    .map_err(|_| ApiError::Internal)
}

/// `SharedVariableCanvas::referencedKeys`.
fn referenced_keys(pattern: &Regex, value: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cap in pattern.captures_iter(value.unwrap_or_default()) {
        let key = cap[1].to_owned();
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// `SharedVariableCanvas::isExactReference`.
fn is_exact_reference(scope: &str, key: &str, value: Option<&str>) -> bool {
    Regex::new(&format!(
        r"^\{{\{{\s*{}\.{}\s*\}}\}}$",
        regex::escape(scope),
        regex::escape(key)
    ))
    .is_ok_and(|r| r.is_match(value.unwrap_or_default().trim()))
}

fn reference(scope: &str, key: &str) -> String {
    format!("{{{{{scope}.{key}}}}}")
}

fn plain_value(state: &AppState, row: &Record) -> Option<String> {
    let enc = state.encrypter.as_ref()?;
    let cipher = row.get("value").and_then(Value::as_str)?;
    enc.decrypt_maybe_serialized(cipher)
        .or_else(|_| enc.decrypt_string(cipher))
        .ok()
}

/// One link while `links` collects it: id, source, target, key, the resource's keys, removable, missing.
type LinkRow = (String, String, String, String, Vec<String>, bool, bool);

/// `SharedVariableCanvas::links`.
async fn links(
    state: &AppState,
    scope: &str,
    variables: &[Record],
    resources: &[&Resource],
) -> Result<Vec<Value>, ApiError> {
    let by_key: HashMap<&str, String> = variables
        .iter()
        .map(|v| (text(v, "key"), format!("variable:{}", int(v, "id"))))
        .collect();
    let by_morph: HashMap<(&str, i64), &Resource> =
        resources.iter().map(|r| ((r.morph, r.id()), *r)).collect();
    let ends: Vec<(&str, i64)> = resources.iter().map(|r| (r.morph, r.id())).collect();
    let rows = db::resource_variables(&state.db, &ends, false).await?;
    let pattern = reference_regex(scope)?;
    // One link per (shared variable, resource) pair, in the order they are first seen.
    let mut out: Vec<LinkRow> = Vec::new();
    for row in &rows {
        let Some(resource) =
            by_morph.get(&(text(row, "resourceable_type"), int(row, "resourceable_id")))
        else {
            continue;
        };
        let plain = plain_value(state, row);
        for key in referenced_keys(&pattern, plain.as_deref()) {
            let known = by_key.get(key.as_str());
            let source = known.cloned().unwrap_or_else(|| format!("missing:{key}"));
            let id = format!("{source}|{}", resource.uuid());
            let at = match out.iter().position(|l| l.0 == id) {
                Some(at) => at,
                None => {
                    out.push((
                        id,
                        source,
                        resource.uuid().to_owned(),
                        key.clone(),
                        Vec::new(),
                        true,
                        known.is_none(),
                    ));
                    out.len() - 1
                }
            };
            out[at].4.push(text(row, "key").to_owned());
            out[at].5 = out[at].5 && is_exact_reference(scope, &key, plain.as_deref());
        }
    }
    Ok(out
        .into_iter()
        .map(|(id, source, target, key, keys, removable, missing)| {
            let mut unique: Vec<String> = Vec::new();
            for k in keys {
                if !unique.contains(&k) {
                    unique.push(k);
                }
            }
            let label = if unique.len() == 1 && unique[0] == key {
                Value::Null
            } else {
                json!(unique.join(", "))
            };
            json!({
                "id": id,
                "source": source,
                "target": target,
                "key": key,
                "keys": unique,
                "label": label,
                "removable": removable,
                "missing": missing,
            })
        })
        .collect())
}

/// The scope owner: a team, project, environment or server.
struct Owner {
    row: Record,
    project: Option<Record>,
}

/// `SharedVariableCanvasController::findOwner`.
async fn find_owner(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &str,
    uuid: &str,
) -> Result<Result<Owner, Response>, ApiError> {
    let found = match scope {
        "project" => db::project_by_uuid(&state.db, ctx.team_id, uuid).await?,
        "environment" => db::environment_in_team(&state.db, ctx.team_id, uuid).await?,
        "server" => db::server_by_uuid(&state.db, ctx.team_id, uuid).await?,
        _ => None,
    };
    let Some(row) = found else {
        return Ok(Err(message(
            StatusCode::NOT_FOUND,
            &format!("{} not found.", headline(scope)),
        )));
    };
    let project = if scope == "environment" {
        db::projects_by_id(&state.db, &[int(&row, "project_id")])
            .await?
            .remove(&int(&row, "project_id"))
    } else {
        None
    };
    Ok(Ok(Owner { row, project }))
}

fn team_owner(ctx: &AuthCtx) -> Owner {
    let mut row = Map::new();
    row.insert("id".into(), json!(ctx.team_id));
    Owner { row, project: None }
}

fn title(scope: &str, o: &Owner) -> String {
    match scope {
        "team" => "Team variables".to_owned(),
        "environment" => format!(
            "{} / {} variables",
            o.project.as_ref().map_or("", |p| text(p, "name")),
            text(&o.row, "name")
        ),
        _ => format!("{} variables", text(&o.row, "name")),
    }
}

fn page_link(scope: &str, o: &Owner, base: &str) -> String {
    let uuid = text(&o.row, "uuid");
    match scope {
        "team" => format!("{base}/shared-variables/team"),
        "project" => format!("{base}/shared-variables/project/{uuid}"),
        "environment" => format!(
            "{base}/shared-variables/environments/project/{}/environment/{uuid}",
            o.project.as_ref().map_or("", |p| text(p, "uuid"))
        ),
        _ => format!("{base}/shared-variables/server/{uuid}"),
    }
}

/// `SharedVariableCanvas::payload`.
async fn payload(
    state: &AppState,
    ctx: &AuthCtx,
    scope: &str,
    o: &Owner,
    base: &str,
) -> Result<Value, ApiError> {
    let hidden: &[&str] = if scope == "server" {
        &BUILT_IN_SERVER_KEYS
    } else {
        &[]
    };
    let variables: Vec<Record> =
        db::shared_of_owner(&state.db, scope, int(&o.row, "id"), ctx.team_id)
            .await?
            .into_iter()
            .filter(|v| !hidden.contains(&text(v, "key")))
            .collect();
    let l = load_scope(state, scope, o, ctx.team_id).await?;
    let refs: Vec<&Resource> = l.resources.iter().collect();
    let all_links: Vec<Value> = links(state, scope, &variables, &refs)
        .await?
        .into_iter()
        .filter(|link| {
            !hidden.contains(&link.get("key").and_then(Value::as_str).unwrap_or_default())
        })
        .collect();
    let label = headline(scope);
    let mut nodes: Vec<Value> = variables
        .iter()
        .map(|v| {
            let comment = text(v, "comment");
            json!({
                "id": format!("variable:{}", int(v, "id")),
                "type": "variable",
                "name": v.get("key"),
                "subtitle": if comment.trim().is_empty() { format!("{label} variable") } else { comment.to_owned() },
                "reference": reference(scope, text(v, "key")),
                "missing": false,
                "zone": SOURCES_ZONE,
                "extra_zones": [],
                "status": null,
            })
        })
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    for link in &all_links {
        if link.get("missing") != Some(&json!(true)) {
            continue;
        }
        let source = link
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !seen.insert(source.to_owned()) {
            continue;
        }
        let key = link.get("key").and_then(Value::as_str).unwrap_or_default();
        nodes.push(json!({
            "id": source,
            "type": "variable",
            "name": key,
            "subtitle": "Deleted shared variable",
            "reference": reference(scope, key),
            "missing": true,
            "zone": SOURCES_ZONE,
            "extra_zones": [],
            "status": null,
        }));
    }
    nodes.extend(resource_nodes(state, &l, base).await?);
    Ok(json!({
        "title": title(scope, o),
        "title_link": page_link(scope, o, base),
        "can_edit": ctx.is_team_admin(),
        "can_move": false,
        "layout": "connect",
        "scope": scope,
        "zones": [
            {"id": SOURCES_ZONE, "name": format!("{label} variables"), "icon": "variable"},
            {"id": CONNECTED_ZONE, "name": "Using these variables", "icon": "link"},
            {"id": RESOURCES_ZONE, "name": "Other resources", "icon": "service"},
        ],
        "nodes": nodes,
        "links": all_links,
        "positions": [],
    }))
}

pub(super) async fn team_show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let o = team_owner(&ctx);
    Ok(ok(payload(&state, &ctx, "team", &o, &base(&headers)).await?).into_response())
}

pub(super) async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Path((scope, uuid)): Path<(String, String)>,
) -> ApiResult {
    if !matches!(scope.as_str(), "project" | "environment" | "server") {
        return Ok(message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let o = match find_owner(&state, &ctx, &scope, &uuid).await? {
        Ok(o) => o,
        Err(r) => return Ok(r),
    };
    Ok(ok(payload(&state, &ctx, &scope, &o, &base(&headers)).await?).into_response())
}

/// The kind `ui_shared::find` takes for a resource.
fn kind_of(r: &Resource) -> &'static str {
    if r.morph == coolify_db::canvas::APPLICATION_MORPH {
        "application"
    } else if r.morph == SERVICE_MORPH {
        "service"
    } else {
        "database"
    }
}

#[allow(clippy::too_many_arguments)]
async fn store(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
    scope: &str,
    o: &Owner,
) -> ApiResult {
    let data = input(headers, uri, body)?;
    let mut errors = Map::new();
    check_end(&mut errors, &data, "source");
    check_end(&mut errors, &data, "target");
    if !errors.is_empty() {
        return Ok(validation(errors));
    }
    let source = data.str("source").unwrap_or_default();
    let target = data.str("target").unwrap_or_default();
    let variables = db::shared_of_owner(&state.db, scope, int(&o.row, "id"), ctx.team_id).await?;
    let variable = variable_of(&variables, source);
    let l = load_scope(state, scope, o, ctx.team_id).await?;
    let resource = l.resources.iter().find(|r| r.uuid() == target);
    let mut errors = Map::new();
    if variable.is_none() {
        add_error(
            &mut errors,
            "source",
            "The source must be a shared variable of this scope.".into(),
        );
    }
    if resource.is_none() {
        add_error(
            &mut errors,
            "target",
            "The target must be a resource that can use this shared variable.".into(),
        );
    }
    let (Some(variable), Some(resource)) = (variable, resource) else {
        return Ok(validation(errors));
    };
    let key = text(variable, "key");
    let existing =
        db::resource_variables(&state.db, &[(resource.morph, resource.id())], false).await?;
    if let Some(found) = existing.iter().find(|v| text(v, "key") == key) {
        let pattern = reference_regex(scope)?;
        let plain = plain_value(state, found);
        let m = if referenced_keys(&pattern, plain.as_deref())
            .iter()
            .any(|k| k == key)
        {
            format!("{} already uses {key}.", resource.name())
        } else {
            format!(
                "{} already has a variable named {key}. Rename or remove it first.",
                resource.name()
            )
        };
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": m, "errors": {"target": [m]}}),
        )
        .into_response());
    }
    let res = crate::ui_shared::find(state, ctx, kind_of(resource), resource.uuid()).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let value = reference(scope, key);
    let cipher = enc.encrypt_serialized(&value);
    let flag = |k: &str| variable.get(k).is_some_and(crate::serialize::php_truthy);
    let n = NewEnv {
        morph: resource.morph,
        resource_id: resource.id(),
        key,
        value_encrypted: Some(&cipher),
        comment: None,
        is_multiline: flag("is_multiline"),
        is_literal: flag("is_literal"),
        is_runtime: true,
        is_buildtime: true,
        is_required: false,
        is_shown_once: false,
        is_preview: false,
        is_shared: is_shared_reference(Some(&value)),
        order: None,
        version: &state.version,
    };
    create_model(state, ctx, meta, &res, &n, ADDED_FIELDS).await?;
    let list = links(state, scope, std::slice::from_ref(variable), &[resource]).await?;
    let wanted = format!("variable:{}", int(variable, "id"));
    let link = list
        .into_iter()
        .find(|l| l.get("source").and_then(Value::as_str) == Some(&wanted))
        .unwrap_or(Value::Null);
    Ok(PhpJson(StatusCode::CREATED, link).into_response())
}

fn variable_of<'a>(variables: &'a [Record], node: &str) -> Option<&'a Record> {
    let id: i64 = node.strip_prefix("variable:")?.parse().ok()?;
    variables.iter().find(|v| int(v, "id") == id)
}

pub(super) async fn team_store_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let o = team_owner(&ctx);
    store(&state, &ctx, &meta, &headers, &uri, &body, "team", &o).await
}

pub(super) async fn store_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    Path((scope, uuid)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    if !matches!(scope.as_str(), "project" | "environment" | "server") {
        return Ok(message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let o = match find_owner(&state, &ctx, &scope, &uuid).await? {
        Ok(o) => o,
        Err(r) => return Ok(r),
    };
    store(&state, &ctx, &meta, &headers, &uri, &body, &scope, &o).await
}

async fn destroy(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    link_id: &str,
    scope: &str,
    o: &Owner,
) -> ApiResult {
    let (source, target) = link_id.split_once('|').unwrap_or((link_id, ""));
    let l = load_scope(state, scope, o, ctx.team_id).await?;
    let resource = l.resources.iter().find(|r| r.uuid() == target);
    let variables = db::shared_of_owner(&state.db, scope, int(&o.row, "id"), ctx.team_id).await?;
    let key = variable_of(&variables, source)
        .map(|v| text(v, "key").to_owned())
        .or_else(|| source.strip_prefix("missing:").map(str::to_owned))
        .filter(|k| !k.trim().is_empty());
    let (Some(resource), Some(key)) = (resource, key) else {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    };
    let pattern = reference_regex(scope)?;
    let rows = db::resource_variables(&state.db, &[(resource.morph, resource.id())], true).await?;
    let references: Vec<(&Record, Option<String>)> = rows
        .iter()
        .map(|r| (r, plain_value(state, r)))
        .filter(|(_, plain)| referenced_keys(&pattern, plain.as_deref()).contains(&key))
        .collect();
    let is_preview = |r: &Record| {
        r.get("is_preview")
            .is_some_and(crate::serialize::php_truthy)
    };
    if references.iter().all(|(r, _)| is_preview(r)) {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    }
    let unique = |list: Vec<&str>| {
        let mut out: Vec<String> = Vec::new();
        for k in list {
            if !out.iter().any(|x| x == k) {
                out.push(k.to_owned());
            }
        }
        out
    };
    let embedded = unique(
        references
            .iter()
            .filter(|(_, plain)| !is_exact_reference(scope, &key, plain.as_deref()))
            .map(|(r, _)| text(r, "key"))
            .collect(),
    );
    if !embedded.is_empty() {
        let m = format!(
            "{} on {} uses {} inside a longer value. Edit it on the resource.",
            embedded.join(", "),
            resource.name(),
            reference(scope, &key)
        );
        return Ok(message(StatusCode::UNPROCESSABLE_ENTITY, &m));
    }
    let res = crate::ui_shared::find(state, ctx, kind_of(resource), resource.uuid()).await?;
    let uses_compose = resource.morph == SERVICE_MORPH
        || (resource.morph == coolify_db::canvas::APPLICATION_MORPH
            && resource.str("build_pack") == Some("dockercompose"));
    if uses_compose {
        let compose = resource.str("docker_compose");
        for k in unique(references.iter().map(|(r, _)| text(r, "key")).collect()) {
            if let Some(reason) = compose_use(compose, &k) {
                let m = format!("Cannot remove {k} from {}. {reason}", resource.name());
                return Ok(message(StatusCode::UNPROCESSABLE_ENTITY, &m));
            }
        }
    }
    let ids: Vec<i64> = references.iter().map(|(r, _)| int(r, "id")).collect();
    coolify_db::resource_envs::delete_ids(&state.db, &ids).await?;
    for (r, _) in &references {
        audit_deleted(state, ctx, meta, &res, r).await;
    }
    Ok(ok(json!({"message": "Link removed."})).into_response())
}

pub(super) async fn team_destroy_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(link_id): Path<String>,
) -> ApiResult {
    let o = team_owner(&ctx);
    destroy(&state, &ctx, &meta, &link_id, "team", &o).await
}

pub(super) async fn destroy_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((scope, uuid, link_id)): Path<(String, String, String)>,
) -> ApiResult {
    if !matches!(scope.as_str(), "project" | "environment" | "server") {
        return Ok(message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let o = match find_owner(&state, &ctx, &scope, &uuid).await? {
        Ok(o) => o,
        Err(r) => return Ok(r),
    };
    destroy(&state, &ctx, &meta, &link_id, &scope, &o).await
}

async fn load_tags(
    state: &AppState,
    ctx: &AuthCtx,
) -> Result<(Vec<Record>, Loaded, HashMap<(String, i64), Vec<i64>>), ApiError> {
    let tags = coolify_db::shared_variables::tags(&state.db, ctx.team_id).await?;
    let ids: Vec<i64> = tags.iter().map(|t| int(t, "id")).collect();
    let environments = db::team_environments(&state.db, ctx.team_id).await?;
    let mut l = graph::load(state, &environments).await?;
    sort_by_name(&mut l);
    let taggables = db::tag_ids(&state.db, &ids).await?;
    Ok((tags, l, taggables))
}

/// `TagCanvas::link`.
fn tag_link(tag: &Record, resource: &Resource) -> Value {
    let source = format!("tag:{}", text(tag, "uuid"));
    json!({
        "id": format!("{source}|{}", resource.uuid()),
        "source": source,
        "target": resource.uuid(),
        "key": tag.get("name"),
        "keys": [],
        "label": null,
        "removable": true,
        "missing": false,
    })
}

/// `TagCanvas::payload`.
pub(super) async fn tags_canvas(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    let base = base(&headers);
    let (tags, l, taggables) = load_tags(&state, &ctx).await?;
    let mut nodes: Vec<Value> = tags
        .iter()
        .map(|t| {
            json!({
                "id": format!("tag:{}", text(t, "uuid")),
                "type": "tag",
                "name": t.get("name"),
                "link": format!("{base}/tags/{}", laravel_segment(text(t, "name"))),
                "missing": false,
                "zone": SOURCES_ZONE,
                "extra_zones": [],
                "status": null,
            })
        })
        .collect();
    nodes.extend(resource_nodes(&state, &l, &base).await?);
    let mut all = Vec::new();
    for r in &l.resources {
        for id in taggables
            .get(&(r.morph.to_owned(), r.id()))
            .map_or(&[][..], Vec::as_slice)
        {
            if let Some(t) = tags.iter().find(|t| int(t, "id") == *id) {
                all.push(tag_link(t, r));
            }
        }
    }
    Ok(ok(json!({
        "title": "Tags",
        "title_link": format!("{base}/tags"),
        "can_edit": ctx.is_team_admin(),
        "can_move": false,
        "layout": "connect",
        "zones": [
            {"id": SOURCES_ZONE, "name": "Tags", "icon": "tag"},
            {"id": CONNECTED_ZONE, "name": "Tagged resources", "icon": "link"},
            {"id": RESOURCES_ZONE, "name": "Other resources", "icon": "service"},
        ],
        "nodes": nodes,
        "links": all,
        "positions": [],
    }))
    .into_response())
}

/// A route parameter as `route()` encodes it.
fn laravel_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~@:;,=+!*$&'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn find_tag<'a>(tags: &'a [Record], node: &str) -> Option<&'a Record> {
    let uuid = node.strip_prefix("tag:")?;
    tags.iter().find(|t| text(t, "uuid") == uuid)
}

pub(super) async fn tags_store_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let data = input(&headers, &uri, &body)?;
    let mut errors = Map::new();
    check_end(&mut errors, &data, "source");
    check_end(&mut errors, &data, "target");
    if !errors.is_empty() {
        return Ok(validation(errors));
    }
    let (tags, l, taggables) = load_tags(&state, &ctx).await?;
    let tag = find_tag(&tags, data.str("source").unwrap_or_default());
    let resource = l
        .resources
        .iter()
        .find(|r| r.uuid() == data.str("target").unwrap_or_default());
    let mut errors = Map::new();
    if tag.is_none() {
        add_error(
            &mut errors,
            "source",
            "The source must be a tag of this team.".into(),
        );
    }
    if resource.is_none() {
        add_error(
            &mut errors,
            "target",
            "The target must be a resource of this team.".into(),
        );
    }
    let (Some(tag), Some(resource)) = (tag, resource) else {
        return Ok(validation(errors));
    };
    let has = taggables
        .get(&(resource.morph.to_owned(), resource.id()))
        .is_some_and(|ids| ids.contains(&int(tag, "id")));
    if has {
        let m = format!(
            "{} already has the {} tag.",
            resource.name(),
            text(tag, "name")
        );
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": m, "errors": {"target": [m]}}),
        )
        .into_response());
    }
    coolify_db::tag_pages::attach(&state.db, int(tag, "id"), resource.morph, resource.id()).await?;
    Ok(PhpJson(StatusCode::CREATED, tag_link(tag, resource)).into_response())
}

pub(super) async fn tags_destroy_link(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(link_id): Path<String>,
) -> ApiResult {
    let (source, target) = link_id.split_once('|').unwrap_or((&link_id, ""));
    let (tags, l, taggables) = load_tags(&state, &ctx).await?;
    let tag = find_tag(&tags, source);
    let resource = l.resources.iter().find(|r| r.uuid() == target);
    let (Some(tag), Some(resource)) = (tag, resource) else {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    };
    let has = taggables
        .get(&(resource.morph.to_owned(), resource.id()))
        .is_some_and(|ids| ids.contains(&int(tag, "id")));
    if !has {
        return Ok(message(StatusCode::NOT_FOUND, "Link not found."));
    }
    coolify_db::tag_pages::detach(&state.db, int(tag, "id"), resource.morph, resource.id()).await?;
    Ok(ok(json!({"message": "Link removed."})).into_response())
}
