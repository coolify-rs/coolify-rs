//! Cloud pages: `App\Livewire\Subscription\{Index,Show,Actions,PricingPlans}`, `App\Actions\Stripe\*`
//! and `App\Livewire\Admin\Index`. `isCloud()` is `SELF_HOSTED=false`; a self-hosted instance
//! redirects these pages like upstream and never talks to Stripe. `STRIPE_API_BASE` points the
//! client somewhere else than Stripe, which is how `compat/diff/subscription_fake.py` tests it.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use coolify_db::records::Record;
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;

const MIN_SERVER_LIMIT: i64 = 2;
const MAX_SERVER_LIMIT: i64 = 100;
const REFUND_WINDOW_DAYS: i64 = 30;
const CONTACT: &str = "https://coolify.io/docs/contact";

/// `isCloud()`.
pub fn is_cloud() -> bool {
    std::env::var("SELF_HOSTED").is_ok_and(|v| matches!(v.to_lowercase().as_str(), "false" | "0"))
}

/// `isDev()`.
pub fn is_dev() -> bool {
    std::env::var("APP_ENV").is_ok_and(|e| e == "local")
}

fn config(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/subscription", get(page))
        .route("/ui/subscription/status", post(stripe_status))
        .route("/ui/subscription/portal", post(portal))
        .route("/ui/subscription/subscribe", post(subscribe))
        .route("/ui/subscription/preview", post(preview))
        .route("/ui/subscription/quantity", post(update_quantity))
        .route(
            "/ui/subscription/refund-eligibility",
            get(refund_eligibility),
        )
        .route("/ui/subscription/refund", post(refund))
        .route("/ui/subscription/cancel-now", post(cancel_now))
        .route(
            "/ui/subscription/cancel-at-period-end",
            post(cancel_at_period_end),
        )
        .route("/ui/subscription/resume", post(resume))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

/// The Stripe API with a form-encoded body, as `StripeClient` sends it.
struct Stripe {
    base: String,
    key: String,
    http: reqwest::Client,
}

#[derive(Debug)]
struct StripeError {
    /// `InvalidRequestException` (a 4xx answer).
    invalid_request: bool,
    message: String,
}

impl Stripe {
    fn new() -> Option<Self> {
        Some(Self {
            base: config("STRIPE_API_BASE").unwrap_or_else(|| "https://api.stripe.com".to_owned()),
            key: config("STRIPE_API_KEY")?,
            http: reqwest::Client::new(),
        })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        params: &[(String, String)],
        idempotency: Option<&str>,
    ) -> Result<Value, StripeError> {
        let url = format!("{}/v1{path}", self.base.trim_end_matches('/'));
        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(&self.key);
        request = if method == reqwest::Method::GET {
            request.query(params)
        } else {
            request.form(params)
        };
        if let Some(key) = idempotency {
            request = request.header("Idempotency-Key", key);
        }
        let response = request.send().await.map_err(|e| StripeError {
            invalid_request: false,
            message: e.to_string(),
        })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        Err(StripeError {
            invalid_request: status.is_client_error(),
            message: body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Stripe request failed.")
                .to_owned(),
        })
    }

    async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value, StripeError> {
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        self.call(reqwest::Method::GET, path, &params, None).await
    }

    async fn post(&self, path: &str, params: &[(&str, String)]) -> Result<Value, StripeError> {
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        self.call(reqwest::Method::POST, path, &params, None).await
    }
}

fn stripe() -> Result<Stripe, ApiError> {
    Stripe::new().ok_or(ApiError::Message(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Stripe is not configured.",
    ))
}

async fn subscription(state: &AppState, team_id: i64) -> Result<Option<Record>, ApiError> {
    Ok(coolify_db::canvas::team_subscription(&state.db, team_id).await?)
}

fn text<'a>(r: &'a Record, key: &str) -> Option<&'a str> {
    r.get(key).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn flag(r: &Record, key: &str) -> bool {
    r.get(key).is_some_and(crate::serialize::php_truthy)
}

fn plain(message: impl Into<String>) -> axum::response::Response {
    ok(json!({"error": message.into()})).into_response()
}

fn guard_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ))
    }
}

fn guard_cloud() -> Result<(), ApiError> {
    if is_cloud() {
        Ok(())
    } else {
        Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ))
    }
}

/// `Subscription::type()` for the one plan family left upstream.
fn plan_type(sub: &Record) -> Option<String> {
    let Some(plan) = text(sub, "stripe_plan_id") else {
        return Some("zero".to_owned());
    };
    if !flag(sub, "stripe_invoice_paid") {
        return None;
    }
    let known = [
        "STRIPE_PRICE_ID_DYNAMIC_MONTHLY",
        "STRIPE_PRICE_ID_DYNAMIC_YEARLY",
    ]
    .iter()
    .any(|k| config(k).as_deref() == Some(plan));
    known.then(|| "dynamic".to_owned())
}

/// `Subscription::billingInterval()`.
fn billing_interval(sub: &Record) -> &'static str {
    if text(sub, "stripe_plan_id").is_some()
        && text(sub, "stripe_plan_id") == config("STRIPE_PRICE_ID_DYNAMIC_YEARLY").as_deref()
    {
        "yearly"
    } else {
        "monthly"
    }
}

/// What the mounts of `Index` and `Show` read. The redirects happen in the page.
async fn page(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !is_cloud() {
        return Ok(ok(json!({"cloud": false})).into_response());
    }
    let sub = subscription(&state, ctx.team_id).await?;
    let team = coolify_db::canvas::team_limits(&state.db, ctx.team_id).await?;
    let limit = if ctx.team_id == 0 {
        999_999_999_999
    } else {
        team.custom_server_limit.unwrap_or(2)
    };
    let active = sub.as_ref().is_some_and(|s| flag(s, "stripe_invoice_paid")) || ctx.team_id == 0;
    Ok(ok(json!({
        "cloud": true,
        "is_admin": ctx.is_team_admin(),
        "has_subscription": sub.is_some(),
        "active": active,
        "provider": config("SUBSCRIPTION_PROVIDER"),
        "team_name": team.name,
        "plan": sub.as_ref().and_then(plan_type),
        "billing_interval": sub.as_ref().map_or("monthly", billing_interval),
        "cancel_at_period_end": sub.as_ref().is_some_and(|s| flag(s, "stripe_cancel_at_period_end")),
        "server_limits": limit,
        "active_servers": team.managed_servers,
        "servers": team.servers,
        "overflow": limit < team.managed_servers,
        "can_delete_account": crate::ui_profile::account_blockers(&state, ctx.user_id).await?.is_empty(),
        "min_server_limit": MIN_SERVER_LIMIT,
        "max_server_limit": MAX_SERVER_LIMIT,
        "contact": CONTACT,
    }))
    .into_response())
}

