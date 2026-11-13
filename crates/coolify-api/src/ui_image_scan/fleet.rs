//! `App\Livewire\ImageScan\FleetVulnerabilities`: the team Security page (spec 15.8): the
//! resources of the session team the user can see with their stored security status, most
//! urgent first, and a search by vulnerability ID or package across their current scans.
//! Resources that were not scanned are hidden until the user shows them or filters by "Not
//! checked".
//!
//! Also the security status of one resource for the resource menus (the security dot of
//! `<x-image-scan.security-dot>`, `SecurityStatus::stored($resource)`).

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::NaiveDateTime;
use coolify_db::databases::Kind as DatabaseKind;
use coolify_domain::grants::AccessLevel;
use coolify_jobs::image_scan::views::team::{
    self, MAX_DECODED_SCANS, STATE_LABELS, STATUS_ORDER, SearchMatch, TeamEntry,
    TeamVulnerabilities,
};
use coolify_jobs::image_scan::views::{self as views, SecurityStatusType, export, status};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    count_text, day_date_time, download, internal, jobs, load_resource, number_format, slug_or,
    strnatcasecmp,
};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::vulnerabilities::can_access;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/vulnerabilities", post(render))
        .route("/ui/vulnerabilities/export", post(export_list))
        .route("/ui/image-scan/{kind}/{uuid}/status", get(security_status))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct FleetState {
    search: String,
    servers: Vec<String>,
    projects: Vec<String>,
    statuses: Vec<String>,
    show_not_checked: bool,
    page: usize,
    per_page: usize,
    format: String,
}

/// `listing()`.
struct Listing {
    fleet: TeamVulnerabilities,
    in_scope: Vec<TeamEntry>,
    rows: Vec<(TeamEntry, Option<SearchMatch>)>,
    searching: bool,
    server_options: Vec<(String, String)>,
    project_options: Vec<(String, String)>,
}

fn rank(status: SecurityStatusType) -> usize {
    STATUS_ORDER
        .iter()
        .position(|s| *s == status.value())
        .unwrap_or(STATUS_ORDER.len())
}

/// `byStatus()`: by status, then the resource name.
fn by_status(mut entries: Vec<TeamEntry>) -> Vec<TeamEntry> {
    entries.sort_by(|a, b| {
        rank(a.security.status)
            .cmp(&rank(b.security.status))
            .then_with(|| strnatcasecmp(&a.resource.name(), &b.resource.name()))
    });
    entries
}

/// `normalizeFilters()` and the options of the filter (`listing()`).
async fn listing(
    state: &AppState,
    ctx: &AuthCtx,
    list: &mut FleetState,
) -> Result<Listing, ApiError> {
    let jobs = jobs(state)?;
    let unique = |values: &mut Vec<String>| {
        let mut seen = Vec::new();
        values.retain(|v| {
            if seen.contains(v) {
                false
            } else {
                seen.push(v.clone());
                true
            }
        });
    };
    unique(&mut list.servers);
    unique(&mut list.projects);
    unique(&mut list.statuses);
    list.statuses.retain(|s| STATUS_ORDER.contains(&s.as_str()));

    let mut fleet = TeamVulnerabilities::load(jobs, ctx.team_id)
        .await
        .map_err(internal)?;
    // Upstream's models hide what a member or token limited to some projects cannot see.
    fleet.retain(|entry| can_access(ctx, &entry.resource, AccessLevel::Read));

    let mut server_options: Vec<(String, String)> = Vec::new();
    let mut project_options: Vec<(String, String)> = Vec::new();
    for entry in &fleet.entries {
        if let Some(server) = &entry.server
            && !server_options.iter().any(|(u, _)| *u == server.uuid)
        {
            server_options.push((server.uuid.clone(), server.name.clone()));
        }
        if let Some(place) = &entry.place
            && !project_options
                .iter()
                .any(|(u, _)| *u == place.project_uuid)
        {
            project_options.push((place.project_uuid.clone(), place.project_name.clone()));
        }
    }
    server_options.sort_by(|a, b| strnatcasecmp(&a.1, &b.1));
    project_options.sort_by(|a, b| strnatcasecmp(&a.1, &b.1));
    list.servers
        .retain(|u| server_options.iter().any(|(o, _)| o == u));
    list.projects
        .retain(|u| project_options.iter().any(|(o, _)| o == u));

    let in_scope: Vec<TeamEntry> = fleet
        .entries
        .iter()
        .filter(|e| {
            (list.servers.is_empty()
                || e.server
                    .as_ref()
                    .is_some_and(|s| list.servers.contains(&s.uuid)))
                && (list.projects.is_empty()
                    || e.place
                        .as_ref()
                        .is_some_and(|p| list.projects.contains(&p.project_uuid)))
        })
        .cloned()
        .collect();
    let filtered: Vec<TeamEntry> = in_scope
        .iter()
        .filter(|e| {
            if list.statuses.is_empty() {
                list.show_not_checked || e.security.status != SecurityStatusType::NotChecked
            } else {
                list.statuses.iter().any(|s| s == e.security.status.value())
            }
        })
        .cloned()
        .collect();
    let searching = !list.search.trim().is_empty();
    let rows = if searching {
        let scanned: Vec<TeamEntry> = filtered.into_iter().filter(|e| e.scanned).collect();
        fleet
            .search(jobs, &scanned, &list.search, MAX_DECODED_SCANS)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|(entry, found)| (entry, Some(found)))
            .collect()
    } else {
        by_status(filtered)
            .into_iter()
            .map(|entry| (entry, None))
            .collect()
    };
    Ok(Listing {
        fleet,
        in_scope,
        rows,
        searching,
        server_options,
        project_options,
    })
}

