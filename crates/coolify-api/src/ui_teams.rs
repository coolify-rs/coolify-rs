//! Page endpoints of the team screens: `App\Livewire\Team\{Create, DangerZone, InviteLink,
//! Invitations, AdminView, AuditLog}`, `NavbarDeleteTeam`, the team avatar of `Team\Index`, and
//! `Controller::{showInvitation, acceptInvitation, link, acceptLink}`.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use coolify_db::team_pages as tp;
use coolify_domain::Role;
use coolify_jobs::notifications::{self, Mail};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::routes::audit_events::{current_url, present_row};
use crate::session::SessionUser;
use crate::state::AppState;
use crate::ui_notifications::{summarize, toast_error, unauthorized};
use crate::validation::{patterns, validate};

pub fn routes(state: AppState) -> Router<AppState> {
    let guarded = Router::new()
        .route("/ui/team/new", post(create))
        .route("/ui/team/danger", get(danger).post(delete_team))
        .route("/ui/team/navbar-delete", post(navbar_delete_team))
        .route("/ui/team/invitations", get(invitations).post(invite))
        .route("/ui/team/invitations/{id}", delete(revoke))
        .route(
            "/ui/team/avatar",
            post(upload_avatar)
                .delete(remove_avatar)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/ui/team/audit-log", get(audit_log))
        .route("/ui/team/admin", get(admin))
        .route("/ui/team/admin/{id}/delete", post(admin_delete))
        .route_layer(from_fn_with_state(state.clone(), crate::session::ui_guard));
    let signed_in = Router::new()
        .route(
            "/ui/invitations/{uuid}",
            get(show_invitation).post(accept_invitation),
        )
        .route("/team/{team_id}/avatar", get(team_avatar))
        .route("/team/members/{user_id}/avatar", get(member_avatar))
        .route_layer(from_fn_with_state(state, crate::session::user_guard));
    Router::new()
        .merge(guarded)
        .merge(signed_in)
        .route("/ui/auth/link", get(show_link).post(accept_link))
}

/// The Livewire error of `$this->validate()` inside `try`: one toast, summarized.
/// The field errors stay in Livewire's error bag, so the page shows them too.
pub(crate) fn validation_toast(e: ApiError) -> Response {
    match e {
        ApiError::Validation(map) | ApiError::ValidationWithMessage(_, map) => {
            let all: Vec<String> = map
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            toast_with_errors(summarize(&all).unwrap_or_default(), map)
        }
        other => other.into_response(),
    }
}

/// A toast and the fields' messages.
pub(crate) fn toast_with_errors(message: String, errors: Map<String, Value>) -> Response {
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

/// Laravel's `email` rule for what a form can send.
pub(crate) fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && !local.contains(char::is_whitespace)
        && !domain.contains(char::is_whitespace)
        && !local.contains('@')
}

fn body_str<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Data dir of `Storage::disk('images')`.
pub(crate) fn images_dir() -> PathBuf {
    std::env::var_os("COOLIFY_RS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("images")
}

/// The rules of `uploadAvatar`, then `AvatarStorageService::compress`: a JPEG of at most 256
/// pixels is kept; anything else is cropped to the centre and scaled to 256 JPEG.
pub(crate) fn prepare_avatar(bytes: &[u8]) -> Result<Vec<u8>, Vec<String>> {
    use image::{GenericImageView, ImageFormat};
    let mut errors = Vec::new();
    if bytes.is_empty() {
        return Err(vec!["The avatar field is required.".to_string()]);
    }
    let format = image::guess_format(bytes).ok();
    let allowed = matches!(
        format,
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
    );
    let decoded = allowed
        .then(|| image::load_from_memory(bytes).ok())
        .flatten();
    if decoded.is_none() {
        errors.push("The avatar field must be an image.".to_string());
    }
    if !allowed {
        errors.push("The avatar field must be a file of type: jpg, jpeg, png, webp.".to_string());
    }
    if bytes.len() > 5120 * 1024 {
        errors.push("The avatar field must not be greater than 5120 kilobytes.".to_string());
    }
    if decoded
        .as_ref()
        .is_some_and(|d| d.width() > 6000 || d.height() > 6000)
    {
        errors.push("The avatar field has invalid image dimensions.".to_string());
    }
    let Some(img) = decoded.filter(|_| errors.is_empty()) else {
        return Err(errors);
    };
    let (w, h) = img.dimensions();
    if format == Some(ImageFormat::Jpeg) && w <= 256 && h <= 256 {
        return Ok(bytes.to_vec());
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let mut canvas = image::RgbImage::from_pixel(256, 256, image::Rgb([255, 255, 255]));
    let scaled = square
        .resize_exact(256, 256, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    image::imageops::overlay(
        &mut canvas,
        &image::DynamicImage::ImageRgba8(scaled).to_rgb8(),
        0,
        0,
    );
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&canvas)
        .map_err(|_| vec!["Unable to compress the profile picture.".to_string()])?;
    Ok(out)
}

pub(crate) async fn write_image(path: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let file = images_dir().join(path);
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(file, bytes).await
}

pub(crate) async fn remove_image(path: &str) {
    let _ = tokio::fs::remove_file(images_dir().join(path)).await;
}

/// `response($contents, 200, [...])` of the avatar controllers.
pub(crate) async fn image_response(path: Option<&str>, local: bool) -> Result<Response, ApiError> {
    const MISSING: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Not Found");
    let path = path.filter(|_| local).ok_or(MISSING)?;
    // Paths are ours (`avatars/{id}/avatar.jpg`), but stay inside the disk regardless.
    if path.contains("..") {
        return Err(MISSING);
    }
    let bytes = tokio::fs::read(images_dir().join(path))
        .await
        .map_err(|_| MISSING)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (
                header::CACHE_CONTROL,
                "immutable, max-age=31536000, private",
            ),
        ],
        bytes,
    )
        .into_response())
}