/// `Subscription\Index::getStripeStatus`.
async fn stripe_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let mut out =
        json!({"redirect": false, "is_unpaid": false, "is_cancelled": false, "error": null});
    let Some(sub) = subscription(&state, ctx.team_id).await? else {
        return Ok(ok(out).into_response());
    };
    let Some(customer) = text(&sub, "stripe_customer_id") else {
        return Ok(ok(out).into_response());
    };
    let client = stripe()?;
    let result: Result<(), StripeError> = async {
        let customer = client.get(&format!("/customers/{customer}"), &[]).await?;
        let id = customer
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let list = client.get("/subscriptions", &[("customer", id)]).await?;
        let data = list
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let found = data.iter().find(|s| {
            s.pointer("/metadata/team_id").is_some_and(|t| {
                t.as_i64() == Some(ctx.team_id)
                    || t.as_str() == Some(ctx.team_id.to_string().as_str())
            })
        });
        if let Some(found) = found {
            let status = found
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let sub_id = found.get("id").and_then(Value::as_str).unwrap_or_default();
            coolify_db::canvas::update_subscription(
                &state.db,
                ctx.team_id,
                &[("stripe_subscription_id", json!(sub_id))],
            )
            .await
            .map_err(|e| StripeError {
                invalid_request: false,
                message: e.to_string(),
            })?;
            if status == "active" {
                coolify_db::canvas::update_subscription(
                    &state.db,
                    ctx.team_id,
                    &[
                        ("stripe_invoice_paid", json!(true)),
                        ("stripe_past_due", json!(false)),
                        (
                            "stripe_plan_id",
                            found
                                .pointer("/items/data/0/price/id")
                                .cloned()
                                .unwrap_or(Value::Null),
                        ),
                        (
                            "stripe_cancel_at_period_end",
                            json!(
                                found
                                    .get("cancel_at_period_end")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false)
                            ),
                        ),
                    ],
                )
                .await
                .map_err(|e| StripeError {
                    invalid_request: false,
                    message: e.to_string(),
                })?;
                let lookup = found
                    .pointer("/items/data/0/price/lookup_key")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if lookup.contains("dynamic") {
                    let quantity = found
                        .pointer("/items/data/0/quantity")
                        .and_then(Value::as_i64)
                        .unwrap_or(2);
                    coolify_db::canvas::set_server_limit(
                        &state.db,
                        ctx.team_id,
                        quantity.clamp(MIN_SERVER_LIMIT, MAX_SERVER_LIMIT),
                    )
                    .await
                    .map_err(|e| StripeError {
                        invalid_request: false,
                        message: e.to_string(),
                    })?;
                }
                out["redirect"] = json!(true);
            } else if status == "unpaid" {
                out["is_unpaid"] = json!(true);
            }
        }
        if data.is_empty() {
            out["is_cancelled"] = json!(true);
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        out["error"] =
            json!("Could not retrieve subscription information. Please try again later.");
    }
    Ok(ok(out).into_response())
}

/// `getStripeCustomerPortalSession`.
async fn portal(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let Some(customer) = subscription(&state, ctx.team_id)
        .await?
        .and_then(|s| text(&s, "stripe_customer_id").map(str::to_owned))
    else {
        return Ok(ok(json!({"url": null})).into_response());
    };
    let session = stripe()?
        .post(
            "/billing_portal/sessions",
            &[
                ("customer", customer),
                (
                    "return_url",
                    format!("{}/subscription", crate::ui_canvas::base(&headers)),
                ),
            ],
        )
        .await
        .map_err(|_| ApiError::Message(StatusCode::BAD_GATEWAY, "Stripe request failed."))?;
    Ok(ok(json!({"url": session.get("url")})).into_response())
}

const BLOCKING: [&str; 6] = [
    "active",
    "incomplete",
    "past_due",
    "paused",
    "trialing",
    "unpaid",
];
const RECOVERABLE: [&str; 4] = ["incomplete", "past_due", "paused", "unpaid"];

fn blocking_message(status: &str) -> &'static str {
    match status {
        "incomplete" => {
            "This team's subscription payment is incomplete. Complete the payment in the billing portal."
        }
        "past_due" => {
            "This team's subscription payment is past due. Update the payment method or settle the outstanding invoice in the billing portal."
        }
        "unpaid" => {
            "This team's subscription is unpaid. Settle the outstanding invoice in the billing portal."
        }
        "paused" => "This team's subscription is paused. Resume it in the billing portal.",
        _ => "Team already has an active subscription.",
    }
}

static CHECKOUT_LOCKS: std::sync::LazyLock<std::sync::Mutex<HashMap<i64, std::time::Instant>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// `PricingPlans::subscribeStripe`, `CreateCheckoutSession`.
async fn subscribe(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let team = ctx.team_id;
    if subscription(&state, team)
        .await?
        .is_some_and(|s| flag(&s, "stripe_invoice_paid"))
    {
        return Ok(plain("Team already has an active subscription."));
    }
    let key = match body.get("type").and_then(Value::as_str) {
        Some("dynamic-yearly") => "STRIPE_PRICE_ID_DYNAMIC_YEARLY",
        _ => "STRIPE_PRICE_ID_DYNAMIC_MONTHLY",
    };
    let Some(price) = config(key) else {
        return Ok(plain(
            "Price ID not found! Please contact the administrator.",
        ));
    };
    // `Cache::lock("stripe-checkout:team:{id}", 30)`.
    {
        let Ok(mut locks) = CHECKOUT_LOCKS.lock() else {
            return Err(ApiError::Internal);
        };
        locks.retain(|_, at| at.elapsed().as_secs() < 30);
        if locks.contains_key(&team) {
            return Ok(plain(
                "A subscription checkout is already being created for this team.",
            ));
        }
        locks.insert(team, std::time::Instant::now());
    }
    let result = checkout(&state, &ctx, &headers, &price).await;
    if let Ok(mut locks) = CHECKOUT_LOCKS.lock() {
        locks.remove(&team);
    }
    match result {
        Ok(Ok(url)) => Ok(ok(json!({"url": url})).into_response()),
        Ok(Err(message)) => Ok(plain(message)),
        Err(_) => Ok(plain(
            "Unable to confirm checkout with Stripe. Please try again shortly.",
        )),
    }
}

