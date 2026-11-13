//! Stars: `App\Livewire\StarToggle` (the star button of a resource or server page) and the star
//! methods of `App\Livewire\Dashboard` (`starredResources()`, `unstar()`, `reorderStars()`).
//!
//! A star belongs to one user. The button names its resource by kind and uuid; the handlers
//! resolve it inside the session team and check `view` before they read or write the star.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use coolify_db::stars::{self, StarKind, Starrable, StarredResource};
use coolify_domain::AccessLevel;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/stars/reorder", post(reorder))
        .route("/ui/stars/star/{star_id}", delete(unstar))
        .route("/ui/stars/resource/{kind}/{uuid}", get(show))
        .route("/ui/stars/resource/{kind}/{uuid}/toggle", post(toggle))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Resource not found.");

/// The resource of a star button: in the session team and visible to the user. A server needs
/// membership of its team (`ServerPolicy::view`), anything else read access
/// (`canAccess(AccessLevel::Read, $resource)`). What the user cannot see answers 404, as the
/// `ScopedByAccessGrants` scope makes `findOrFail` do.
async fn starrable(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Starrable, ApiError> {
    // `abort_unless(in_array($this->starrableType, Star::STARRABLE_TYPES, true), 404)`.
    let kind = StarKind::parse(kind).ok_or(NOT_FOUND)?;
    let resource = stars::find_starrable(&state.db, ctx.team_id, kind, uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if kind != StarKind::Server
        && !ctx.can_resource(AccessLevel::Read, resource.environment_id, &resource.uuid)
    {
        return Err(NOT_FOUND);
    }
    Ok(resource)
}

/// `StarToggle::mount()`: whether the user starred the resource.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let resource = starrable(&state, &ctx, &kind, &uuid).await?;
    let starred = stars::is_starred(&state.db, ctx.user_id, resource.morph, resource.id).await?;
    Ok(ok(json!({"starred": starred})).into_response())
}

/// `StarToggle::toggle()`.
async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let resource = starrable(&state, &ctx, &kind, &uuid).await?;
    let starred = stars::toggle(
        &state.db,
        ctx.user_id,
        resource.morph,
        resource.id,
        state.now(),
    )
    .await?;
    Ok(ok(json!({"starred": starred})).into_response())
}

/// `Dashboard::unstar()`: only a star of the user is deleted; any other id changes nothing.
async fn unstar(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(star_id): Path<String>,
) -> ApiResult {
    if let Ok(star_id) = star_id.parse::<i64>() {
        stars::unstar(&state.db, ctx.user_id, star_id).await?;
    }
    Ok(ok(json!({"message": "Star removed."})).into_response())
}

/// `Dashboard::reorderStars()`.
async fn reorder(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Value>,
) -> ApiResult {
    let ids = star_ids(&body);
    stars::reorder(&state.db, ctx.user_id, &ids, state.now()).await?;
    Ok(ok(json!({"message": "Order saved."})).into_response())
}

