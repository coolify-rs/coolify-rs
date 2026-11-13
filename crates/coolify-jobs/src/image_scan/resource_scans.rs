//! `App\Services\ImageScan\ResourceScans` and `RefreshImageScanSummariesJob`: the current scan
//! results of a resource page: what the Vulnerabilities page, the sidebar count, and the SBOM
//! download read. A service shows the results of its applications and databases.
//!
//! The queries are in `coolify_db::image_scans` (`find_resource`, `owners`, `current_for`,
//! `current_scan_ids`, `scans_by_ids`, `deployment_scan_rows`).

use std::collections::HashMap;
use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::image_scans::{
    self, ImageScanRow, SERVICE_MORPH, STATUS_COMPLETED, ScanOwner, ScanValue,
};
use coolify_db::{Db, environment_variables};
use regex::Regex;
use serde_json::{Value, json};

use super::compose_image_variable::ComposeImageVariable;
use super::finding::Finding;
use super::grype::EolDistro;
use super::image_ref::ImageRef;
use super::rules::IgnoreMatcher;
use super::scan_image;
use crate::Jobs;

/// `ResourceScans::RESOURCE_TYPES`.
pub const RESOURCE_TYPES: [&str; 3] = ["application", "service", "database"];

static ADVISORY_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._:-]{0,63}\z").unwrap_or_else(|_| unreachable!())
});

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// `ResourceScans::scanTargets($resource)`: the images Coolify scans for a resource without a
/// deployment: each application and database of a service, or the resource itself (a standalone
/// database), with its image. Owners excluded from the status and owners without an image are
/// not scanned.
pub async fn scan_targets(
    db: &Db,
    resource: &ScanOwner,
) -> Result<Vec<(ScanOwner, String)>, String> {
    let owners = image_scans::owners(db, resource).await.map_err(err)?;
    Ok(owners
        .into_iter()
        .filter(is_scan_target)
        .map(|owner| {
            let image = owner.image();
            (owner, image)
        })
        .collect())
}

/// `ResourceScans::isScanTarget($owner)`: whether Coolify scans the image of an application or
/// database of a service, or of a standalone database.
pub fn is_scan_target(owner: &ScanOwner) -> bool {
    !owner.exclude_from_status() && !owner.image().is_empty()
}

/// `ResourceScans::current($resource)`: the current scans of every owner of the resource.
pub async fn current(db: &Db, resource: &ScanOwner) -> Result<Vec<ImageScanRow>, String> {
    let mut scans = Vec::new();
    for owner in image_scans::owners(db, resource).await.map_err(err)? {
        scans.extend(
            image_scans::current_for(db, owner.morph, owner.id)
                .await
                .map_err(err)?,
        );
    }
    Ok(scans)
}

/// `ResourceScans::currentFor($owner)`.
pub async fn current_for(db: &Db, owner: &ScanOwner) -> Result<Vec<ImageScanRow>, String> {
    image_scans::current_for(db, owner.morph, owner.id)
        .await
        .map_err(err)
}

/// `ResourceScans::currentForMany($owners)`: the current scans of many owners with two queries,
/// keyed by `key()` of the owner, the latest scan first. With `light`, `findings` and
/// `candidates` read as null.
pub async fn current_for_many(
    db: &Db,
    owners: &[ScanOwner],
    light: bool,
) -> Result<HashMap<String, Vec<ImageScanRow>>, String> {
    let keys: Vec<(String, i64)> = owners.iter().map(|o| (o.morph.to_owned(), o.id)).collect();
    let ids = image_scans::current_scan_ids(db, &keys)
        .await
        .map_err(err)?;
    let mut by_owner: HashMap<String, Vec<ImageScanRow>> = HashMap::new();
    for scan in image_scans::scans_by_ids(db, &ids, light)
        .await
        .map_err(err)?
    {
        by_owner.entry(scan.owner_key()).or_default().push(scan);
    }
    Ok(by_owner)
}

/// `ResourceScans::ownersOfMany($resources)`: the owners of many resources, keyed by `key()` of
/// the resource.
pub async fn owners_of_many(
    db: &Db,
    resources: &[ScanOwner],
) -> Result<HashMap<String, Vec<ScanOwner>>, String> {
    let mut out = HashMap::new();
    for resource in resources {
        out.insert(
            resource.key(),
            image_scans::owners(db, resource).await.map_err(err)?,
        );
    }
    Ok(out)
}

