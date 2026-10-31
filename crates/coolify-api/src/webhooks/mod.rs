//! `routes/webhooks.php`, mounted under `/webhooks` (without Stripe).

use axum::Router;
use axum::routing::get;

use crate::state::AppState;
use crate::ui_sources as s;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/webhooks/source/github/redirect", get(s::github_redirect))
        .route("/webhooks/source/github/install", get(s::github_install))
        .route("/webhooks/source/gitlab/redirect", get(s::gitlab_redirect))
        .route("/webhooks/source/gitea/redirect", get(s::gitea_redirect))
}
