//! What the layout shows on every page: `App\Livewire\LayoutPopups`, `DeploymentsIndicator`,
//! `Dashboard\ActiveDeployments` and the data of `GlobalSearch`.
//!
//! `GlobalSearch::loadSearchableItems` keeps the resources in a cache for 300 seconds, which models
//! clear on every change (`ClearsGlobalSearchCache`). Reading them for each opening shows the same
//! results; the search itself runs in the browser, as upstream's Alpine component does.

use axum::extract::{Query, State};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::notification_settings as ns;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::toast_error;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/layout", get(layout))
        .route("/ui/layout/deployments", get(deployments))
        .route("/ui/layout/recent-deployments", get(recent_deployments))
        .route("/ui/search/items", get(search_items))
        .route("/ui/search/step", get(search_step))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// `Team::isAnyNotificationEnabled`.
async fn any_notification_enabled(state: &AppState, team_id: i64) -> Result<bool, ApiError> {
    for ch in ns::ALL {
        let sql = match ch.name {
            "email" => format!(
                "SELECT count(*) FROM {} WHERE team_id = $1 AND (smtp_enabled OR resend_enabled OR use_instance_email_settings)",
                ch.table
            ),
            name => format!(
                "SELECT count(*) FROM {} WHERE team_id = $1 AND {name}_enabled",
                ch.table
            ),
        };
        let n: i64 = sqlx::query_scalar(&sql)
            .bind(team_id)
            .fetch_one(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
        if n > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn layout(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let row = coolify_db::instance_settings::row(&state.db).await?;
    Ok(ok(json!({
        "is_sponsorship_popup_enabled": row.get("is_sponsorship_popup_enabled"),
        "any_notification_enabled": any_notification_enabled(&state, ctx.team_id).await?,
    }))
    .into_response())
}

/// `ApplicationDeploymentQueue::isImageScanBlocked()` in SQL: `(bool) data_get($this->scan_gate_result,
/// 'blocked', false)`. A schema without the column (before `create_image_scanning_tables`) has no gate.
fn image_scan_blocked_sql(state: &AppState) -> &'static str {
    if state
        .db
        .schema()
        .has_column("application_deployment_queues", "scan_gate_result")
    {
        "COALESCE(q.scan_gate_result->>'blocked', '') NOT IN ('', 'false', '0')"
    } else {
        "false"
    }
}

/// `ApplicationDeploymentQueue::ownedByTeam` with `application.environment.project`.
fn deployment_select(state: &AppState) -> String {
    format!(
        "SELECT json_build_object(
        'id', q.id, 'application_name', q.application_name, 'deployment_url', q.deployment_url,
        'deployment_uuid', q.deployment_uuid, 'pull_request_id', q.pull_request_id,
        'server_name', q.server_name, 'status', q.status, 'created_at', q.created_at,
        'finished_at', q.finished_at, 'image_scan_blocked', {blocked},
        'application_uuid', a.uuid, 'project', p.name, 'environment', e.name,
        'environment_id', e.id)::text
    FROM application_deployment_queues q
    JOIN applications a ON a.id::text = q.application_id
    JOIN environments e ON e.id = a.environment_id
    JOIN projects p ON p.id = e.project_id
    WHERE p.team_id = $1",
        blocked = image_scan_blocked_sql(state)
    )
}

async fn queue_rows(state: &AppState, sql: String, team: i64) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<String> = sqlx::query_scalar(&sql)
        .bind(team)
        .fetch_all(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(r).ok())
        // `visibleToCurrentActor()`: deployments of applications the actor can see.
        .filter(|v| {
            coolify_db::access::resource_visible(
                v.get("environment_id").and_then(Value::as_i64),
                v.get("application_uuid")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
        })
        .map(|mut v| {
            if let Value::Object(m) = &mut v {
                m.remove("environment_id");
            }
            v
        })
        .collect())
}