/// One image of a Compose application without a completed result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncheckedRef {
    /// The Compose service, when the file names the image.
    pub name: Option<String>,
    pub image_ref: String,
}

fn is_compose(owner: &ScanOwner) -> bool {
    owner.build_pack() == Some("dockercompose")
}

/// `ResourceScans::uncheckedComposeImages()`: the number of [`unchecked_compose_refs`].
pub async fn unchecked_compose_images(
    db: &Db,
    owner: &ScanOwner,
    current_scans: &[ImageScanRow],
    deployment_rows: Option<&[ImageScanRow]>,
) -> Result<usize, String> {
    Ok(
        unchecked_compose_refs(db, owner, current_scans, deployment_rows)
            .await?
            .len(),
    )
}

/// `ResourceScans::uncheckedComposeRefs()`: the images of a Compose application without a
/// completed result in its current deployment: rows of that deployment that failed or did not
/// finish (their image ref), and images of the stored Compose file without a row of it (the
/// image as the file writes it). Images set with a variable are left out. Empty for other
/// resources. `deployment_rows` are the rows of [`compose_deployment_rows`] for lists, else one
/// query here.
pub async fn unchecked_compose_refs(
    db: &Db,
    owner: &ScanOwner,
    current_scans: &[ImageScanRow],
    deployment_rows: Option<&[ImageScanRow]>,
) -> Result<Vec<UncheckedRef>, String> {
    if !is_compose(owner) {
        return Ok(Vec::new());
    }
    let deployment_id = current_scans.first().and_then(|s| s.deployment_queue_id);
    let fetched;
    let rows: &[ImageScanRow] = match (deployment_id, deployment_rows) {
        (None, _) => current_scans,
        (Some(_), Some(rows)) => rows,
        (Some(id), None) => {
            fetched = image_scans::deployment_scan_rows(db, &[id])
                .await
                .map_err(err)?
                .into_iter()
                .filter(|row| row.resource_id == owner.id)
                .collect::<Vec<_>>();
            &fetched
        }
    };
    Ok(unchecked_refs_of(owner, deployment_id.is_some(), rows))
}

/// The pure part of [`unchecked_compose_refs`]: `rows` are the scan rows of the current
/// deployment, or the current scans when they have no deployment.
pub fn unchecked_refs_of(
    owner: &ScanOwner,
    has_deployment: bool,
    rows: &[ImageScanRow],
) -> Vec<UncheckedRef> {
    if !is_compose(owner) {
        return Vec::new();
    }
    let images = compose_images(owner);
    let service_of = compose_service_names(owner);
    let mut unchecked = Vec::new();
    // Current scans are completed; only the rows of a deployment can be unfinished.
    if has_deployment {
        for row in rows.iter().filter(|row| row.status != STATUS_COMPLETED) {
            let name = scan_image::image_name(&row.image_ref);
            unchecked.push(UncheckedRef {
                name: service_of
                    .iter()
                    .find(|(image, _)| *image == name)
                    .map(|(_, service)| service.clone()),
                image_ref: row.image_ref.clone(),
            });
        }
    }
    let row_names: Vec<String> = rows
        .iter()
        .map(|row| scan_image::image_name(&row.image_ref))
        .collect();
    for (image_name, service) in &service_of {
        if !row_names.contains(image_name) {
            let image = images
                .iter()
                .find(|(s, _)| s == service)
                .map(|(_, image)| image.trim().to_owned())
                .unwrap_or_default();
            unchecked.push(UncheckedRef {
                name: Some(service.clone()),
                image_ref: image,
            });
        }
    }
    unchecked
}