fn storage_is_local(kind: Option<&str>) -> bool {
    kind.is_none() || kind == Some("local")
}

/// `TeamAvatarController`.
async fn team_avatar(
    State(state): State<AppState>,
    Extension(user): Extension<SessionUser>,
    Path(team_id): Path<i64>,
) -> Result<Response, ApiError> {
    let member = coolify_db::auth::membership_role(&state.db, team_id, user.user_id)
        .await?
        .is_some();
    if !member {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    }
    let team = tp::facts(&state.db, team_id).await?;
    let team = team.ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    image_response(
        team.avatar_path.as_deref(),
        storage_is_local(team.avatar_storage_type.as_deref()),
    )
    .await
}

/// `ProfileAvatarController` for another user of a shared team.
async fn member_avatar(
    State(state): State<AppState>,
    Extension(user): Extension<SessionUser>,
    Path(user_id): Path<i64>,
) -> Result<Response, ApiError> {
    let row = coolify_db::profile::avatar_of_teammate(&state.db, user.user_id, user_id).await?;
    let row = row.ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    image_response(row.0.as_deref(), storage_is_local(row.1.as_deref())).await
}

/// `Team\Create::submit`.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let Value::Object(map) = body else {
        return Err(ApiError::InvalidRequest("Invalid JSON."));
    };
    let map = map
        .into_iter()
        .map(|(k, v)| match v {
            Value::String(s) if s.trim().is_empty() => (k, Value::Null),
            v => (k, v),
        })
        .collect();
    let input = crate::input::Input(map);
    let fields = [patterns::name(true), patterns::description()];
    if let Err(ApiError::Validation(mut map)) = validate(
        &input,
        &fields,
        patterns::TEAM_MESSAGES,
        &["name", "description"],
    ) {
        // Only `Team\Index` overrides `name.required`.
        if input.get("name").is_none_or(Value::is_null) {
            map.insert("name".into(), json!(["The name field is required."]));
        }
        return Ok(validation_toast(ApiError::Validation(map)));
    }
    let team = tp::create_team(
        &state.db,
        ctx.user_id,
        input.str("name").unwrap_or_default(),
        input.str("description"),
        state.now(),
    )
    .await?;
    coolify_db::notification_settings::create_defaults(&state.db, team).await?;
    let row = tp::team_row(&state.db, team).await?;
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    crate::audit::team_mutation(&state, &actor, team, "created", None, row.as_ref()).await;
    if let Some(id) = crate::session::session_id(&headers) {
        crate::session::activate_team(&state, &id, ctx.user_id, team).await?;
    }
    Ok(ok(json!({"team_id": team})).into_response())
}

