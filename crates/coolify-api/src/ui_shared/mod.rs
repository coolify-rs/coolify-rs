//! Page endpoints of `App\Livewire\Project\Shared\*`: the sections application, service and
//! database pages share. Every route takes a resource kind and uuid:
//! `/ui/resources/{kind}/{uuid}/...`, with `kind` one of `application`, `service`, `database`,
//! `service-application` and `service-database`.

pub mod envs;
pub mod secret_manager;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use coolify_db::applications as apps;
use coolify_db::databases as dbs;
use coolify_db::records::Record;
use coolify_db::services::{self as svc, SubKind};
use serde_json::Value;

use crate::auth::AuthCtx;
use crate::response::ApiError;
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .merge(envs::routes())
        .merge(secret_manager::routes())
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// Which model a resource is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Application,
    Service,
    Database,
    ServiceApplication,
    ServiceDatabase,
}

/// A resource as the shared sections read it.
#[derive(Debug, Clone)]
pub struct Res {
    pub class: Class,
    /// `getMorphClass()`.
    pub morph: String,
    /// `$resource->type()`.
    pub type_name: String,
    pub table: &'static str,
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub team_id: i64,
    pub project_id: Option<i64>,
    pub project_uuid: Option<String>,
    pub environment_id: Option<i64>,
    pub environment_uuid: Option<String>,
    pub server_id: Option<i64>,
    pub raw: Record,
    /// `application_settings` of an application.
    pub settings: Option<Record>,
}

impl Res {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(Value::as_i64)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn is_application(&self) -> bool {
        self.class == Class::Application
    }
    pub fn is_service(&self) -> bool {
        self.class == Class::Service
    }
    pub fn setting(&self, key: &str) -> Option<&Value> {
        self.settings.as_ref().and_then(|s| s.get(key))
    }
    pub fn setting_bool(&self, key: &str) -> bool {
        self.setting(key).and_then(Value::as_bool).unwrap_or(false)
    }
    /// `$resource->docker_compose_raw ?? $resource->docker_compose`.
    pub fn compose_raw(&self) -> Option<&str> {
        self.str("docker_compose_raw")
            .or_else(|| self.str("docker_compose"))
    }
    /// `$resource->type() === 'service' || $resource?->build_pack === 'dockercompose'`.
    pub fn is_compose(&self) -> bool {
        self.is_service() || self.str("build_pack") == Some("dockercompose")
    }
}

pub const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Resource not found.");

fn id_of(r: &Record) -> Option<i64> {
    r.get("id").and_then(Value::as_i64)
}

fn uuid_of(r: &Record) -> Option<String> {
    r.get("uuid").and_then(Value::as_str).map(str::to_owned)
}

/// Finds a resource of the session team by kind and uuid (`ownedByCurrentTeam`).
pub async fn find(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Res, ApiError> {
    let db = &state.db;
    match kind {
        "application" => {
            let a = apps::find_in_team(db, ctx.team_id, uuid)
                .await?
                .ok_or(NOT_FOUND)?;
            Ok(Res {
                class: Class::Application,
                morph: apps::MORPH.to_owned(),
                type_name: "application".to_owned(),
                table: "applications",
                id: a.id,
                uuid: a.uuid.clone(),
                name: a.str("name").unwrap_or_default().to_owned(),
                team_id: a.team_id,
                project_id: id_of(&a.project),
                project_uuid: uuid_of(&a.project),
                environment_id: id_of(&a.environment),
                environment_uuid: uuid_of(&a.environment),
                server_id: a.server_id(),
                raw: a.raw.clone(),
                settings: Some(a.settings.clone()),
            })
        }
        "service" => {
            let s = svc::find_in_team(db, ctx.team_id, uuid)
                .await?
                .ok_or(NOT_FOUND)?;
            Ok(Res {
                class: Class::Service,
                morph: svc::MORPH.to_owned(),
                type_name: "service".to_owned(),
                table: "services",
                id: s.id,
                uuid: s.uuid.clone(),
                name: s.name().to_owned(),
                team_id: s.team_id,
                project_id: id_of(&s.project),
                project_uuid: uuid_of(&s.project),
                environment_id: id_of(&s.environment),
                environment_uuid: uuid_of(&s.environment),
                server_id: s.server_id(),
                raw: s.raw.clone(),
                settings: None,
            })
        }
        "database" => {
            let d = dbs::find_in_team(db, ctx.team_id, uuid)
                .await?
                .ok_or(NOT_FOUND)?;
            Ok(Res {
                class: Class::Database,
                morph: d.kind.morph().to_owned(),
                type_name: d.kind.type_name().to_owned(),
                table: d.kind.table(),
                id: d.id,
                uuid: d.uuid.clone(),
                name: d
                    .raw
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                team_id: d.team_id,
                project_id: id_of(&d.project),
                project_uuid: uuid_of(&d.project),
                environment_id: id_of(&d.environment),
                environment_uuid: uuid_of(&d.environment),
                server_id: d.server_id(),
                raw: d.raw.clone(),
                settings: None,
            })
        }
        "service-application" | "service-database" => {
            let sub_kind = if kind == "service-application" {
                SubKind::Application
            } else {
                SubKind::Database
            };
            let service_id = coolify_db::resource_envs::sub_service_id(db, sub_kind.table(), uuid)
                .await?
                .ok_or(NOT_FOUND)?;
            let s = svc::find_by_id(db, service_id)
                .await?
                .filter(|s| s.team_id == ctx.team_id)
                .ok_or(NOT_FOUND)?;
            let sub = svc::sub_by_uuid(db, sub_kind, service_id, uuid)
                .await?
                .ok_or(NOT_FOUND)?;
            Ok(Res {
                class: if sub_kind == SubKind::Application {
                    Class::ServiceApplication
                } else {
                    Class::ServiceDatabase
                },
                morph: sub_kind.morph().to_owned(),
                type_name: sub_kind.sub_type().to_owned(),
                table: sub_kind.table(),
                id: sub.id,
                uuid: sub.uuid.clone(),
                name: sub.name.clone(),
                team_id: s.team_id,
                project_id: id_of(&s.project),
                project_uuid: uuid_of(&s.project),
                environment_id: id_of(&s.environment),
                environment_uuid: uuid_of(&s.environment),
                server_id: s.server_id(),
                raw: sub.raw.clone(),
                settings: None,
            })
        }
        _ => Err(NOT_FOUND),
    }
}