/// `ResourceScans::composeDeploymentRows()`: the scan rows of the current deployment of each
/// Compose application among the owners, with one query, keyed by `key()` of the owner.
pub async fn compose_deployment_rows(
    db: &Db,
    owners: &[ScanOwner],
    current_scans: &HashMap<String, Vec<ImageScanRow>>,
) -> Result<HashMap<String, Vec<ImageScanRow>>, String> {
    let deployments: Vec<(String, i64)> = owners
        .iter()
        .filter(|owner| is_compose(owner))
        .filter_map(|owner| {
            let id = current_scans
                .get(&owner.key())
                .and_then(|scans| scans.first())
                .and_then(|scan| scan.deployment_queue_id)?;
            Some((owner.key(), id))
        })
        .collect();
    if deployments.is_empty() {
        return Ok(HashMap::new());
    }
    let mut ids: Vec<i64> = deployments.iter().map(|(_, id)| *id).collect();
    ids.sort_unstable();
    ids.dedup();
    let rows = image_scans::deployment_scan_rows(db, &ids)
        .await
        .map_err(err)?;
    Ok(deployments
        .into_iter()
        .map(|(key, deployment_id)| {
            let of_owner = rows
                .iter()
                .filter(|row| {
                    row.owner_key() == key && row.deployment_queue_id == Some(deployment_id)
                })
                .cloned()
                .collect();
            (key, of_owner)
        })
        .collect())
}

fn php_get<'a>(value: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    match value {
        PhpValue::Array(items) => items
            .iter()
            .find(|(k, _)| k.as_string() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

/// The stored Compose file of an application (`docker_compose ?: docker_compose_raw`), parsed.
/// `None` when it is not valid YAML.
pub(crate) fn parsed_compose(application: &ScanOwner) -> Option<PhpValue> {
    let yaml = match application.str("docker_compose") {
        Some(compose) if !compose.is_empty() && compose != "0" => compose,
        _ => application.str("docker_compose_raw").unwrap_or_default(),
    };
    coolify_compat::yaml_parse::parse(yaml, 1024).ok()
}

/// The services of a parsed Compose file, by name.
pub(crate) fn compose_services(compose: &PhpValue) -> Vec<(String, &PhpValue)> {
    match php_get(compose, "services") {
        Some(PhpValue::Array(services)) => services
            .iter()
            .map(|(name, service)| (Key::as_string(name), service))
            .collect(),
        _ => Vec::new(),
    }
}

/// `data_get($service, $key)` of a Compose service.
pub(crate) fn service_value<'a>(service: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    php_get(service, key).filter(|v| !matches!(v, PhpValue::Null))
}

/// `ResourceScans::composeImages($application)`: the images of the services of the stored
/// Compose file of an application, by service name, in the order of the file. Images set with a
/// variable are left out.
pub fn compose_images(application: &ScanOwner) -> Vec<(String, String)> {
    if !is_compose(application) {
        return Vec::new();
    }
    let Some(compose) = parsed_compose(application) else {
        return Vec::new();
    };
    compose_services(&compose)
        .into_iter()
        .filter_map(|(name, service)| match service_value(service, "image") {
            Some(PhpValue::Str(image)) if !image.trim().is_empty() && !image.contains('$') => {
                Some((name, image.trim().to_owned()))
            }
            _ => None,
        })
        .collect()
}

/// `ResourceScans::composeServiceNames($owner)`: the Compose service of each image name (without
/// tag) of the stored Compose file, the first service when several use the image, as
/// `(image name, service)` in the order of the file. Empty for other resources.
pub fn compose_service_names(owner: &ScanOwner) -> Vec<(String, String)> {
    let mut service_of: Vec<(String, String)> = Vec::new();
    for (service, image) in compose_images(owner) {
        let name = scan_image::image_name(&image);
        if !service_of.iter().any(|(n, _)| *n == name) {
            service_of.push((name, service));
        }
    }
    service_of
}

/// `ResourceScans::storedFixedCount($scan)`: the relevant findings of a scan that an action
/// fixes, from its stored security status.
pub fn stored_fixed_count(scan: &ImageScanRow) -> i64 {
    scan.summary
        .as_ref()
        .and_then(|s| s.pointer("/security/facts/counts/relevant_fixed"))
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
}

/// `ResourceScans::summaryFor()`: the summary of a scan before its security status is added
/// (`SecurityStatus::store()`): the number of ignored findings, the end-of-life OS, and whether
/// the report was cut.
pub fn summary_for(
    findings: &[Finding],
    ignores: &IgnoreMatcher,
    eol: Option<&EolDistro>,
    truncated: bool,
) -> Value {
    json!({
        "ignored": findings.iter().filter(|f| ignores.matches(f).is_some()).count(),
        "eol_distro": eol.map(EolDistro::to_value),
        "truncated": truncated,
    })
}

