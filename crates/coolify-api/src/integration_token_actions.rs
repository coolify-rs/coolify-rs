//! `CreateIntegrationToken` and `UpdateIntegrationToken` (v4.6.0): the token rules and the create and
//! update of an integration token, shared by the API and the Keys & Tokens page.

use coolify_db::integration_tokens as it;
use coolify_db::records::Record;
use coolify_jobs::integrations::cloudflare;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::ApiError;
use crate::state::AppState;

/// `IntegrationToken::PROVIDER_NAMES`.
pub const PROVIDERS: &[&str] = &["cloudflare", "doppler", "infisical", "vault"];

/// Why a create or an update did not happen.
#[derive(Debug)]
pub enum ActionError {
    /// `ValidationException`: field => messages.
    Validation(Map<String, Value>),
    /// `ConnectionException`: `connectionErrorMessage($provider)`.
    Connection(String),
    /// `RuntimeException` with its message.
    Runtime(String),
    Api(ApiError),
}

impl From<ApiError> for ActionError {
    fn from(e: ApiError) -> Self {
        ActionError::Api(e)
    }
}

impl From<coolify_db::DbError> for ActionError {
    fn from(e: coolify_db::DbError) -> Self {
        ActionError::Api(e.into())
    }
}

/// `IntegrationToken::allowedCapabilities($provider)`.
pub fn allowed_capabilities(provider: &str) -> &'static [&'static str] {
    if provider == "cloudflare" {
        &["dns", "tunnel"]
    } else {
        &["secrets"]
    }
}

/// `filled($value)`.
pub fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// A `json`/`array` cast column of a token row.
pub fn json_column(r: &Record, k: &str) -> Value {
    match r.get(k) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

fn text<'a>(r: &'a Record, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// `automaticDnsEnabled()`.
pub fn automatic_dns_enabled(provider: &str, metadata: &Value) -> bool {
    provider == "cloudflare" && metadata.get("automatic_dns") != Some(&Value::Bool(false))
}

/// `proxiedDnsEnabled()`.
pub fn proxied_dns_enabled(provider: &str, metadata: &Value) -> bool {
    provider == "cloudflare" && metadata.get("proxied_dns") == Some(&Value::Bool(true))
}

fn add(errors: &mut Map<String, Value>, field: &str, message: impl Into<String>) {
    let message = Value::String(message.into());
    match errors.get_mut(field) {
        Some(Value::Array(list)) => list.push(message),
        _ => {
            errors.insert(field.to_owned(), Value::Array(vec![message]));
        }
    }
}

/// `url:http,https`.
fn http_url(v: &str) -> bool {
    (v.starts_with("http://") || v.starts_with("https://")) && crate::laravel_url::is_url(v)
}

/// The messages of a request's rules: `required` is `This field is required.` in the API
/// (`customApiValidator`), Laravel's default on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Messages {
    Api,
    Page,
}

impl Messages {
    fn required(self, attribute: &str) -> String {
        match self {
            Messages::Api => "This field is required.".to_owned(),
            Messages::Page => format!("The {attribute} field is required."),
        }
    }
}