/// The engine words in a database's `search_text` (`GlobalSearch::loadSearchableItems()`): a Redis
/// database is also found as Valkey.
fn database_search_word(subtype: &str) -> &str {
    if subtype == "redis" {
        "redis valkey"
    } else {
        subtype
    }
}

/// `DeploymentsIndicator::deployments`.
async fn deployments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let sql = format!(
        "{} AND q.status IN ('in_progress', 'queued') ORDER BY q.id",
        deployment_select(&state)
    );
    Ok(ok(json!({"deployments": queue_rows(&state, sql, ctx.team_id).await?})).into_response())
}

/// `ActiveDeployments::recentDeployments()` on PostgreSQL: the five newest finished deployments of
/// each of the team's applications (`idx_deployment_queues_app_id`), then the five newest of those,
/// instead of scanning the primary key backwards.
fn recent_deployments_select(state: &AppState) -> String {
    format!(
        "SELECT json_build_object(
        'id', q.id, 'application_name', q.application_name, 'deployment_url', q.deployment_url,
        'deployment_uuid', q.deployment_uuid, 'pull_request_id', q.pull_request_id,
        'server_name', q.server_name, 'status', q.status, 'created_at', q.created_at,
        'finished_at', q.finished_at, 'image_scan_blocked', {blocked},
        'application_uuid', a.uuid, 'project', p.name, 'environment', e.name,
        'environment_id', e.id)::text
    FROM applications a
    JOIN environments e ON e.id = a.environment_id
    JOIN projects p ON p.id = e.project_id
    CROSS JOIN LATERAL (
        SELECT *
          FROM application_deployment_queues
         WHERE application_id = a.id::text AND status NOT IN ('in_progress', 'queued')
         ORDER BY id DESC LIMIT 5) q
    WHERE p.team_id = $1
    ORDER BY q.id DESC",
        blocked = image_scan_blocked_sql(state)
    )
}

/// `ActiveDeployments::activeDeployments()` and `recentDeployments()`.
async fn recent_deployments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    // A restricted member's rows are filtered before the limit, as the query scope does.
    let limit = if ctx.covers_whole_team() {
        " LIMIT 5"
    } else {
        ""
    };
    let active = format!(
        "{} AND q.status IN ('in_progress', 'queued') ORDER BY q.status, q.id{limit}",
        deployment_select(&state)
    );
    let recent = format!("{}{limit}", recent_deployments_select(&state));
    let mut active = queue_rows(&state, active, ctx.team_id).await?;
    let mut recent = queue_rows(&state, recent, ctx.team_id).await?;
    active.truncate(5);
    recent.truncate(5);
    Ok(ok(json!({
        "active": active,
        "recent": recent,
    }))
    .into_response())
}

/// `service_logo_urls`.
fn logo_urls(logo: Option<&str>) -> (String, String) {
    const DEFAULT: &str = "svgs/default.webp";
    let basename = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let Some(mut l) = logo
        .filter(|l| !l.is_empty() && basename(l) != basename(DEFAULT))
        .map(str::to_owned)
    else {
        return (DEFAULT.to_owned(), DEFAULT.to_owned());
    };
    if let Some(rest) = l.strip_prefix("svg/") {
        l = format!("svgs/{rest}");
    }
    (l.trim_start_matches('/').to_owned(), DEFAULT.to_owned())
}

/// `Str::headline` of a service key: `mb_convert_case` title case starts a word after any
/// non-letter, so `n8n` becomes `N8N`.
fn headline(key: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in key.chars() {
        let c = if matches!(c, '-' | '_') { ' ' } else { c };
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphabetic();
    }
    out
}