/// The `star_ids` list of a reorder request. PHP casts each entry with `(int)`: numbers and
/// numeric strings count, anything else is 0, which is no star. The index of an entry is its
/// position, so entries are never dropped.
fn star_ids(body: &Value) -> Vec<i64> {
    body.get("star_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .map(|id| match id {
                    Value::Number(n) => n.as_i64().unwrap_or(0),
                    Value::String(s) => s.trim().parse().unwrap_or(0),
                    _ => 0,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `context` line of a starred card: `Server`, or `{project} / {environment}`.
fn context(star: &StarredResource) -> String {
    if star.is_server() {
        return "Server".to_owned();
    }
    format!(
        "{} / {}",
        star.project_name.as_deref().unwrap_or_default(),
        star.environment_name.as_deref().unwrap_or_default()
    )
}

/// `type` of a starred card for the page: which icon and logo it shows.
fn card_type(star: &StarredResource) -> &'static str {
    if star.is_server() {
        "server"
    } else if star.starrable_type == coolify_db::applications::MORPH {
        "application"
    } else if star.starrable_type == coolify_db::services::MORPH {
        "service"
    } else {
        "database"
    }
}

/// `route('server.show')` for a server, `$resource->link()` for anything else.
fn href(star: &StarredResource) -> String {
    if star.is_server() {
        return format!("/server/{}", star.uuid);
    }
    format!(
        "/project/{}/environment/{}/{}/{}",
        star.project_uuid.as_deref().unwrap_or_default(),
        star.environment_uuid.as_deref().unwrap_or_default(),
        card_type(star),
        star.uuid
    )
}

/// `Dashboard::starredResources()`: the stars of the user on resources of the session team that the
/// user can still see, in the order the user chose. `shows_team_sections` is `$showsTeamSections`:
/// members limited to some projects see no servers.
pub async fn starred_resources(
    state: &AppState,
    ctx: &AuthCtx,
    shows_team_sections: bool,
) -> Result<Vec<Value>, ApiError> {
    let mut out = Vec::new();
    for star in stars::starred(&state.db, ctx.user_id).await? {
        if star.team_id != Some(ctx.team_id) {
            continue;
        }
        let visible = if star.is_server() {
            shows_team_sections
        } else {
            ctx.can_resource(AccessLevel::Read, star.environment_id, &star.uuid)
        };
        if !visible {
            continue;
        }
        // What `<x-resource-logo>` reads of the resource (`resource_logo()`).
        let kind = card_type(&star);
        let application = if kind == "application" {
            stars::application_logo(&state.db, star.starrable_id)
                .await?
                .map(|(build_pack, source_type, fqdn, basic_auth)| {
                    json!({
                        "build_pack": build_pack,
                        "source_type": source_type,
                        "fqdn": fqdn,
                        "is_http_basic_auth_enabled": basic_auth,
                    })
                })
        } else {
            None
        };
        let service = if kind == "service" {
            let logo = coolify_db::services::find_by_id(&state.db, star.starrable_id)
                .await?
                .map_or(Value::Null, |s| crate::ui_services::resource_logo(&s));
            Some(json!({"logo": logo}))
        } else {
            None
        };
        out.push(json!({
            "star_id": star.star_id,
            "type": kind,
            "name": star.name,
            "context": context(&star),
            "href": href(&star),
            "database_type": star.database_type(),
            "application": application,
            "service": service,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use coolify_db::stars::{SERVER_MORPH, StarredResource};
    use serde_json::json;

    use super::{card_type, context, href, star_ids};

    fn star(morph: &'static str) -> StarredResource {
        StarredResource {
            star_id: 1,
            position: 0,
            starrable_type: morph,
            starrable_id: 7,
            uuid: "res".into(),
            name: "Thing".into(),
            team_id: Some(1),
            environment_id: Some(3),
            environment_uuid: Some("env".into()),
            environment_name: Some("production".into()),
            project_uuid: Some("proj".into()),
            project_name: Some("Web".into()),
        }
    }

    #[test]
    fn a_server_card_links_to_the_server_and_says_server() {
        let mut s = star(SERVER_MORPH);
        s.environment_id = None;
        s.environment_uuid = None;
        s.environment_name = None;
        s.project_uuid = None;
        s.project_name = None;
        assert_eq!(card_type(&s), "server");
        assert_eq!(context(&s), "Server");
        assert_eq!(href(&s), "/server/res");
        assert_eq!(s.database_type(), None);
    }

    #[test]
    fn resource_cards_link_like_link_and_name_project_and_environment() {
        let app = star("App\\Models\\Application");
        assert_eq!(card_type(&app), "application");
        assert_eq!(context(&app), "Web / production");
        assert_eq!(href(&app), "/project/proj/environment/env/application/res");

        let service = star("App\\Models\\Service");
        assert_eq!(card_type(&service), "service");
        assert_eq!(href(&service), "/project/proj/environment/env/service/res");

        let database = star("App\\Models\\StandaloneKeydb");
        assert_eq!(card_type(&database), "database");
        assert_eq!(
            href(&database),
            "/project/proj/environment/env/database/res"
        );
        assert_eq!(database.database_type(), Some("standalone-keydb"));
    }

    #[test]
    fn star_ids_keep_every_position() {
        assert_eq!(
            star_ids(&json!({"star_ids": [5, "9", "x", null]})),
            vec![5, 9, 0, 0]
        );
        assert_eq!(star_ids(&json!({"star_ids": "5"})), Vec::<i64>::new());
        assert_eq!(star_ids(&json!({})), Vec::<i64>::new());
    }
}