/// `notScannedSentence($resources, $servers)`.
pub(crate) fn not_scanned_sentence(resources: usize, servers: usize) -> String {
    let mut subject = count_text(resources, "resource", "resources");
    if servers > 0 {
        subject = format!("{subject} on {}", count_text(servers, "server", "servers"));
    }
    format!(
        "{subject}{} not scanned, so Coolify cannot tell.",
        if resources == 1 { " was" } else { " were" }
    )
}

fn badge_type(status: SecurityStatusType) -> Option<&'static str> {
    match status {
        SecurityStatusType::UpdateNow => Some("error"),
        SecurityStatusType::UpdateAvailable => Some("warning"),
        SecurityStatusType::NoFix | SecurityStatusType::CheckNeeded => Some("neutral"),
        _ => None,
    }
}

/// What `<x-resource-logo :resource>` needs of a resource.
fn logo(entry: &TeamEntry) -> Value {
    let resource = &entry.resource;
    if resource.is_application() {
        let s = |k: &str| resource.str(k).map(str::to_owned);
        return json!({"application": {
            "buildPack": s("build_pack"),
            "sourceType": s("source_type"),
            "fqdn": s("fqdn"),
            "basicAuth": resource.record.get("is_http_basic_auth_enabled").and_then(Value::as_bool).unwrap_or(false),
        }});
    }
    if resource.is_service() {
        return json!({"service": {"logo": Value::Null}});
    }
    let database_type = DatabaseKind::ALL
        .iter()
        .find(|k| k.morph() == resource.morph)
        .map(|k| k.type_name())
        .unwrap_or_default();
    json!({"databaseType": database_type})
}

fn checked_at(entry: &TeamEntry) -> Option<NaiveDateTime> {
    entry
        .security
        .checked_at
        .or_else(|| entry.scans.iter().filter_map(|s| s.scanned_at).max())
}

fn entry_value(entry: &TeamEntry, found: Option<&SearchMatch>, now: NaiveDateTime) -> Value {
    let security = &entry.security;
    let status = security.status;
    let checked = checked_at(entry);
    json!({
        "key": entry.key,
        "name": entry.resource.name(),
        "link": entry.link,
        "logo": logo(entry),
        "project_name": entry.place.as_ref().map(|p| p.project_name.clone()),
        "environment_name": entry.place.as_ref().map(|p| p.environment_name.clone()),
        "server_name": entry.server.as_ref().map(|s| s.name.clone()),
        "status": status.value(),
        "status_label": team::status_label(status),
        "badge_type": badge_type(status),
        "images_line": team::images_line(security, entry.images),
        "action": if found.is_some() { None } else { team::action(security, now) },
        "checked_at": checked.map(|t| json!({
            "human": views::diff_for_humans(t, now),
            "title": day_date_time(t),
        })),
        "match": found.map(|m| json!({
            "id": m.id,
            "package": m.package,
            "version": m.version,
            "fixed_in": m.fixed_in,
            "severity": m.severity,
            "state": m.state,
            "state_label": team::state_label(&m.state),
            "more": m.more,
            "more_text": number_format(m.more),
        })),
    })
}