/// `GlobalSearch::getServicesProperty`.
fn services() -> Vec<Value> {
    coolify_jobs::services::templates::get_service_templates()
        .iter()
        .map(|(key, service)| {
            let name = headline(key);
            let (logo, fallback) = logo_urls(service.get("logo").and_then(Value::as_str));
            let description = match service.get("slogan") {
                Some(Value::String(s)) => s.clone(),
                _ => format!("Deploy {name}"),
            };
            let mut item = json!({
                "name": name,
                "description": description,
                "type": format!("one-click-service-{key}"),
                "category": "Services",
                "resourceType": "service",
                "logo": logo,
                "logo_default_url": fallback,
            });
            if service.get("amd_only") == Some(&json!(true)) {
                item["amd_only"] = json!(true);
            }
            if service.get("arm_only") == Some(&json!(true)) {
                item["arm_only"] = json!(true);
            }
            item
        })
        .collect()
}

/// PHP's `strtolower`: ASCII only since PHP 8.
fn lower(s: String) -> String {
    s.to_ascii_lowercase()
}

async fn rows(state: &AppState, sql: &str, team: i64) -> Result<Vec<Value>, ApiError> {
    let list: Vec<String> = sqlx::query_scalar(sql)
        .bind(team)
        .fetch_all(state.db.pool())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "global search query failed");
            ApiError::Internal
        })?;
    Ok(list
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `GlobalSearch::loadSearchableItems`: applications, services, databases, servers, projects and
/// environments, in that order.
#[allow(clippy::too_many_lines)]
async fn resources(state: &AppState, team: i64) -> Result<Vec<Value>, ApiError> {
    let mut items = Vec::new();

    for a in rows(
        state,
        "SELECT json_build_object('id', a.id, 'name', a.name, 'uuid', a.uuid, 'description', a.description,
            'fqdn', a.fqdn, 'build_pack', a.build_pack, 'docker_compose_domains', a.docker_compose_domains,
            'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid,
            'environment_id', e.id)::text
         FROM applications a JOIN environments e ON e.id = a.environment_id JOIN projects p ON p.id = e.project_id
         WHERE p.team_id = $1 AND a.deleted_at IS NULL ORDER BY a.name",
        team,
    )
    .await?
    {
        let mut fqdns: Vec<String> = s(&a, "fqdn")
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect();
        if a.get("fqdn").is_some_and(|f| f.is_string()) && s(&a, "fqdn").is_empty() {
            fqdns.clear();
        }
        if s(&a, "build_pack") == "dockercompose"
            && let Ok(domains) = serde_json::from_str::<Value>(s(&a, "docker_compose_domains"))
            && let Some(map) = domains.as_object()
        {
            for v in map.values() {
                if let Some(o) = v.as_object() {
                    for d in o.values().filter_map(Value::as_str) {
                        fqdns.push(d.to_owned());
                    }
                }
            }
        }
        items.push(json!({
            "id": a.get("id"),
            "name": a.get("name"),
            "type": "application",
            "uuid": a.get("uuid"),
            "description": a.get("description"),
            "link": format!("/project/{}/environment/{}/application/{}", s(&a, "project_uuid"), s(&a, "environment_uuid"), s(&a, "uuid")),
            "environment_id": a.get("environment_id"),
            "project": a.get("project"),
            "environment": a.get("environment"),
            "fqdns": fqdns.iter().take(2).cloned().collect::<Vec<_>>().join(", "),
            "search_text": lower(format!(
                // Upstream adds PR terms only when `preview_enabled`, which no application has.
                "{} {} {} {}  application applications app apps",
                s(&a, "name"), s(&a, "description"), fqdns.join(" "), s(&a, "uuid"),
            )),
        }));
    }

    for v in rows(
        state,
        "SELECT json_build_object('id', v.id, 'name', v.name, 'uuid', v.uuid, 'description', v.description,
            'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid,
            'environment_id', e.id,
            'fqdns', (SELECT coalesce(json_agg(sa.fqdn ORDER BY sa.id), '[]') FROM service_applications sa WHERE sa.service_id = v.id AND sa.deleted_at IS NULL),
            'apps', (SELECT coalesce(string_agg(sa.name, ' ' ORDER BY sa.id), '') FROM service_applications sa WHERE sa.service_id = v.id AND sa.deleted_at IS NULL),
            'dbs', (SELECT coalesce(string_agg(sd.name, ' ' ORDER BY sd.id), '') FROM service_databases sd WHERE sd.service_id = v.id AND sd.deleted_at IS NULL))::text
         FROM services v JOIN environments e ON e.id = v.environment_id JOIN projects p ON p.id = e.project_id
         WHERE p.team_id = $1 AND v.deleted_at IS NULL ORDER BY v.name",
        team,
    )
    .await?
    {
        let fqdns: Vec<String> = v
            .get("fqdns")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .filter(|f| !f.is_empty())
                    .flat_map(|f| f.split(',').map(|x| x.trim().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        items.push(json!({
            "id": v.get("id"),
            "name": v.get("name"),
            "type": "service",
            "uuid": v.get("uuid"),
            "description": v.get("description"),
            "link": format!("/project/{}/environment/{}/service/{}", s(&v, "project_uuid"), s(&v, "environment_uuid"), s(&v, "uuid")),
            "environment_id": v.get("environment_id"),
            "project": v.get("project"),
            "environment": v.get("environment"),
            "fqdns": fqdns.iter().take(2).cloned().collect::<Vec<_>>().join(", "),
            "search_text": lower(format!(
                "{} {} {} {} {} {} service services",
                s(&v, "name"), s(&v, "description"), fqdns.join(" "), s(&v, "uuid"), s(&v, "apps"), s(&v, "dbs"),
            )),
        }));
    }

    for (table, subtype) in [
        ("standalone_postgresqls", "postgresql"),
        ("standalone_mysqls", "mysql"),
        ("standalone_mariadbs", "mariadb"),
        ("standalone_mongodbs", "mongodb"),
        ("standalone_redis", "redis"),
        ("standalone_keydbs", "keydb"),
        ("standalone_dragonflies", "dragonfly"),
        ("standalone_clickhouses", "clickhouse"),
        ("standalone_sqlites", "sqlite"),
    ] {
        let sql = format!(
            "SELECT json_build_object('id', d.id, 'name', d.name, 'uuid', d.uuid, 'description', d.description,
                'project', p.name, 'project_uuid', p.uuid, 'environment', e.name, 'environment_uuid', e.uuid,
                'environment_id', e.id)::text
             FROM {table} d JOIN environments e ON e.id = d.environment_id JOIN projects p ON p.id = e.project_id
             WHERE p.team_id = $1 AND d.deleted_at IS NULL ORDER BY d.name"
        );
        for d in rows(state, &sql, team).await? {
            items.push(json!({
                "id": d.get("id"),
                "name": d.get("name"),
                "type": "database",
                "subtype": subtype,
                "uuid": d.get("uuid"),
                "description": d.get("description"),
                "link": format!("/project/{}/environment/{}/database/{}", s(&d, "project_uuid"), s(&d, "environment_uuid"), s(&d, "uuid")),
                "environment_id": d.get("environment_id"),
                "project": d.get("project"),
                "environment": d.get("environment"),
                "search_text": lower(format!(
                    "{} {} {} {} database databases db",
                    s(&d, "name"), s(&d, "uuid"), database_search_word(subtype), s(&d, "description"),
                )),
            }));
        }
    }

    for v in rows(
        state,
        "SELECT json_build_object('id', v.id, 'name', v.name, 'uuid', v.uuid, 'description', v.description, 'ip', v.ip)::text
         FROM servers v WHERE v.team_id = $1 AND v.deleted_at IS NULL ORDER BY v.name",
        team,
    )
    .await?
    {
        items.push(json!({
            "id": v.get("id"),
            "name": v.get("name"),
            "type": "server",
            "uuid": v.get("uuid"),
            "description": v.get("description"),
            "link": format!("/server/{}", s(&v, "uuid")),
            "project": null,
            "environment": null,
            "search_text": lower(format!("{} {} {} server servers", s(&v, "name"), s(&v, "ip"), s(&v, "description"))),
        }));
    }

    let summary = |count: i64| {
        if count > 0 {
            format!("{count} resource{}", if count == 1 { "" } else { "s" })
        } else {
            "No resources".to_owned()
        }
    };

    for p in rows(
        state,
        "SELECT json_build_object('id', p.id, 'name', p.name, 'uuid', p.uuid, 'description', p.description,
            'environments', (SELECT coalesce(json_agg(e.uuid ORDER BY e.id), '[]') FROM environments e WHERE e.project_id = p.id),
            'resources', (SELECT count(*) FROM applications a JOIN environments e ON e.id = a.environment_id WHERE e.project_id = p.id AND a.deleted_at IS NULL)
              + (SELECT count(*) FROM services v JOIN environments e ON e.id = v.environment_id WHERE e.project_id = p.id AND v.deleted_at IS NULL))::text
         FROM projects p WHERE p.team_id = $1 ORDER BY LOWER(p.name)",
        team,
    )
    .await?
    {
        let envs = p
            .get("environments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // `Project::navigateTo`.
        let link = match envs.as_slice() {
            [only] => format!(
                "/project/{}/environment/{}",
                s(&p, "uuid"),
                only.as_str().unwrap_or("")
            ),
            _ => format!("/project/{}", s(&p, "uuid")),
        };
        items.push(json!({
            "id": p.get("id"),
            "name": p.get("name"),
            "type": "project",
            "uuid": p.get("uuid"),
            "description": p.get("description"),
            "link": link,
            "project": null,
            "environment": null,
            "resource_count": summary(p.get("resources").and_then(Value::as_i64).unwrap_or(0)),
            "environment_count": envs.len(),
            "search_text": lower(format!("{} {} project projects", s(&p, "name"), s(&p, "description"))),
        }));
    }

    for e in rows(
        state,
        "SELECT json_build_object('id', e.id, 'name', e.name, 'uuid', e.uuid, 'description', e.description,
            'project', p.name, 'project_uuid', p.uuid,
            'resources', (SELECT count(*) FROM applications a WHERE a.environment_id = e.id AND a.deleted_at IS NULL)
              + (SELECT count(*) FROM services v WHERE v.environment_id = e.id AND v.deleted_at IS NULL))::text
         FROM environments e JOIN projects p ON p.id = e.project_id WHERE p.team_id = $1 ORDER BY e.name",
        team,
    )
    .await?
    {
        let resource_summary = summary(e.get("resources").and_then(Value::as_i64).unwrap_or(0));
        let mut parts = vec![format!("Project: {}", s(&e, "project"))];
        if !s(&e, "description").is_empty() {
            parts.push(s(&e, "description").to_owned());
        }
        items.push(json!({
            "id": e.get("id"),
            "name": e.get("name"),
            "type": "environment",
            "uuid": e.get("uuid"),
            "description": parts.join(" • "),
            "link": format!("/project/{}/environment/{}", s(&e, "project_uuid"), s(&e, "uuid")),
            "project": e.get("project"),
            "environment": null,
            "resource_count": resource_summary,
            "search_text": lower(format!("{} {} {} environment", s(&e, "name"), s(&e, "description"), s(&e, "project"))),
        }));
    }
    Ok(items)
}