/// `CreateIntegrationToken::rules($provider)` (`provider` only on create, `token` nullable on update)
/// against `data`, in rule order. `token_field` is the attribute the token is validated as
/// (`newToken` on the editor, `token` elsewhere).
pub fn rule_errors(
    provider: &str,
    data: &Map<String, Value>,
    creating: bool,
    messages: Messages,
) -> Map<String, Value> {
    let mut errors = Map::new();
    let present = |k: &str| data.get(k).filter(|v| filled(v));
    if creating {
        match data.get("provider") {
            v if v.is_none_or(|v| !filled(v)) => {
                add(&mut errors, "provider", messages.required("provider"))
            }
            Some(Value::String(p)) if PROVIDERS.contains(&p.as_str()) => {}
            _ => add(&mut errors, "provider", "The selected provider is invalid."),
        }
    }
    match present("name") {
        None => add(&mut errors, "name", messages.required("name")),
        Some(Value::String(n)) => {
            if n.chars().count() > 255 {
                add(
                    &mut errors,
                    "name",
                    "The name field must not be greater than 255 characters.",
                );
            }
        }
        Some(_) => add(&mut errors, "name", "The name field must be a string."),
    }
    match present("token") {
        None if creating => add(&mut errors, "token", messages.required("token")),
        None => {}
        Some(Value::String(t)) => {
            if provider == "doppler" && !(t.starts_with("dp.st.") || t.starts_with("dp.sa.")) {
                add(&mut errors, "token", "The token field format is invalid.");
            }
        }
        Some(_) => add(&mut errors, "token", "The token field must be a string."),
    }
    match data.get("capabilities") {
        v if v.is_none_or(|v| !filled(v)) => add(
            &mut errors,
            "capabilities",
            messages.required("capabilities"),
        ),
        Some(Value::Array(caps)) => {
            if caps.iter().any(|c| c == "tunnel") && !caps.iter().any(|c| c == "dns") {
                add(
                    &mut errors,
                    "capabilities",
                    "Cloudflare Tunnel also needs the DNS capability.",
                );
            }
            let allowed = allowed_capabilities(provider);
            for (i, c) in caps.iter().enumerate() {
                let field = format!("capabilities.{i}");
                if !filled(c) {
                    add(&mut errors, &field, messages.required(&field));
                } else if !c.as_str().is_some_and(|c| allowed.contains(&c)) {
                    add(
                        &mut errors,
                        &field,
                        format!("The selected {field} is invalid."),
                    );
                }
            }
        }
        Some(_) => add(
            &mut errors,
            "capabilities",
            "The capabilities field must be an array.",
        ),
    }
    let metadata = data.get("metadata").and_then(Value::as_object);
    let meta = |k: &str| metadata.and_then(|m| m.get(k)).filter(|v| filled(v));
    if matches!(provider, "infisical" | "vault") {
        match meta("base_url") {
            None => add(
                &mut errors,
                "metadata.base_url",
                messages.required("metadata.base url"),
            ),
            Some(Value::String(u)) if http_url(u) => {}
            Some(_) => add(
                &mut errors,
                "metadata.base_url",
                "The metadata.base url field must be a valid URL.",
            ),
        }
    }
    if provider == "infisical" {
        match meta("client_id") {
            None => add(
                &mut errors,
                "metadata.client_id",
                messages.required("metadata.client id"),
            ),
            Some(Value::String(_)) => {}
            Some(_) => add(
                &mut errors,
                "metadata.client_id",
                "The metadata.client id field must be a string.",
            ),
        }
    }
    if provider == "vault"
        && let Some(v) = meta("namespace")
        && !v.is_string()
    {
        add(
            &mut errors,
            "metadata.namespace",
            "The metadata.namespace field must be a string.",
        );
    }
    if matches!(provider, "infisical" | "vault")
        && let Some(v) = meta("timeout")
    {
        let n = v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
        match n {
            None => add(
                &mut errors,
                "metadata.timeout",
                "The metadata.timeout field must be an integer.",
            ),
            Some(n) if n < 1 => add(
                &mut errors,
                "metadata.timeout",
                "The metadata.timeout field must be at least 1.",
            ),
            Some(n) if n > 300 => add(
                &mut errors,
                "metadata.timeout",
                "The metadata.timeout field must not be greater than 300.",
            ),
            _ => {}
        }
    }
    errors
}

/// The `metadata`, `automatic_dns` and `proxied_dns` rules of the API: settings only for a secret
/// manager, the DNS switches only for Cloudflare.
pub fn flag_errors(provider: &str, body: &Map<String, Value>, errors: &mut Map<String, Value>) {
    let cloudflare = provider == "cloudflare";
    if body.contains_key("metadata") {
        if cloudflare {
            add(errors, "metadata", "The metadata field is prohibited.");
        } else if !body.get("metadata").is_some_and(Value::is_object) {
            add(errors, "metadata", "The metadata field must be an array.");
        }
    }
    for (key, attribute) in [
        ("automatic_dns", "automatic dns"),
        ("proxied_dns", "proxied dns"),
    ] {
        if !body.contains_key(key) {
            continue;
        }
        if !cloudflare {
            add(errors, key, format!("The {attribute} field is prohibited."));
        } else if !body
            .get(key)
            .is_some_and(crate::validation::is_laravel_boolean)
        {
            add(
                errors,
                key,
                format!("The {attribute} field must be true or false."),
            );
        }
    }
}

