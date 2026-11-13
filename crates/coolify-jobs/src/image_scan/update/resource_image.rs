//! `App\Actions\ImageScan\UpdateResourceImage`: switches a resource to a candidate image that
//! fixes vulnerabilities (spec 7.3). Only a better candidate of the resource's current scan is
//! accepted. A major version is never accepted.
//!
//! A newer tag of the same line changes the configuration, so it stays after later deploys and
//! restarts:
//! - Docker image app: the tag, then a deploy. Standalone database: `image`, then a restart.
//! - Service: the image line of every application and database that uses the image, in the
//!   compose file too; or the Coolify environment variable when a variable sets the image
//!   (`${IMAGE}` or `repo:${TAG}`); then a restart.
//! - Compose app: the compose file comes from Git, so Coolify keeps an image override per Compose
//!   service (`docker_compose_image_overrides`) that the parser applies while the file still
//!   names the replaced image; or the environment variable as above; then a deploy.
//!
//! A newer build of the same tag changes no configuration: the next start pulls the scanned
//! digest and gives it the tag ([`image_start_pin`]), so the containers run exactly the build
//! that Coolify scanned. Every path puts the old configuration back when the restart or deploy
//! cannot start.
//!
//! Differences from upstream, because coolify-rs starts services and databases in the
//! background (`Jobs::spawn_service_start`, `Jobs::spawn_database_restart`):
//! - Service: `RestartService::run()` is the parse of the service (`$service->parse()`, the
//!   first step of `StartService`, here in the request) and then the start in the background.
//!   Only a failed parse puts the old configuration back; a failure later in the start is in
//!   the start log, as for every other restart.
//! - Standalone database: the checks of `RestartDatabase` that answer with a message (server
//!   not functional, the CA of a database with SSL, another operation in progress) run in the
//!   request; the stop and the start run in the background.

use std::collections::HashMap;

use coolify_compat::yaml::{self, Key, PhpValue};
use coolify_db::applications as apps;
use coolify_db::audit as audit_log;
use coolify_db::databases::{self as dbs, Kind};
use coolify_db::environment_variables as env_store;
use coolify_db::image_scans::update as store;
use coolify_db::image_scans::{self, ImageScanRow, SERVICE_MORPH, ScanOwner};
use coolify_db::servers::{self, ColumnValue};
use coolify_db::services as svc;
use coolify_remote::text::php_trim;
use serde_json::{Value, json};

use super::super::compose_image_variable::ComposeImageVariable;
use super::super::error::ImageUpdateRefused;
use super::super::image_ref::ImageRef;
use super::super::resource_scans;
use super::image_start_pin;
use crate::Jobs;
use crate::applications::queue::{QueueOptions, Queued};
use crate::compose_parser::{php, validate};
use crate::server::ServerInfo;

/// `UpdateResourceImage::NO_COMPOSE_SERVICE`.
const NO_COMPOSE_SERVICE: &str = "No service of the compose file runs this image in a way Coolify can change. Change the version in the compose file.";

/// The image names a variable in a form that `ComposeImageVariable` does not support.
const UNSUPPORTED_VARIABLE: &str = "The compose file sets this image in a way Coolify cannot change. Change the version in the compose file.";

/// Why `UpdateResourceImage` did not update the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// `AuthorizationException`: the resource belongs to Coolify (id 0).
    Forbidden(String),
    /// `ImageUpdateRefused`: the ref is not a better candidate, or the update cannot start. The
    /// message is shown to the user.
    Refused(ImageUpdateRefused),
    /// Any other failure (the database, the encryption key).
    Other(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Forbidden(message) | UpdateError::Other(message) => f.write_str(message),
            UpdateError::Refused(refused) => f.write_str(&refused.0),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<ImageUpdateRefused> for UpdateError {
    fn from(refused: ImageUpdateRefused) -> Self {
        UpdateError::Refused(refused)
    }
}

fn refuse(message: impl Into<String>) -> ImageUpdateRefused {
    ImageUpdateRefused(message.into())
}

fn refused(message: impl Into<String>) -> UpdateError {
    UpdateError::Refused(refuse(message))
}

fn db_error(e: coolify_db::DbError) -> UpdateError {
    UpdateError::Other(e.to_string())
}

/// One part of a merged update of a service (`array{owner: Model, ref: string}`): the
/// application or database of the service, and the candidate ref of its current scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePart {
    /// `$part['owner']->getMorphClass()`.
    pub owner_morph: String,
    /// `$part['owner']->getKey()`.
    pub owner_id: i64,
    /// `$part['ref']`.
    pub reference: String,
}

/// `ImageChange`: the owner, the scanned image, the new tag, and for a newer build of the same
/// tag its digest.
#[derive(Debug, Clone)]
struct ImageChange {
    owner: ScanOwner,
    old: String,
    tag: String,
    same_tag: bool,
    digest: Option<String>,
}

/// Waits the scanned build of a change for the next start. A closure, so the plans stay pure.
type PinFn<'a> = &'a mut dyn FnMut(&ImageChange) -> Result<(), UpdateError>;

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// `UpdateResourceImage::handle($resource, $newRef, $by)`: `resource` is an application, a
/// service, an application or database of a service, or a standalone database.
///
/// The caller authorizes `update` on the resource (for a part of a service: on the service)
/// for the user before it calls this.
pub async fn handle(
    jobs: &Jobs,
    resource: &ScanOwner,
    new_ref: &str,
    by_user_id: i64,
) -> Result<(), UpdateError> {
    let db = &jobs.0.db;
    let service = if resource.is_service() {
        Some(resource.clone())
    } else {
        image_scans::owner_service(db, resource)
            .await
            .map_err(db_error)?
    };

    authorize_for(jobs, service.as_ref().unwrap_or(resource), resource.id == 0).await?;

    if resource.is_application()
        && !matches!(resource.build_pack(), Some("dockerimage" | "dockercompose"))
    {
        return Err(refused(
            "Coolify builds the image of this app, so there is no image to update.",
        ));
    }
    if !resource.is_application() && service.is_none() && database_kind(resource).is_none() {
        return Err(refused("Coolify cannot update the image of this resource."));
    }

    let owners = if resource.is_service() {
        image_scans::owners(db, resource).await.map_err(db_error)?
    } else {
        vec![resource.clone()]
    };
    let (owner, scan, candidate) = candidate(jobs, &owners, new_ref).await?;

    let old_ref = scan.image_ref.clone();
    let change = change(&owner, &scan, &candidate, new_ref)?;

    let updated = match &service {
        Some(service) => update_service(jobs, service, &[change]).await?,
        None if owner.build_pack() == Some("dockercompose") => {
            update_compose_application(jobs, &owner, &[change]).await?
        }
        None if owner.is_application() => update_application(jobs, &owner, &change).await?,
        None => update_database(jobs, &owner, &change, by_user_id).await?,
    };

    audit(
        jobs,
        service.as_ref().unwrap_or(&owner),
        by_user_id,
        &updated,
        &[json!({"old_ref": old_ref, "new_ref": new_ref, "kind": candidate.get("kind")})],
    )
    .await;
    Ok(())
}

/// `UpdateResourceImage::handleParts($service, $parts, $by)`: updates several images of one
/// service in one change and one restart: the parts of a merged update ("Update n8n and n8n
/// runners to 2.10.4."). Each part must be a better candidate of the current scan of its own
/// application or database. When the restart fails, every image and the compose file are put
/// back.
///
/// The caller authorizes `update` on the service for the user before it calls this.
pub async fn handle_parts(
    jobs: &Jobs,
    service: &ScanOwner,
    parts: &[UpdatePart],
    by_user_id: i64,
) -> Result<(), UpdateError> {
    if !service.is_service() {
        return Err(refused("Coolify cannot update the image of this resource."));
    }
    authorize_for(jobs, service, false).await?;

    if parts.is_empty() {
        return Err(refused("This update has no images."));
    }

    let owners = image_scans::owners(&jobs.0.db, service)
        .await
        .map_err(db_error)?;
    let mut changes = Vec::with_capacity(parts.len());
    let mut log = Vec::with_capacity(parts.len());
    for part in parts {
        let Some(owner) = owners
            .iter()
            .find(|owner| owner.morph == part.owner_morph && owner.id == part.owner_id)
        else {
            return Err(refused(
                "An image of this update does not belong to this service.",
            ));
        };

        let (_, scan, candidate) =
            candidate(jobs, std::slice::from_ref(owner), &part.reference).await?;
        changes.push(change(owner, &scan, &candidate, &part.reference)?);
        log.push(json!({
            "old_ref": scan.image_ref,
            "new_ref": part.reference,
            "kind": candidate.get("kind"),
        }));
    }

    let updated = update_service(jobs, service, &changes).await?;

    audit(jobs, service, by_user_id, &updated, &log).await;
    Ok(())
}

/// `UpdateResourceImage::authorizeFor($target, $by, $isCoolifyOwner)`.
async fn authorize_for(
    jobs: &Jobs,
    target: &ScanOwner,
    is_coolify_owner: bool,
) -> Result<(), UpdateError> {
    // `Gate::forUser($by)->authorize('update', $target)` is checked by the caller, which has the
    // policies: `coolify_api::routes::image_scan::update_resource_image` and
    // `update_resource_images`.

    if is_coolify_owner || target.id == 0 {
        return Err(UpdateError::Forbidden(
            "Coolify does not change the images of its own resources.".to_owned(),
        ));
    }

    // Another Coolify instance manages a transferred server; this one keeps only an archive of it.
    if let Some(server_id) = target.main_server_id
        && let Some(server) = servers::find_by_id(&jobs.0.db, server_id)
            .await
            .map_err(db_error)?
        && ServerInfo::from_row(&server).transferred
    {
        return Err(refused("Another Coolify instance manages this server now."));
    }
    Ok(())
}

/// The `auditLog('ui.image_scan.image_updated', …)` of `UpdateResourceImage::audit()`. The
/// address and the browser of the request are not known here.
async fn audit(
    jobs: &Jobs,
    target: &ScanOwner,
    by_user_id: i64,
    updated: &[String],
    changes: &[Value],
) {
    let db = &jobs.0.db;
    let (name, email) = audit_log::actor_user(db, by_user_id)
        .await
        .ok()
        .flatten()
        .unzip();
    let context = audit_context(
        target,
        by_user_id,
        name.as_deref(),
        email.as_deref(),
        updated,
        changes,
    );
    let actor = audit_log::Actor {
        user_id: Some(by_user_id),
        name,
        email,
        ..audit_log::Actor::default()
    };
    if let Err(e) = audit_log::record(
        db,
        target.team_id,
        "ui.image_scan.image_updated",
        &context,
        "info",
        &actor,
        None,
        Jobs::now(),
    )
    .await
    {
        tracing::warn!(error = %e, "audit log of an image update failed");
    }
    // The `Auditable` rows of the changed models (Application, Service, the database,
    // EnvironmentVariable) are written by the caller in coolify-api
    // (`routes::image_scan::update_resource_image`), which has the request and `audit::mutation`.
}

