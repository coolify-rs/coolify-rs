//! The browser side of the terminal.
//!
//! - `App\Livewire\Terminal\Index`: the target list of /terminal.
//! - `Project\Shared\ExecuteContainerCommand`: the terminal page of a resource or server.
//! - `Project\Shared\Terminal::sendTerminalCommand`: authorises a target and issues a token.
//! - `App\Services\TerminalSessionService`: the tokens and the remote commands.
//!
//! The websocket itself (upstream's `docker/coolify-terminal`) is in [`crate::terminal_ws`].

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::servers::{self, ServerRow};
use coolify_db::services::{self as svc, SubKind};
use coolify_jobs::file_browser::is_valid_container_name;
use coolify_jobs::server::ServerInfo;
use coolify_jobs::server_ops::Srv;
use coolify_jobs::server_ops::proxy::container_status;
use coolify_remote::RunOptions;
use coolify_remote::Target;
use coolify_remote::shell::quote;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::toast_error;
use crate::ui_shared::{Class, Res, find};

/// `CanAccessTerminal`.
const ADMINS_ONLY: &str = "Access to terminal functionality is restricted to team administrators";
/// `Terminal::NOT_ALLOWED_MESSAGE`: the same text for denied and unknown targets.
const NOT_ALLOWED: &str = "You are not allowed to open a terminal for this resource.";
/// `TerminalSessionService::TOKEN_TTL_SECONDS`.
const TOKEN_TTL: Duration = Duration::from_secs(60);
/// `constants.terminal.detach_keys`.
const DETACH_KEYS: &str = "ctrl-p,ctrl-q";
/// `constants.terminal.console_history_lines`.
const CONSOLE_HISTORY_LINES: u32 = 200;

/// `TerminalSessionService::issue()`, what `Cache::put` stores.
#[derive(Debug, Clone)]
struct Issued {
    created: Instant,
    user_id: i64,
    team_id: i64,
    server_uuid: String,
    container: Option<String>,
    attach: bool,
}

static TOKENS: LazyLock<Mutex<HashMap<String, Issued>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/terminal", get(servers_list))
        .route("/ui/terminal/containers", get(containers))
        .route("/ui/terminal/session", post(session))
        .route("/ui/terminal/keep-alive", post(keep_alive))
        .route("/ui/resources/{kind}/{uuid}/terminal", get(mount))
        .route(
            "/ui/resources/{kind}/{uuid}/terminal/containers",
            post(load_containers),
        )
        .route(
            "/ui/resources/{kind}/{uuid}/terminal/connect",
            post(connect),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
        .route("/terminal/ws", get(crate::terminal_ws::upgrade))
}

/// `Server::isReachable()->get()->filter(isTerminalEnabled)`.
async fn targets(
    state: &AppState,
    team: i64,
) -> Result<Vec<(i64, String, String, bool)>, ApiError> {
    sqlx::query_as(
        "SELECT v.id, v.uuid, v.name, (ss.is_reachable AND ss.is_usable AND NOT ss.force_disabled)
         FROM servers v JOIN server_settings ss ON ss.server_id = v.id
         WHERE v.team_id = $1 AND v.deleted_at IS NULL AND ss.is_reachable AND ss.is_terminal_enabled
         ORDER BY v.name",
    )
    .bind(team)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)
}

fn admins_only() -> axum::response::Response {
    toast_error(StatusCode::FORBIDDEN, ADMINS_ONLY)
}