/// `IntegrationTokenValidator::validate`: a Cloudflare connection failure is `Connection`, a base
/// URL that `SafeExternalUrl` refuses a `Validation` of `base_url`.
async fn verify(
    state: &AppState,
    provider: &str,
    token: &str,
    capabilities: &[String],
    metadata: &Map<String, Value>,
) -> Result<bool, ActionError> {
    if provider == "cloudflare" {
        return cloudflare::validate_token(token, capabilities)
            .await
            .map_err(|_| ActionError::Connection(cloudflare::connection_error_message(provider)));
    }
    coolify_jobs::integrations::secret_managers::validate(&state.db, provider, token, metadata, "")
        .await
        .map_err(|message| {
            let mut errors = Map::new();
            errors.insert("base_url".into(), json!([message]));
            ActionError::Validation(errors)
        })
}

/// `$cloudflare->syncZones($token)` inside the transaction of the action.
async fn sync_zones(
    state: &AppState,
    provider: &str,
    id: i64,
    token: &str,
) -> Result<(), ActionError> {
    cloudflare::sync_zones_checked(&state.db, id, token, state.now())
        .await
        .map(|_| ())
        .map_err(|e| match e {
            cloudflare::SyncZonesError::Connection(_) => {
                ActionError::Connection(cloudflare::connection_error_message(provider))
            }
            cloudflare::SyncZonesError::Failed(m) => ActionError::Runtime(m),
        })
}