/// The context of the audit event, in upstream's key order.
fn audit_context(
    target: &ScanOwner,
    by_user_id: i64,
    actor_name: Option<&str>,
    actor_email: Option<&str>,
    updated: &[String],
    changes: &[Value],
) -> Value {
    let first = changes.first();
    let of_first = |key: &str| first.and_then(|change| change.get(key)).cloned();
    let mut context = serde_json::Map::new();
    context.insert("team_id".to_owned(), json!(target.team_id));
    context.insert("resource_type".to_owned(), json!(target.morph));
    context.insert("resource_uuid".to_owned(), json!(target.uuid));
    context.insert("resource_name".to_owned(), json!(target.name()));
    context.insert("updated_uuids".to_owned(), json!(updated));
    context.insert("old_ref".to_owned(), json!(of_first("old_ref")));
    context.insert("new_ref".to_owned(), json!(of_first("new_ref")));
    context.insert("kind".to_owned(), json!(of_first("kind")));
    if changes.len() > 1 {
        context.insert("changes".to_owned(), Value::Array(changes.to_vec()));
    }
    context.insert("user_id".to_owned(), json!(by_user_id));
    context.insert("actor_id".to_owned(), json!(by_user_id));
    context.insert("actor_name".to_owned(), json!(actor_name));
    context.insert("actor_email".to_owned(), json!(actor_email));
    Value::Object(context)
}

// ---------------------------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------------------------

/// `ImageScan::updateCandidates()`: the better candidates Coolify can switch to: newer tags of
/// the same line first, then a newer build of the same tag. A major version is never one of
/// them.
pub fn update_candidates(scan: &ImageScanRow) -> Vec<Value> {
    let Ok(current) = ImageRef::parse(&scan.image_ref) else {
        return Vec::new();
    };
    if current.digest.is_some() {
        return Vec::new();
    }

    let current_tag = current.tag.as_deref().unwrap_or("latest");
    let mut line = Vec::new();
    let mut same_tag = Vec::new();
    for entry in scan.candidate_entries() {
        if entry.get("better") != Some(&Value::Bool(true)) {
            continue;
        }
        let Some(Ok(reference)) = entry
            .get("ref")
            .and_then(Value::as_str)
            .map(ImageRef::parse)
        else {
            continue;
        };
        let Some(tag) = reference.tag.as_deref() else {
            continue;
        };
        if reference.qualified_name() != current.qualified_name() || reference.digest.is_some() {
            continue;
        }

        let kind = entry.get("kind").and_then(Value::as_str);
        let is_line = kind == Some("line") && tag != current_tag;
        let is_same_tag = kind == Some("same_tag") && tag == current_tag;
        if is_line {
            line.push(entry);
        } else if is_same_tag {
            same_tag.push(entry);
        }
    }

    line.extend(same_tag);
    line
}

/// `UpdateResourceImage::candidate($owners, $newRef)`: the owner, its current scan, and the
/// candidate entry that matches the ref.
async fn candidate(
    jobs: &Jobs,
    owners: &[ScanOwner],
    new_ref: &str,
) -> Result<(ScanOwner, ImageScanRow, Value), UpdateError> {
    for owner in owners {
        for scan in resource_scans::current_for(&jobs.0.db, owner)
            .await
            .map_err(UpdateError::Other)?
        {
            for entry in update_candidates(&scan) {
                if entry.get("ref").and_then(Value::as_str) != Some(new_ref) {
                    continue;
                }
                if let Some(blocked) = blocked_reason(jobs, &scan, owner)
                    .await
                    .map_err(UpdateError::Other)?
                {
                    return Err(refused(blocked));
                }

                return Ok((owner.clone(), scan, entry));
            }
        }
    }

    Err(refused(
        "This version is not one that fixes vulnerabilities of the current image.",
    ))
}

/// `UpdateResourceImage::blockedReason($scan, $owner)`: why Coolify cannot switch the image of
/// a scan, even with a better candidate. `None` when it can.
pub async fn blocked_reason(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<Option<String>, String> {
    if owner.build_pack() == Some("dockercompose") {
        if let Some(refusal) = compose_refusal(owner) {
            return Ok(Some(refusal.to_owned()));
        }

        let services = match compose_services(owner) {
            Ok(services) => services,
            Err(refused) => return Ok(Some(refused.0)),
        };
        let values = compose_variable_values(jobs, owner, &services).await?;
        let overrides = active_compose_image_overrides(owner);
        if compose_matches(&services, &overrides, &values, &scan.image_ref).is_empty() {
            return Ok(Some(NO_COMPOSE_SERVICE.to_owned()));
        }
    }

    if let Some(reason) = unsupported_variable(owner) {
        return Ok(Some(reason.to_owned()));
    }

    if resource_scans::runs_other_image(jobs, scan, owner).await? {
        return Ok(Some(
            "The running image is not the scanned image. Scan it again to check for newer versions."
                .to_owned(),
        ));
    }

    Ok(None)
}

/// The second rule of `blockedReason()`: the image of the owner names a variable in a form
/// that Coolify cannot change.
fn unsupported_variable(owner: &ScanOwner) -> Option<&'static str> {
    let image = owner.string("image");
    (ComposeImageVariable::has_variable(&image) && ComposeImageVariable::parse(&image).is_none())
        .then_some(UNSUPPORTED_VARIABLE)
}