/// `SecurityStatus::store($scan)`: adds the stored security status of the scan to its summary
/// (`summary.security`), from its findings, its candidates and the rules of its owner. The
/// caller saves the scan.
pub(crate) async fn store_security_status(
    jobs: &Jobs,
    scan: &mut ImageScanRow,
    owner: &ScanOwner,
) -> Result<(), String> {
    super::views::status::store(jobs, scan, owner).await
}

/// `ResourceScans::refreshSummaries($scans)`: writes the summaries again, for example after an
/// ignore or a version check, so the stored security status matches the findings.
pub async fn refresh_summaries(jobs: &Jobs, scans: Vec<ImageScanRow>) -> Result<(), String> {
    let db = &jobs.0.db;
    for mut scan in scans {
        let Some(owner) = image_scans::find_owner(db, &scan.resource_type, scan.resource_id)
            .await
            .map_err(err)?
        else {
            continue;
        };
        let now = Jobs::now();
        let previous = scan.summary.clone().unwrap_or(Value::Null);
        let ignores = IgnoreMatcher::for_resource(db, &owner, now).await?;
        scan.summary = Some(summary_for(
            &findings(&scan),
            &ignores,
            EolDistro::from_value(previous.get("eol_distro")).as_ref(),
            php_bool(previous.get("truncated")),
        ));
        store_security_status(jobs, &mut scan, &owner).await?;
        image_scans::update_scan(
            db,
            scan.id,
            &[("summary", ScanValue::Json(scan.summary.clone()))],
            now,
        )
        .await
        .map_err(err)?;
    }
    Ok(())
}

/// `(bool) $value` of a decoded JSON value.
fn php_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// `ResourceScans::currentForTeam($teamId)`: the current scans of every resource of a team, for
/// example to refresh the counts after a team-wide ignore.
pub async fn current_for_team(db: &Db, team_id: i64) -> Result<Vec<ImageScanRow>, String> {
    let mut scans = Vec::new();
    for (resource_type, resource_id) in image_scans::scan_owner_keys_of_team(db, team_id)
        .await
        .map_err(err)?
    {
        let Some(owner) = image_scans::find_owner(db, &resource_type, resource_id)
            .await
            .map_err(err)?
        else {
            continue;
        };
        if owner.team_id != Some(team_id) {
            continue;
        }
        scans.extend(current_for(db, &owner).await?);
    }
    Ok(scans)
}

/// `RefreshImageScanSummariesJob::handle()`: writes the summaries of the current scans of every
/// resource of a team again, with their stored security status, after a team-wide ignore was
/// added or removed, a team-wide ignore ended, or the upgrade to security statuses.
pub async fn refresh_team_summaries(jobs: &Jobs, team_id: i64) -> Result<(), String> {
    let scans = current_for_team(&jobs.0.db, team_id).await?;
    refresh_summaries(jobs, scans).await
}

impl Jobs {
    /// `RefreshImageScanSummariesJob::dispatch($teamId)`.
    pub fn dispatch_refresh_image_scan_summaries(&self, team_id: i64) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = refresh_team_summaries(&jobs, team_id).await {
                tracing::warn!(team_id, error = %e, "RefreshImageScanSummariesJob failed");
            }
        });
    }
}

/// `ResourceScans::findings($scan)`: the findings of a scan.
pub fn findings(scan: &ImageScanRow) -> Vec<Finding> {
    Finding::list_from_value(scan.findings.as_ref())
}

/// `ResourceScans::runsOtherImage($scan, $owner)`: whether the resource now runs another image
/// than the scanned one. Decided from the configuration and the deployments, without a call to
/// the server.
pub async fn runs_other_image(
    jobs: &Jobs,
    scan: &ImageScanRow,
    owner: &ScanOwner,
) -> Result<bool, String> {
    if owner.is_application() {
        return match owner.build_pack() {
            Some("dockercompose") => Ok(false),
            Some("dockerimage") => Ok(!same_image(
                docker_image_ref(owner).as_deref(),
                &scan.image_ref,
            )),
            _ => built_commit_changed(&jobs.0.db, owner, scan).await,
        };
    }
    let configured = configured_image(jobs, owner).await?;
    Ok(!same_image(Some(&configured), &scan.image_ref))
}