/// `CreateIntegrationToken::handle`: the uuid of the new token.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    state: &AppState,
    team_id: i64,
    provider: &str,
    name: &str,
    token: &str,
    capabilities: &[String],
    metadata: &Map<String, Value>,
    automatic_dns: bool,
    proxied_dns: bool,
) -> Result<String, ActionError> {
    let mut metadata: Map<String, Value> = metadata
        .iter()
        .filter(|(_, v)| filled(v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if provider == "cloudflare" && !automatic_dns {
        metadata.insert("automatic_dns".into(), json!(false));
    }
    if provider == "cloudflare" && proxied_dns {
        metadata.insert("proxied_dns".into(), json!(true));
    }
    if !verify(state, provider, token, capabilities, &metadata).await? {
        return Err(ActionError::Runtime(
            crate::routes::integration_tokens::error_message(provider).to_owned(),
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let caps_json = coolify_compat::php_json::encode(&json!(capabilities));
    let meta_json = (!metadata.is_empty())
        .then(|| coolify_compat::php_json::encode(&Value::Object(metadata.clone())));
    let id = it::create(
        &state.db,
        &it::NewToken {
            uuid: &uuid,
            team_id,
            provider,
            name,
            token_encrypted: &enc.encrypt_string(token),
            capabilities: &caps_json,
            metadata: meta_json.as_deref(),
        },
        state.now(),
    )
    .await?;
    if provider == "cloudflare"
        && let Err(e) = sync_zones(state, provider, id, token).await
    {
        // `DB::transaction`: the token is not kept when its zones cannot be read.
        let _ = sqlx::query("DELETE FROM dns_provider_zones WHERE integration_token_id = $1")
            .bind(id)
            .execute(state.db.pool())
            .await;
        it::delete(&state.db, id).await?;
        return Err(e);
    }
    Ok(uuid)
}

/// `baseUrlForAudit`: the base URL without user info, query, or fragment.
pub fn base_url_for_audit(v: Option<&Value>) -> Value {
    let Some(url) = v.and_then(Value::as_str).filter(|u| !u.is_empty()) else {
        return Value::Null;
    };
    let Some((scheme, rest)) = url.split_once("://") else {
        return json!("[invalid-url]");
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].rsplit('@').next().unwrap_or_default();
    if authority.is_empty() {
        return json!("[invalid-url]");
    }
    let path_end = rest[end..].find(['?', '#']).map_or(rest.len(), |i| end + i);
    json!(format!("{scheme}://{authority}{}", &rest[end..path_end]))
}

/// What an update changes.
#[derive(Debug, Clone)]
pub struct Update<'a> {
    pub name: &'a str,
    pub new_token: Option<&'a str>,
    pub capabilities: Vec<String>,
    /// The complete provider settings.
    pub metadata: Map<String, Value>,
    pub automatic_dns: bool,
    pub proxied_dns: bool,
    /// `api` or `ui`.
    pub audit_source: &'a str,
}

/// `UpdateIntegrationToken::handle`.
pub async fn update(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    row: &Record,
    u: Update<'_>,
) -> Result<(), ActionError> {
    let provider = text(row, "provider").to_owned();
    let uuid = text(row, "uuid").to_owned();
    let id = coolify_db::database_parts::id_of(row);
    let new_token = u.new_token.filter(|t| !t.trim().is_empty());
    let stored = json_column(row, "metadata")
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut metadata: Map<String, Value> = u
        .metadata
        .iter()
        .filter(|(_, v)| filled(v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if provider == "cloudflare" {
        // Cloudflare has no user settings in metadata; system keys such as zones_synced_at stay.
        metadata = stored.clone();
        metadata.shift_remove("automatic_dns");
        metadata.shift_remove("proxied_dns");
        if !u.automatic_dns {
            metadata.insert("automatic_dns".into(), json!(false));
        }
        if u.proxied_dns {
            metadata.insert("proxied_dns".into(), json!(true));
        }
    }
    let fields: &[&str] = match provider.as_str() {
        "vault" => &["base_url", "namespace"],
        "infisical" => &["base_url", "client_id"],
        _ => &[],
    };
    let as_text = |m: &Map<String, Value>, k: &str| match m.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(Value::Bool(true)) => "1".to_owned(),
        Some(Value::Bool(false)) => String::new(),
        Some(v) => v.to_string(),
    };
    let changed: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|k| as_text(&stored, k) != as_text(&metadata, k))
        .collect();
    if !changed.is_empty() && new_token.is_none() {
        let message = match provider.as_str() {
            "infisical" => {
                "Enter the client secret again when you change the base URL or the client ID."
            }
            "vault" => "Enter the token again when you change the base URL or the namespace.",
            _ => "Enter the token again when you change the connection settings.",
        };
        let mut errors = Map::new();
        errors.insert("token".into(), json!([message]));
        return Err(ActionError::Validation(errors));
    }
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let stored_caps: Vec<String> = json_column(row, "capabilities")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let caps_changed = sorted(u.capabilities.clone()) != sorted(stored_caps);
    let meta_changed = Value::Object(metadata.clone()) != Value::Object(stored.clone());
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let current = match new_token {
        Some(t) => t.to_owned(),
        None => row
            .get("token")
            .and_then(Value::as_str)
            .and_then(|c| enc.decrypt_string(c).ok())
            .unwrap_or_default(),
    };
    if (new_token.is_some() || caps_changed || meta_changed)
        && !verify(state, &provider, &current, &u.capabilities, &metadata).await?
    {
        return Err(ActionError::Runtime(
            crate::routes::integration_tokens::error_message(&provider).to_owned(),
        ));
    }
    let caps_json = coolify_compat::php_json::encode(&json!(u.capabilities));
    let meta_json = (!metadata.is_empty())
        .then(|| coolify_compat::php_json::encode(&Value::Object(metadata.clone())));
    let pool = state.db.pool();
    let write = move |name: &str,
                      caps: String,
                      meta: Option<String>,
                      token: Option<String>,
                      at: chrono::NaiveDateTime| {
        sqlx::query(
            "UPDATE integration_tokens SET name = $1, capabilities = $2::json, metadata = $3::json, \
             token = COALESCE($4, token), updated_at = $5 WHERE id = $6",
        )
        .bind(name.to_owned())
        .bind(caps)
        .bind(meta)
        .bind(token)
        .bind(at)
        .bind(id)
        .execute(pool)
    };
    write(
        u.name,
        caps_json,
        meta_json,
        new_token.map(|t| enc.encrypt_string(t)),
        state.now(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    if provider == "cloudflare"
        && (new_token.is_some() || caps_changed)
        && let Err(e) = sync_zones(state, &provider, id, &current).await
    {
        // `DB::transaction`: the update is undone when the zones cannot be read.
        let old_meta = (!stored.is_empty())
            .then(|| coolify_compat::php_json::encode(&Value::Object(stored.clone())));
        let old_caps = coolify_compat::php_json::encode(&json_column(row, "capabilities"));
        let old_updated = row
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(|t| chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").ok())
            .unwrap_or_else(|| state.now());
        let _ = write(
            text(row, "name"),
            old_caps,
            old_meta,
            row.get("token").and_then(Value::as_str).map(str::to_owned),
            old_updated,
        )
        .await;
        return Err(e);
    }
    let mut context = json!({
        "team_id": row.get("team_id"),
        "integration_token_uuid": uuid,
        "integration_token_name": u.name,
        "provider": provider,
        "rotated": new_token.is_some(),
        "connection_changed_fields": changed,
    });
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("{}.integration_token.updated", u.audit_source),
        context.take(),
        "info",
    )
    .await;
    if changed.contains(&"base_url") {
        crate::audit::log(
            state,
            ctx,
            meta,
            &format!("{}.integration_token.base_url_changed", u.audit_source),
            json!({
                "team_id": row.get("team_id"),
                "integration_token_uuid": uuid,
                "integration_token_name": u.name,
                "provider": provider,
                "previous_base_url": base_url_for_audit(stored.get("base_url")),
                "base_url": base_url_for_audit(metadata.get("base_url")),
            }),
            "warning",
        )
        .await;
    }
    Ok(())
}

/// `IntegrationToken::deletionBlocker()`.
pub async fn deletion_blocker(state: &AppState, id: i64) -> Result<Option<&'static str>, ApiError> {
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM secret_manager_links WHERE integration_token_id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    if links > 0 {
        return Ok(Some(
            "This token is used by one or more resources as a secret manager source. Remove those links first.",
        ));
    }
    let records: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM managed_dns_records WHERE integration_token_id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    if records > 0 {
        return Ok(Some(
            "This token manages DNS records. Remove those domains or records first.",
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    /// The rules of `CreateIntegrationToken::rules()`, in rule order, with the API messages.
    #[test]
    fn create_rules() {
        let errors = rule_errors(
            "vault",
            &data(
                json!({"provider": "vault", "name": "", "capabilities": ["dns", "secrets"],
                "metadata": {"base_url": "ftp://x", "timeout": 0}}),
            ),
            true,
            Messages::Api,
        );
        assert_eq!(
            Value::Object(errors),
            json!({
                "name": ["This field is required."],
                "token": ["This field is required."],
                "capabilities.0": ["The selected capabilities.0 is invalid."],
                "metadata.base_url": ["The metadata.base url field must be a valid URL."],
                "metadata.timeout": ["The metadata.timeout field must be at least 1."],
            })
        );
        let errors = rule_errors(
            "cloudflare",
            &data(
                json!({"provider": "cloudflare", "name": "cf", "token": "t", "capabilities": ["tunnel"]}),
            ),
            true,
            Messages::Api,
        );
        assert_eq!(
            Value::Object(errors),
            json!({"capabilities": ["Cloudflare Tunnel also needs the DNS capability."]})
        );
        // Update: the token is optional.
        let errors = rule_errors(
            "doppler",
            &data(json!({"name": "d", "token": null, "capabilities": ["secrets"]})),
            false,
            Messages::Api,
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn flags() {
        let mut errors = Map::new();
        flag_errors(
            "cloudflare",
            &data(json!({"metadata": {}, "automatic_dns": "x"})),
            &mut errors,
        );
        flag_errors("vault", &data(json!({"proxied_dns": true})), &mut errors);
        assert_eq!(
            Value::Object(errors),
            json!({
                "metadata": ["The metadata field is prohibited."],
                "automatic_dns": ["The automatic dns field must be true or false."],
                "proxied_dns": ["The proxied dns field is prohibited."],
            })
        );
    }

    #[test]
    fn dns_switches() {
        assert!(automatic_dns_enabled("cloudflare", &json!(null)));
        assert!(!automatic_dns_enabled(
            "cloudflare",
            &json!({"automatic_dns": false})
        ));
        assert!(!automatic_dns_enabled("vault", &json!(null)));
        assert!(proxied_dns_enabled(
            "cloudflare",
            &json!({"proxied_dns": true})
        ));
        assert!(!proxied_dns_enabled("cloudflare", &json!({})));
    }
}