async fn checkout(
    state: &AppState,
    ctx: &AuthCtx,
    headers: &HeaderMap,
    price: &str,
) -> Result<Result<String, String>, StripeError> {
    let client = Stripe::new().ok_or(StripeError {
        invalid_request: false,
        message: "Stripe is not configured.".into(),
    })?;
    let db_error = |e: coolify_db::DbError| StripeError {
        invalid_request: false,
        message: e.to_string(),
    };
    let team = ctx.team_id;
    let email = coolify_db::users::get(&state.db, ctx.user_id)
        .await
        .map_err(db_error)?
        .map(|u| u.email)
        .unwrap_or_default();
    let base = crate::ui_canvas::base(headers);
    let existing = coolify_db::canvas::team_subscription(&state.db, team)
        .await
        .map_err(db_error)?;
    let customer = match existing
        .as_ref()
        .and_then(|s| text(s, "stripe_customer_id"))
    {
        Some(c) => c.to_owned(),
        None => {
            let made = client
                .call(
                    reqwest::Method::POST,
                    "/customers",
                    &[
                        ("email".to_owned(), email),
                        ("metadata[team_id]".to_owned(), team.to_string()),
                    ],
                    Some(&format!("coolify-team-{team}-customer")),
                )
                .await?;
            let id = made
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            coolify_db::canvas::assign_customer(&state.db, team, &id)
                .await
                .map_err(db_error)?;
            id
        }
    };
    let list = client
        .get(
            "/subscriptions",
            &[
                ("customer", customer.clone()),
                ("limit", "10".to_owned()),
                ("status", "all".to_owned()),
            ],
        )
        .await?;
    let blocking = list
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("status").and_then(Value::as_str))
        .find(|s| BLOCKING.contains(s));
    if let Some(status) = blocking {
        let mut message = blocking_message(status).to_owned();
        if RECOVERABLE.contains(&status) {
            let portal = client
                .post(
                    "/billing_portal/sessions",
                    &[
                        ("customer", customer.clone()),
                        ("return_url", format!("{base}/subscription")),
                    ],
                )
                .await
                .ok()
                .and_then(|s| s.get("url").and_then(Value::as_str).map(str::to_owned));
            if let Some(url) = portal {
                message.push_str(&format!(
                    " <a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\" class=\"underline\">Open billing portal</a>",
                    html_escape(&url)
                ));
            }
        }
        return Ok(Err(message));
    }
    let sessions = client
        .get(
            "/checkout/sessions",
            &[
                ("customer", customer.clone()),
                ("limit", "10".to_owned()),
                ("status", "open".to_owned()),
            ],
        )
        .await?;
    let open = sessions
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| {
            s.get("mode").and_then(Value::as_str) == Some("subscription")
                && s.get("status").and_then(Value::as_str) == Some("open")
        });
    if let Some(open) = open {
        let id = open.get("id").and_then(Value::as_str).unwrap_or_default();
        let items = client
            .get(&format!("/checkout/sessions/{id}/line_items"), &[])
            .await?;
        let lines = items
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if lines.len() == 1 && lines[0].pointer("/price/id").and_then(Value::as_str) == Some(price)
        {
            return Ok(Ok(open
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()));
        }
        client
            .post(&format!("/checkout/sessions/{id}/expire"), &[])
            .await?;
    }
    let expires = Utc::now().timestamp() + 35 * 60;
    let session = client
        .post(
            "/checkout/sessions",
            &[
                ("allow_promotion_codes", "true".to_owned()),
                ("billing_address_collection", "required".to_owned()),
                ("client_reference_id", format!("{}:{team}", ctx.user_id)),
                ("customer", customer),
                ("customer_update[name]", "auto".to_owned()),
                ("customer_update[address]", "auto".to_owned()),
                ("line_items[0][price]", price.to_owned()),
                (
                    "line_items[0][adjustable_quantity][enabled]",
                    "true".to_owned(),
                ),
                (
                    "line_items[0][adjustable_quantity][minimum]",
                    "2".to_owned(),
                ),
                ("line_items[0][quantity]", "2".to_owned()),
                ("tax_id_collection[enabled]", "true".to_owned()),
                ("automatic_tax[enabled]", "true".to_owned()),
                (
                    "subscription_data[metadata][user_id]",
                    ctx.user_id.to_string(),
                ),
                ("subscription_data[metadata][team_id]", team.to_string()),
                ("payment_method_collection", "if_required".to_owned()),
                ("mode", "subscription".to_owned()),
                ("expires_at", expires.to_string()),
                ("success_url", format!("{base}?success=1")),
                ("cancel_url", format!("{base}/subscription/new?cancelled=1")),
            ],
        )
        .await?;
    Ok(Ok(session
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `UpdateSubscriptionQuantity::fetchPricePreview`.
async fn preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let quantity = body.get("quantity").and_then(Value::as_i64).unwrap_or(0);
    let Some(sub) = subscription(&state, ctx.team_id).await? else {
        return Ok(ok(json!({"preview": null})).into_response());
    };
    let (Some(id), true) = (
        text(&sub, "stripe_subscription_id"),
        flag(&sub, "stripe_invoice_paid"),
    ) else {
        return Ok(ok(json!({"preview": null})).into_response());
    };
    let client = stripe()?;
    let result = price_preview(
        &client,
        id,
        text(&sub, "stripe_customer_id").unwrap_or_default(),
        quantity,
    )
    .await;
    Ok(ok(json!({"preview": result.ok().flatten()})).into_response())
}

async fn price_preview(
    client: &Stripe,
    id: &str,
    customer: &str,
    quantity: i64,
) -> Result<Option<Value>, StripeError> {
    let current = client.get(&format!("/subscriptions/{id}"), &[]).await?;
    let Some(item) = current.pointer("/items/data/0") else {
        return Ok(None);
    };
    let currency = item
        .pointer("/price/currency")
        .and_then(Value::as_str)
        .unwrap_or("usd")
        .to_uppercase();
    let interval = item
        .pointer("/price/recurring/interval")
        .and_then(Value::as_str)
        .unwrap_or("month");
    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let invoice = client
        .get(
            "/invoices/upcoming",
            &[
                ("customer", customer.to_owned()),
                ("subscription", id.to_owned()),
                ("subscription_items[0][id]", item_id.to_owned()),
                ("subscription_items[0][quantity]", quantity.to_string()),
                (
                    "subscription_proration_behavior",
                    "create_prorations".to_owned(),
                ),
            ],
        )
        .await?;
    let mut tax_percentage = 0.0_f64;
    let mut tax_description = Value::Null;
    if let Some(rate) = invoice
        .pointer("/total_tax_amounts/0/tax_rate")
        .and_then(Value::as_str)
    {
        let rate = client.get(&format!("/tax_rates/{rate}"), &[]).await?;
        tax_percentage = rate
            .get("percentage")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        tax_description = json!(format!(
            "{} ({}) {}%",
            rate.get("display_name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            rate.get("jurisdiction")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            tax_percentage
        ));
    }
    let tax = invoice.get("tax").and_then(Value::as_i64).unwrap_or(0);
    let subtotal = invoice.get("subtotal").and_then(Value::as_i64).unwrap_or(0);
    if tax_percentage == 0.0 && tax > 0 && subtotal > 0 {
        tax_percentage = ((tax as f64 / subtotal as f64) * 10_000.0).round() / 100.0;
    }
    let recurring: i64 = invoice
        .pointer("/lines/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|l| !l.get("proration").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|l| l.get("amount").and_then(Value::as_i64))
        .sum();
    let unit = if quantity > 0 {
        (recurring as f64 / quantity as f64).round() as i64
    } else {
        0
    };
    let recurring_tax = if tax_percentage > 0.0 {
        (recurring as f64 * tax_percentage / 100.0).round() as i64
    } else {
        0
    };
    let total = recurring + recurring_tax;
    let due = invoice
        .get("amount_due")
        .and_then(Value::as_i64)
        .or_else(|| invoice.get("total").and_then(Value::as_i64))
        .unwrap_or(0);
    Ok(Some(json!({
        "due_now": due - total,
        "recurring_subtotal": recurring,
        "recurring_tax": recurring_tax,
        "recurring_total": total,
        "unit_price": unit,
        "tax_description": tax_description,
        "quantity": quantity,
        "currency": currency,
        "billing_interval": interval,
    })))
}

async fn audit(state: &AppState, ctx: &AuthCtx, meta: &Meta, action: &str, extra: Value) {
    let sub = coolify_db::canvas::team_subscription(&state.db, ctx.team_id)
        .await
        .ok()
        .flatten();
    let team = coolify_db::canvas::team_limits(&state.db, ctx.team_id)
        .await
        .ok();
    let mut context = json!({
        "team_id": ctx.team_id,
        "resource": "subscription",
        "subscription_id": sub.as_ref().and_then(|s| s.get("id")),
        "team_name": team.map(|t| t.name),
    });
    if let (Some(m), Value::Object(e)) = (context.as_object_mut(), extra) {
        m.extend(e);
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("ui.subscription.{action}"),
        context,
        "info",
    )
    .await;
}

/// `Actions::updateQuantity`, `UpdateSubscriptionQuantity::execute`.
async fn update_quantity(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let quantity = body.get("quantity").and_then(Value::as_i64).unwrap_or(0);
    let previous = coolify_db::canvas::team_limits(&state.db, ctx.team_id)
        .await?
        .custom_server_limit
        .unwrap_or(2);
    if quantity < MIN_SERVER_LIMIT {
        return Ok(ok(json!({"error": format!("Minimum server limit is {MIN_SERVER_LIMIT}."), "quantity": MIN_SERVER_LIMIT})).into_response());
    }
    if quantity == previous {
        return Ok(ok(json!({"unchanged": true})).into_response());
    }
    let failed = |m: String| ok(json!({"error": m, "quantity": previous})).into_response();
    let Some(sub) = subscription(&state, ctx.team_id)
        .await?
        .filter(|s| text(s, "stripe_subscription_id").is_some())
    else {
        return Ok(failed("No active subscription found.".into()));
    };
    if !flag(&sub, "stripe_invoice_paid") {
        return Ok(failed("Subscription is not active.".into()));
    }
    let id = text(&sub, "stripe_subscription_id").unwrap_or_default();
    let client = stripe()?;
    let describe = |e: StripeError| {
        if e.invalid_request {
            format!("Stripe error: {}", e.message)
        } else {
            "An unexpected error occurred. Please contact support.".to_owned()
        }
    };
    let current = match client.get(&format!("/subscriptions/{id}"), &[]).await {
        Ok(c) => c,
        Err(e) => return Ok(failed(describe(e))),
    };
    let Some(item_id) = current.pointer("/items/data/0/id").and_then(Value::as_str) else {
        return Ok(failed("Could not find subscription item.".into()));
    };
    let old_quantity = current
        .pointer("/items/data/0/quantity")
        .and_then(Value::as_i64)
        .unwrap_or(previous);
    let updated = client
        .post(
            &format!("/subscriptions/{id}"),
            &[
                ("items[0][id]", item_id.to_owned()),
                ("items[0][quantity]", quantity.to_string()),
                ("proration_behavior", "always_invoice".to_owned()),
                ("expand[0]", "latest_invoice".to_owned()),
            ],
        )
        .await;
    let updated = match updated {
        Ok(u) => u,
        Err(e) => return Ok(failed(describe(e))),
    };
    if let Some(invoice) = updated.get("latest_invoice").filter(|i| i.is_object())
        && invoice.get("status").and_then(Value::as_str) != Some("paid")
    {
        let _ = client
            .post(
                &format!("/subscriptions/{id}"),
                &[
                    ("items[0][id]", item_id.to_owned()),
                    ("items[0][quantity]", old_quantity.to_string()),
                    ("proration_behavior", "none".to_owned()),
                ],
            )
            .await;
        if let Some(invoice_id) = invoice.get("id").and_then(Value::as_str) {
            let _ = client
                .post(&format!("/invoices/{invoice_id}/void"), &[])
                .await;
        }
        return Ok(failed("Payment failed. Your server limit was not changed. Please check your payment method and try again.".into()));
    }
    coolify_db::canvas::set_server_limit(&state.db, ctx.team_id, quantity).await?;
    audit(
        &state,
        &ctx,
        &meta,
        "quantity_updated",
        json!({"from_server_limit": previous, "to_server_limit": quantity}),
    )
    .await;
    Ok(ok(json!({"message": format!("Server limit updated to {quantity}."), "server_limits": quantity})).into_response())
}

/// `RefundSubscription::checkEligibility`.
async fn eligibility(sub: Option<&Record>) -> Result<Value, ApiError> {
    let no = |reason: &str, end: Value| json!({"eligible": false, "days_remaining": 0, "reason": reason, "current_period_end": end});
    let Some(sub) = sub else {
        return Ok(no("No active subscription found.", Value::Null));
    };
    if text(sub, "stripe_refunded_at").is_some() {
        return Ok(no(
            "A refund has already been processed for this team.",
            Value::Null,
        ));
    }
    let Some(id) = text(sub, "stripe_subscription_id") else {
        return Ok(no("No active subscription found.", Value::Null));
    };
    if !flag(sub, "stripe_invoice_paid") {
        return Ok(no("Subscription invoice is not paid.", Value::Null));
    }
    let current = match stripe()?.get(&format!("/subscriptions/{id}"), &[]).await {
        Ok(c) => c,
        Err(e) if e.invalid_request => {
            return Ok(no("Subscription not found in Stripe.", Value::Null));
        }
        Err(_) => return Err(ApiError::Internal),
    };
    let end = current
        .get("current_period_end")
        .cloned()
        .unwrap_or(Value::Null);
    let status = current
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "active" && status != "trialing" {
        return Ok(no(&format!("Subscription status is '{status}'."), end));
    }
    let start = current
        .get("start_date")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let days = (Utc::now().timestamp() - start) / 86_400;
    let remaining = REFUND_WINDOW_DAYS - days;
    if remaining <= 0 {
        return Ok(no("The 30-day refund window has expired.", end));
    }
    Ok(
        json!({"eligible": true, "days_remaining": remaining, "reason": "Eligible for refund.", "current_period_end": end}),
    )
}

/// `Actions::loadRefundEligibility`.
async fn refund_eligibility(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let sub = subscription(&state, ctx.team_id).await?;
    let mut out = json!({"eligible": false, "days_remaining": 0, "already_used": false, "next_billing_date": null});
    let Some(sub) = sub.filter(|s| text(s, "stripe_subscription_id").is_some()) else {
        return Ok(ok(out).into_response());
    };
    out["already_used"] = json!(text(&sub, "stripe_refunded_at").is_some());
    if let Ok(result) = eligibility(Some(&sub)).await {
        out["eligible"] = result.get("eligible").cloned().unwrap_or(json!(false));
        out["days_remaining"] = result.get("days_remaining").cloned().unwrap_or(json!(0));
        if let Some(end) = result.get("current_period_end").and_then(Value::as_i64)
            && let Some(date) = chrono::DateTime::from_timestamp(end, 0)
        {
            out["next_billing_date"] = json!(date.format("%b %-d, %Y").to_string());
        }
    }
    Ok(ok(out).into_response())
}

fn contact_error(what: &str) -> axum::response::Response {
    plain(format!(
        "Something went wrong with the {what}. Please <a href=\"{CONTACT}\" target=\"_blank\" class=\"underline\">contact us</a>."
    ))
}

fn timestamp() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `RefundSubscription::execute`: false when anything failed.
async fn run_refund(state: &AppState, ctx: &AuthCtx, sub: &Record) -> Result<bool, ApiError> {
    let result = eligibility(Some(sub)).await?;
    if result.get("eligible") != Some(&json!(true)) {
        return Ok(false);
    }
    let id = text(sub, "stripe_subscription_id").unwrap_or_default();
    let client = stripe()?;
    let Ok(invoices) = client
        .get(
            "/invoices",
            &[
                ("subscription", id.to_owned()),
                ("status", "paid".to_owned()),
                ("limit", "1".to_owned()),
            ],
        )
        .await
    else {
        return Ok(false);
    };
    let Some(intent) = invoices
        .pointer("/data/0/payment_intent")
        .and_then(Value::as_str)
    else {
        return Ok(false);
    };
    if client
        .post("/refunds", &[("payment_intent", intent.to_owned())])
        .await
        .is_err()
    {
        return Ok(false);
    }
    // Recorded immediately, so it cannot be retried if the cancel fails.
    coolify_db::canvas::update_subscription(
        &state.db,
        ctx.team_id,
        &[
            ("stripe_refunded_at", json!(timestamp())),
            ("stripe_feedback", json!("Refund requested by user")),
            (
                "stripe_comment",
                json!(format!(
                    "Full refund processed within 30-day window at {}",
                    timestamp()
                )),
            ),
        ],
    )
    .await?;
    if client
        .call(
            reqwest::Method::DELETE,
            &format!("/subscriptions/{id}"),
            &[],
            None,
        )
        .await
        .is_err()
    {
        return Ok(false);
    }
    coolify_db::canvas::subscription_ended(&state.db, ctx.team_id).await?;
    Ok(true)
}

/// `Actions::refundSubscription`.
async fn refund(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    if !crate::ui_teams::session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Ok(ok(json!({"invalid_password": "Invalid password."})).into_response());
    }
    let sub = subscription(&state, ctx.team_id).await?;
    let done = match &sub {
        Some(s) => run_refund(&state, &ctx, s).await?,
        None => false,
    };
    if !done {
        return Ok(contact_error("refund"));
    }
    audit(&state, &ctx, &meta, "refunded", json!({})).await;
    Ok(ok(
        json!({"message": "Subscription refunded successfully.", "redirect": "/subscription/new"}),
    )
    .into_response())
}

/// `Actions::cancelImmediately`.
async fn cancel_now(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    if !crate::ui_teams::session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Ok(ok(json!({"invalid_password": "Invalid password."})).into_response());
    }
    let selected: Vec<&str> = body
        .get("selected")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let delete_account = selected.contains(&"deleteAccount");
    if delete_account {
        let blockers = crate::ui_profile::account_blockers(&state, ctx.user_id).await?;
        if !blockers.is_empty() {
            return Ok(ok(json!({"invalid_password": blockers.join(" ")})).into_response());
        }
    }
    let with_refund = selected.contains(&"refundLatestPayment");
    let sub = subscription(&state, ctx.team_id).await?;
    let done = match &sub {
        None => false,
        Some(s) if with_refund => {
            let refunded = run_refund(&state, &ctx, s).await?;
            if refunded {
                audit(&state, &ctx, &meta, "refunded", json!({})).await;
            }
            refunded
        }
        Some(s) => match text(s, "stripe_subscription_id") {
            None => false,
            Some(id) => {
                let cancelled = stripe()?
                    .call(
                        reqwest::Method::DELETE,
                        &format!("/subscriptions/{id}"),
                        &[],
                        None,
                    )
                    .await
                    .is_ok();
                if cancelled {
                    coolify_db::canvas::update_subscription(
                        &state.db,
                        ctx.team_id,
                        &[
                            ("stripe_cancel_at_period_end", json!(false)),
                            ("stripe_invoice_paid", json!(false)),
                            ("stripe_trial_already_ended", json!(false)),
                            ("stripe_past_due", json!(false)),
                            ("stripe_feedback", json!("Cancelled immediately by user")),
                            (
                                "stripe_comment",
                                json!(format!(
                                    "Subscription cancelled immediately by user at {}",
                                    timestamp()
                                )),
                            ),
                        ],
                    )
                    .await?;
                    coolify_db::canvas::subscription_ended(&state.db, ctx.team_id).await?;
                    audit(
                        &state,
                        &ctx,
                        &meta,
                        "cancelled",
                        json!({"cancel_at_period_end": false}),
                    )
                    .await;
                }
                cancelled
            }
        },
    };
    if !done {
        return Ok(contact_error(if with_refund {
            "refund"
        } else {
            "cancellation"
        }));
    }
    if delete_account {
        return Ok(ok(json!({"delete_account": true})).into_response());
    }
    let message = if with_refund {
        "Subscription refunded and cancelled successfully."
    } else {
        "Subscription cancelled successfully."
    };
    Ok(ok(json!({"message": message, "redirect": "/subscription/new"})).into_response())
}

/// `Actions::cancelAtPeriodEnd`, `CancelSubscriptionAtPeriodEnd`.
async fn cancel_at_period_end(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    if !crate::ui_teams::session_password_ok(&state, &headers, ctx.user_id, &body).await? {
        return Ok(ok(json!({"invalid_password": "Invalid password."})).into_response());
    }
    let sub = subscription(&state, ctx.team_id).await?;
    let id = sub
        .as_ref()
        .filter(|s| flag(s, "stripe_invoice_paid") && !flag(s, "stripe_cancel_at_period_end"))
        .and_then(|s| text(s, "stripe_subscription_id"));
    let Some(id) = id else {
        return Ok(contact_error("cancellation"));
    };
    if stripe()?
        .post(
            &format!("/subscriptions/{id}"),
            &[("cancel_at_period_end", "true".to_owned())],
        )
        .await
        .is_err()
    {
        return Ok(contact_error("cancellation"));
    }
    coolify_db::canvas::update_subscription(
        &state.db,
        ctx.team_id,
        &[("stripe_cancel_at_period_end", json!(true))],
    )
    .await?;
    audit(
        &state,
        &ctx,
        &meta,
        "cancellation_scheduled",
        json!({"cancel_at_period_end": true}),
    )
    .await;
    Ok(
        ok(json!({"message": "Subscription will be cancelled at the end of the billing period."}))
            .into_response(),
    )
}

/// `Actions::resumeSubscription`, `ResumeSubscription`.
async fn resume(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
) -> ApiResult {
    guard_cloud()?;
    guard_admin(&ctx)?;
    let sub = subscription(&state, ctx.team_id).await?;
    let id = sub
        .as_ref()
        .filter(|s| flag(s, "stripe_cancel_at_period_end"))
        .and_then(|s| text(s, "stripe_subscription_id"));
    let Some(id) = id else {
        return Ok(plain(format!(
            "Something went wrong resuming the subscription. Please <a href=\"{CONTACT}\" target=\"_blank\" class=\"underline\">contact us</a>."
        )));
    };
    if stripe()?
        .post(
            &format!("/subscriptions/{id}"),
            &[("cancel_at_period_end", "false".to_owned())],
        )
        .await
        .is_err()
    {
        return Ok(plain(format!(
            "Something went wrong resuming the subscription. Please <a href=\"{CONTACT}\" target=\"_blank\" class=\"underline\">contact us</a>."
        )));
    }
    coolify_db::canvas::update_subscription(
        &state.db,
        ctx.team_id,
        &[("stripe_cancel_at_period_end", json!(false))],
    )
    .await?;
    audit(&state, &ctx, &meta, "resumed", json!({})).await;
    Ok(ok(json!({"message": "Subscription resumed successfully."})).into_response())
}

/// `Admin\Index`: `isCloud() || isDev()`, and the root user or someone impersonating from it.
pub fn admin_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/admin", get(admin_page))
        .route("/ui/admin/search", post(admin_search))
        .route("/ui/admin/switch", post(admin_switch))
        .route("/ui/admin/back", post(admin_back))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn admin_access(state: &AppState, ctx: &AuthCtx, headers: &HeaderMap) -> Result<(), ApiError> {
    if admin_allowed(state, ctx.user_id, headers) {
        Ok(())
    } else {
        Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ))
    }
}