/// `ResourceScans::configuredImage($owner)`: the image of a database or a service application or
/// database, with the Coolify variable of the service resolved when one sets it
/// (`${N8N_IMAGE}`, `n8nio/n8n:${N8N_VERSION}`).
pub async fn configured_image(jobs: &Jobs, owner: &ScanOwner) -> Result<String, String> {
    let image = owner.image();
    let (Some(variable), Some(service_id)) = (
        ComposeImageVariable::parse(&image),
        owner.service_id.filter(|_| owner.is_service_part()),
    ) else {
        return Ok(image);
    };
    let row = environment_variables::first_by_key(
        &jobs.0.db,
        SERVICE_MORPH,
        service_id,
        &variable.key,
        false,
    )
    .await
    .map_err(err)?;
    let value = row
        .as_ref()
        .and_then(|r| r.get("value"))
        .and_then(Value::as_str)
        .and_then(|cipher| {
            jobs.encrypter()
                .and_then(|enc| enc.decrypt_maybe_serialized(cipher).ok())
        });
    Ok(variable.resolve(value.as_deref()).unwrap_or(image))
}

/// `ResourceScans::currentImageRef($application)`: the image an application runs now, named like
/// the deploy job names it. `None` when Coolify cannot tell, for example before the first deploy.
pub async fn current_image_ref(db: &Db, application: &ScanOwner) -> Result<Option<String>, String> {
    let name_or_uuid = || {
        application
            .str("docker_registry_image_name")
            .filter(|n| !n.is_empty() && *n != "0")
            .unwrap_or(&application.uuid)
            .to_owned()
    };
    let reference = match application.build_pack() {
        Some("dockercompose") => None,
        Some("dockerimage") => docker_image_ref(application),
        _ if application
            .str("dockerfile")
            .is_some_and(|d| !d.trim().is_empty()) =>
        {
            Some(format!("{}:latest", name_or_uuid()))
        }
        _ => latest_deployed_commit(db, application)
            .await?
            .map(|commit| {
                format!(
                    "{}:{}",
                    name_or_uuid(),
                    commit.chars().take(128).collect::<String>()
                )
            }),
    };
    Ok(reference.filter(|r| ImageRef::parse(r).is_ok()))
}

/// `ResourceScans::advisoryUrl($id)`: a link to the advisory, built by Coolify: NVD for CVE IDs,
/// GitHub for GHSA IDs, else none.
pub fn advisory_url(id: &str) -> Option<String> {
    if !is_advisory_id(id) {
        return None;
    }
    if id.starts_with("CVE-") {
        Some(format!("https://nvd.nist.gov/vuln/detail/{id}"))
    } else if id.starts_with("GHSA-") {
        Some(format!("https://github.com/advisories/{id}"))
    } else {
        None
    }
}

/// `ResourceScans::isAdvisoryId($id)`.
pub fn is_advisory_id(id: &str) -> bool {
    ADVISORY_ID.is_match(id)
}

/// `ResourceScans::exploitLikelihood($epss)`: "low", "medium", or "high" from the EPSS score.
/// `None` without a score.
pub fn exploit_likelihood(epss: Option<f64>) -> Option<&'static str> {
    epss.map(|epss| {
        if epss < 0.1 {
            "low"
        } else if epss < 0.5 {
            "medium"
        } else {
            "high"
        }
    })
}

/// `ResourceScans::dockerImageRef($application)`.
pub(crate) fn docker_image_ref(application: &ScanOwner) -> Option<String> {
    let name = application.string("docker_registry_image_name");
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let tag = application.string("docker_registry_image_tag");
    let tag = match tag.trim() {
        "" | "0" => "latest",
        tag => tag,
    };
    Some(match tag.strip_prefix("sha256-") {
        Some(hash) => format!("{name}@sha256:{hash}"),
        None => format!("{name}:{tag}"),
    })
}

async fn latest_deployed_commit(
    db: &Db,
    application: &ScanOwner,
) -> Result<Option<String>, String> {
    let latest = image_scans::latest_finished_deployment(db, application.id)
        .await
        .map_err(err)?;
    Ok(usable_commit(
        latest.and_then(|(_, commit)| commit).as_deref(),
    ))
}