/// `UpdateResourceImage::change($owner, $scan, $candidate, $newRef)`.
fn change(
    owner: &ScanOwner,
    scan: &ImageScanRow,
    candidate: &Value,
    new_ref: &str,
) -> Result<ImageChange, UpdateError> {
    let tag = ImageRef::parse(new_ref)
        .map_err(|e| UpdateError::Other(e.to_string()))?
        .tag
        .unwrap_or_default();
    Ok(ImageChange {
        owner: owner.clone(),
        old: scan.image_ref.clone(),
        tag,
        same_tag: candidate.get("kind").and_then(Value::as_str) == Some("same_tag"),
        digest: candidate
            .get("digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// `UpdateResourceImage::pin($starter, $change)`: waits the scanned build of a newer build of
/// the same tag for the next start of `starter`.
fn pin(starter: &ScanOwner, change: &ImageChange) -> Result<(), UpdateError> {
    let Some(digest) = change.digest.as_deref() else {
        return Err(refused(
            "Coolify does not know the digest of the newer build. Check for newer versions again.",
        ));
    };

    image_start_pin::put(starter, &change.old, digest)
        .map_err(|e| UpdateError::Other(e.to_string()))
}

// ---------------------------------------------------------------------------------------------
// Docker image application
// ---------------------------------------------------------------------------------------------

/// `UpdateResourceImage::updateApplication($application, $change)`.
async fn update_application(
    jobs: &Jobs,
    application: &ScanOwner,
    change: &ImageChange,
) -> Result<Vec<String>, UpdateError> {
    let db = &jobs.0.db;
    let old_tag = application
        .str("docker_registry_image_tag")
        .map(str::to_owned);
    if change.same_tag {
        pin(application, change)?;
    } else {
        set_application_tag(jobs, application.id, Some(change.tag.clone())).await?;
    }

    if let Err(refusal) = deploy(jobs, application).await {
        if !change.same_tag
            && let Err(e) = apps::update_if_changed(
                db,
                application.id,
                &[("docker_registry_image_tag", ColumnValue::Text(old_tag))],
                Jobs::now(),
            )
            .await
        {
            tracing::error!(application = %application.uuid, error = %e, "the old image tag could not be put back");
        }
        image_start_pin::forget(application);
        return Err(refusal);
    }

    Ok(vec![application.uuid.clone()])
}

/// `$application->docker_registry_image_tag = $tag; $application->save()`.
async fn set_application_tag(
    jobs: &Jobs,
    application_id: i64,
    tag: Option<String>,
) -> Result<(), UpdateError> {
    apps::update_if_changed(
        &jobs.0.db,
        application_id,
        &[("docker_registry_image_tag", ColumnValue::Text(tag))],
        Jobs::now(),
    )
    .await
    .map(|_| ())
    .map_err(db_error)
}

/// `UpdateResourceImage::deploy($application, $restore)`: queues a deploy of the app. The
/// `Err` is the refusal when it cannot start; the caller then puts the old configuration back
/// and forgets the pin.
async fn deploy(jobs: &Jobs, application: &ScanOwner) -> Result<(), UpdateError> {
    let queued = match apps::find_by_id(&jobs.0.db, application.id).await {
        Ok(Some(row)) => {
            jobs.queue_application_deployment(
                &row,
                &coolify_domain::new_public_id(),
                QueueOptions::default(),
            )
            .await
        }
        Ok(None) => Err("Application not found.".to_owned()),
        Err(e) => Err(e.to_string()),
    };
    deploy_result(queued).inspect_err(|_| {
        tracing::warn!(application = %application.uuid, "the deploy of an image update did not start");
    })
}

/// The decision of `deploy()` on the answer of `queue_application_deployment()`.
fn deploy_result(queued: Result<Queued, String>) -> Result<(), UpdateError> {
    match queued {
        Ok(Queued::Queued { .. }) => Ok(()),
        Ok(Queued::Skipped { .. }) => Err(refused(
            "A deploy of this app is already queued. Try again when it is done.",
        )),
        Ok(other) => Err(refused(other.message())),
        Err(e) => {
            // `report($e)`.
            tracing::error!(error = %e, "the deploy of an image update could not start");
            Err(refused(
                "The deploy could not start. Coolify kept the old image.",
            ))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Compose application
// ---------------------------------------------------------------------------------------------

/// One entry of `docker_compose_image_overrides`: the image the compose file names, and the
/// image a security update set instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOverride {
    pub from: String,
    pub to: String,
}

/// Image overrides by Compose service name, in the stored order.
pub type ImageOverrides = Vec<(String, ImageOverride)>;

/// `$application->docker_compose_image_overrides` (cast `array`); `None` for null.
fn stored_overrides(application: &ScanOwner) -> Option<PhpValue> {
    match application.record.get("docker_compose_image_overrides") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => php::json_decode(text),
        Some(value) => php::json_decode(&value.to_string()),
    }
}

/// `Application::composeImageOverride($serviceName, $composeImage)`: the image that a security
/// update set for a Compose service, while the compose file still names the image it replaced.
/// When the user changes that image in the compose file (in Git), the compose file wins again.
pub fn compose_image_override(
    application: &ScanOwner,
    service_name: &str,
    compose_image: Option<&str>,
) -> Option<String> {
    let stored = stored_overrides(application)?;
    override_for(&stored, service_name, compose_image).map(|found| found.to)
}

fn override_for(
    stored: &PhpValue,
    service_name: &str,
    compose_image: Option<&str>,
) -> Option<ImageOverride> {
    // `data_get()` reads a dot in the name as a path, as upstream does.
    let found = php::data_get(stored, service_name)?;
    let (Some(PhpValue::Str(from)), Some(PhpValue::Str(to))) =
        (php::get(found, "from"), php::get(found, "to"))
    else {
        return None;
    };
    (php_trim(compose_image?) == php_trim(from)).then(|| ImageOverride {
        from: from.clone(),
        to: to.clone(),
    })
}

/// `Application::activeComposeImageOverrides()`: the overrides that still apply to the compose
/// file, keyed by the service name.
pub fn active_compose_image_overrides(application: &ScanOwner) -> ImageOverrides {
    let Some(PhpValue::Array(stored_entries)) = stored_overrides(application) else {
        return Vec::new();
    };
    // `Yaml::parse()`: a compose file that cannot be read has no active override.
    let Ok(compose) = coolify_compat::yaml_parse::parse(
        application.str("docker_compose_raw").unwrap_or_default(),
        128,
    ) else {
        return Vec::new();
    };
    let services = php::get(&compose, "services");
    let stored = PhpValue::Array(stored_entries.clone());

    let mut active = Vec::new();
    for (name, _) in &stored_entries {
        let name = name.as_string();
        let image = services
            .and_then(|services| php::get(services, &name))
            .and_then(|service| php::get(service, "image"));
        let image = match image {
            Some(PhpValue::Str(image)) => Some(image.as_str()),
            _ => None,
        };
        if let Some(found) = override_for(&stored, &name, image) {
            active.push((name, found));
        }
    }
    active
}

/// The overrides as Laravel's `array` cast stores them (`json_encode()`); `None` (null) without
/// overrides.
fn overrides_json(overrides: &[(String, ImageOverride)]) -> Option<String> {
    if overrides.is_empty() {
        return None;
    }
    let entries = overrides
        .iter()
        .map(|(name, found)| {
            (
                Key::from_php_string(name),
                PhpValue::map([
                    ("from", PhpValue::Str(found.from.clone())),
                    ("to", PhpValue::Str(found.to.clone())),
                ]),
            )
        })
        .collect();
    Some(php::json_encode(&PhpValue::Array(entries)))
}

/// `UpdateResourceImage::composeRefusal($application)`: why Coolify cannot change the images of
/// a Compose app at all. `None` when it can.
fn compose_refusal(application: &ScanOwner) -> Option<&'static str> {
    let raw_deployment = application
        .settings
        .as_ref()
        .and_then(|settings| settings.get("is_raw_compose_deployment_enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    if raw_deployment {
        return Some(
            "This app deploys its compose file as written. Change the version in the compose file, then redeploy.",
        );
    }
    let parsing_version = application
        .string("compose_parsing_version")
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    if parsing_version < 3 {
        return Some("Change the version in the compose file of this app, then redeploy.");
    }
    // The command can build, pull, or start other images than the compose file names.
    let filled = |column: &str| {
        application
            .str(column)
            .is_some_and(|value| !value.trim().is_empty())
    };
    if filled("docker_compose_custom_build_command") {
        return Some(
            "This app uses a custom build command, so Coolify cannot choose the image. Change the image in your compose file.",
        );
    }
    if filled("docker_compose_custom_start_command") {
        return Some(
            "This app uses a custom start command, so Coolify cannot choose the image. Change the image in your compose file.",
        );
    }

    None
}

/// `UpdateResourceImage::composeServices($application)`: the images of the services of the
/// compose file from Git, as written, by service name.
fn compose_services(application: &ScanOwner) -> Result<Vec<(String, String)>, ImageUpdateRefused> {
    compose_service_images(application.str("docker_compose_raw").unwrap_or_default())
}

fn compose_service_images(raw_compose: &str) -> Result<Vec<(String, String)>, ImageUpdateRefused> {
    let compose = validate::parse_docker_compose_yaml(raw_compose).ok();
    let Some(PhpValue::Array(services)) = compose
        .as_ref()
        .and_then(|compose| php::get(compose, "services"))
    else {
        return Err(refuse("The compose file of this app is not valid."));
    };

    Ok(services
        .iter()
        .filter_map(
            |(name, service)| match resource_scans::service_value(service, "image") {
                Some(PhpValue::Str(image)) if !php_trim(image).is_empty() => {
                    Some((name.as_string(), php_trim(image).to_owned()))
                }
                _ => None,
            },
        )
        .collect())
}

/// One service of the compose file that runs the image: the variable that sets its image (if
/// one does), and the image it runs now.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposeMatch {
    variable: Option<ComposeImageVariable>,
    current: String,
}

/// `UpdateResourceImage::composeMatches()`: the services of the compose file that run the image
/// in a way Coolify can change: an image line (or the override that replaced it), or a variable
/// that sets the image. By service name, with the image it runs now. `values` are the Coolify
/// environment variables of the application that are set, by key.
fn compose_matches(
    services: &[(String, String)],
    overrides: &[(String, ImageOverride)],
    values: &HashMap<String, String>,
    image_ref: &str,
) -> Vec<(String, ComposeMatch)> {
    let mut matches = Vec::new();
    for (name, image) in services {
        let variable = ComposeImageVariable::parse(image);
        let current = match &variable {
            Some(variable) => variable.resolve(values.get(&variable.key).map(String::as_str)),
            None if ComposeImageVariable::has_variable(image) => None,
            None => Some(
                overrides
                    .iter()
                    .find(|(service, _)| service == name)
                    .map_or_else(|| image.clone(), |(_, found)| found.to.clone()),
            ),
        };

        if let Some(current) = current
            && same_image(&current, image_ref)
        {
            matches.push((name.clone(), ComposeMatch { variable, current }));
        }
    }

    matches
}

/// The values of the Coolify environment variables that set an image of the compose file, for
/// [`compose_matches`].
async fn compose_variable_values(
    jobs: &Jobs,
    application: &ScanOwner,
    services: &[(String, String)],
) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    for (_, image) in services {
        if let Some(variable) = ComposeImageVariable::parse(image)
            && !values.contains_key(&variable.key)
            && let Some(value) =
                variable_value(jobs, application.morph, application.id, &variable.key).await?
        {
            values.insert(variable.key, value);
        }
    }
    Ok(values)
}

/// What an update of a Compose app changes.
#[derive(Debug, Default, PartialEq, Eq)]
struct ComposePlan {
    /// The new `docker_compose_image_overrides`.
    overrides: ImageOverrides,
    /// The environment variables to set, by key.
    variables: Vec<(String, String)>,
    /// The variable per compose service whose image it sets.
    variable_services: Vec<(String, String)>,
    pinned: bool,
}

/// The loop of `UpdateResourceImage::updateComposeApplication()`: an image override per Compose
/// service, or the environment variable that sets the image. `active` are the overrides that
/// still apply.
fn plan_compose_application(
    services: &[(String, String)],
    active: ImageOverrides,
    values: &HashMap<String, String>,
    changes: &[ImageChange],
    pin: PinFn<'_>,
) -> Result<ComposePlan, UpdateError> {
    let mut plan = ComposePlan {
        overrides: active,
        ..ComposePlan::default()
    };

    for change in changes {
        if change.same_tag {
            pin(change)?;
            plan.pinned = true;

            continue;
        }

        let matches = compose_matches(services, &plan.overrides, values, &change.old);
        if matches.is_empty() {
            return Err(refused(NO_COMPOSE_SERVICE));
        }
        for (name, found) in matches {
            match found.variable {
                Some(variable) => {
                    let value = variable.value_for(&found.current, &change.tag);
                    add_variable(&mut plan.variables, &variable.key, value)?;
                    set_entry(&mut plan.variable_services, &name, variable.key);
                }
                None => {
                    let from = services
                        .iter()
                        .find(|(service, _)| *service == name)
                        .map(|(_, image)| image.clone())
                        .unwrap_or_default();
                    let to = with_tag(&found.current, &change.tag);
                    set_entry(&mut plan.overrides, &name, ImageOverride { from, to });
                }
            }
        }
    }

    // A variable that also sets the image of a service that this update does not change would change that image too.
    for (name, image) in services {
        if let Some(variable) = ComposeImageVariable::parse(image)
            && plan.variables.iter().any(|(key, _)| *key == variable.key)
            && !plan
                .variable_services
                .iter()
                .any(|(service, _)| service == name)
        {
            return Err(refused(format!(
                "The variable {} also sets the image of {name}. Change the versions in the compose file.",
                variable.key
            )));
        }
    }

    Ok(plan)
}

/// `UpdateResourceImage::updateComposeApplication($application, $changes)`: a Compose app from
/// Git: an image override per Compose service, or the environment variable that sets the
/// image, then a deploy. Refused while the app deploys its compose file as written.
async fn update_compose_application(
    jobs: &Jobs,
    application: &ScanOwner,
    changes: &[ImageChange],
) -> Result<Vec<String>, UpdateError> {
    let db = &jobs.0.db;
    if let Some(refusal) = compose_refusal(application) {
        return Err(refused(refusal));
    }

    let services = compose_services(application)?;
    let old_overrides = stored_overrides(application);
    let values = compose_variable_values(jobs, application, &services)
        .await
        .map_err(UpdateError::Other)?;
    let plan = plan_compose_application(
        &services,
        active_compose_image_overrides(application),
        &values,
        changes,
        &mut |change| pin(application, change),
    )?;
    refuse_other_variable_uses(
        application.str("docker_compose_raw").unwrap_or_default(),
        &plan.variable_services,
    )?;

    // Eloquent's dirty check of the `array` cast compares the decoded values.
    let new_overrides = overrides_json(&plan.overrides);
    let overrides_changed = new_overrides.as_deref().and_then(php::json_decode) != old_overrides;
    if overrides_changed {
        store::set_compose_image_overrides(
            db,
            application.id,
            new_overrides.as_deref(),
            Jobs::now(),
        )
        .await
        .map_err(db_error)?;
    }
    let restore = set_variables(jobs, application.morph, application.id, &plan.variables).await?;

    if let Err(refusal) = deploy(jobs, application).await {
        if overrides_changed
            && let Err(e) = store::set_compose_image_overrides(
                db,
                application.id,
                old_overrides.as_ref().map(php::json_encode).as_deref(),
                Jobs::now(),
            )
            .await
        {
            tracing::error!(application = %application.uuid, error = %e, "the old image overrides could not be put back");
        }
        restore_variables(jobs, &restore).await;
        image_start_pin::forget(application);
        return Err(refusal);
    }

    Ok(vec![application.uuid.clone()])
}

// ---------------------------------------------------------------------------------------------
// Standalone database
// ---------------------------------------------------------------------------------------------

/// The kind of a standalone database (`STANDALONE_DATABASE_MODELS`); `None` for other resources.
fn database_kind(resource: &ScanOwner) -> Option<Kind> {
    Kind::ALL
        .into_iter()
        .find(|kind| kind.morph() == resource.morph)
}

/// `UpdateResourceImage::updateDatabase($database, $change)`.
async fn update_database(
    jobs: &Jobs,
    database: &ScanOwner,
    change: &ImageChange,
    by_user_id: i64,
) -> Result<Vec<String>, UpdateError> {
    let db = &jobs.0.db;
    let Some(kind) = database_kind(database) else {
        return Err(refused("Coolify cannot update the image of this resource."));
    };
    let old_image = database.string("image");
    if change.same_tag {
        pin(database, change)?;
    } else {
        dbs::update_if_changed(
            db,
            kind,
            database.id,
            &[(
                "image",
                ColumnValue::Text(Some(with_tag(&old_image, &change.tag))),
            )],
            Jobs::now(),
        )
        .await
        .map_err(db_error)?;
    }

    let refusal = match restart_database(jobs, database, kind, by_user_id).await {
        Ok(None) => return Ok(vec![database.uuid.clone()]),
        // `RestartDatabase` answered with why it did not start.
        Ok(Some(message)) => refused(message),
        Err(e) => {
            // `report($e)`.
            tracing::error!(database = %database.uuid, error = %e, "the restart of an image update failed");
            refused("The restart failed. Coolify kept the old image.")
        }
    };

    image_start_pin::forget(database);
    if !change.same_tag
        && let Err(e) = dbs::update_if_changed(
            db,
            kind,
            database.id,
            &[("image", ColumnValue::Text(Some(old_image)))],
            Jobs::now(),
        )
        .await
    {
        tracing::error!(database = %database.uuid, error = %e, "the old image could not be put back");
    }
    Err(refusal)
}

/// `RestartDatabase::run($database)`: `Ok(None)` when the restart started, `Ok(Some(message))`
/// when `RestartDatabase` answers with a message instead, `Err` when it throws. The stop and
/// the start run in the background (`Jobs::spawn_database_restart`).
async fn restart_database(
    jobs: &Jobs,
    database: &ScanOwner,
    kind: Kind,
    by_user_id: i64,
) -> Result<Option<String>, String> {
    let row = dbs::find_by_id(&jobs.0.db, kind, database.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Database not found.")?;
    if !jobs.server_for(&row).await?.is_functional() {
        return Ok(Some("Server is not functional".to_owned()));
    }
    if let Some(message) = jobs.start_prerequisite_error(&row).await {
        return Ok(Some(message));
    }
    Ok(jobs
        .spawn_database_restart(row, Some(by_user_id))
        .err()
        .map(str::to_owned))
}

// ---------------------------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------------------------

/// One application or database of a service whose image line changes.
#[derive(Debug, Clone)]
struct ServiceTarget {
    resource: ScanOwner,
    tag: String,
}

/// What an update of a service changes.
#[derive(Debug, Default)]
struct ServicePlan {
    /// By `morph:id` of the application or database, in the order they were found.
    targets: Vec<(String, ServiceTarget)>,
    /// The environment variables to set, by key.
    variables: Vec<(String, String)>,
    /// The uuids of the applications and databases whose image a variable sets.
    variable_owners: Vec<String>,
    /// The variable per compose service whose image it sets.
    variable_services: Vec<(String, String)>,
    pinned: bool,
}

/// The loop of `UpdateResourceImage::updateService()`. `owners` are the applications and
/// databases of the service, `configured` the image each changed owner runs now
/// (`ResourceScans::configuredImage()`) by `key()`, for owners whose image a variable sets.
fn plan_service(
    owners: &[ScanOwner],
    changes: &[ImageChange],
    configured: &HashMap<String, String>,
    pin: PinFn<'_>,
) -> Result<ServicePlan, UpdateError> {
    let mut plan = ServicePlan::default();
    for change in changes {
        if change.same_tag {
            pin(change)?;
            plan.pinned = true;

            continue;
        }

        let raw = change.owner.image();
        if ComposeImageVariable::has_variable(&raw) {
            let current = configured
                .get(&change.owner.key())
                .cloned()
                .unwrap_or_else(|| raw.clone());
            let Some(variable) =
                ComposeImageVariable::parse(&raw).filter(|_| same_image(&current, &change.old))
            else {
                return Err(refused(UNSUPPORTED_VARIABLE));
            };
            refuse_shared_variable(owners, &variable.key, changes)?;
            let value = variable.value_for(&current, &change.tag);
            add_variable(&mut plan.variables, &variable.key, value)?;
            plan.variable_owners.push(change.owner.uuid.clone());
            set_entry(
                &mut plan.variable_services,
                &change.owner.name(),
                variable.key,
            );

            continue;
        }

        let sub_resources = sub_resources_of(owners, &change.old);
        if sub_resources.is_empty() {
            return Err(refused(
                "No application or database of this service runs this image.",
            ));
        }
        for sub_resource in sub_resources {
            let key = sub_resource.key();
            let target = ServiceTarget {
                resource: sub_resource,
                tag: change.tag.clone(),
            };
            set_entry(&mut plan.targets, &key, target);
        }
    }
    Ok(plan)
}

/// `UpdateResourceImage::updateService($service, $changes)`: changes the images of the service
/// to their new tags and restarts it once: the compose file and the image of each application
/// and database, or the environment variable that sets the image. A newer build of the same
/// tag only waits its scanned digest for the restart.
async fn update_service(
    jobs: &Jobs,
    service: &ScanOwner,
    changes: &[ImageChange],
) -> Result<Vec<String>, UpdateError> {
    let db = &jobs.0.db;
    if svc::has_running_start(db, &service.uuid)
        .await
        .map_err(db_error)?
    {
        return Err(refused(
            "A start of this service is running. Try again when it is done.",
        ));
    }

    let owners = image_scans::owners(db, service).await.map_err(db_error)?;
    let mut configured = HashMap::new();
    for change in changes.iter().filter(|change| !change.same_tag) {
        if ComposeImageVariable::has_variable(&change.owner.image()) {
            let current = resource_scans::configured_image(jobs, &change.owner)
                .await
                .map_err(UpdateError::Other)?;
            configured.insert(change.owner.key(), current);
        }
    }
    let plan = plan_service(&owners, changes, &configured, &mut |change| {
        pin(service, change)
    })?;

    let old_compose = service.str("docker_compose_raw").map(str::to_owned);
    refuse_other_variable_uses(
        old_compose.as_deref().unwrap_or_default(),
        &plan.variable_services,
    )?;

    let old_images: Vec<String> = plan
        .targets
        .iter()
        .map(|(_, target)| target.resource.string("image"))
        .collect();

    if !plan.targets.is_empty() {
        let compose =
            compose_with_images(old_compose.as_deref().unwrap_or_default(), &plan.targets)?;
        let new_images: Vec<String> = plan
            .targets
            .iter()
            .map(|(_, target)| with_tag(&target.resource.string("image"), &target.tag))
            .collect();
        store::write_service_images(
            db,
            service.id,
            Some(&yaml::dump_with_indent(&compose, 10, 2)),
            &sub_images(&plan.targets, &new_images),
            Jobs::now(),
        )
        .await
        .map_err(db_error)?;
    }
    let restore = set_variables(jobs, SERVICE_MORPH, service.id, &plan.variables).await?;

    if let Err(e) = restart_service(jobs, service.id).await {
        if !plan.targets.is_empty()
            && let Err(e) = store::write_service_images(
                db,
                service.id,
                old_compose.as_deref(),
                &sub_images(&plan.targets, &old_images),
                Jobs::now(),
            )
            .await
        {
            tracing::error!(service = %service.uuid, error = %e, "the old images could not be put back");
        }
        restore_variables(jobs, &restore).await;
        if plan.pinned {
            image_start_pin::forget(service);
        }
        // `report($e)`.
        tracing::error!(service = %service.uuid, error = %e, "the restart of an image update failed");

        return Err(refused("The restart failed. Coolify kept the old image."));
    }

    let mut updated: Vec<String> = Vec::new();
    let target_uuids = plan
        .targets
        .iter()
        .map(|(_, target)| target.resource.uuid.clone());
    for uuid in target_uuids.chain(plan.variable_owners) {
        if !updated.contains(&uuid) {
            updated.push(uuid);
        }
    }
    Ok(updated)
}

fn sub_images<'a>(
    targets: &'a [(String, ServiceTarget)],
    images: &'a [String],
) -> Vec<store::SubImage<'a>> {
    targets
        .iter()
        .zip(images)
        .map(|((_, target), image)| store::SubImage {
            morph: target.resource.morph,
            id: target.resource.id,
            image,
        })
        .collect()
}

/// `RestartService::run($service, true)`: `StartService` with a pull of the latest images and a
/// stop before the start. The parse runs here, so a compose file that cannot be parsed is an
/// `Err`; the start itself runs in the background.
async fn restart_service(jobs: &Jobs, service_id: i64) -> Result<(), String> {
    jobs.parse_service(service_id).await?;
    jobs.spawn_service_start(service_id, true, true);
    Ok(())
}

/// `UpdateResourceImage::refuseSharedVariable($service, $key, $changes)`: a variable that also
/// sets the image of another application or database that this update does not change would
/// change that image too. `owners` are the applications and databases of the service.
fn refuse_shared_variable(
    owners: &[ScanOwner],
    key: &str,
    changes: &[ImageChange],
) -> Result<(), ImageUpdateRefused> {
    let changing: Vec<String> = changes.iter().map(|change| change.owner.key()).collect();
    for owner in owners {
        if let Some(image) = ComposeImageVariable::parse(&owner.string("image"))
            && image.key == key
            && !changing.contains(&owner.key())
        {
            return Err(refuse(format!(
                "The variable {key} also sets the image of {}. Change the versions in the compose file.",
                owner.name()
            )));
        }
    }
    Ok(())
}

/// `UpdateResourceImage::subResourcesUsing($service, $imageRef)`: the applications and
/// databases of a service that run the image.
pub async fn sub_resources_using(
    jobs: &Jobs,
    service: &ScanOwner,
    image_ref: &str,
) -> Result<Vec<ScanOwner>, String> {
    let owners = image_scans::owners(&jobs.0.db, service)
        .await
        .map_err(|e| e.to_string())?;
    Ok(sub_resources_of(&owners, image_ref))
}

/// The filter of `subResourcesUsing()` on the applications and databases of a service.
fn sub_resources_of(owners: &[ScanOwner], image_ref: &str) -> Vec<ScanOwner> {
    if ImageRef::parse(image_ref).is_err() {
        return Vec::new();
    }
    owners
        .iter()
        .filter(|sub_resource| same_image(&sub_resource.string("image"), image_ref))
        .cloned()
        .collect()
}

/// `UpdateResourceImage::composeWithImages($service, $targets)`: the compose file with the new
/// image for each sub-resource. Refuses when the compose file names another image for one of
/// them, for example a variable, because then the image does not come from the field Coolify
/// changes.
fn compose_with_images(
    raw_compose: &str,
    targets: &[(String, ServiceTarget)],
) -> Result<PhpValue, ImageUpdateRefused> {
    let mut compose = match validate::parse_docker_compose_yaml(raw_compose) {
        Ok(compose @ PhpValue::Array(_))
            if matches!(php::get(&compose, "services"), Some(PhpValue::Array(_))) =>
        {
            compose
        }
        _ => return Err(refuse("The compose file of this service is not valid.")),
    };

    for (_, target) in targets {
        let name = target.resource.name();
        let image = target.resource.string("image");
        let configured = entry_mut(&mut compose, "services")
            .and_then(|services| entry_mut(services, &name))
            .and_then(|service| entry_mut(service, "image"));
        match configured {
            Some(PhpValue::Str(configured))
                if php_trim(configured.as_str()) == php_trim(&image) =>
            {
                *configured = with_tag(&image, &target.tag);
            }
            _ => {
                return Err(refuse(format!(
                    "The compose file sets the image of {name} in another way. Change it in the compose file."
                )));
            }
        }
    }

    Ok(compose)
}

// ---------------------------------------------------------------------------------------------
// Shared rules
// ---------------------------------------------------------------------------------------------

/// `$array[$key]` for writing: the value of a key of a parsed YAML mapping.
fn entry_mut<'a>(value: &'a mut PhpValue, key: &str) -> Option<&'a mut PhpValue> {
    match value {
        PhpValue::Array(items) => items
            .iter_mut()
            .find(|(existing, _)| existing.as_string() == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

/// `$entries[$key] = $value`: replaces in place or appends.
fn set_entry<T>(entries: &mut Vec<(String, T)>, key: &str, value: T) {
    match entries.iter_mut().find(|(existing, _)| existing == key) {
        Some(slot) => slot.1 = value,
        None => entries.push((key.to_owned(), value)),
    }
}

/// `UpdateResourceImage::refuseOtherVariableUses($rawCompose, $variableServices)`: a variable
/// that the update changes must set only the image lines it changes. Anywhere else in the
/// compose file (environment, labels, a command, another image) the new value would change
/// more than the image. `variable_services` is the variable per compose service whose image it
/// sets.
fn refuse_other_variable_uses(
    raw_compose: &str,
    variable_services: &[(String, String)],
) -> Result<(), ImageUpdateRefused> {
    if variable_services.is_empty() {
        return Ok(());
    }

    let Ok(mut compose @ PhpValue::Array(_)) = validate::parse_docker_compose_yaml(raw_compose)
    else {
        return Err(refuse("The compose file is not valid."));
    };

    for (name, _) in variable_services {
        if let Some(PhpValue::Array(service)) =
            entry_mut(&mut compose, "services").and_then(|services| entry_mut(services, name))
        {
            service.retain(|(key, _)| key.as_string() != "image");
        }
    }

    // Upstream encodes without escaped slashes and Unicode; neither can be part of a match.
    let rest = php::json_encode(&compose);
    let mut checked: Vec<&str> = Vec::new();
    for (_, key) in variable_services {
        if checked.contains(&key.as_str()) {
            continue;
        }
        checked.push(key);
        if uses_variable(&rest, key) {
            return Err(refuse(format!(
                "The compose file uses the variable {key} in another place too. Change the version in the compose file."
            )));
        }
    }
    Ok(())
}

/// `preg_match('/\$\{?'.preg_quote($key, '/').'(?![A-Za-z0-9_])/', $text) === 1`: the text
/// names the variable as `$KEY` or `${KEY…`.
fn uses_variable(text: &str, key: &str) -> bool {
    let ends_name = |after: &str| {
        !after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    text.match_indices('$').any(|(at, _)| {
        let rest = &text[at + 1..];
        [rest.strip_prefix('{'), Some(rest)]
            .into_iter()
            .flatten()
            .filter_map(|candidate| candidate.strip_prefix(key))
            .any(ends_name)
    })
}

/// `UpdateResourceImage::sameImage($a, $b)`: the same repository and tag, neither pinned by a
/// digest.
fn same_image(a: &str, b: &str) -> bool {
    let (Ok(first), Ok(second)) = (ImageRef::parse(php_trim(a)), ImageRef::parse(php_trim(b)))
    else {
        return false;
    };

    first.digest.is_none()
        && second.digest.is_none()
        && first.qualified_name() == second.qualified_name()
        && first.tag.as_deref().unwrap_or("latest") == second.tag.as_deref().unwrap_or("latest")
}

/// `UpdateResourceImage::withTag($image, $tag)`: the configured image with another tag, keeping
/// how the user wrote the name (`postgres`, `docker.io/library/postgres`).
fn with_tag(image: &str, tag: &str) -> String {
    let image = php_trim(image);
    let name = match image.rfind(':') {
        Some(colon) if image.rfind('/').is_none_or(|slash| colon > slash) => &image[..colon],
        _ => image,
    };

    format!("{name}:{tag}")
}

/// `UpdateResourceImage::addVariable($variables, $key, $value)`.
fn add_variable(
    variables: &mut Vec<(String, String)>,
    key: &str,
    value: String,
) -> Result<(), ImageUpdateRefused> {
    if variables
        .iter()
        .any(|(existing, set)| existing == key && *set != value)
    {
        return Err(refuse(format!(
            "The variable {key} sets more than one of these images. Change the versions in the compose file."
        )));
    }
    set_entry(variables, key, value);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Environment variables
// ---------------------------------------------------------------------------------------------

/// `EnvironmentVariable::withoutEdgeWhitespace($value)`: single-line values lose their edge
/// whitespace; multiline content is kept as it is.
fn without_edge_whitespace(value: &str) -> String {
    let trimmed = php_trim(value);
    if trimmed.contains('\n') {
        value.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `$variable->value` of a stored variable: `None` for null (and for an empty stored value).
fn plain_value(jobs: &Jobs, row: &coolify_db::records::Record) -> Result<Option<String>, String> {
    let Some(cipher) = row
        .get("value")
        .and_then(Value::as_str)
        .filter(|cipher| !cipher.is_empty())
    else {
        return Ok(None);
    };
    let encrypter = jobs
        .encrypter()
        .ok_or("No application encryption key has been specified.")?;
    let plain = encrypter
        .decrypt_maybe_serialized(cipher)
        .map_err(|_| "The payload is invalid.".to_owned())?;
    Ok(Some(without_edge_whitespace(&plain)))
}

/// `UpdateResourceImage::variableValue($resource, $key)`: the value of a Coolify environment
/// variable of the resource; `None` when it is not set.
async fn variable_value(
    jobs: &Jobs,
    morph: &str,
    id: i64,
    key: &str,
) -> Result<Option<String>, String> {
    let row = env_store::first_by_key(&jobs.0.db, morph, id, key, false)
        .await
        .map_err(|e| e.to_string())?;
    match row {
        Some(row) => Ok(Some(plain_value(jobs, &row)?.unwrap_or_default())),
        None => Ok(None),
    }
}

/// `$resource->environment_variables()->where('key', $key)->where('is_preview', false)->first()`.
async fn first_variable(
    db: &coolify_db::Db,
    morph: &str,
    id: i64,
    key: &str,
) -> Result<Option<coolify_db::records::Record>, UpdateError> {
    env_store::first_by_key(db, morph, id, key, false)
        .await
        .map_err(db_error)
}

/// What puts one environment variable back.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VariableRestore {
    /// The update created the variable.
    Created { id: i64 },
    /// The update changed the value; `value_encrypted` is the stored value before it.
    Changed {
        id: i64,
        value_encrypted: Option<String>,
    },
}

/// `UpdateResourceImage::setVariables($resource, $variables)`: sets the environment variables
/// of the resource, and returns what puts the old values back. A value that refers to a shared
/// variable or a secret is not replaced.
async fn set_variables(
    jobs: &Jobs,
    morph: &str,
    id: i64,
    variables: &[(String, String)],
) -> Result<Vec<VariableRestore>, UpdateError> {
    if variables.is_empty() {
        return Ok(Vec::new());
    }
    let db = &jobs.0.db;
    for (key, _) in variables {
        let Some(variable) = first_variable(db, morph, id, key).await? else {
            continue;
        };
        if variable.get("is_shown_once").and_then(Value::as_bool) == Some(true) {
            return Err(refused(format!(
                "The variable {key} is locked, so Coolify does not change it."
            )));
        }
        if plain_value(jobs, &variable)
            .map_err(UpdateError::Other)?
            .is_some_and(|value| value.contains("{{"))
        {
            return Err(refused(format!(
                "The variable {key} refers to another variable. Change the version there."
            )));
        }
    }

    let encrypter = jobs.encrypter().ok_or_else(|| {
        UpdateError::Other("No application encryption key has been specified.".to_owned())
    })?;
    let mut old = Vec::with_capacity(variables.len());
    for (key, value) in variables {
        let value = without_edge_whitespace(value);
        match first_variable(db, morph, id, key).await? {
            Some(variable) => {
                let variable_id = variable
                    .get("id")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                old.push(VariableRestore::Changed {
                    id: variable_id,
                    value_encrypted: variable
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
                store::set_variable_value(
                    db,
                    variable_id,
                    Some(&encrypter.encrypt_serialized(&value)),
                    Jobs::now(),
                )
                .await
                .map_err(db_error)?;
            }
            None => {
                // `$resource->environment_variables()->create(['key' => …, 'value' => …, 'is_preview' => false])`
                // with the defaults of the model (runtime and build time) and its `created` hook
                // (the preview copy of a variable of an application).
                let created = env_store::create_on_resource(
                    db,
                    (morph, id),
                    &env_store::NewResourceVariable {
                        key: key.clone(),
                        value_encrypted: Some(encrypter.encrypt_serialized(&value)),
                        is_preview: false,
                        is_literal: false,
                        is_multiline: false,
                        is_shown_once: false,
                        is_runtime: true,
                        is_buildtime: true,
                        comment: None,
                        is_shared: false,
                    },
                    Some(&encrypter.encrypt_serialized(&value)),
                    &jobs.version(),
                    Jobs::now(),
                )
                .await
                .map_err(db_error)?;
                old.push(VariableRestore::Created {
                    id: created
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                });
            }
        }
    }

    Ok(old)
}

/// The closure `setVariables()` returns: deletes the variables the update created and puts the
/// old values of the others back.
async fn restore_variables(jobs: &Jobs, old: &[VariableRestore]) {
    let db = &jobs.0.db;
    for entry in old {
        let result = match entry {
            VariableRestore::Created { id } => env_store::force_delete(db, *id).await,
            VariableRestore::Changed {
                id,
                value_encrypted,
            } => store::set_variable_value(db, *id, value_encrypted.as_deref(), Jobs::now()).await,
        };
        if let Err(e) = result {
            tracing::error!(error = %e, "an environment variable of an image update could not be put back");
        }
    }
}

#[cfg(test)]
mod tests {
    use coolify_db::image_scans::{
        APPLICATION_MORPH, SERVICE_APPLICATION_MORPH, SERVICE_DATABASE_MORPH, STATUS_COMPLETED,
    };

    use super::*;
    use crate::image_scan::resource_scans::tests::{owner as scan_owner, scan as scan_row};

    const NEW_BUILD: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// `updateImageEntry()` of `UpdateResourceImageTest`.
    fn entry(reference: &str, kind: &str, better: bool) -> Value {
        json!({
            "ref": reference,
            "digest": NEW_BUILD,
            "kind": kind,
            "better": better,
            "fixed_ids": ["CVE-2025-1094"],
            "fixes": {"critical": 2, "high": 15, "medium": 4},
            "left": {"critical": 3, "high": 0, "medium": 1},
            "eol": null,
        })
    }

    /// `updateImageScan()`.
    fn scan_with(image_ref: &str, entries: Vec<Value>) -> ImageScanRow {
        let mut scan = scan_row(image_ref, STATUS_COMPLETED, None);
        scan.candidates = Some(json!({"status": "ok", "error": null, "entries": entries}));
        scan
    }

    fn refs(entries: &[Value]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|entry| entry.get("ref").and_then(Value::as_str))
            .collect()
    }

    /// `updateImageSubResource()`.
    fn sub_resource(morph: &'static str, id: i64, name: &str, image: &str) -> ScanOwner {
        let mut owner = scan_owner(morph, json!({"name": name, "image": image}));
        owner.id = id;
        owner.uuid = format!("{name}-uuid");
        owner.service_id = Some(3);
        owner
    }

    fn web(image: &str) -> ScanOwner {
        sub_resource(SERVICE_APPLICATION_MORPH, 1, "web", image)
    }

    /// The compose file of `updateImageService()` and `updateImageComposeApplication()`.
    fn compose_of(images: &[(&str, &str)]) -> String {
        let services = images
            .iter()
            .map(|(name, image)| {
                (
                    Key::Str((*name).to_owned()),
                    PhpValue::map([("image", PhpValue::Str((*image).to_owned()))]),
                )
            })
            .collect();
        yaml::dump_with_indent(
            &PhpValue::map([("services", PhpValue::Array(services))]),
            10,
            2,
        )
    }

    /// `updateImageComposeApplication()`.
    fn compose_application(images: &[(&str, &str)], overrides: Value) -> ScanOwner {
        let mut application = scan_owner(
            APPLICATION_MORPH,
            json!({
                "build_pack": "dockercompose",
                "docker_compose_raw": compose_of(images),
                "compose_parsing_version": "5",
                "docker_compose_image_overrides": overrides,
            }),
        );
        application.settings = Some(serde_json::Map::new());
        application
    }

    fn change_of(owner: &ScanOwner, old: &str, new_ref: &str, kind: &str) -> ImageChange {
        change(
            owner,
            &scan_with(old, Vec::new()),
            &entry(new_ref, kind, true),
            new_ref,
        )
        .unwrap()
    }

    fn no_pin(_: &ImageChange) -> Result<(), UpdateError> {
        Ok(())
    }

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    fn refusal<T: std::fmt::Debug>(result: Result<T, UpdateError>) -> String {
        match result {
            Err(UpdateError::Refused(refused)) => refused.0,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // --- candidates ---------------------------------------------------------------------------

    /// `it('refuses a ref that is not a better candidate')` and
    /// `it('refuses a major entry even when it is marked better')`.
    #[test]
    fn only_better_candidates_of_the_same_line_or_tag_are_accepted() {
        let scan = scan_with(
            "postgres:15.0-alpine",
            vec![
                entry("postgres:15.0-alpine", "same_tag", true),
                entry("postgres:15.14-alpine", "line", false),
                entry("postgres:16.4-alpine", "major", true),
                entry("postgres:15.15-alpine", "line", true),
                // Another repository, a digest, no tag, a kind that does not fit the tag.
                entry("mysql:8", "line", true),
                entry(&format!("postgres@{NEW_BUILD}"), "line", true),
                entry("postgres", "line", true),
                entry("postgres:15.0-alpine", "line", true),
                entry("postgres:15.16-alpine", "same_tag", true),
                json!({"ref": 15, "kind": "line", "better": true}),
                json!({"ref": "postgres:15.17-alpine", "kind": "line", "better": 1}),
            ],
        );
        // Newer tags of the same line first, then a newer build of the same tag.
        assert_eq!(
            refs(&update_candidates(&scan)),
            ["postgres:15.15-alpine", "postgres:15.0-alpine"]
        );

        // The image name may be written with its registry.
        let qualified = scan_with(
            "docker.io/library/postgres:15.0-alpine",
            vec![entry("postgres:15.15-alpine", "line", true)],
        );
        assert_eq!(
            refs(&update_candidates(&qualified)),
            ["postgres:15.15-alpine"]
        );

        // An image without a tag runs `latest`.
        let untagged = scan_with(
            "postgres",
            vec![
                entry("postgres:latest", "same_tag", true),
                entry("postgres:latest", "line", true),
            ],
        );
        assert_eq!(update_candidates(&untagged).len(), 1);

        // A scan of a pinned digest, or of something that is no image ref, has none.
        let pinned = scan_with(
            &format!("postgres@{NEW_BUILD}"),
            vec![entry("postgres:15.15-alpine", "line", true)],
        );
        assert!(update_candidates(&pinned).is_empty());
        let invalid = scan_with(
            "Postgres:15!",
            vec![entry("postgres:15.15-alpine", "line", true)],
        );
        assert!(update_candidates(&invalid).is_empty());
        assert!(update_candidates(&scan_row("postgres:15", STATUS_COMPLETED, None)).is_empty());
    }

    #[test]
    fn a_change_reads_the_tag_the_kind_and_the_digest() {
        let database = scan_owner(
            r"App\Models\StandalonePostgresql",
            json!({"image": "postgres:15.0-alpine"}),
        );
        let line = change_of(
            &database,
            "postgres:15.0-alpine",
            "postgres:15.15-alpine",
            "line",
        );
        assert_eq!(line.old, "postgres:15.0-alpine");
        assert_eq!(line.tag, "15.15-alpine");
        assert!(!line.same_tag);
        assert_eq!(line.digest.as_deref(), Some(NEW_BUILD));

        let same = change(
            &database,
            &scan_with("postgres:15.0-alpine", Vec::new()),
            &json!({"ref": "postgres:15.0-alpine", "kind": "same_tag", "digest": null}),
            "postgres:15.0-alpine",
        )
        .unwrap();
        assert!(same.same_tag);
        assert_eq!(same.digest, None);
        // A newer build without a known digest cannot wait for the start.
        assert_eq!(
            refusal(pin(&database, &same)),
            "Coolify does not know the digest of the newer build. Check for newer versions again."
        );
    }

    /// `it('waits the scanned build of the same tag for the restart, and keeps the image')`.
    #[test]
    fn a_newer_build_of_the_same_tag_waits_for_the_start() {
        let mut database = scan_owner(
            r"App\Models\StandalonePostgresql",
            json!({"image": "postgres:15.0-alpine"}),
        );
        database.id = 920_001;
        let same = change_of(
            &database,
            "postgres:15.0-alpine",
            "postgres:15.0-alpine",
            "same_tag",
        );
        pin(&database, &same).unwrap();

        assert_eq!(
            image_start_pin::take(&database, None),
            vec![(
                "docker.io/library/postgres:15.0-alpine".to_owned(),
                image_start_pin::Pin {
                    reference: "postgres:15.0-alpine".to_owned(),
                    pull: format!("docker.io/library/postgres@{NEW_BUILD}"),
                }
            )]
        );
    }

    // --- image names --------------------------------------------------------------------------

    /// `it('changes the image and restarts the database')` and
    /// `it('keeps how the image name was written')`.
    #[test]
    fn with_tag_keeps_how_the_image_name_was_written() {
        assert_eq!(
            with_tag("postgres:15.0-alpine", "15.15-alpine"),
            "postgres:15.15-alpine"
        );
        assert_eq!(
            with_tag("docker.io/library/postgres:15.0-alpine", "15.15-alpine"),
            "docker.io/library/postgres:15.15-alpine"
        );
        assert_eq!(with_tag(" postgres ", "16"), "postgres:16");
        assert_eq!(
            with_tag("localhost:5000/team/app", "2"),
            "localhost:5000/team/app:2"
        );
        assert_eq!(
            with_tag("localhost:5000/team/app:1", "2"),
            "localhost:5000/team/app:2"
        );
    }

    #[test]
    fn same_image_needs_the_same_repository_and_tag_without_a_digest() {
        assert!(same_image("nginx:1.27", " docker.io/library/nginx:1.27 "));
        assert!(same_image("nginx", "nginx:latest"));
        assert!(!same_image("nginx:1.27", "nginx:1.28"));
        assert!(!same_image("nginx:1.27", "caddy:1.27"));
        assert!(!same_image(
            &format!("nginx@{NEW_BUILD}"),
            &format!("nginx@{NEW_BUILD}")
        ));
        assert!(!same_image("nginx:1.27", &format!("nginx@{NEW_BUILD}")));
        assert!(!same_image("${IMAGE}", "${IMAGE}"));
        assert!(!same_image("", ""));
    }

    // --- deploy -------------------------------------------------------------------------------

    /// `it('puts the old tag back when the deploy queue is full')`,
    /// `it('puts the old tag back when a deploy is already queued')`.
    #[test]
    fn a_deploy_that_is_not_queued_refuses_the_update() {
        assert_eq!(
            deploy_result(Ok(Queued::Queued {
                deployment_uuid: "d".to_owned()
            })),
            Ok(())
        );
        assert_eq!(
            refusal(deploy_result(Ok(Queued::Skipped {
                deployment_uuid: "d".to_owned()
            }))),
            "A deploy of this app is already queued. Try again when it is done."
        );
        assert!(
            refusal(deploy_result(Ok(Queued::QueueFull))).contains("Deployment queue is full.")
        );
        assert_eq!(
            refusal(deploy_result(Err("ssh: connect to host failed".to_owned()))),
            "The deploy could not start. Coolify kept the old image."
        );
    }

    // --- compose application ------------------------------------------------------------------

    /// `it('keeps the new image as an override of the compose file from Git and deploys')`.
    #[test]
    fn a_compose_app_keeps_the_new_image_as_an_override() {
        let application =
            compose_application(&[("web", "nginx:1.27"), ("db", "postgres:16")], Value::Null);
        let services = compose_services(&application).unwrap();
        assert_eq!(
            services,
            pairs(&[("web", "nginx:1.27"), ("db", "postgres:16")])
        );

        let plan = plan_compose_application(
            &services,
            active_compose_image_overrides(&application),
            &HashMap::new(),
            &[change_of(&application, "nginx:1.27", "nginx:1.29", "line")],
            &mut no_pin,
        )
        .unwrap();

        assert_eq!(
            plan,
            ComposePlan {
                overrides: vec![(
                    "web".to_owned(),
                    ImageOverride {
                        from: "nginx:1.27".to_owned(),
                        to: "nginx:1.29".to_owned()
                    }
                )],
                ..ComposePlan::default()
            }
        );
        assert_eq!(
            overrides_json(&plan.overrides).as_deref(),
            Some(r#"{"web":{"from":"nginx:1.27","to":"nginx:1.29"}}"#)
        );
        assert_eq!(overrides_json(&[]), None);
    }

    #[test]
    fn overrides_are_stored_as_laravel_encodes_them() {
        let overrides = vec![(
            "web".to_owned(),
            ImageOverride {
                from: "ghcr.io/acme/web:1".to_owned(),
                to: "ghcr.io/acme/web:2".to_owned(),
            },
        )];
        assert_eq!(
            overrides_json(&overrides).as_deref(),
            Some(r#"{"web":{"from":"ghcr.io\/acme\/web:1","to":"ghcr.io\/acme\/web:2"}}"#)
        );
    }

    /// `it('stops the override when the compose file in Git names another image')`.
    #[test]
    fn an_override_stops_when_the_compose_file_names_another_image() {
        let overrides = json!({"web": {"from": "nginx:1.27", "to": "nginx:1.29"}});
        let active = compose_application(&[("web", "nginx:1.27")], overrides.clone());
        assert_eq!(
            active_compose_image_overrides(&active),
            vec![(
                "web".to_owned(),
                ImageOverride {
                    from: "nginx:1.27".to_owned(),
                    to: "nginx:1.29".to_owned()
                }
            )]
        );
        assert_eq!(
            compose_image_override(&active, "web", Some(" nginx:1.27 ")).as_deref(),
            Some("nginx:1.29")
        );
        assert_eq!(
            compose_image_override(&active, "web", Some("nginx:1.30")),
            None
        );
        assert_eq!(compose_image_override(&active, "web", None), None);
        assert_eq!(
            compose_image_override(&active, "db", Some("nginx:1.27")),
            None
        );

        let changed = compose_application(&[("web", "nginx:1.30")], overrides.clone());
        assert!(active_compose_image_overrides(&changed).is_empty());
        let removed = compose_application(&[("api", "nginx:1.27")], overrides);
        assert!(active_compose_image_overrides(&removed).is_empty());
        // An entry that is not `{from, to}` is no override.
        let broken = compose_application(
            &[("web", "nginx:1.27")],
            json!({"web": {"from": "nginx:1.27"}}),
        );
        assert!(active_compose_image_overrides(&broken).is_empty());
        let none = compose_application(&[("web", "nginx:1.27")], Value::Null);
        assert!(active_compose_image_overrides(&none).is_empty());
    }

    /// `it('updates an image that an override already replaced')`.
    #[test]
    fn a_compose_app_updates_an_image_that_an_override_already_replaced() {
        let application = compose_application(
            &[("web", "nginx:1.27")],
            json!({"web": {"from": "nginx:1.27", "to": "nginx:1.29"}}),
        );
        let services = compose_services(&application).unwrap();
        let plan = plan_compose_application(
            &services,
            active_compose_image_overrides(&application),
            &HashMap::new(),
            &[change_of(&application, "nginx:1.29", "nginx:1.31", "line")],
            &mut no_pin,
        )
        .unwrap();

        assert_eq!(
            overrides_json(&plan.overrides).as_deref(),
            Some(r#"{"web":{"from":"nginx:1.27","to":"nginx:1.31"}}"#)
        );
        // The compose file itself no longer runs the scanned image once the override applies.
        assert_eq!(
            refusal(plan_compose_application(
                &services,
                active_compose_image_overrides(&application),
                &HashMap::new(),
                &[change_of(&application, "nginx:1.27", "nginx:1.28", "line")],
                &mut no_pin,
            )),
            NO_COMPOSE_SERVICE
        );
    }

    /// `it('changes the variable that sets the image of a compose service')`.
    #[test]
    fn a_compose_app_changes_the_variable_that_sets_the_image() {
        let application =
            compose_application(&[("web", "nginx:${NGINX_VERSION:-1.27}")], Value::Null);
        let services = compose_services(&application).unwrap();
        let plan = plan_compose_application(
            &services,
            Vec::new(),
            &HashMap::new(),
            &[change_of(&application, "nginx:1.27", "nginx:1.29", "line")],
            &mut no_pin,
        )
        .unwrap();

        assert_eq!(plan.variables, pairs(&[("NGINX_VERSION", "1.29")]));
        assert_eq!(plan.variable_services, pairs(&[("web", "NGINX_VERSION")]));
        assert_eq!(overrides_json(&plan.overrides), None);
        assert_eq!(
            refuse_other_variable_uses(
                application.str("docker_compose_raw").unwrap(),
                &plan.variable_services
            ),
            Ok(())
        );

        // The value of the Coolify variable wins over the default of the compose file.
        let values = HashMap::from([("NGINX_VERSION".to_owned(), "1.28".to_owned())]);
        assert!(compose_matches(&services, &[], &values, "nginx:1.27").is_empty());
        assert_eq!(
            compose_matches(&services, &[], &values, "nginx:1.28"),
            vec![(
                "web".to_owned(),
                ComposeMatch {
                    variable: ComposeImageVariable::parse("nginx:${NGINX_VERSION:-1.27}"),
                    current: "nginx:1.28".to_owned()
                }
            )]
        );
    }

    /// `it('deploys the scanned build of the same tag without an override')`.
    #[test]
    fn a_compose_app_pins_the_scanned_build_of_the_same_tag() {
        let application = compose_application(&[("web", "nginx:1.27")], Value::Null);
        let services = compose_services(&application).unwrap();
        let mut pinned = Vec::new();
        let plan = plan_compose_application(
            &services,
            Vec::new(),
            &HashMap::new(),
            &[change_of(
                &application,
                "nginx:1.27",
                "nginx:1.27",
                "same_tag",
            )],
            &mut |change| {
                pinned.push((change.old.clone(), change.digest.clone()));
                Ok(())
            },
        )
        .unwrap();

        assert!(plan.pinned);
        assert_eq!(overrides_json(&plan.overrides), None);
        assert!(plan.variables.is_empty());
        assert_eq!(
            pinned,
            [("nginx:1.27".to_owned(), Some(NEW_BUILD.to_owned()))]
        );
    }

    /// `it('refuses a compose app that deploys its compose file as written')` and
    /// `it('refuses a compose app with a custom command and offers no update')`.
    #[test]
    fn compose_apps_that_coolify_cannot_change_are_refused() {
        let application = compose_application(&[("web", "nginx:1.27")], Value::Null);
        assert_eq!(compose_refusal(&application), None);

        let mut raw = application.clone();
        raw.settings = json!({"is_raw_compose_deployment_enabled": true})
            .as_object()
            .cloned();
        assert_eq!(
            compose_refusal(&raw),
            Some(
                "This app deploys its compose file as written. Change the version in the compose file, then redeploy."
            )
        );

        let mut old_parser = application.clone();
        old_parser
            .record
            .insert("compose_parsing_version".to_owned(), json!("2"));
        assert_eq!(
            compose_refusal(&old_parser),
            Some("Change the version in the compose file of this app, then redeploy.")
        );
        old_parser.record.remove("compose_parsing_version");
        assert!(compose_refusal(&old_parser).is_some());

        for (field, message) in [
            (
                "docker_compose_custom_build_command",
                "This app uses a custom build command, so Coolify cannot choose the image. Change the image in your compose file.",
            ),
            (
                "docker_compose_custom_start_command",
                "This app uses a custom start command, so Coolify cannot choose the image. Change the image in your compose file.",
            ),
        ] {
            let mut custom = application.clone();
            custom
                .record
                .insert(field.to_owned(), json!("docker compose up -d"));
            assert_eq!(compose_refusal(&custom), Some(message));
            custom.record.insert(field.to_owned(), json!("  "));
            assert_eq!(compose_refusal(&custom), None);
        }
    }

    /// `it('offers no update for a compose service image that Coolify cannot change')` and
    /// `it('offers the update for a compose service that an override or a variable sets')`.
    #[test]
    fn compose_matches_are_image_lines_overrides_and_supported_variables() {
        let unsupported = compose_application(&[("web", "${REGISTRY}/acme/app:1.0")], Value::Null);
        assert!(
            compose_matches(
                &compose_services(&unsupported).unwrap(),
                &[],
                &HashMap::new(),
                "ghcr.io/acme/app:1.0"
            )
            .is_empty()
        );

        let overridden = compose_application(
            &[("web", "nginx:1.27")],
            json!({"web": {"from": "nginx:1.27", "to": "nginx:1.29"}}),
        );
        assert_eq!(
            compose_matches(
                &compose_services(&overridden).unwrap(),
                &active_compose_image_overrides(&overridden),
                &HashMap::new(),
                "nginx:1.29"
            )
            .len(),
            1
        );

        let variable = compose_application(&[("web", "nginx:${NGINX_VERSION:-1.29}")], Value::Null);
        assert_eq!(
            compose_matches(
                &compose_services(&variable).unwrap(),
                &[],
                &HashMap::new(),
                "nginx:1.29"
            )
            .len(),
            1
        );

        // Every service that runs the image changes.
        let two = compose_application(
            &[
                ("web", "nginx:1.27"),
                ("api", "docker.io/library/nginx:1.27"),
            ],
            Value::Null,
        );
        let plan = plan_compose_application(
            &compose_services(&two).unwrap(),
            Vec::new(),
            &HashMap::new(),
            &[change_of(&two, "nginx:1.27", "nginx:1.29", "line")],
            &mut no_pin,
        )
        .unwrap();
        assert_eq!(
            overrides_json(&plan.overrides).as_deref(),
            Some(
                r#"{"web":{"from":"nginx:1.27","to":"nginx:1.29"},"api":{"from":"docker.io\/library\/nginx:1.27","to":"docker.io\/library\/nginx:1.29"}}"#
            )
        );
    }

    #[test]
    fn a_compose_file_that_is_not_valid_is_refused() {
        for raw in ["", "services: [unclosed", "version: '3'", "services: web"] {
            assert_eq!(
                compose_service_images(raw),
                Err(refuse("The compose file of this app is not valid.")),
                "{raw}"
            );
        }
        // Services without an image line (a build) are left out.
        assert_eq!(
            compose_service_images(
                "services:\n  web:\n    image: ' nginx:1.27 '\n  job:\n    build: .\n  n:\n    image: 3\n"
            ),
            Ok(pairs(&[("web", "nginx:1.27")]))
        );
    }

    #[test]
    fn a_variable_that_sets_another_image_of_a_compose_app_is_refused() {
        let application = compose_application(
            &[
                ("web", "nginx:${VERSION:-1.27}"),
                ("proxy", "acme/proxy:${VERSION:-1.27}"),
            ],
            Value::Null,
        );
        assert_eq!(
            refusal(plan_compose_application(
                &compose_services(&application).unwrap(),
                Vec::new(),
                &HashMap::new(),
                &[change_of(&application, "nginx:1.27", "nginx:1.29", "line")],
                &mut no_pin,
            )),
            "The variable VERSION also sets the image of proxy. Change the versions in the compose file."
        );
    }

    /// `it('refuses a variable that the compose file also uses in another place')` and the
    /// service case `it('refuses a variable that the service compose file also uses in another
    /// place')`.
    #[test]
    fn a_variable_used_in_another_place_of_the_compose_file_is_refused() {
        let variable_services = pairs(&[("web", "NGINX_VERSION")]);
        let message = "The compose file uses the variable NGINX_VERSION in another place too. Change the version in the compose file.";
        for rest in [
            "    environment:\n      APP_VERSION: '${NGINX_VERSION}'\n",
            "    labels:\n      - 'version=$NGINX_VERSION'\n",
            "    command: 'echo ${NGINX_VERSION:-none}'\n",
            "    labels:\n      - 'version=${NGINX_VERSION}'\n",
        ] {
            let raw =
                format!("services:\n  web:\n    image: 'nginx:${{NGINX_VERSION:-1.27}}'\n{rest}");
            assert_eq!(
                refuse_other_variable_uses(&raw, &variable_services),
                Err(refuse(message)),
                "{rest}"
            );
        }
        // Another service that names the variable in its image.
        let other = "services:\n  web:\n    image: 'nginx:${NGINX_VERSION:-1.27}'\n  api:\n    image: 'nginx:${NGINX_VERSION:-1.27}-alpine'\n";
        assert_eq!(
            refuse_other_variable_uses(other, &variable_services),
            Err(refuse(message))
        );

        // Only the image line, or another variable that starts with the same name.
        let alone = "services:\n  web:\n    image: 'nginx:${NGINX_VERSION:-1.27}'\n    environment:\n      A: '${NGINX_VERSION_FULL}'\n      B: '$NGINX_VERSIONS'\n";
        assert_eq!(
            refuse_other_variable_uses(alone, &variable_services),
            Ok(())
        );
        // Without a changed variable the compose file is not read.
        assert_eq!(
            refuse_other_variable_uses("services: [unclosed", &[]),
            Ok(())
        );
        assert_eq!(
            refuse_other_variable_uses("services: [unclosed", &variable_services),
            Err(refuse("The compose file is not valid."))
        );
        assert_eq!(
            refuse_other_variable_uses("", &variable_services),
            Err(refuse("The compose file is not valid."))
        );
    }

    #[test]
    fn uses_variable_matches_the_whole_name() {
        assert!(uses_variable("a $TAG b", "TAG"));
        assert!(uses_variable("${TAG}", "TAG"));
        assert!(uses_variable("${TAG:-1}", "TAG"));
        assert!(uses_variable("x$TAG", "TAG"));
        assert!(uses_variable("$TAG", "TAG"));
        assert!(uses_variable("$TAG_2 and ${TAG}", "TAG"));
        assert!(!uses_variable("$TAG_2", "TAG"));
        assert!(!uses_variable("${TAGS}", "TAG"));
        assert!(!uses_variable("TAG", "TAG"));
        assert!(!uses_variable("$ {TAG}", "TAG"));
    }

    #[test]
    fn a_variable_cannot_get_two_values() {
        let mut variables = Vec::new();
        add_variable(&mut variables, "VERSION", "1.29".to_owned()).unwrap();
        add_variable(&mut variables, "VERSION", "1.29".to_owned()).unwrap();
        add_variable(&mut variables, "OTHER", "2".to_owned()).unwrap();
        assert_eq!(variables, pairs(&[("VERSION", "1.29"), ("OTHER", "2")]));
        assert_eq!(
            add_variable(&mut variables, "VERSION", "1.30".to_owned()),
            Err(refuse(
                "The variable VERSION sets more than one of these images. Change the versions in the compose file."
            ))
        );
    }

    // --- service ------------------------------------------------------------------------------

    /// `it('changes every application and database that uses the old image, and no other')`.
    #[test]
    fn a_service_changes_every_application_and_database_that_uses_the_old_image() {
        let raw = compose_of(&[
            ("web", "nginx:1.27"),
            ("worker", "nginx:1.27"),
            ("cache", "nginx:1.26"),
            ("db", "postgres:16"),
        ]);
        let owners = vec![
            web("nginx:1.27"),
            sub_resource(SERVICE_APPLICATION_MORPH, 2, "worker", "nginx:1.27"),
            sub_resource(SERVICE_APPLICATION_MORPH, 3, "cache", "nginx:1.26"),
            sub_resource(SERVICE_DATABASE_MORPH, 1, "db", "postgres:16"),
        ];

        let plan = plan_service(
            &owners,
            &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
            &HashMap::new(),
            &mut no_pin,
        )
        .unwrap();

        let targets: Vec<(&str, String)> = plan
            .targets
            .iter()
            .map(|(key, target)| {
                (
                    key.as_str(),
                    with_tag(&target.resource.string("image"), &target.tag),
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                (r"App\Models\ServiceApplication:1", "nginx:1.29".to_owned()),
                (r"App\Models\ServiceApplication:2", "nginx:1.29".to_owned()),
            ]
        );
        assert!(plan.variables.is_empty());
        assert!(!plan.pinned);

        let compose = compose_with_images(&raw, &plan.targets).unwrap();
        assert_eq!(
            compose_service_images(&yaml::dump_with_indent(&compose, 10, 2)),
            Ok(pairs(&[
                ("web", "nginx:1.29"),
                ("worker", "nginx:1.29"),
                ("cache", "nginx:1.26"),
                ("db", "postgres:16"),
            ]))
        );
        assert_eq!(
            sub_images(&plan.targets, &["a".to_owned(), "b".to_owned()]),
            [
                store::SubImage {
                    morph: SERVICE_APPLICATION_MORPH,
                    id: 1,
                    image: "a"
                },
                store::SubImage {
                    morph: SERVICE_APPLICATION_MORPH,
                    id: 2,
                    image: "b"
                },
            ]
        );
    }

    #[test]
    fn sub_resources_using_an_image_compare_repository_and_tag() {
        let owners = vec![
            web("nginx:1.27"),
            sub_resource(
                SERVICE_APPLICATION_MORPH,
                2,
                "worker",
                " docker.io/library/nginx:1.27 ",
            ),
            sub_resource(SERVICE_APPLICATION_MORPH, 3, "cache", "nginx:1.26"),
            sub_resource(
                SERVICE_APPLICATION_MORPH,
                4,
                "pinned",
                &format!("nginx@{NEW_BUILD}"),
            ),
            sub_resource(SERVICE_DATABASE_MORPH, 1, "db", "${DB_IMAGE}"),
        ];
        let names = |image_ref: &str| -> Vec<String> {
            sub_resources_of(&owners, image_ref)
                .iter()
                .map(ScanOwner::name)
                .collect()
        };
        assert_eq!(names("nginx:1.27"), ["web", "worker"]);
        assert_eq!(names("nginx:1.26"), ["cache"]);
        assert!(names("nginx:1.28").is_empty());
        assert!(names(&format!("nginx@{NEW_BUILD}")).is_empty());
        assert!(names("not an image").is_empty());

        assert_eq!(
            refusal(plan_service(
                &owners,
                &[change_of(&owners[0], "nginx:1.28", "nginx:1.29", "line")],
                &HashMap::new(),
                &mut no_pin,
            )),
            "No application or database of this service runs this image."
        );
    }

    /// `it('changes the variable that sets the tag of the image, and keeps the compose file')`.
    #[test]
    fn a_service_changes_the_variable_that_sets_the_tag() {
        let owners = vec![web("nginx:${NGINX_VERSION:-1.27}")];
        // `ResourceScans::configuredImage()` resolved the default of the compose file.
        let configured = HashMap::from([(owners[0].key(), "nginx:1.27".to_owned())]);

        let plan = plan_service(
            &owners,
            &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
            &configured,
            &mut no_pin,
        )
        .unwrap();

        assert!(plan.targets.is_empty());
        assert_eq!(plan.variables, pairs(&[("NGINX_VERSION", "1.29")]));
        assert_eq!(plan.variable_owners, ["web-uuid"]);
        assert_eq!(plan.variable_services, pairs(&[("web", "NGINX_VERSION")]));
    }

    /// `it('changes the variable that sets the whole image')`.
    #[test]
    fn a_service_changes_the_variable_that_sets_the_whole_image() {
        let owners = vec![web("${WEB_IMAGE}")];
        let configured =
            HashMap::from([(owners[0].key(), "docker.io/library/nginx:1.27".to_owned())]);

        let plan = plan_service(
            &owners,
            &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
            &configured,
            &mut no_pin,
        )
        .unwrap();

        assert_eq!(
            plan.variables,
            pairs(&[("WEB_IMAGE", "docker.io/library/nginx:1.29")])
        );

        // The variable is not set, so the image is not known; or it runs another image.
        for configured in [
            HashMap::new(),
            HashMap::from([(owners[0].key(), "nginx:1.26".to_owned())]),
        ] {
            assert_eq!(
                refusal(plan_service(
                    &owners,
                    &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
                    &configured,
                    &mut no_pin,
                )),
                UNSUPPORTED_VARIABLE
            );
        }
    }

    /// `it('refuses a variable that also sets the image of another application')`.
    #[test]
    fn a_service_refuses_a_variable_that_also_sets_another_image() {
        let owners = vec![
            web("nginx:${VERSION:-1.27}"),
            sub_resource(
                SERVICE_APPLICATION_MORPH,
                2,
                "proxy",
                "acme/proxy:${VERSION:-1.27}",
            ),
        ];
        let configured = HashMap::from([
            (owners[0].key(), "nginx:1.27".to_owned()),
            (owners[1].key(), "acme/proxy:1.27".to_owned()),
        ]);
        assert_eq!(
            refusal(plan_service(
                &owners,
                &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
                &configured,
                &mut no_pin,
            )),
            "The variable VERSION also sets the image of proxy. Change the versions in the compose file."
        );

        // A merged update that changes both to the same version is one value of the variable.
        let both = [
            change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line"),
            change_of(&owners[1], "acme/proxy:1.27", "acme/proxy:1.29", "line"),
        ];
        let plan = plan_service(&owners, &both, &configured, &mut no_pin).unwrap();
        assert_eq!(plan.variables, pairs(&[("VERSION", "1.29")]));
        assert_eq!(plan.variable_owners, ["web-uuid", "proxy-uuid"]);

        // To two versions it is refused.
        let apart = [
            change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line"),
            change_of(&owners[1], "acme/proxy:1.27", "acme/proxy:1.30", "line"),
        ];
        assert_eq!(
            refusal(plan_service(&owners, &apart, &configured, &mut no_pin)),
            "The variable VERSION sets more than one of these images. Change the versions in the compose file."
        );
    }

    /// `it('offers no update for an image that the compose file sets in a way Coolify cannot
    /// change')`.
    #[test]
    fn an_image_set_by_an_unsupported_variable_is_blocked() {
        let unsupported = web("${REPO}:${VERSION}");
        assert_eq!(
            unsupported_variable(&unsupported),
            Some(UNSUPPORTED_VARIABLE)
        );
        assert_eq!(unsupported_variable(&web("nginx:${VERSION}")), None);
        assert_eq!(unsupported_variable(&web("nginx:1.27")), None);
        assert_eq!(
            refusal(plan_service(
                std::slice::from_ref(&unsupported),
                &[change_of(&unsupported, "nginx:1.27", "nginx:1.29", "line")],
                &HashMap::new(),
                &mut no_pin,
            )),
            UNSUPPORTED_VARIABLE
        );
    }

    /// `it('refuses when the compose file names another image than the application')`.
    #[test]
    fn a_service_refuses_when_the_compose_file_names_another_image() {
        let owners = vec![web("nginx:1.27")];
        let plan = plan_service(
            &owners,
            &[change_of(&owners[0], "nginx:1.27", "nginx:1.29", "line")],
            &HashMap::new(),
            &mut no_pin,
        )
        .unwrap();

        let message =
            "The compose file sets the image of web in another way. Change it in the compose file.";
        for raw in [
            compose_of(&[("web", "nginx:1.26")]),
            compose_of(&[("web", "nginx:${VERSION}")]),
            compose_of(&[("api", "nginx:1.27")]),
            "services:\n  web:\n    build: .\n".to_owned(),
        ] {
            assert_eq!(
                compose_with_images(&raw, &plan.targets),
                Err(refuse(message)),
                "{raw}"
            );
        }
        for raw in ["", "services: [unclosed", "version: '3'"] {
            assert_eq!(
                compose_with_images(raw, &plan.targets),
                Err(refuse("The compose file of this service is not valid.")),
                "{raw}"
            );
        }
        // The rest of the compose file stays.
        let compose = compose_with_images(
            "services:\n  web:\n    image: ' nginx:1.27 '\n    restart: always\nvolumes:\n  data: null\n",
            &plan.targets,
        )
        .unwrap();
        assert_eq!(
            php::data_get(&compose, "services.web.image"),
            Some(&PhpValue::Str("nginx:1.29".to_owned()))
        );
        assert_eq!(
            php::data_get(&compose, "services.web.restart"),
            Some(&PhpValue::Str("always".to_owned()))
        );
        assert!(php::get(&compose, "volumes").is_some());
    }

    /// `it('pulls and restarts the service with the scanned build of the same tag')`.
    #[test]
    fn a_service_pins_the_scanned_build_of_the_same_tag() {
        let owners = vec![web("nginx:1.27")];
        let mut pinned = Vec::new();
        let plan = plan_service(
            &owners,
            &[change_of(
                &owners[0],
                "nginx:1.27",
                "nginx:1.27",
                "same_tag",
            )],
            &HashMap::new(),
            &mut |change| {
                pinned.push(change.old.clone());
                Ok(())
            },
        )
        .unwrap();

        assert!(plan.pinned);
        assert!(plan.targets.is_empty());
        assert!(plan.variables.is_empty());
        assert_eq!(pinned, ["nginx:1.27"]);
    }

    // --- environment variables and the audit event ----------------------------------------------

    #[test]
    fn single_line_values_lose_their_edge_whitespace() {
        assert_eq!(without_edge_whitespace(" 1.29 \n"), "1.29");
        assert_eq!(without_edge_whitespace(" a\nb \n"), " a\nb \n");
    }

    /// `it('records the update with the team of the database')`.
    #[test]
    fn the_audit_event_names_the_resource_and_the_change() {
        let mut database = scan_owner(
            r"App\Models\StandalonePostgresql",
            json!({"name": "db", "image": "postgres:15.0-alpine"}),
        );
        database.uuid = "dbuuid".to_owned();
        let one = json!({"old_ref": "postgres:15.0-alpine", "new_ref": "postgres:15.15-alpine", "kind": "line"});
        assert_eq!(
            audit_context(
                &database,
                5,
                Some("Admin"),
                Some("admin@example.com"),
                &["dbuuid".to_owned()],
                std::slice::from_ref(&one),
            )
            .to_string(),
            r#"{"team_id":1,"resource_type":"App\\Models\\StandalonePostgresql","resource_uuid":"dbuuid","resource_name":"db","updated_uuids":["dbuuid"],"old_ref":"postgres:15.0-alpine","new_ref":"postgres:15.15-alpine","kind":"line","user_id":5,"actor_id":5,"actor_name":"Admin","actor_email":"admin@example.com"}"#
        );

        // A merged update lists every change after the first one's fields.
        let two = json!({"old_ref": "a:1", "new_ref": "a:2", "kind": "line"});
        let merged = audit_context(&database, 5, None, None, &[], &[one.clone(), two.clone()]);
        assert_eq!(merged.get("changes"), Some(&json!([one, two])));
        assert_eq!(merged.get("old_ref"), Some(&json!("postgres:15.0-alpine")));
        assert_eq!(merged.get("actor_name"), Some(&Value::Null));
    }

    #[test]
    fn errors_show_their_message() {
        assert_eq!(refused("No.").to_string(), "No.");
        assert_eq!(
            UpdateError::Forbidden("Never.".to_owned()).to_string(),
            "Never."
        );
        assert_eq!(
            UpdateError::from(refuse("Refused.")),
            UpdateError::Refused(ImageUpdateRefused("Refused.".to_owned()))
        );
        assert_eq!(database_kind(&web("nginx:1")), None);
        assert_eq!(
            database_kind(&scan_owner(r"App\Models\StandalonePostgresql", json!({}))),
            Some(Kind::Postgresql)
        );
    }
}