/// What `livewire/team/danger-zone.blade.php` branches on.
async fn danger(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if ctx.role != Role::Owner {
        return Ok(toast_error(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ));
    }
    let team = tp::facts(&state.db, ctx.team_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let b = tp::deletion_blockers(&state.db, ctx.team_id).await?;
    let (projects, servers) = tp::resources(&state.db, ctx.team_id).await?;
    let pairs = |v: Vec<(String, String)>| {
        v.into_iter()
            .map(|(uuid, name)| json!({"uuid": uuid, "name": name}))
            .collect::<Vec<_>>()
    };
    let teams = coolify_db::users::memberships(&state.db, ctx.user_id).await?;
    Ok(ok(json!({
        "name": team.name,
        "personal_team": team.personal_team,
        "team_count": teams.len(),
        "has_subscription": tp::has_subscription(&state.db, ctx.team_id).await?,
        "blockers": {"projects": b.projects, "servers": b.servers, "sources": b.sources},
        "projects": pairs(projects),
        "servers": pairs(servers),
    }))
    .into_response())
}

async fn run_delete_team(state: &AppState, ctx: &AuthCtx, meta: &Meta) -> ApiResult {
    use tp::DeleteTeamError as E;
    let facts = tp::facts(&state.db, ctx.team_id).await?;
    let avatar = facts.and_then(|f| f.avatar_path);
    let row = tp::team_row(&state.db, ctx.team_id).await?;
    match tp::delete_team(&state.db, ctx.team_id, ctx.user_id).await {
        Ok(members) => {
            let actor = crate::audit::actor(state, ctx, meta).await;
            crate::audit::team_mutation(state, &actor, ctx.team_id, "deleted", row.as_ref(), None)
                .await;
            for m in members {
                state.sessions.remove_user(m);
            }
            if let Some(p) = avatar {
                remove_image(&p).await;
            }
            // `refreshSession($newTeam)`: `live_session` resolves the next team on the next
            // request. With several teams left, that is the selection screen.
            Ok(ok(json!({"message": "Team deleted."})).into_response())
        }
        Err(E::NotOwner) => Ok(toast_error(
            StatusCode::FORBIDDEN,
            "Only team owners can delete a team.",
        )),
        Err(E::RunningApplications) => Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Stop all running applications before deleting this team.",
        )),
        Err(E::Servers) => Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Delete all team servers before deleting this team.",
        )),
        Err(E::Resources) => Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Delete all team resources before deleting this team.",
        )),
        Err(E::Db(e)) => Err(e.into()),
    }
}

/// `Team\DangerZone::delete`.
async fn delete_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
) -> ApiResult {
    if ctx.role != Role::Owner {
        return Ok(unauthorized());
    }
    run_delete_team(&state, &ctx, &meta).await
}

/// `NavbarDeleteTeam::delete($password)`.
async fn navbar_delete_team(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    if !session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Ok(password_incorrect());
    }
    if ctx.role != Role::Owner {
        return Ok(unauthorized());
    }
    run_delete_team(&state, &ctx, &meta).await
}

/// `shouldSkipPasswordConfirmation` with this session's recent confirmation.
pub(crate) async fn confirm_steps(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
) -> Result<(bool, bool), ApiError> {
    let (text, password) = crate::ui::confirmation_steps(state, user_id).await?;
    let recent = crate::session::session_id(headers)
        .is_some_and(|id| state.sessions.password_recently_confirmed(&id));
    Ok((text, password && !recent))
}

/// `verifyPasswordConfirmation` with this session's recent confirmation.
pub(crate) async fn session_password_ok(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
    body: &Value,
) -> Result<bool, ApiError> {
    let (_, required) = confirm_steps(state, headers, user_id).await?;
    if !required {
        return Ok(true);
    }
    let hash = coolify_db::users::get(&state.db, user_id)
        .await?
        .and_then(|u| u.password);
    Ok(match (body.get("password").and_then(Value::as_str), hash) {
        (Some(p), Some(h)) => coolify_compat::password::verify(p, &h),
        _ => false,
    })
}

/// The message `modal-confirmation` shows under the password field.
pub(crate) fn password_incorrect() -> Response {
    let mut errors = Map::new();
    errors.insert(
        "password".into(),
        json!(["The provided password is incorrect."]),
    );
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": "The provided password is incorrect.", "errors": errors}),
    )
    .into_response()
}

// ---- Team\InviteLink, Team\Invitations ----

fn is_transactional_enabled(row: &Map<String, Value>) -> bool {
    let flag = |k: &str| row.get(k).and_then(Value::as_bool).unwrap_or(false);
    flag("smtp_enabled") || flag("resend_enabled")
}

async fn invitations(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(ok(json!({"invitations": [], "can_manage": false})).into_response());
    }
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let rows = tp::invitations(&state.db, ctx.team_id).await?;
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|i| {
            json!({
                "id": i.id,
                "email": i.email,
                "via": i.via,
                "role": i.role,
                "link": decrypt_link(&state, &i.link),
            })
        })
        .collect();
    Ok(ok(json!({
        "invitations": rows,
        "can_manage": true,
        "email_enabled": is_transactional_enabled(&settings),
        "instance_admin": coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?,
    }))
    .into_response())
}