/// `mount`, behind `CanAccessTerminal`.
async fn servers_list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(admins_only());
    }
    let list = targets(&state, ctx.team_id).await?;
    Ok(ok(json!({
        "servers": list.iter().map(|(_, uuid, name, _)| json!({"uuid": uuid, "name": name})).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `loadContainers`: the running containers of each functional server, sorted by name.
async fn containers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(admins_only());
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut out: Vec<Value> = Vec::new();
    for (id, uuid, _, functional) in targets(&state, ctx.team_id).await? {
        if !functional {
            continue;
        }
        let Some(row) = servers::find_by_id(&state.db, id).await? else {
            continue;
        };
        let text = jobs
            .instant_remote(
                &row,
                &["docker ps -a --format '{{json .}}'".to_owned()],
                true,
                "terminal.containers",
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .unwrap_or_default();
        for line in text.lines() {
            let Ok(c) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let state_text = c
                .get("State")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            if !state_text.contains("running") {
                continue;
            }
            let name = c.get("Names").and_then(Value::as_str).unwrap_or_default();
            out.push(json!({
                "name": name,
                "connection_name": name,
                "uuid": format!("{uuid}:{name}"),
                "status": state_text,
                "server_uuid": uuid,
            }));
        }
    }
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    Ok(ok(json!({"containers": out})).into_response())
}

/// `keepTerminalPageAlive`: the guard in front of it has already renewed the session.
async fn keep_alive() -> StatusCode {
    StatusCode::NO_CONTENT
}

fn terminal_enabled(row: &ServerRow) -> bool {
    ServerInfo::from_row(row).setting_bool("is_terminal_enabled") == Some(true)
}

fn force_disabled(row: &ServerRow) -> bool {
    ServerInfo::from_row(row).setting_bool("force_disabled") == Some(true)
}

fn name_of(row: &ServerRow) -> &str {
    row.record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The refusal of `failTerminalSession()`.
fn failed(message: &str) -> Value {
    json!({"ok": false, "message": message})
}

/// `$server->additional_servers` of an application.
async fn additional_servers(state: &AppState, res: &Res) -> Result<Vec<ServerRow>, ApiError> {
    if res.class != Class::Application {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT server_id FROM additional_destinations WHERE application_id = $1 ORDER BY id",
    )
    .bind(res.id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let mut rows = Vec::new();
    for id in ids {
        if let Some(row) = servers::find_by_id(&state.db, id).await? {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The resource of `ExecuteContainerCommand::mount()`; `None` for a server.
enum Subject {
    Resource(Box<Res>),
    Server(Box<ServerRow>),
}

async fn subject(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<Subject, ApiError> {
    if kind == "server" {
        return servers::find(&state.db, ctx.team_id, uuid)
            .await?
            .map(|row| Subject::Server(Box::new(row)))
            .ok_or(crate::ui_shared::NOT_FOUND);
    }
    let res = find(state, ctx, kind, uuid).await?;
    if !matches!(
        res.class,
        Class::Application | Class::Service | Class::Database
    ) {
        return Err(crate::ui_shared::NOT_FOUND);
    }
    Ok(Subject::Resource(Box::new(res)))
}

/// `$this->servers` after `mount()`: functional servers (a server page always lists itself), the
/// ones with terminal access first.
async fn subject_servers(state: &AppState, subject: &Subject) -> Result<Vec<ServerRow>, ApiError> {
    let mut rows: Vec<ServerRow> = match subject {
        Subject::Server(row) => vec![(**row).clone()],
        Subject::Resource(res) => {
            let mut rows = Vec::new();
            if let Some(id) = res.server_id
                && let Some(row) = servers::find_by_id(&state.db, id).await?
            {
                rows.push(row);
            }
            rows.extend(additional_servers(state, res).await?);
            rows.retain(|r| ServerInfo::from_row(r).is_functional());
            rows
        }
    };
    rows.sort_by_key(|r| !terminal_enabled(r));
    Ok(rows)
}

/// `mount()` and what the view reads from it.
async fn mount(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(admins_only());
    }
    let subject = subject(&state, &ctx, &kind, &uuid).await?;
    let list = subject_servers(&state, &subject).await?;
    let name = match &subject {
        Subject::Server(row) => name_of(row).to_owned(),
        Subject::Resource(res) => res.name.clone(),
    };
    Ok(ok(json!({
        "type": kind,
        "name": name,
        "servers": list.iter().map(|r| json!({
            "uuid": r.uuid,
            "name": name_of(r),
            "terminal_enabled": terminal_enabled(r),
            "functional": ServerInfo::from_row(r).is_functional(),
        })).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `$this->containers` from `loadContainers()`: (server, container name), sorted by name.
async fn resource_containers(
    state: &AppState,
    res: &Res,
    list: &[ServerRow],
) -> Result<Vec<(ServerRow, String)>, ApiError> {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let mut out: Vec<(ServerRow, String)> = Vec::new();
    for row in list {
        let info = ServerInfo::from_row(row);
        match res.class {
            Class::Application => {
                // A Swarm entry has no `State`, so it never counts as running.
                if info.is_swarm() || !terminal_enabled(row) {
                    continue;
                }
                let t = info.target(jobs).await.map_err(|e| {
                    tracing::warn!(error = %e, "terminal: no connection to the server");
                    ApiError::Internal
                })?;
                let found = jobs
                    .application_containers(&t, &res.uuid, true)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "terminal: containers could not be read");
                        ApiError::Internal
                    })?;
                for c in found {
                    if c.get("State").and_then(Value::as_str) == Some("running")
                        && let Some(name) = c.get("Names").and_then(Value::as_str)
                    {
                        out.push((row.clone(), name.to_owned()));
                    }
                }
            }
            Class::Database => {
                let running = res.str("status").is_some_and(|s| s.contains("running"));
                if running && terminal_enabled(row) {
                    out.push((row.clone(), res.uuid.clone()));
                }
            }
            Class::Service => {
                for kind in [SubKind::Application, SubKind::Database] {
                    for sub in svc::subs(&state.db, kind, res.id).await? {
                        let running = sub
                            .raw
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s.contains("running"));
                        if running && terminal_enabled(row) {
                            out.push((row.clone(), format!("{}-{}", sub.name, res.uuid)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// `loadContainers()`.
async fn load_containers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(admins_only());
    }
    let subject = subject(&state, &ctx, &kind, &uuid).await?;
    let Subject::Resource(res) = &subject else {
        return Ok(ok(json!({"containers": []})).into_response());
    };
    let list = subject_servers(&state, &subject).await?;
    let found = resource_containers(&state, res, &list).await?;
    Ok(ok(json!({
        "containers": found.iter().map(|(row, name)| json!({
            "name": name,
            "server_uuid": row.uuid,
            "server_name": name_of(row),
            "value": format!("{}:{name}", row.uuid),
            "label": format!("{name} · {}", name_of(row)),
        })).collect::<Vec<_>>(),
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
struct ConnectBody {
    #[serde(default)]
    selected_container: Option<String>,
    #[serde(default)]
    requested_mode: Option<String>,
}

/// `connectToServer()` on a server page, `connectToContainer()` elsewhere, then the
/// `send-terminal-command` they dispatch.
async fn connect(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<ConnectBody>,
) -> ApiResult {
    // `authorize('canAccessTerminal')`, reported as `sessionFailureMessage()`.
    if !ctx.is_team_admin() {
        return Ok(ok(failed(NOT_ALLOWED)).into_response());
    }
    let Ok(subject) = subject(&state, &ctx, &kind, &uuid).await else {
        return Ok(ok(failed(NOT_ALLOWED)).into_response());
    };
    let list = subject_servers(&state, &subject).await?;
    let res = match &subject {
        Subject::Server(_) => {
            let Some(server) = list.first() else {
                return Ok(ok(failed(NOT_ALLOWED)).into_response());
            };
            if force_disabled(server) {
                return Ok(ok(failed("Server is disabled.")).into_response());
            }
            let answer = send_terminal_command(
                &state,
                &ctx,
                false,
                name_of(server),
                &server.uuid,
                body.requested_mode.as_deref(),
            )
            .await?;
            return Ok(ok(answer).into_response());
        }
        Subject::Resource(res) => res,
    };
    let selected = body
        .selected_container
        .filter(|s| s != "default")
        .unwrap_or_default();
    if selected.is_empty() {
        return Ok(ok(failed("Please select a container.")).into_response());
    }
    let name = selected
        .split_once(':')
        .map_or(selected.as_str(), |(_, name)| name);
    if !is_valid_container_name(name) {
        return Ok(ok(failed("Invalid container name format")).into_response());
    }
    let found = resource_containers(&state, res, &list).await?;
    let Some((server, name)) = found
        .iter()
        .find(|(row, name)| format!("{}:{name}", row.uuid) == selected)
    else {
        return Ok(ok(failed("Container not found.")).into_response());
    };
    if force_disabled(server) {
        return Ok(ok(failed("Server is disabled.")).into_response());
    }
    let answer = send_terminal_command(
        &state,
        &ctx,
        true,
        name,
        &server.uuid,
        body.requested_mode.as_deref(),
    )
    .await?;
    Ok(ok(answer).into_response())
}

#[derive(Debug, Deserialize)]
struct SessionBody {
    #[serde(default)]
    is_container: bool,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    server_uuid: String,
    #[serde(default)]
    requested_mode: Option<String>,
}

/// The `send-terminal-command` event of /terminal.
async fn session(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<SessionBody>,
) -> ApiResult {
    let answer = send_terminal_command(
        &state,
        &ctx,
        body.is_container,
        &body.identifier,
        &body.server_uuid,
        body.requested_mode.as_deref(),
    )
    .await?;
    Ok(ok(answer).into_response())
}

/// `Terminal::checkShellAvailability()`.
async fn shell_available(srv: &Srv, container: &str) -> bool {
    let name = quote(container);
    let sudo = if srv.info.is_non_root() { "sudo " } else { "" };
    let command = format!(
        "{sudo}docker exec {name} bash -c 'exit 0' 2>/dev/null || {sudo}docker exec {name} sh -c 'exit 0' 2>/dev/null"
    );
    let Ok(t) = srv.target().await else {
        return false;
    };
    srv.jobs
        .remote()
        .run(
            &t,
            &[command.as_str()],
            RunOptions {
                no_sudo: true,
                ..RunOptions::op("terminal.shell_available")
            },
        )
        .await
        .is_ok()
}

/// `Terminal::containerKeepsStdinOpen()`.
async fn keeps_stdin_open(srv: &Srv, container: &str) -> bool {
    let sudo = if srv.info.is_non_root() { "sudo " } else { "" };
    let command = format!(
        "{sudo}docker inspect --format '{{{{.Config.OpenStdin}}}}' {} 2>/dev/null",
        quote(container)
    );
    let Ok(t) = srv.target().await else {
        return false;
    };
    srv.jobs
        .remote()
        .run_quiet(
            &t,
            &[command.as_str()],
            RunOptions {
                no_sudo: true,
                ..RunOptions::op("terminal.keeps_stdin_open")
            },
        )
        .await
        .is_some_and(|out| out.trim().starts_with("true"))
}

/// `Terminal::resolveMode()`.
fn resolve_mode(requested: Option<&str>, attach_available: bool) -> &'static str {
    match requested {
        Some("attach") | None if attach_available => "attach",
        Some("shell") | Some("attach") | None => "shell",
        Some(_) if attach_available => "attach",
        Some(_) => "shell",
    }
}

/// `Terminal::sendTerminalCommand()`. The answer carries the component state the view reads.
async fn send_terminal_command(
    state: &AppState,
    ctx: &AuthCtx,
    is_container: bool,
    identifier: &str,
    server_uuid: &str,
    requested_mode: Option<&str>,
) -> Result<Value, ApiError> {
    if !ctx.is_team_admin() {
        return Ok(failed(NOT_ALLOWED));
    }
    let Some(row) = servers::find(&state.db, ctx.team_id, server_uuid).await? else {
        return Ok(failed(NOT_ALLOWED));
    };
    if !terminal_enabled(&row) || force_disabled(&row) {
        return Ok(failed("Terminal access is disabled on this server."));
    }
    let mut answer =
        json!({"ok": true, "has_shell": true, "attach_available": false, "mode": "shell"});
    let mut attach = false;
    if is_container {
        if !is_valid_container_name(identifier) {
            return Ok(failed("The container name is not valid."));
        }
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        let srv = Srv::from_row(jobs, row.clone());
        if container_status(&srv, identifier).await != "running" {
            return Ok(failed("The container is not running."));
        }
        let has_shell = shell_available(&srv, identifier).await;
        let attach_available = keeps_stdin_open(&srv, identifier).await;
        let mode = resolve_mode(requested_mode, attach_available);
        answer = json!({"ok": true, "has_shell": has_shell, "attach_available": attach_available, "mode": mode});
        if mode == "shell" && !has_shell {
            answer["ok"] = json!(false);
            answer["message"] = json!("No shell is available in this container.");
            return Ok(answer);
        }
        attach = mode == "attach";
    }
    answer["token"] = json!(issue(
        ctx,
        &row.uuid,
        is_container.then(|| identifier.to_owned()),
        attach
    ));
    Ok(answer)
}

/// `TerminalSessionService::issue()`.
fn issue(ctx: &AuthCtx, server_uuid: &str, container: Option<String>, attach: bool) -> String {
    let token = crate::ui_applications::random_str(64);
    if let Ok(mut map) = TOKENS.lock() {
        map.retain(|_, t| t.created.elapsed() < TOKEN_TTL);
        map.insert(
            token.clone(),
            Issued {
                created: Instant::now(),
                user_id: ctx.user_id,
                team_id: ctx.team_id,
                server_uuid: server_uuid.to_owned(),
                container,
                attach,
            },
        );
    }
    token
}

/// What a redeemed token opens: the server and the command, never anything from the browser.
#[allow(missing_debug_implementations)]
pub struct Session {
    pub target: Target,
    pub command: String,
    pub attach: bool,
    /// The live session record of `TerminalSessionService::redeem()` (v4.7.5).
    pub live: LiveSession,
}

/// What upstream keeps under `terminal-live-session:{id}` while a terminal runs: who uses which
/// server, to check the session again while it runs and to record its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub user_id: i64,
    pub team_id: i64,
    pub server_uuid: String,
    pub server_name: String,
    pub container: Option<String>,
    /// `host`, `attach` or `shell`.
    pub mode: &'static str,
}

impl LiveSession {
    /// `auditContext()`.
    fn audit_context(&self, action: &str) -> Value {
        let target = self.container.as_deref().unwrap_or("host");
        json!({
            "resource": "server",
            "team_id": self.team_id,
            "user_id": self.user_id,
            "server_uuid": self.server_uuid,
            "server_name": self.server_name,
            "container": self.container,
            "mode": self.mode,
            // The browser IP and user agent are not kept with the token in coolify-rs.
            "actor_ip": null,
            "actor_user_agent": null,
            "audit_description": format!(
                "Terminal session {action} on {} ({target})",
                self.server_name
            ),
        })
    }
}

async fn audit_session(state: &AppState, live: &LiveSession, event: &str, context: Value) {
    // `auth()->onceUsingId($session['user_id'])`: the event is recorded as the session user.
    let user = coolify_db::audit::actor_user(&state.db, live.user_id)
        .await
        .ok()
        .flatten();
    let actor = coolify_db::audit::Actor {
        user_id: Some(live.user_id),
        name: user.as_ref().map(|u| u.0.clone()),
        email: user.map(|u| u.1),
        ..Default::default()
    };
    if let Err(e) = coolify_db::audit::record(
        &state.db,
        Some(live.team_id),
        event,
        &context,
        "info",
        &actor,
        None,
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, event, "audit event persistence failed");
    }
}

/// `auditLog('terminal.session.started', ...)` of `redeem()`.
pub(crate) async fn session_started(state: &AppState, live: &LiveSession) {
    audit_session(
        state,
        live,
        "terminal.session.started",
        live.audit_context("started"),
    )
    .await;
}

/// `TerminalSessionService::end()`: records the end of a live session.
pub(crate) async fn session_ended(
    state: &AppState,
    live: &LiveSession,
    reason: &str,
    duration_seconds: u64,
) {
    let mut context = live.audit_context("ended");
    context["reason"] = json!(reason);
    context["duration_seconds"] = json!(duration_seconds);
    audit_session(state, live, "terminal.session.ended", context).await;
}

/// `TerminalSessionService::check()`: whether the user may keep a live session open. Uses the
/// team of the server, not the session team, because the user can switch teams while the
/// terminal runs. `Err` when the database could not answer.
pub(crate) async fn session_allowed(state: &AppState, live: &LiveSession) -> Result<bool, ()> {
    let row = servers::find(&state.db, live.team_id, &live.server_uuid)
        .await
        .map_err(|_| ())?;
    let Some(row) = row else {
        return Ok(false);
    };
    let role = coolify_db::auth::membership_role(&state.db, live.team_id, live.user_id)
        .await
        .map_err(|_| ())?;
    if !role.is_some_and(|r| r.is_admin()) {
        return Ok(false);
    }
    // `$server->team?->is_two_factor_required && blank($user->two_factor_confirmed_at)`.
    let two_factor_missing: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT is_two_factor_required FROM teams WHERE id = $1), false) \
         AND (SELECT two_factor_confirmed_at IS NULL FROM users WHERE id = $2)",
    )
    .bind(live.team_id)
    .bind(live.user_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ())?
    .flatten()
    .unwrap_or(true);
    Ok(!two_factor_missing
        && terminal_enabled(&row)
        && !force_disabled(&row)
        && !ServerInfo::from_row(&row).transferred)
}

/// `TerminalSessionService::redeem()`: single use, for the user and team it was issued to, and
/// every check again, since the server or the session may have changed. `Err` is a refusal.
pub(crate) async fn redeem(state: &AppState, ctx: &AuthCtx, token: &str) -> Result<Session, ()> {
    let issued = TOKENS
        .lock()
        .map_err(|_| ())?
        .remove(token)
        .filter(|t| t.created.elapsed() < TOKEN_TTL)
        .ok_or(())?;
    if issued.user_id != ctx.user_id || issued.team_id != ctx.team_id {
        return Err(());
    }
    let row = servers::find(&state.db, ctx.team_id, &issued.server_uuid)
        .await
        .map_err(|_| ())?
        .ok_or(())?;
    // v4.7.5: another instance manages a transferred server.
    if !terminal_enabled(&row) || force_disabled(&row) || ServerInfo::from_row(&row).transferred {
        return Err(());
    }
    let live = LiveSession {
        user_id: ctx.user_id,
        team_id: row.team_id,
        server_uuid: row.uuid.clone(),
        server_name: name_of(&row).to_owned(),
        container: issued.container.clone(),
        mode: match (&issued.container, issued.attach) {
            (None, _) => "host",
            (Some(_), true) => "attach",
            (Some(_), false) => "shell",
        },
    };
    let key_id = row.record.get("private_key_id").and_then(Value::as_i64);
    let key_team: Option<i64> =
        sqlx::query_scalar("SELECT team_id FROM private_keys WHERE id = $1")
            .bind(key_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ())?;
    if key_team != Some(ctx.team_id) {
        return Err(());
    }
    let jobs = state.jobs.as_ref().ok_or(())?;
    let srv = Srv::from_row(jobs, row);
    let command = match &issued.container {
        None => shell_command().to_owned(),
        Some(container) => {
            if !is_valid_container_name(container)
                || container_status(&srv, container).await != "running"
            {
                return Err(());
            }
            container_command(srv.info.is_non_root(), container, issued.attach)
        }
    };
    let target = srv.target().await.map_err(|_| ())?;
    Ok(Session {
        target,
        command,
        attach: issued.attach,
        live,
    })
}

/// `TerminalSessionService::shellCommand()`.
fn shell_command() -> &'static str {
    "PATH=$PATH:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin && if [ -f ~/.profile ]; then . ~/.profile; fi && if [ -n \"$SHELL\" ] && [ -x \"$SHELL\" ] && [ \"${SHELL##*/}\" != nologin ] && [ \"${SHELL##*/}\" != false ]; then exec $SHELL; elif command -v bash >/dev/null 2>&1; then exec bash; else sh; fi"
}

/// `TerminalSessionService::containerCommand()`.
fn container_command(non_root: bool, container: &str, attach: bool) -> String {
    let name = quote(container);
    let sudo = if non_root { "sudo " } else { "" };
    if attach {
        return format!(
            "{sudo}docker logs --tail {CONSOLE_HISTORY_LINES} {name} 2>&1; exec {sudo}docker attach --detach-keys=\"{DETACH_KEYS}\" --sig-proxy=false {name}"
        );
    }
    format!(
        "{sudo}docker exec -it {name} sh -c {}",
        quote(shell_command())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_upstreams() {
        assert_eq!(
            shell_command(),
            r#"PATH=$PATH:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin && if [ -f ~/.profile ]; then . ~/.profile; fi && if [ -n "$SHELL" ] && [ -x "$SHELL" ] && [ "${SHELL##*/}" != nologin ] && [ "${SHELL##*/}" != false ]; then exec $SHELL; elif command -v bash >/dev/null 2>&1; then exec bash; else sh; fi"#
        );
        assert_eq!(
            container_command(false, "app", false),
            format!("docker exec -it 'app' sh -c '{}'", shell_command())
        );
        assert_eq!(
            container_command(true, "app", false),
            format!("sudo docker exec -it 'app' sh -c '{}'", shell_command())
        );
        assert_eq!(
            container_command(false, "app", true),
            r#"docker logs --tail 200 'app' 2>&1; exec docker attach --detach-keys="ctrl-p,ctrl-q" --sig-proxy=false 'app'"#
        );
        assert_eq!(
            container_command(true, "app", true),
            r#"sudo docker logs --tail 200 'app' 2>&1; exec sudo docker attach --detach-keys="ctrl-p,ctrl-q" --sig-proxy=false 'app'"#
        );
    }

    #[test]
    fn mode_falls_back_to_shell() {
        assert_eq!(resolve_mode(Some("attach"), false), "shell");
        assert_eq!(resolve_mode(Some("attach"), true), "attach");
        assert_eq!(resolve_mode(Some("shell"), true), "shell");
        assert_eq!(resolve_mode(None, true), "attach");
        assert_eq!(resolve_mode(None, false), "shell");
        assert_eq!(resolve_mode(Some("other"), true), "attach");
        assert_eq!(resolve_mode(Some("other"), false), "shell");
    }
}

#[cfg(test)]
mod live_session_tests {
    use super::LiveSession;

    // `TerminalSessionService::auditContext()`.
    #[test]
    fn audit_context_names_the_server_and_the_target() {
        let mut live = LiveSession {
            user_id: 7,
            team_id: 3,
            server_uuid: "srv".into(),
            server_name: "web-1".into(),
            container: None,
            mode: "host",
        };
        let context = live.audit_context("started");
        assert_eq!(context["resource"], "server");
        assert_eq!(context["team_id"], 3);
        assert_eq!(context["user_id"], 7);
        assert_eq!(context["mode"], "host");
        assert_eq!(
            context["audit_description"],
            "Terminal session started on web-1 (host)"
        );
        live.container = Some("app-abc".into());
        live.mode = "shell";
        assert_eq!(
            live.audit_context("ended")["audit_description"],
            "Terminal session ended on web-1 (app-abc)"
        );
    }
}