/// `Admin\Index::mount`: also decides the status of the `/admin` document.
pub(crate) fn admin_allowed(state: &AppState, user_id: i64, headers: &HeaderMap) -> bool {
    if !is_cloud() && !is_dev() {
        return false;
    }
    let impersonator =
        crate::session::session_id(headers).and_then(|id| state.sessions.impersonator(&id));
    user_id == 0 || impersonator == Some(0)
}

async fn admin_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    admin_access(&state, &ctx, &headers)?;
    let (active, inactive) = coolify_db::canvas::subscriber_counts(&state.db).await?;
    let user = coolify_db::users::get(&state.db, ctx.user_id).await?;
    Ok(ok(json!({
        "name": user.as_ref().map(|u| u.name.clone()),
        "email": user.map(|u| u.email),
        "active": active,
        "inactive": inactive,
        "impersonating": crate::session::session_id(&headers).and_then(|id| state.sessions.impersonator(&id)).is_some(),
    }))
    .into_response())
}

async fn admin_search(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    admin_access(&state, &ctx, &headers)?;
    let search = body
        .get("search")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let users = if search.is_empty() {
        Vec::new()
    } else {
        coolify_db::canvas::search_users(&state.db, search).await?
    };
    Ok(ok(json!({"users": users})).into_response())
}