/// `render()`.
async fn render(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(mut list): Json<FleetState>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let listing = listing(&state, &ctx, &mut list).await?;
    let now = views::now();
    let not_scanned: Vec<&TeamEntry> = listing
        .in_scope
        .iter()
        .filter(|e| e.security.status == SecurityStatusType::NotChecked)
        .collect();
    let mut not_scanned_servers: Vec<i64> = Vec::new();
    for e in &not_scanned {
        if let Some(s) = &e.server
            && !not_scanned_servers.contains(&s.id)
        {
            not_scanned_servers.push(s.id);
        }
    }
    let status_options: Vec<Value> = STATUS_ORDER
        .iter()
        .map(|s| {
            let count = listing
                .in_scope
                .iter()
                .filter(|e| e.security.status.value() == *s)
                .count();
            let label = SecurityStatusType::from_value(s)
                .map(team::status_label)
                .unwrap_or_default();
            json!({"value": s, "label": format!("{label} ({})", number_format(count))})
        })
        .collect();
    let per_page = if list.per_page == 0 {
        25
    } else {
        list.per_page.clamp(1, 100)
    };
    let total = listing.rows.len();
    let last_page = total.div_ceil(per_page).max(1);
    let page = list.page.clamp(1, last_page);
    let items: Vec<Value> = listing
        .rows
        .iter()
        .skip((page - 1) * per_page)
        .take(per_page)
        .map(|(entry, found)| entry_value(entry, found.as_ref(), now))
        .collect();
    let servers_not_scanning = if ctx.is_team_admin() {
        team::servers_not_scanning(jobs, ctx.team_id)
            .await
            .map_err(internal)?
    } else {
        0
    };
    let has_filters =
        !list.servers.is_empty() || !list.projects.is_empty() || !list.statuses.is_empty();
    Ok(ok(json!({
        "team_id": ctx.team_id,
        "items": items,
        "pagination": {"total": total, "page": page, "per_page": per_page, "last_page": last_page},
        "shown_count": total,
        "searching": listing.searching,
        "not_scanned_line": (!not_scanned.is_empty())
            .then(|| not_scanned_sentence(not_scanned.len(), not_scanned_servers.len())),
        // The toggle only matters while no status filter is set.
        "not_checked_toggle": if not_scanned.is_empty() || !list.statuses.is_empty() || listing.searching {
            None
        } else if list.show_not_checked {
            Some("Hide them")
        } else {
            Some("Show them")
        },
        "hides_not_checked": list.statuses.is_empty() && !list.show_not_checked && !not_scanned.is_empty(),
        "search_capped": listing.searching && listing.fleet.search_was_capped(),
        "max_decoded_scans": MAX_DECODED_SCANS,
        "servers_not_scanning": servers_not_scanning,
        "server_options": listing.server_options.iter().map(|(u, n)| json!({"value": u, "label": n})).collect::<Vec<_>>(),
        "project_options": listing.project_options.iter().map(|(u, n)| json!({"value": u, "label": n})).collect::<Vec<_>>(),
        "status_options": status_options,
        "has_filters": has_filters,
        "has_resources": !listing.fleet.entries.is_empty(),
        "servers": list.servers,
        "projects": list.projects,
        "statuses": list.statuses,
        "state_labels": STATE_LABELS.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<String, Value>>(),
    }))
    .into_response())
}

/// `copyList('markdown')` and `downloadList($format)`.
async fn export_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(mut list): Json<FleetState>,
) -> ApiResult {
    if !["markdown", "csv", "json"].contains(&list.format.as_str()) {
        return Err(super::not_found());
    }
    let format = list.format.clone();
    let listing = listing(&state, &ctx, &mut list).await?;
    let now = views::now();
    let base_url = crate::server_transfer::instance_url(&state.db).await;
    let entries = export::fleet_entries(&listing.rows, listing.searching, &base_url, now);
    let team_name = coolify_db::teams::get(&state.db, ctx.team_id)
        .await?
        .and_then(|t| t.get("name").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    match format.as_str() {
        "markdown" => {
            let count = count_text(listing.rows.len(), "resource", "resources");
            let title = if listing.searching {
                format!("Resources with {} ({count})", list.search.trim())
            } else {
                format!("Security of {team_name} ({count})")
            };
            Ok(
                ok(json!({"content": export::fleet_markdown(&title, &entries, listing.searching)}))
                    .into_response(),
            )
        }
        format => {
            let content = if format == "csv" {
                export::fleet_csv(&entries, listing.searching)
            } else {
                let mut meta = Map::new();
                meta.insert("team".to_owned(), json!(team_name));
                meta.insert(
                    "search".to_owned(),
                    json!(listing.searching.then(|| list.search.trim().to_owned())),
                );
                export::fleet_json(meta, &entries, now)
            };
            let file = format!(
                "{}-security-{}.{format}",
                slug_or(&team_name, "team"),
                now.format("%Y-%m-%d")
            );
            let mime = if format == "csv" {
                "text/csv; charset=UTF-8"
            } else {
                "application/json"
            };
            download(file, mime, content)
        }
    }
}

/// `SecurityStatus::stored($resource)->status->value`: the security dot of a resource menu.
async fn security_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    let security = status::stored(jobs, &resource).await.map_err(internal)?;
    Ok(ok(json!({"status": security.status.value()})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_scanned_sentences() {
        assert_eq!(
            not_scanned_sentence(1, 0),
            "1 resource was not scanned, so Coolify cannot tell."
        );
        assert_eq!(
            not_scanned_sentence(3, 2),
            "3 resources on 2 servers were not scanned, so Coolify cannot tell."
        );
        assert_eq!(
            not_scanned_sentence(2, 1),
            "2 resources on 1 server were not scanned, so Coolify cannot tell."
        );
    }

    #[test]
    fn badge_types_follow_the_status() {
        assert_eq!(badge_type(SecurityStatusType::UpdateNow), Some("error"));
        assert_eq!(badge_type(SecurityStatusType::Ok), None);
        assert!(rank(SecurityStatusType::UpdateNow) < rank(SecurityStatusType::NotChecked));
    }
}