fn decrypt_link(state: &AppState, stored: &str) -> String {
    state
        .encrypter
        .as_ref()
        .and_then(|e| e.decrypt_string(stored).ok())
        .unwrap_or_default()
}

/// `InviteLink::invitationUrl`.
async fn invitation_url(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
) -> Result<String, ApiError> {
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    Ok(
        match settings
            .get("fqdn")
            .and_then(Value::as_str)
            .filter(|f| !f.trim().is_empty())
        {
            Some(fqdn) => format!("{}{path}", fqdn.trim_end_matches('/')),
            None => current_url(headers, path),
        },
    )
}

/// `new_public_id(32)`.
fn public_id(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// `Str::password()`.
fn random_password() -> String {
    const ALPHABET: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789~!#$%^&*()-_.,<>?/\\{}[]|:;";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// `http_build_query` with RFC 3986 encoding.
fn query_value(s: &str) -> String {
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

/// `$invitation->hasExpired()`: older than `constants.invitation.link.expiration_days` (3).
fn expired(state: &AppState, created_at: Option<chrono::NaiveDateTime>) -> bool {
    created_at.is_none_or(|c| (state.now() - c).num_seconds() > 3 * 86_400)
}

/// The tail of `$invitation->isValid()` when it expired, and of `deleteInvitation`.
async fn delete_invitation_and_invitee(
    state: &AppState,
    invitation: &tp::Invitation,
    actor: &coolify_db::audit::Actor,
) -> Result<(), ApiError> {
    tp::delete_invitation(&state.db, invitation.id).await?;
    if let Some(user) = tp::is_disposable_invitee(&state.db, &invitation.email).await? {
        // `$this->delete()`: a failure here is the same exception upstream reports.
        let teams = tp::delete_user(&state.db, user)
            .await
            .map_err(|_| ApiError::Internal)?;
        for row in &teams {
            let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
            crate::audit::team_mutation(state, actor, id, "deleted", Some(row), None).await;
        }
        state.sessions.remove_user(user);
    }
    Ok(())
}

/// `InviteLink::generateInviteLink`: the `catch` passes the message twice to `handleError`.
fn doubled(message: &str) -> Response {
    toast_error(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!("{message} {message}"),
    )
}

async fn invite(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(doubled("This action is unauthorized."));
    }
    let via_email = body_str(&body, "via") == "email";
    let email = body_str(&body, "email");
    let role = body_str(&body, "role");
    let mut errors = Vec::new();
    let mut fields = Map::new();
    let mut add = |field: &str, message: &str| {
        errors.push(message.to_string());
        fields.insert(field.into(), json!([message]));
    };
    if email.trim().is_empty() {
        add("email", "The email field is required.");
    } else if !valid_email(email) {
        add("email", "The email field must be a valid email address.");
    }
    if role.trim().is_empty() {
        add("role", "The role field is required.");
    }
    if let Some(m) = summarize(&errors) {
        return Ok(toast_with_errors(format!("{m} {m}"), fields));
    }
    if ctx.role == Role::Admin && role == "owner" {
        return Ok(doubled("Admins cannot invite owners."));
    }
    let email = email.to_lowercase();
    let team_name = coolify_db::teams::find_name(&state.db, ctx.team_id)
        .await?
        .unwrap_or_default();
    let members = coolify_db::teams::member_rows(&state.db, ctx.team_id).await?;
    if members.iter().any(|m| m.email == email) {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{email} is already a member of {team_name}."),
        ));
    }
    let Some(encrypter) = state.encrypter.as_ref() else {
        return Ok(toast_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "APP_KEY is not configured.",
        ));
    };
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    let uuid = public_id(32);
    let mut link = invitation_url(&state, &headers, &format!("/invitations/{uuid}")).await?;
    if coolify_db::users::find_by_email(&state.db, &email)
        .await?
        .is_none()
    {
        let password = random_password();
        let hash = coolify_compat::password::hash(&password).ok_or(ApiError::Internal)?;
        let name = email.split('@').next().unwrap_or_default();
        let (_, personal) =
            tp::create_invited_user(&state.db, name, &email, &hash, state.now()).await?;
        coolify_db::notification_settings::create_defaults(&state.db, personal).await?;
        let row = tp::team_row(&state.db, personal).await?;
        crate::audit::team_mutation(&state, &actor, personal, "created", None, row.as_ref()).await;
        let token = encrypter.encrypt_string(&format!("{email}@@@{uuid}@@@{password}"));
        link = invitation_url(
            &state,
            &headers,
            &format!("/auth/link?token={}", query_value(&token)),
        )
        .await?;
    }
    if let Some(existing) = tp::invitation_by_email(&state.db, ctx.team_id, &email).await? {
        if !expired(&state, existing.created_at) {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Pending invitation already exists for {email}."),
            ));
        }
        delete_invitation_and_invitee(&state, &existing, &actor).await?;
    }
    let via = if via_email { "email" } else { "link" };
    if let Err(e) = tp::create_invitation(
        &state.db,
        ctx.team_id,
        &uuid,
        &email,
        role,
        &encrypter.encrypt_string(&link),
        via,
        state.now(),
    )
    .await
    {
        tracing::warn!(error = %e, "could not store invitation");
        return Ok(doubled("Invitation already sent."));
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.team_invitation.created",
        json!({
            "team_id": ctx.team_id,
            "invitation_uuid": uuid,
            "invitation_email": email,
            "role": role,
            "via": via,
        }),
        "info",
    )
    .await;
    if via_email {
        let sent = notifications::send_transactional(
            &state.db,
            state.encrypter.as_ref(),
            &email,
            Mail::TeamInvitation {
                team: team_name,
                invitation_link: link,
            },
        )
        .await;
        if let Err(m) = sent {
            return Ok(doubled(&m));
        }
        return Ok(ok(json!({"message": "Invitation sent via email."})).into_response());
    }
    Ok(ok(json!({"message": "Invitation link generated."})).into_response())
}

