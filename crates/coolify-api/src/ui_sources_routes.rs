//! The session-guarded routes of the sources pages and the private repository forms.

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;
use crate::ui_applications::private_git as private;
use crate::ui_sources as s;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/private-repository/keys", get(private::keys))
        .route(
            "/ui/private-repository/{provider}/sources",
            get(private::sources),
        )
        .route(
            "/ui/private-repository/{provider}/repositories",
            post(private::repositories),
        )
        .route(
            "/ui/private-repository/{provider}/branches",
            post(private::branches),
        )
        .route(
            "/ui/private-repository/{provider}/scan",
            post(private::scan),
        )
        .route("/ui/private-repository/{provider}", post(private::create))
        .route("/ui/new/dockerfile", post(private::create_dockerfile))
        .route("/ui/new/docker-compose", post(private::create_compose))
        .route("/ui/sources", get(s::index))
        .route("/ui/sources/github", post(s::create_github))
        .route("/ui/sources/gitlab", post(s::create_gitlab))
        .route("/ui/sources/gitea", post(s::create_gitea))
        .route(
            "/ui/sources/github/{uuid}",
            get(s::show_github)
                .patch(s::update_github)
                .delete(s::delete_github),
        )
        .route("/ui/sources/github/{uuid}/manual", post(s::github_manual))
        .route(
            "/ui/sources/github/{uuid}/permissions",
            post(s::github_check_permissions),
        )
        .route("/ui/sources/github/{uuid}/test", post(s::github_test))
        .route(
            "/ui/sources/github/{uuid}/sync-name",
            post(s::github_sync_name),
        )
        .route(
            "/ui/sources/gitlab/{uuid}",
            get(s::show_gitlab)
                .patch(s::update_gitlab)
                .delete(s::delete_gitlab),
        )
        .route("/ui/sources/gitlab/{uuid}/test", post(s::test_gitlab))
        .route(
            "/ui/sources/gitlab/{uuid}/redirect-uri",
            post(s::redirect_gitlab),
        )
        .route(
            "/ui/sources/gitea/{uuid}",
            get(s::show_gitea)
                .patch(s::update_gitea)
                .delete(s::delete_gitea),
        )
        .route("/ui/sources/gitea/{uuid}/test", post(s::test_gitea))
        .route(
            "/ui/sources/gitea/{uuid}/redirect-uri",
            post(s::redirect_gitea),
        )
}
