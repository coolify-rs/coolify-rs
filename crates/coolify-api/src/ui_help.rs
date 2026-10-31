//! `App\Livewire\Help`: the support form behind "Feedback" and "Contact support".

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Extension, Json, Router};
use coolify_jobs::notifications::Mail;
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{blank, toast_error, validation_failed};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/help", post(submit))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

#[derive(serde::Deserialize)]
struct HelpForm {
    #[serde(default)]
    subject: Value,
    #[serde(default)]
    description: Value,
}

/// `submit`: `#[Validate]` of `$description`, then `$subject`, then `rateLimit(3, 30)`.
async fn submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(form): Json<HelpForm>,
) -> ApiResult {
    let mut errors = Vec::new();
    let check = |errors: &mut Vec<String>, v: &Value, attr: &str, min: usize, max: usize| {
        if blank(Some(v)) {
            errors.push(format!("The {attr} field is required."));
            return;
        }
        let n = v.as_str().map_or(0, |s| s.chars().count());
        if n < min {
            errors.push(format!(
                "The {attr} field must be at least {min} characters."
            ));
        } else if n > max {
            errors.push(format!(
                "The {attr} field must not be greater than {max} characters."
            ));
        }
    };
    check(&mut errors, &form.description, "description", 10, 1000);
    check(&mut errors, &form.subject, "subject", 3, 600);
    if let Some(r) = validation_failed(&errors) {
        return Ok(r);
    }
    // `WithRateLimiting`: the key is the class, the method and the address.
    let key = format!(
        "livewire-rate-limiter:help-submit:{}",
        meta.ip.as_deref().unwrap_or("")
    );
    if let Some(secs) = crate::rules::throttle(&key, 3, Duration::from_secs(30)) {
        return Ok(toast_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many requests. Please try again in {secs} seconds."),
        ));
    }
    let subject = form.subject.as_str().unwrap_or_default();
    let description = form.description.as_str().unwrap_or_default();
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(ctx.user_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    let email = email.unwrap_or_default();
    let row = coolify_db::instance_settings::row(&state.db).await?;
    let configured = row.get("smtp_enabled") == Some(&json!(true))
        || row.get("resend_enabled") == Some(&json!(true));
    if configured {
        if let Err(m) = coolify_jobs::notifications::send_transactional(
            &state.db,
            state.encrypter.as_ref(),
            &email,
            Mail::Help {
                subject: subject.to_owned(),
                description: description.to_owned(),
            },
        )
        .await
        {
            return Ok(toast_error(StatusCode::BAD_REQUEST, m));
        }
    } else {
        // "Sending feedback through Cloud API". `COOLIFY_RS_FEEDBACK_URL` points it at a fake in tests.
        let url = std::env::var("COOLIFY_RS_FEEDBACK_URL")
            .unwrap_or_else(|_| "https://app.coolify.io/api/feedback".to_owned());
        let sent = reqwest::Client::new()
            .post(&url)
            .timeout(Duration::from_secs(10))
            .json(&json!({
                "content": format!(
                    "User: `{email}` with subject: `{subject}` has the following problem: `{description}`"
                ),
            }))
            .send()
            .await;
        if !sent.is_ok_and(|r| r.status().is_success()) {
            return Ok(ok(json!({
                "error": "Feedback could not be sent.",
                "error_description": "Please try again later or contact us on Discord.",
            }))
            .into_response());
        }
    }
    Ok(ok(json!({
        "message": "Feedback sent.",
        "description": "We will get in touch with you as soon as possible.",
    }))
    .into_response())
}