/// `Invitations::deleteInvitation`.
async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(id): Path<i64>,
) -> ApiResult {
    let not_found = || toast_error(StatusCode::NOT_FOUND, "Invitation not found.");
    if !ctx.is_team_admin() {
        return Ok(not_found());
    }
    let Some(inv) = tp::invitation_in_team(&state.db, ctx.team_id, id).await? else {
        return Ok(not_found());
    };
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    delete_invitation_and_invitee(&state, &inv, &actor).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.team_invitation.revoked",
        json!({
            "team_id": ctx.team_id,
            "invitation_uuid": inv.uuid,
            "invitation_email": inv.email,
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Invitation revoked."})).into_response())
}

async fn user_actor(state: &AppState, user: &SessionUser) -> coolify_db::audit::Actor {
    crate::audit::actor(state, &user.ctx, &Meta::default()).await
}

/// The checks shared by `showInvitation` and `acceptInvitation`.
async fn checked_invitation(
    state: &AppState,
    user: &SessionUser,
    uuid: &str,
) -> Result<tp::Invitation, ApiError> {
    let invitation = tp::invitation_by_uuid(&state.db, uuid)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    let invitee = coolify_db::users::find_by_email(&state.db, &invitation.email)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"))?;
    if invitee.id != user.user_id {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "You are not allowed to accept this invitation.",
        ));
    }
    if expired(state, invitation.created_at) {
        delete_invitation_and_invitee(state, &invitation, &user_actor(state, user).await).await?;
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Invitation expired.",
        ));
    }
    Ok(invitation)
}

/// `Controller::showInvitation`.
async fn show_invitation(
    State(state): State<AppState>,
    Extension(user): Extension<SessionUser>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let invitation = checked_invitation(&state, &user, &uuid).await?;
    let team = coolify_db::teams::find_name(&state.db, invitation.team_id).await?;
    let member = coolify_db::auth::membership_role(&state.db, invitation.team_id, user.user_id)
        .await?
        .is_some();
    Ok(ok(json!({
        "team": team,
        "role": invitation.role,
        "already_member": member,
    }))
    .into_response())
}