async fn admin_switch(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    admin_access(&state, &ctx, &headers)?;
    if ctx.user_id != 0 {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ));
    }
    let id = body.get("user_id").and_then(Value::as_i64).unwrap_or(-1);
    let team = crate::session::impersonate(&state, &headers, id, Some(ctx.user_id)).await?;
    let user = coolify_db::users::get(&state.db, id).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.user.impersonated",
        json!({
            "team_id": ctx.team_id,
            "resource": "user",
            "user_name": user.as_ref().map(|u| u.name.clone()),
            "target_user_id": id,
            "target_user_email": user.map(|u| u.email),
            "target_team_id": team,
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"redirect": "/"})).into_response())
}

async fn admin_back(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
) -> ApiResult {
    admin_access(&state, &ctx, &headers)?;
    let impersonator =
        crate::session::session_id(&headers).and_then(|id| state.sessions.impersonator(&id));
    if impersonator == Some(0) {
        crate::session::impersonate(&state, &headers, 0, None).await?;
        return Ok(ok(json!({"redirect": "/admin"})).into_response());
    }
    Ok(ok(json!({})).into_response())
}

/// `Webhook\\Stripe::events`: checks the signature, answers, then runs `StripeProcessJob`.
pub async fn stripe_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let answer = |code: StatusCode, text: &'static str| (code, text).into_response();
    let (Some(_), Some(secret)) = (config("STRIPE_API_KEY"), config("STRIPE_WEBHOOK_SECRET"))
    else {
        crate::webhooks::audit_failure("stripe", "stripe_not_configured", "");
        return answer(StatusCode::BAD_REQUEST, "Invalid signature.");
    };
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !signature_ok(signature, &body, &secret, Utc::now().timestamp()) {
        crate::webhooks::audit_failure("stripe", "invalid_signature", "");
        return answer(StatusCode::BAD_REQUEST, "Invalid signature.");
    }
    let Ok(event) = serde_json::from_slice::<Value>(&body) else {
        crate::webhooks::audit_failure("stripe", "invalid_payload", "");
        return answer(StatusCode::BAD_REQUEST, "Invalid webhook.");
    };
    tokio::spawn(async move {
        if let Err(e) = process_event(&state, &event).await {
            // `send_internal_notification('StripeProcessJob error: ...')`.
            tracing::error!(error = %e, "StripeProcessJob error");
        }
    });
    answer(
        StatusCode::OK,
        "Webhook received. Cool cool cool cool cool.",
    )
}