async fn built_commit_changed(
    db: &Db,
    application: &ScanOwner,
    scan: &ImageScanRow,
) -> Result<bool, String> {
    let deployed = latest_deployed_commit(db, application).await?;
    let scan_commit = match scan.deployment_queue_id {
        Some(id) => image_scans::deployment_facts(db, id)
            .await
            .map_err(err)?
            .and_then(|d| d.commit),
        None => None,
    };
    let scanned = usable_commit(scan_commit.as_deref()).or_else(|| tag_of(&scan.image_ref));
    Ok(commit_changed(deployed.as_deref(), scanned.as_deref()))
}

/// The decision of `builtCommitChanged()`: both commits are known, the scanned one is not
/// `latest`, and neither starts with the other.
fn commit_changed(deployed: Option<&str>, scanned: Option<&str>) -> bool {
    match (deployed, scanned) {
        (Some(deployed), Some(scanned)) if scanned != "latest" => {
            !deployed.starts_with(scanned) && !scanned.starts_with(deployed)
        }
        _ => false,
    }
}

fn usable_commit(commit: Option<&str>) -> Option<String> {
    let commit = commit.unwrap_or_default().trim();
    (!commit.is_empty() && commit != "HEAD").then(|| commit.to_owned())
}

fn tag_of(reference: &str) -> Option<String> {
    ImageRef::parse(reference).ok().and_then(|r| r.tag)
}

/// `ResourceScans::sameImage($configured, $scanned)`: same repository and tag. A scan of a pinned
/// digest matches any tag of the repository, because the tag it came from is not stored.
pub(crate) fn same_image(configured: Option<&str>, scanned: &str) -> bool {
    let configured = configured.unwrap_or_default().trim();
    let scanned = scanned.trim();
    if configured.is_empty() || scanned.is_empty() {
        return true;
    }
    let (Ok(running), Ok(result)) = (ImageRef::parse(configured), ImageRef::parse(scanned)) else {
        return configured == scanned;
    };
    if running.qualified_name() != result.qualified_name() {
        return false;
    }
    if result.digest.is_some() {
        return running.digest.is_none() || running.digest == result.digest;
    }
    if running.digest.is_some() {
        return false;
    }
    running.tag.as_deref().unwrap_or("latest") == result.tag.as_deref().unwrap_or("latest")
}

#[cfg(test)]
pub(crate) mod tests {
    use coolify_db::image_scans::{APPLICATION_MORPH, STATUS_FAILED};
    use coolify_db::records::Record;

    use super::super::rules::tests::{finding, ignore, now};
    use super::*;

    pub(crate) fn owner(morph: &'static str, record: Value) -> ScanOwner {
        ScanOwner {
            morph,
            id: 7,
            uuid: "appuuid".to_owned(),
            team_id: Some(1),
            main_server_id: Some(2),
            service_id: None,
            record: record.as_object().cloned().unwrap_or_default(),
            settings: None::<Record>,
        }
    }