/// `Controller::auditInvitationAccepted`.
async fn audit_accepted(
    state: &AppState,
    meta: &Meta,
    user_id: i64,
    invitation: &tp::Invitation,
    already_member: bool,
) {
    let (name, email) = tp::user_email(&state.db, user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let actor = coolify_db::audit::Actor {
        user_id: Some(user_id),
        name: Some(name.clone()),
        email: Some(email.clone()),
        ip: meta.ip.clone(),
        user_agent: meta.user_agent.clone(),
        ..coolify_db::audit::Actor::default()
    };
    crate::audit::log_as(
        state,
        Some(invitation.team_id),
        &actor,
        "ui.team_invitation.accepted",
        json!({
            "team_id": invitation.team_id,
            "invitation_uuid": invitation.uuid,
            "invitation_email": invitation.email,
            "role": invitation.role,
            "via": invitation.via,
            "already_member": already_member,
            "member_id": user_id,
            "member_name": name,
            "member_email": email,
            "actor_id": user_id,
            "actor_name": name,
            "actor_email": email,
        }),
    )
    .await;
}

/// `Controller::acceptInvitation`.
async fn accept_invitation(
    State(state): State<AppState>,
    Extension(user): Extension<SessionUser>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let invitation = checked_invitation(&state, &user, &uuid).await?;
    let member = coolify_db::auth::membership_role(&state.db, invitation.team_id, user.user_id)
        .await?
        .is_some();
    if member {
        tp::delete_invitation(&state.db, invitation.id).await?;
        audit_accepted(&state, &meta, user.user_id, &invitation, true).await;
        return Ok(ok(json!({"message": "ok"})).into_response());
    }
    tp::attach_member(
        &state.db,
        invitation.team_id,
        user.user_id,
        &invitation.role,
    )
    .await?;
    tp::delete_invitation(&state.db, invitation.id).await?;
    audit_accepted(&state, &meta, user.user_id, &invitation, false).await;
    crate::session::activate_team(&state, &user.id, user.user_id, invitation.team_id).await?;
    Ok(ok(json!({"message": "ok"})).into_response())
}

#[derive(Deserialize)]
struct LinkQuery {
    token: Option<String>,
}

/// `Controller::magicLinkCredentials`.
async fn magic_link_credentials(
    state: &AppState,
    token: &str,
) -> Result<Option<(coolify_db::users::LoginUser, tp::Invitation)>, ApiError> {
    let Some(encrypter) = state.encrypter.as_ref().filter(|_| !token.is_empty()) else {
        return Ok(None);
    };
    let Ok(plain) = encrypter.decrypt_string(token) else {
        return Ok(None);
    };
    let parts: Vec<&str> = plain.splitn(3, "@@@").collect();
    let (email, uuid, password) = match parts.as_slice() {
        [e, u, p] => (e.to_lowercase(), Some(*u), *p),
        [e, p] => (e.to_lowercase(), None, *p),
        _ => return Ok(None),
    };
    let user = coolify_db::users::find_by_email(&state.db, &email).await?;
    let candidates = tp::invitations_by_email(&state.db, &email).await?;
    let matches = |i: &tp::Invitation| link_matches_token(&decrypt_link(state, &i.link), token);
    let invitation = match uuid {
        Some(u) => candidates.into_iter().find(|i| i.uuid == u),
        None => candidates.into_iter().find(|i| matches(i)),
    };
    let (Some(user), Some(invitation)) = (user, invitation) else {
        return Ok(None);
    };
    if expired(state, invitation.created_at) || !matches(&invitation) {
        return Ok(None);
    }
    let ok = user
        .password
        .as_deref()
        .is_some_and(|h| coolify_compat::password::verify(password, h));
    Ok(ok.then_some((user, invitation)))
}

/// `Controller::invitationLinkMatchesToken`.
fn link_matches_token(link: &str, token: &str) -> bool {
    use subtle::ConstantTimeEq;
    let Some(query) = link.split_once('?').map(|(_, q)| q) else {
        return false;
    };
    let stored = query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| percent_decode(v));
    stored.is_some_and(|s| bool::from(s.as_bytes().ct_eq(token.as_bytes())))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const LINK_EXPIRED: &str = "Invitation has expired or been revoked.";

/// `Controller::link`. A page reached without a valid token goes to the login page.
async fn show_link(State(state): State<AppState>, Query(q): Query<LinkQuery>) -> ApiResult {
    let creds = magic_link_credentials(&state, q.token.as_deref().unwrap_or_default()).await?;
    let Some((user, invitation)) = creds else {
        return Ok(toast_error(StatusCode::GONE, LINK_EXPIRED));
    };
    let team = coolify_db::teams::find_name(&state.db, invitation.team_id).await?;
    let member = coolify_db::auth::membership_role(&state.db, invitation.team_id, user.id)
        .await?
        .is_some();
    Ok(ok(json!({
        "team": team,
        "role": invitation.role,
        "already_member": member,
    }))
    .into_response())
}

/// `Controller::acceptLink`, with `throttle:magic-link`.
async fn accept_link(
    State(state): State<AppState>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let token = body_str(&body, "token");
    let key = format!(
        "magic-link:{}",
        state
            .encrypter
            .as_ref()
            .map_or_else(String::new, |e| e.hmac_with_app_key(
                format!("{token}|{}", meta.ip.as_deref().unwrap_or_default()).as_bytes()
            ))
    );
    if let Some(secs) = crate::rules::throttle(&key, 5, Duration::from_secs(60)) {
        return Err(ApiError::Throttled(secs));
    }
    let Some((user, invitation)) = magic_link_credentials(&state, token).await? else {
        return Ok(toast_error(StatusCode::GONE, LINK_EXPIRED));
    };
    let member = coolify_db::auth::membership_role(&state.db, invitation.team_id, user.id)
        .await?
        .is_some();
    if !member {
        tp::attach_member(&state.db, invitation.team_id, user.id, &invitation.role).await?;
    }
    let random: String = public_id(64);
    let hash = coolify_compat::password::hash(&random).ok_or(ApiError::Internal)?;
    tp::set_password(&state.db, user.id, &hash, state.now()).await?;
    tp::delete_invitation(&state.db, invitation.id).await?;
    state.sessions.remove_user(user.id);
    let (cookie, _) = crate::session::start_session(&state, user.id, invitation.team_id, &hash);
    audit_accepted(&state, &meta, user.id, &invitation, member).await;
    let mut res = ok(json!({"message": "ok"})).into_response();
    res.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(res)
}

// ---- Team\Index avatar ----

/// `Team\Index::uploadAvatar`.
async fn upload_avatar(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let contents = match prepare_avatar(&body) {
        Ok(c) => c,
        Err(errors) => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                summarize(&errors).unwrap_or_default(),
            ));
        }
    };
    let path = format!("team-avatars/{}/avatar.jpg", ctx.team_id);
    if write_image(&path, &contents).await.is_err() {
        return Ok(toast_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to store the team avatar.",
        ));
    }
    let before = tp::team_row(&state.db, ctx.team_id).await?;
    tp::set_team_avatar(&state.db, ctx.team_id, Some(&path), state.now()).await?;
    let after = tp::team_row(&state.db, ctx.team_id).await?;
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    crate::audit::team_mutation(
        &state,
        &actor,
        ctx.team_id,
        "updated",
        before.as_ref(),
        after.as_ref(),
    )
    .await;
    Ok(ok(json!({"message": "Team avatar updated."})).into_response())
}