/// `Stripe\\Webhook::constructEvent`: `t=...,v1=...` over `{t}.{payload}`, within five minutes.
fn signature_ok(header: &str, payload: &[u8], secret: &str, now: i64) -> bool {
    use hmac::{Hmac, Mac};
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", v)) => signatures.push(v.to_owned()),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else {
        return false;
    };
    if (now - timestamp).abs() > 300 {
        return false;
    }
    signatures.iter().any(|sig| {
        let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(format!("{timestamp}.").as_bytes());
        mac.update(payload);
        let bytes: Vec<u8> = (0..sig.len() / 2)
            .filter_map(|i| {
                sig.get(i * 2..i * 2 + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect();
        mac.verify_slice(&bytes).is_ok()
    })
}

fn excluded(plan: Option<&str>) -> bool {
    match (config("STRIPE_EXCLUDED_PLANS"), plan) {
        (Some(list), Some(plan)) => list.contains(plan),
        _ => false,
    }
}

/// `StripeProcessJob::handle`. `send_internal_notification` is a log line here; the jobs it would
/// dispatch (`VerifyStripeSubscriptionStatusJob`, `SubscriptionInvoiceFailedJob`,
/// `ServerLimitCheckJob`) are not ported.
async fn process_event(state: &AppState, event: &Value) -> Result<(), String> {
    let db = &state.db;
    let data = event
        .pointer("/data/object")
        .cloned()
        .unwrap_or(Value::Null);
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let get = |p: &str| data.pointer(p).and_then(Value::as_str).map(str::to_owned);
    let failed = |e: coolify_db::DbError| e.to_string();
    let team_of = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.parse::<i64>().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    let customer = get("/customer");
    match kind {
        "radar.early_fraud_warning.created" => {
            let client = Stripe::new().ok_or("Stripe is not configured.")?;
            let describe = |e: StripeError| e.message;
            if let Some(charge) = get("/charge") {
                client
                    .post("/refunds", &[("charge", charge)])
                    .await
                    .map_err(describe)?;
            }
            let intent = get("/payment_intent").unwrap_or_default();
            let intent = client
                .get(&format!("/payment_intents/{intent}"), &[])
                .await
                .map_err(describe)?;
            let customer = intent
                .get("customer")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(customer) = customer else {
                return Err("Early fraud warning: subscription not found.".to_owned());
            };
            let list = client
                .get(
                    "/subscriptions",
                    &[("customer", customer.clone()), ("limit", "100".to_owned())],
                )
                .await
                .map_err(describe)?;
            let mut ids: Vec<String> = list
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|s| s.get("status").and_then(Value::as_str) != Some("incomplete_expired"))
                .filter_map(|s| s.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            let row = coolify_db::canvas::subscription_by_customer(db, &customer)
                .await
                .map_err(failed)?;
            if let Some(id) = row.as_ref().and_then(|r| text(r, "stripe_subscription_id"))
                && !ids.iter().any(|i| i == id)
            {
                ids.push(id.to_owned());
            }
            if ids.is_empty() {
                return Err("Early fraud warning: subscription not found.".to_owned());
            }
            for id in &ids {
                client
                    .call(
                        reqwest::Method::DELETE,
                        &format!("/subscriptions/{id}"),
                        &[],
                        None,
                    )
                    .await
                    .map_err(describe)?;
            }
            if let Some(team) = row
                .as_ref()
                .and_then(|r| r.get("team_id"))
                .and_then(Value::as_i64)
            {
                coolify_db::canvas::subscription_ended(db, team)
                    .await
                    .map_err(failed)?;
            }
        }
        "checkout.session.completed" => {
            let Some(reference) = get("/client_reference_id") else {
                return Ok(());
            };
            let (user, team) = reference.split_once(':').unwrap_or((&reference, ""));
            let (Ok(user), Ok(team)) = (user.parse::<i64>(), team.parse::<i64>()) else {
                return Err(format!("Bad client reference {reference}"));
            };
            if !coolify_db::canvas::is_team_admin(db, team, user)
                .await
                .map_err(failed)?
            {
                return Err(format!(
                    "User {user} is not an admin or owner of team {team}"
                ));
            }
            coolify_db::canvas::store_subscription(
                db,
                team,
                get("/subscription").as_deref(),
                customer.as_deref(),
                true,
                true,
            )
            .await
            .map_err(failed)?;
        }
        "invoice.paid" => {
            if excluded(get("/lines/data/0/plan/id").as_deref()) {
                return Ok(());
            }
            let Some(row) = coolify_db::canvas::subscription_by_customer(
                db,
                customer.as_deref().unwrap_or_default(),
            )
            .await
            .map_err(failed)?
            else {
                return Ok(());
            };
            let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
            if let Some(id) = text(&row, "stripe_subscription_id") {
                let client = Stripe::new().ok_or("Stripe is not configured.")?;
                let current = client
                    .get(&format!("/subscriptions/{id}"), &[])
                    .await
                    .map_err(|e| e.message)?;
                match current.get("status").and_then(Value::as_str) {
                    Some("active") => {
                        coolify_db::canvas::update_subscription(
                            db,
                            team,
                            &[
                                ("stripe_invoice_paid", json!(true)),
                                ("stripe_past_due", json!(false)),
                            ],
                        )
                        .await
                        .map_err(failed)?;
                    }
                    Some("past_due") => {
                        coolify_db::canvas::update_subscription(
                            db,
                            team,
                            &[
                                ("stripe_invoice_paid", json!(true)),
                                ("stripe_past_due", json!(true)),
                            ],
                        )
                        .await
                        .map_err(failed)?;
                    }
                    other => {
                        tracing::warn!(status = ?other, "invoice.paid for a subscription that is not active")
                    }
                }
            }
        }
        "invoice.payment_failed" | "payment_intent.payment_failed" => {
            tracing::warn!(kind, customer = ?customer, "Stripe payment failed");
        }
        "customer.subscription.created" => {
            let team = team_of(data.pointer("/metadata/team_id"));
            let user = team_of(data.pointer("/metadata/user_id"));
            let (Some(team), Some(user)) = (team, user) else {
                return Err("No team id or user id found".to_owned());
            };
            if !coolify_db::canvas::is_team_admin(db, team, user)
                .await
                .map_err(failed)?
            {
                return Err(format!(
                    "User {user} is not an admin or owner of team {team}"
                ));
            }
            let id = get("/id");
            coolify_db::canvas::store_subscription(
                db,
                team,
                id.as_deref(),
                customer.as_deref(),
                false,
                false,
            )
            .await
            .map_err(failed)?;
            let row = coolify_db::canvas::team_subscription(db, team)
                .await
                .map_err(failed)?;
            if let Some(row) = row {
                if text(&row, "stripe_customer_id") != customer.as_deref() {
                    return Err(format!("Stripe customer ID mismatch for team {team}"));
                }
                if text(&row, "stripe_subscription_id").is_none() {
                    coolify_db::canvas::update_subscription(
                        db,
                        team,
                        &[("stripe_subscription_id", json!(id))],
                    )
                    .await
                    .map_err(failed)?;
                }
            }
        }
        "customer.subscription.updated" => {
            let plan = get("/items/data/0/plan/id").or_else(|| get("/plan/id"));
            if excluded(plan.as_deref()) {
                return Ok(());
            }
            let status = get("/status").unwrap_or_default();
            let id = get("/items/data/0/subscription").or_else(|| get("/id"));
            let found = coolify_db::canvas::subscription_by_customer(
                db,
                customer.as_deref().unwrap_or_default(),
            )
            .await
            .map_err(failed)?;
            let team = match &found {
                Some(row) => row.get("team_id").and_then(Value::as_i64).unwrap_or(-1),
                None => {
                    if status == "incomplete_expired" {
                        return Err("Subscription incomplete expired".to_owned());
                    }
                    let Some(team) = team_of(data.pointer("/metadata/team_id")) else {
                        return Err("No subscription and team id found".to_owned());
                    };
                    coolify_db::canvas::store_subscription(
                        db,
                        team,
                        id.as_deref(),
                        customer.as_deref(),
                        false,
                        false,
                    )
                    .await
                    .map_err(failed)?;
                    team
                }
            };
            let row = coolify_db::canvas::team_subscription(db, team)
                .await
                .map_err(failed)?
                .ok_or("Subscription is gone")?;
            if get("/items/data/0/price/lookup_key").is_some_and(|l| l.contains("dynamic")) {
                let quantity = data
                    .pointer("/items/data/0/quantity")
                    .and_then(Value::as_i64)
                    .unwrap_or(2)
                    .clamp(MIN_SERVER_LIMIT, MAX_SERVER_LIMIT);
                coolify_db::canvas::set_server_limit(db, team, quantity)
                    .await
                    .map_err(failed)?;
            }
            coolify_db::canvas::update_subscription(
                db,
                team,
                &[
                    (
                        "stripe_feedback",
                        data.pointer("/cancellation_details/feedback")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "stripe_comment",
                        data.pointer("/cancellation_details/comment")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    ("stripe_plan_id", json!(plan)),
                    (
                        "stripe_cancel_at_period_end",
                        json!(
                            data.get("cancel_at_period_end")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                        ),
                    ),
                ],
            )
            .await
            .map_err(failed)?;
            let same = text(&row, "stripe_subscription_id") == id.as_deref();
            match status.as_str() {
                "paused" | "incomplete_expired" if same => {
                    coolify_db::canvas::update_subscription(
                        db,
                        team,
                        &[("stripe_invoice_paid", json!(false))],
                    )
                    .await
                    .map_err(failed)?;
                }
                "past_due" if same => {
                    coolify_db::canvas::update_subscription(
                        db,
                        team,
                        &[("stripe_past_due", json!(true))],
                    )
                    .await
                    .map_err(failed)?;
                }
                "unpaid" => {
                    if same {
                        coolify_db::canvas::update_subscription(
                            db,
                            team,
                            &[("stripe_invoice_paid", json!(false))],
                        )
                        .await
                        .map_err(failed)?;
                    }
                    coolify_db::canvas::subscription_ended(db, team)
                        .await
                        .map_err(failed)?;
                }
                "active" if same => {
                    coolify_db::canvas::update_subscription(
                        db,
                        team,
                        &[
                            ("stripe_past_due", json!(false)),
                            ("stripe_invoice_paid", json!(true)),
                        ],
                    )
                    .await
                    .map_err(failed)?;
                }
                _ => {}
            }
        }
        "customer.subscription.deleted" => {
            let row = coolify_db::canvas::subscription_by_customer(
                db,
                customer.as_deref().unwrap_or_default(),
            )
            .await
            .map_err(failed)?;
            if let Some(row) =
                row.filter(|r| text(r, "stripe_subscription_id") == get("/id").as_deref())
            {
                let team = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
                coolify_db::canvas::subscription_ended(db, team)
                    .await
                    .map_err(failed)?;
            }
        }
        other => return Err(format!("Unhandled event type: {other}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};

    fn sign(payload: &str, secret: &str, t: i64) -> String {
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("key");
        mac.update(format!("{t}.{payload}").as_bytes());
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("t={t},v1={hex}")
    }

    #[test]
    fn stripe_signature() {
        let now = 1_800_000_000;
        assert!(signature_ok(&sign("{}", "whsec", now), b"{}", "whsec", now));
        assert!(!signature_ok(
            &sign("{}", "other", now),
            b"{}",
            "whsec",
            now
        ));
        assert!(!signature_ok(
            &sign("{}", "whsec", now - 301),
            b"{}",
            "whsec",
            now
        ));
        assert!(!signature_ok("", b"{}", "whsec", now));
    }
}