    pub(crate) fn scan(image_ref: &str, status: &str, deployment: Option<i64>) -> ImageScanRow {
        ImageScanRow {
            id: 1,
            uuid: "scan".to_owned(),
            server_id: 2,
            image_sbom_id: None,
            image_id: format!("sha256:{}", "a".repeat(64)),
            repo_digest: None,
            resource_type: APPLICATION_MORPH.to_owned(),
            resource_id: 7,
            image_ref: image_ref.to_owned(),
            deployment_queue_id: deployment,
            status: status.to_owned(),
            error_code: None,
            error_context: None,
            summary: None,
            findings: None,
            candidates: None,
            base_image_ref: None,
            builds_image: false,
            is_baseline: false,
            grype_db_built_at: None,
            scanned_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    const COMPOSE: &str = "services:\n  web:\n    image: 'nginx:1.27'\n  worker:\n    build: .\n    image: ghcr.io/acme/worker:2\n  cache:\n    image: 'nginx:1.28'\n  db:\n    image: ${DB_IMAGE:-postgres:16}\n  job:\n    build: ./job\n";

    fn compose_app() -> ScanOwner {
        owner(
            APPLICATION_MORPH,
            json!({"build_pack": "dockercompose", "docker_compose_raw": COMPOSE}),
        )
    }

    #[test]
    fn compose_images_leave_out_variables_and_services_without_an_image() {
        assert_eq!(
            compose_images(&compose_app()),
            vec![
                ("web".to_owned(), "nginx:1.27".to_owned()),
                ("worker".to_owned(), "ghcr.io/acme/worker:2".to_owned()),
                ("cache".to_owned(), "nginx:1.28".to_owned()),
            ]
        );
        // The first service that uses an image names it.
        assert_eq!(
            compose_service_names(&compose_app()),
            vec![
                ("nginx".to_owned(), "web".to_owned()),
                ("ghcr.io/acme/worker".to_owned(), "worker".to_owned()),
            ]
        );
        let other = owner(
            APPLICATION_MORPH,
            json!({"build_pack": "nixpacks", "docker_compose_raw": COMPOSE}),
        );
        assert!(compose_images(&other).is_empty());
        let broken = owner(
            APPLICATION_MORPH,
            json!({"build_pack": "dockercompose", "docker_compose_raw": "services: [unclosed"}),
        );
        assert!(compose_images(&broken).is_empty());
    }

    #[test]
    fn the_parsed_compose_file_wins_over_the_raw_one() {
        let app = owner(
            APPLICATION_MORPH,
            json!({"build_pack": "dockercompose", "docker_compose": "services:\n  web:\n    image: caddy:2\n", "docker_compose_raw": COMPOSE}),
        );
        assert_eq!(
            compose_images(&app),
            vec![("web".to_owned(), "caddy:2".to_owned())]
        );
    }

    #[test]
    fn unchecked_compose_refs_list_unfinished_rows_and_images_without_a_row() {
        let app = compose_app();
        // Without a deployment only the images of the file without a scan are unchecked.
        let current = [scan("nginx:1.27", STATUS_COMPLETED, None)];
        assert_eq!(
            unchecked_refs_of(&app, false, &current),
            vec![UncheckedRef {
                name: Some("worker".to_owned()),
                image_ref: "ghcr.io/acme/worker:2".to_owned()
            }]
        );
        // With a deployment its failed rows are unchecked too.
        let rows = [
            scan("nginx:1.27", STATUS_COMPLETED, Some(9)),
            scan("ghcr.io/acme/worker:2", STATUS_FAILED, Some(9)),
            scan("appuuid_job:abc", STATUS_FAILED, Some(9)),
        ];
        assert_eq!(
            unchecked_refs_of(&app, true, &rows),
            vec![
                UncheckedRef {
                    name: Some("worker".to_owned()),
                    image_ref: "ghcr.io/acme/worker:2".to_owned()
                },
                UncheckedRef {
                    name: None,
                    image_ref: "appuuid_job:abc".to_owned()
                },
            ]
        );
        assert!(
            unchecked_refs_of(
                &owner(APPLICATION_MORPH, json!({"build_pack": "nixpacks"})),
                true,
                &rows
            )
            .is_empty()
        );
    }

    #[test]
    fn summary_counts_ignored_findings() {
        let ignores = IgnoreMatcher::from_ignores(vec![ignore(Some("CVE-2024-1"), None)], now());
        let findings = [finding("CVE-2024-1", "high"), finding("CVE-2024-2", "high")];
        let eol = EolDistro {
            name: "alpine".into(),
            version: "3.16".into(),
        };
        assert_eq!(
            summary_for(&findings, &ignores, Some(&eol), true).to_string(),
            r#"{"ignored":1,"eol_distro":{"name":"alpine","version":"3.16"},"truncated":true}"#
        );
        assert_eq!(
            summary_for(&[], &ignores, None, false).to_string(),
            r#"{"ignored":0,"eol_distro":null,"truncated":false}"#
        );
    }

    #[test]
    fn stored_fixed_count_reads_the_security_status() {
        let mut s = scan("nginx:1", STATUS_COMPLETED, None);
        assert_eq!(stored_fixed_count(&s), 0);
        s.summary = Some(json!({"security": {"facts": {"counts": {"relevant_fixed": 4}}}}));
        assert_eq!(stored_fixed_count(&s), 4);
    }

    #[test]
    fn same_image_compares_repository_and_tag() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        assert!(same_image(
            Some("nginx:1.27"),
            "docker.io/library/nginx:1.27"
        ));
        assert!(same_image(Some("nginx"), "nginx:latest"));
        assert!(!same_image(Some("nginx:1.27"), "nginx:1.28"));
        assert!(!same_image(Some("nginx:1.27"), "caddy:1.27"));
        // A scan of a pinned digest matches any tag of the repository.
        assert!(same_image(Some("nginx:1.27"), &format!("nginx@{digest}")));
        assert!(same_image(
            Some(&format!("nginx@{digest}")),
            &format!("nginx@{digest}")
        ));
        assert!(!same_image(
            Some(&format!("nginx@sha256:{}", "cd".repeat(32))),
            &format!("nginx@{digest}")
        ));
        // A configured digest never matches a scanned tag.
        assert!(!same_image(Some(&format!("nginx@{digest}")), "nginx:1.27"));
        // Nothing to compare, or not an image reference.
        assert!(same_image(None, "nginx:1.27"));
        assert!(same_image(Some("nginx:1.27"), " "));
        assert!(same_image(Some("${IMAGE}"), "${IMAGE}"));
        assert!(!same_image(Some("${IMAGE}"), "nginx:1.27"));
    }

    #[test]
    fn docker_image_ref_of_an_application() {
        let app = |name: &str, tag: Value| {
            owner(
                APPLICATION_MORPH,
                json!({"build_pack": "dockerimage", "docker_registry_image_name": name, "docker_registry_image_tag": tag}),
            )
        };
        assert_eq!(
            docker_image_ref(&app("nginx", json!("1.27"))).as_deref(),
            Some("nginx:1.27")
        );
        assert_eq!(
            docker_image_ref(&app(" nginx ", json!(null))).as_deref(),
            Some("nginx:latest")
        );
        assert_eq!(
            docker_image_ref(&app("nginx", json!(format!("sha256-{}", "ab".repeat(32)))))
                .as_deref(),
            Some(format!("nginx@sha256:{}", "ab".repeat(32)).as_str())
        );
        assert_eq!(docker_image_ref(&app("", json!("1"))), None);
    }

    #[test]
    fn commits_and_advisories() {
        assert!(!commit_changed(None, Some("abc")));
        assert!(!commit_changed(Some("abcdef"), Some("latest")));
        assert!(!commit_changed(Some("abcdef123"), Some("abcdef")));
        assert!(!commit_changed(Some("abc"), Some("abcdef")));
        assert!(commit_changed(Some("abcdef"), Some("123456")));
        assert_eq!(usable_commit(Some(" HEAD ")), None);
        assert_eq!(usable_commit(Some("")), None);
        assert_eq!(usable_commit(Some("abc")).as_deref(), Some("abc"));

        assert_eq!(
            advisory_url("CVE-2024-1").as_deref(),
            Some("https://nvd.nist.gov/vuln/detail/CVE-2024-1")
        );
        assert_eq!(
            advisory_url("GHSA-aaaa-bbbb").as_deref(),
            Some("https://github.com/advisories/GHSA-aaaa-bbbb")
        );
        assert_eq!(advisory_url("ALAS-2024-1"), None);
        assert_eq!(advisory_url("CVE-2024-1/../x"), None);
        assert_eq!(exploit_likelihood(None), None);
        assert_eq!(exploit_likelihood(Some(0.05)), Some("low"));
        assert_eq!(exploit_likelihood(Some(0.1)), Some("medium"));
        assert_eq!(exploit_likelihood(Some(0.5)), Some("high"));
    }

    #[test]
    fn scan_targets_need_an_image_and_a_status() {
        assert!(is_scan_target(&owner(
            r"App\Models\ServiceDatabase",
            json!({"image": " postgres:16 ", "exclude_from_status": false})
        )));
        assert!(!is_scan_target(&owner(
            r"App\Models\ServiceDatabase",
            json!({"image": "postgres:16", "exclude_from_status": true})
        )));
        assert!(!is_scan_target(&owner(
            r"App\Models\ServiceApplication",
            json!({"image": " "})
        )));
    }
}