/// `Team\Index::removeAvatar`.
async fn remove_avatar(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    if let Some(p) = tp::facts(&state.db, ctx.team_id)
        .await?
        .and_then(|f| f.avatar_path)
    {
        remove_image(&p).await;
    }
    let before = tp::team_row(&state.db, ctx.team_id).await?;
    tp::set_team_avatar(&state.db, ctx.team_id, None, state.now()).await?;
    let after = tp::team_row(&state.db, ctx.team_id).await?;
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    crate::audit::team_mutation(
        &state,
        &actor,
        ctx.team_id,
        "updated",
        before.as_ref(),
        after.as_ref(),
    )
    .await;
    Ok(ok(json!({"message": "Team avatar removed."})).into_response())
}

// ---- Team\AuditLog ----

#[derive(Deserialize)]
struct AuditQuery {
    search: Option<String>,
    action: Option<String>,
    source: Option<String>,
    per_page: Option<i64>,
    page: Option<i64>,
}

/// `Team\AuditLog::render`.
async fn audit_log(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(toast_error(StatusCode::FORBIDDEN, "Forbidden"));
    }
    let instance = ctx.team_id == 0
        && coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?;
    let actions =
        coolify_db::audit::distinct_values(&state.db, ctx.team_id, instance, "action").await?;
    let per_page = q.per_page.unwrap_or(25).clamp(10, 100);
    let page = q.page.unwrap_or(1).max(1);
    let opt = |v: &Option<String>| v.clone().filter(|s| s != "all" && !s.is_empty());
    let filter = coolify_db::audit::Filter {
        search: q.search.unwrap_or_default().trim().to_string(),
        action: opt(&q.action),
        source: opt(&q.source),
        search_sensitive: true,
    };
    let (total, rows) = coolify_db::audit::page(
        &state.db,
        ctx.team_id,
        instance,
        &filter,
        &[
            "id",
            "event",
            "source",
            "action",
            "actor_type",
            "actor_name",
            "actor_email",
            "actor_token_name",
            "description",
            "changes",
            "created_at",
        ],
        per_page,
        page,
    )
    .await?;
    let events: Vec<Value> = rows.into_iter().map(|r| present_row(&state, r)).collect();
    Ok(ok(json!({
        "actions": actions.iter().map(|a| json!({"value": a, "label": coolify_db::audit::headline(a)})).collect::<Vec<_>>(),
        "events": events,
        "total": total,
        "per_page": per_page,
        "page": page,
    }))
    .into_response())
}

// ---- Team\AdminView ----

