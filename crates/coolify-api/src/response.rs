//! Responses encoded exactly like Laravel's `JsonResponse` (PHP default
//! `json_encode` flags) plus the upstream error vocabulary.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use coolify_compat::php_json;
use serde_json::{Value, json};

/// JSON body encoded with PHP's `json_encode` defaults.
#[derive(Debug)]
pub struct PhpJson(pub StatusCode, pub Value);

impl IntoResponse for PhpJson {
    fn into_response(self) -> Response {
        let body = php_json::encode(&self.1);
        let mut res = (self.0, body).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        res
    }
}

pub fn ok(v: Value) -> PhpJson {
    PhpJson(StatusCode::OK, v)
}

pub fn created(v: Value) -> PhpJson {
    PhpJson(StatusCode::CREATED, v)
}

/// `response($string)`: Laravel defaults to `text/html; charset=UTF-8`.
pub fn text(body: String) -> Response {
    let mut res = (StatusCode::OK, body).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    res
}

/// Errors with upstream's exact status codes and bodies.
#[derive(Debug)]
pub enum ApiError {
    /// 401 `{"message":"Unauthenticated."}`
    Unauthenticated,
    /// 401 `{"message":"Invalid token."}` (`api.token.team`)
    InvalidToken,
    /// 403 `{"message":"Missing required team role."}`
    MissingTeamRole,
    /// 403 `{"message":"Missing required permissions: a, b"}`
    MissingAbilities(Vec<&'static str>),
    /// 403 `{"success":true,"message":"API is disabled."}`
    ApiDisabled,
    /// 403 `{"success":true,"message":"You are not allowed to access the API."}`
    IpNotAllowed,
    /// 403 policy denial.
    Unauthorized,
    /// `{"message": ...}` with an arbitrary status.
    Message(StatusCode, &'static str),
    /// `response()->json($body, $status)` with a body of its own.
    Status(StatusCode, Value),
    /// `abort($status, $message)`: Laravel's exception handler, pretty-printed JSON.
    Abort(StatusCode, &'static str),
    /// 422 `{"message":"Validation failed.","errors":{...}}`
    Validation(serde_json::Map<String, Value>),
    /// 422 Laravel `ValidationException` (message = first error).
    ValidationWithMessage(&'static str, serde_json::Map<String, Value>),
    /// 429 login throttle, with `Retry-After`.
    Throttled(u64),
    /// 400 `{"message":"Invalid request.","error": ...}`
    InvalidRequest(&'static str),
    /// 405 for GET on POST-only routes, with `Allow: POST`.
    PostRequired,
    /// 404 for unknown `/api/*` paths.
    RouteNotFound,
    /// 501: the route exists upstream but is not ported yet.
    NotPorted,
    /// 503: writes refused by the schema guard.
    ReadOnly(String),
    /// 500 without leaking details.
    Internal,
}

impl From<coolify_db::DbError> for ApiError {
    fn from(e: coolify_db::DbError) -> Self {
        match e {
            coolify_db::DbError::ReadOnly(reason) => Self::ReadOnly(reason),
            other => {
                tracing::error!(error = %other, "database error");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Abort(status, m) = self {
            let body = php_json::encode_pretty(&json!({"message": m}));
            let mut res = (status, body).into_response();
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return res;
        }
        let (status, body) = match self {
            Self::Abort(..) => unreachable!("handled above"),
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                json!({"message": "Unauthenticated."}),
            ),
            Self::InvalidToken => (
                StatusCode::UNAUTHORIZED,
                json!({"message": "Invalid token."}),
            ),
            Self::MissingTeamRole => (
                StatusCode::FORBIDDEN,
                json!({"message": "Missing required team role."}),
            ),
            Self::MissingAbilities(a) => (
                StatusCode::FORBIDDEN,
                json!({"message": format!("Missing required permissions: {}", a.join(", "))}),
            ),
            Self::ApiDisabled => (
                StatusCode::FORBIDDEN,
                json!({"success": true, "message": "API is disabled."}),
            ),
            Self::IpNotAllowed => (
                StatusCode::FORBIDDEN,
                json!({"success": true, "message": "You are not allowed to access the API."}),
            ),
            Self::Unauthorized => (
                StatusCode::FORBIDDEN,
                json!({"message": "You are not authorized to perform this action.", "error": "Unauthorized"}),
            ),
            Self::Message(status, m) => (status, json!({"message": m})),
            Self::Status(status, body) => (status, body),
            Self::Validation(errors) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": "Validation failed.", "errors": errors}),
            ),
            Self::ValidationWithMessage(m, errors) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": m, "errors": errors}),
            ),
            Self::Throttled(secs) => {
                let mut res = PhpJson(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"message": format!("Too many login attempts. Please try again in {secs} seconds.")}),
                )
                .into_response();
                if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                    res.headers_mut().insert(header::RETRY_AFTER, v);
                }
                return res;
            }
            Self::InvalidRequest(e) => (
                StatusCode::BAD_REQUEST,
                json!({"message": "Invalid request.", "error": e}),
            ),
            Self::PostRequired => {
                let mut res = PhpJson(
                    StatusCode::METHOD_NOT_ALLOWED,
                    json!({"message": "This endpoint has changed to a POST request."}),
                )
                .into_response();
                res.headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("POST"));
                return res;
            }
            Self::RouteNotFound => (
                StatusCode::NOT_FOUND,
                json!({"message": "Not found.", "docs": "https://coolify.io/docs"}),
            ),
            Self::NotPorted => (
                StatusCode::NOT_IMPLEMENTED,
                json!({"message": "This endpoint is not implemented in coolify-rs yet."}),
            ),
            Self::ReadOnly(reason) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"message": "coolify-rs is in read-only mode.", "reason": reason}),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"message": "Server Error"}),
            ),
        };
        PhpJson(status, body).into_response()
    }
}

pub type ApiResult<T = Response> = Result<T, ApiError>;