async fn search_items(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    let mut items = resources(&state, ctx.team_id).await?;
    // Members limited to some projects: the index of the queries the access grants filter,
    // without servers (`searchableResources(includeServers: false)`).
    if !ctx.covers_whole_team() {
        items.retain(|i| {
            let id = i.get("id").and_then(Value::as_i64).unwrap_or_default();
            let env = i.get("environment_id").and_then(Value::as_i64);
            let uuid = i.get("uuid").and_then(Value::as_str).unwrap_or_default();
            match i.get("type").and_then(Value::as_str).unwrap_or_default() {
                "application" | "service" | "database" => {
                    coolify_db::access::resource_visible(env, uuid)
                }
                "project" => coolify_db::access::project_visible(id),
                "environment" => coolify_db::access::environment_visible(id),
                _ => false,
            }
        });
    }
    Ok(ok(json!({
        "team_wide": ctx.covers_whole_team(),
        "resources": items,
        "services": if ctx.is_team_admin() { services() } else { Vec::new() },
    }))
    .into_response())
}

/// The selection steps of `GlobalSearch::navigateToResourceCreation`.
async fn search_step(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let team = ctx.team_id;
    let id = q.get("id").map_or("", String::as_str);
    match q.get("step").map_or("", String::as_str) {
        // `loadServers`: `Server::isUsable()`.
        "servers" => {
            let list = rows(
                &state,
                "SELECT json_build_object('id', v.id, 'name', v.name, 'description', v.description)::text
                 FROM servers v JOIN server_settings ss ON ss.server_id = v.id
                 WHERE v.team_id = $1 AND v.deleted_at IS NULL AND ss.is_reachable AND ss.is_usable
                   AND NOT ss.is_swarm_worker AND NOT ss.force_disabled
                   AND COALESCE(ss.server_role, CASE WHEN ss.is_build_server THEN 'build' ELSE 'both' END) IN ('deployment', 'both')
                 ORDER BY v.name",
                team,
            )
            .await?;
            // `->filter(fn (Server $server) => Gate::allows('placeResource', $server))`.
            let allowed = crate::place_resource::allowed_server_ids(&state, &ctx).await?;
            let list: Vec<Value> = list
                .into_iter()
                .filter(|s| {
                    s.get("id")
                        .and_then(Value::as_i64)
                        .is_some_and(|id| allowed.contains(&id))
                })
                .collect();
            Ok(ok(json!({"items": list})).into_response())
        }
        // `loadDestinations`.
        "destinations" => {
            let owned: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM servers WHERE team_id = $1 AND id = $2 AND deleted_at IS NULL",
            )
            .bind(team)
            .bind(id.parse::<i64>().unwrap_or(-1))
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            let Some(server) = owned else {
                return Ok(toast_error(
                    axum::http::StatusCode::NOT_FOUND,
                    "Server not found",
                ));
            };
            let list = rows(
                &state,
                "SELECT json_build_object('uuid', uuid, 'name', name, 'network', network)::text FROM (
                    SELECT uuid, name, network, 0 AS k, id FROM standalone_dockers WHERE server_id = $1
                    UNION ALL SELECT uuid, name, network, 1 AS k, id FROM swarm_dockers WHERE server_id = $1
                 ) d ORDER BY k, id",
                server,
            )
            .await?;
            if list.is_empty() {
                return Ok(toast_error(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "No destinations found on this server",
                ));
            }
            Ok(ok(json!({"items": list})).into_response())
        }
        // `loadProjects`.
        "projects" => {
            let list = rows(
                &state,
                "SELECT json_build_object('uuid', uuid, 'name', name, 'description', description)::text FROM projects WHERE team_id = $1 ORDER BY id",
                team,
            )
            .await?;
            if list.is_empty() {
                return Ok(toast_error(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "Please create a project first",
                ));
            }
            Ok(ok(json!({"items": list})).into_response())
        }
        // `loadEnvironments`.
        "environments" => {
            let list: Vec<String> = sqlx::query_scalar(
                "SELECT json_build_object('uuid', e.uuid, 'name', e.name, 'description', e.description)::text
                 FROM environments e JOIN projects p ON p.id = e.project_id
                 WHERE p.team_id = $1 AND p.uuid = $2 ORDER BY e.id",
            )
            .bind(team)
            .bind(id)
            .fetch_all(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            if list.is_empty() {
                return Ok(toast_error(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "No environments found in project",
                ));
            }
            let items: Vec<Value> = list
                .iter()
                .filter_map(|r| serde_json::from_str(r).ok())
                .collect();
            Ok(ok(json!({"items": items})).into_response())
        }
        _ => Err(ApiError::Message(
            axum::http::StatusCode::NOT_FOUND,
            "Not found.",
        )),
    }
}