#[derive(Deserialize)]
struct AdminQuery {
    search: Option<String>,
    team_filter: Option<String>,
    sort: Option<String>,
    per_page: Option<i64>,
    page: Option<i64>,
}

async fn admin(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Query(q): Query<AdminQuery>,
) -> ApiResult {
    if !coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await? {
        return Ok(toast_error(StatusCode::FORBIDDEN, "Forbidden"));
    }
    let per_page = q.per_page.unwrap_or(10).clamp(1, 100);
    let page = q.page.unwrap_or(1).max(1);
    let (total, users) = tp::admin_users(
        &state.db,
        ctx.user_id,
        ctx.team_id,
        q.search.as_deref().unwrap_or_default().trim(),
        q.team_filter.as_deref().unwrap_or("all"),
        q.sort.as_deref().unwrap_or("name_asc"),
        per_page,
        page,
    )
    .await?;
    let (text, password) = confirm_steps(&state, &headers, ctx.user_id).await?;
    Ok(ok(json!({
        "users": users.iter().map(|u| json!({"id": u.id, "name": u.name, "email": u.email})).collect::<Vec<_>>(),
        "total": total,
        "per_page": per_page,
        "page": page,
        "confirm": {"text": text, "password": password},
    }))
    .into_response())
}

fn role_rank(role: &str) -> u8 {
    match role {
        "owner" => 3,
        "admin" => 2,
        "member" => 1,
        _ => 0,
    }
}

/// `AdminView::delete` with `deletionError`.
async fn admin_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await? {
        return Ok(toast_error(StatusCode::FORBIDDEN, "Forbidden"));
    }
    if !session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Ok(password_incorrect());
    }
    let err = |m: &str| toast_error(StatusCode::UNPROCESSABLE_ENTITY, m);
    let Some((name, email)) = tp::user_email(&state.db, id).await? else {
        return Ok(err("User not found"));
    };
    if id == 0 {
        return Ok(err("The root user cannot be deleted."));
    }
    if id == ctx.user_id {
        return Ok(err("Delete your own account from your profile."));
    }
    let actor_role = tp::root_team_role(&state.db, ctx.user_id).await?;
    let target_role = tp::root_team_role(&state.db, id).await?;
    if actor_role.as_deref().is_none_or(|a| {
        target_role
            .as_deref()
            .is_some_and(|t| role_rank(t) > role_rank(a))
    }) {
        return Ok(err(
            "You cannot delete a user with a higher role in the root team.",
        ));
    }
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    let result = tp::delete_user(&state.db, id).await;
    match result {
        Ok(teams) => {
            for row in &teams {
                let team = row.get("id").and_then(Value::as_i64).unwrap_or_default();
                crate::audit::team_mutation(&state, &actor, team, "deleted", Some(row), None).await;
            }
        }
        Err(tp::DeleteUserError::AloneInRootTeam) => {
            return Ok(err("User is alone in the root team, cannot delete"));
        }
        Err(tp::DeleteUserError::SoloTeamWithResources(team)) => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "The team \"{team}\" still owns servers or projects. coolify-rs cannot remove them with the user yet: delete them first."
                ),
            ));
        }
        Err(tp::DeleteUserError::Db(e)) => return Err(e.into()),
    }
    state.sessions.remove_user(id);
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.user.deleted",
        json!({
            "team_id": ctx.team_id,
            "resource": "user",
            "user_name": name,
            "deleted_user_id": id,
            "deleted_user_email": email,
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "ok"})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails() {
        assert!(valid_email("a@b.test"));
        assert!(valid_email("a@localhost"));
        assert!(!valid_email("a"));
        assert!(!valid_email("a b@c.test"));
        assert!(!valid_email("@c.test"));
    }

    #[test]
    fn link_tokens() {
        let token = "eyJpdiI6IjEi+/=";
        let link = format!("http://x/auth/link?token={}", query_value(token));
        assert!(link_matches_token(&link, token));
        assert!(!link_matches_token(&link, "other"));
        assert!(!link_matches_token("http://x/invitations/1", token));
    }

    #[test]
    fn avatars() {
        assert!(prepare_avatar(b"").is_err());
        let errors = prepare_avatar(b"not an image").unwrap_err();
        assert_eq!(errors.len(), 2);
        let mut png = Vec::new();
        image::RgbImage::new(300, 200)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let jpeg = prepare_avatar(&png).unwrap();
        let out = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((out.width(), out.height()), (256, 256));
        // A small JPEG stays byte for byte.
        assert_eq!(prepare_avatar(&jpeg).unwrap(), jpeg);
    }
}
