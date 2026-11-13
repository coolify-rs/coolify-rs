//! `ImageScanPolicyResolver`, `ResolvedImageScanPolicy` and the enums `ImageScanGateMode` and
//! `ImageScanOnError`: the scan rules of a resource, resolved through Instance, Team, Server and
//! App. A null value inherits. Team and server come from the loaded resource, never from the
//! session.

use std::collections::HashMap;

use coolify_db::Db;
use coolify_db::image_scans::{
    self, APPLICATION_MORPH, ImageScanPolicyRow, SERVER_MORPH, ScanOwner, TEAM_MORPH,
};

/// `ImageScanGateMode`: "Block deploys" (spec 15.8): which problems of a new image stop a deploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateMode {
    Off,
    On,
    Strict,
}

impl GateMode {
    pub const ALL: [GateMode; 3] = [GateMode::Off, GateMode::On, GateMode::Strict];

    pub fn value(self) -> &'static str {
        match self {
            GateMode::Off => "off",
            GateMode::On => "on",
            GateMode::Strict => "strict",
        }
    }

    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.value() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            GateMode::Off => "Off",
            GateMode::On => "On",
            GateMode::Strict => "Strict",
        }
    }

    pub fn description(self) -> Option<&'static str> {
        match self {
            GateMode::Off => None,
            GateMode::On => Some(
                "Blocks a deploy when the new image adds a problem that attackers are using or are likely to use.",
            ),
            GateMode::Strict => Some(
                "Also blocks critical, high, and counted medium problems that have a fix, and operating systems that no longer get security fixes.",
            ),
        }
    }
}

/// `ImageScanOnError`: what a deploy does when its scan could not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnScanError {
    Deploy,
    Block,
}

impl OnScanError {
    pub const ALL: [OnScanError; 2] = [OnScanError::Deploy, OnScanError::Block];

    pub fn value(self) -> &'static str {
        match self {
            OnScanError::Deploy => "deploy",
            OnScanError::Block => "block",
        }
    }

    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.value() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            OnScanError::Deploy => "Deploy anyway",
            OnScanError::Block => "Block the deploy",
        }
    }
}

/// A resolved value with the level it comes from: instance, team, server or app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: &'static str,
}

/// The values of `resolveLevels()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLevels {
    pub gate_mode: Sourced<GateMode>,
    pub on_scan_error: Sourced<OnScanError>,
    pub count_medium: Sourced<bool>,
}

/// `ResolvedImageScanPolicy`: the scan rules of one resource after inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPolicy {
    /// Off for everything but an application: services and databases have no deploy to block.
    pub gate_mode: GateMode,
    pub on_scan_error: OnScanError,
    /// Scanning is on for the scan server, and the application did not turn its scan off.
    pub scan_enabled: bool,
    /// The level each value came from, also for the gate mode a non-application does not use.
    pub sources: ResolvedLevels,
    /// The server the image is scanned on (`$policy->scanServer`).
    pub scan_server_id: Option<i64>,
    pub count_medium: bool,
}

/// `resolveLevels($levels)`: the levels from the top level down; a set field of a lower level
/// wins.
pub fn resolve_levels(levels: &[(&'static str, Option<ImageScanPolicyRow>)]) -> ResolvedLevels {
    let mut resolved = ResolvedLevels {
        gate_mode: Sourced {
            value: GateMode::Off,
            source: "instance",
        },
        on_scan_error: Sourced {
            value: OnScanError::Deploy,
            source: "instance",
        },
        count_medium: Sourced {
            value: false,
            source: "instance",
        },
    };
    for (source, row) in levels {
        let Some(row) = row else {
            continue;
        };
        let source = *source;
        if let Some(value) = row.gate_mode.as_deref().and_then(GateMode::from_value) {
            resolved.gate_mode = Sourced { value, source };
        }
        if let Some(value) = row
            .on_scan_error
            .as_deref()
            .and_then(OnScanError::from_value)
        {
            resolved.on_scan_error = Sourced { value, source };
        }
        if let Some(value) = row.count_medium {
            resolved.count_medium = Sourced { value, source };
        }
    }
    resolved
}

fn err(e: coolify_db::DbError) -> String {
    e.to_string()
}

/// `levelsFor($resource, $mainServer)`.
async fn levels_for(
    db: &Db,
    resource: &ScanOwner,
) -> Result<Vec<(&'static str, Option<ImageScanPolicyRow>)>, String> {
    let mut levels = vec![(
        "instance",
        image_scans::instance_policy(db).await.map_err(err)?,
    )];
    if let Some(team_id) = resource.team_id {
        levels.push((
            "team",
            image_scans::scope_policy(db, TEAM_MORPH, team_id)
                .await
                .map_err(err)?,
        ));
    }
    if let Some(server_id) = resource.main_server_id {
        levels.push((
            "server",
            image_scans::scope_policy(db, SERVER_MORPH, server_id)
                .await
                .map_err(err)?,
        ));
    }
    if resource.is_application() {
        levels.push((
            "app",
            image_scans::scope_policy(db, APPLICATION_MORPH, resource.id)
                .await
                .map_err(err)?,
        ));
    }
    Ok(levels)
}

/// `ImageScanPolicyResolver::for($resource, $server)`. Rules always resolve through the main
/// destination server (spec 3.1). `scan_server_id` only decides where the image is scanned and
/// whether scanning is on there (the build server for a gated build); `None` is the main server.
pub async fn resolve(
    db: &Db,
    resource: &ScanOwner,
    scan_server_id: Option<i64>,
) -> Result<ResolvedPolicy, String> {
    let scan_server_id = scan_server_id.or(resource.main_server_id);
    let sources = resolve_levels(&levels_for(db, resource).await?);
    Ok(ResolvedPolicy {
        gate_mode: if resource.is_application() {
            sources.gate_mode.value
        } else {
            GateMode::Off
        },
        on_scan_error: sources.on_scan_error.value,
        scan_enabled: off_state(db, resource, scan_server_id).await?.is_none(),
        sources,
        scan_server_id,
        count_medium: sources.count_medium.value,
    })
}

/// `ImageScanPolicyResolver::countMediumFor($resource)`: the "Count medium problems" rule of a
/// resource. Reads only the rule rows, not the scan settings.
pub async fn count_medium_for(db: &Db, resource: &ScanOwner) -> Result<bool, String> {
    Ok(resolve_levels(&levels_for(db, resource).await?)
        .count_medium
        .value)
}

/// `ImageScanPolicyResolver::countMediumForMany($teamId, $resources)`: the rule for many
/// resources of one team with a fixed number of queries, keyed by `ResourceScans::key()`.
pub async fn count_medium_for_many(
    db: &Db,
    team_id: i64,
    resources: &[ScanOwner],
) -> Result<HashMap<String, bool>, String> {
    if resources.is_empty() {
        return Ok(HashMap::new());
    }
    let mut scopes: Vec<(String, i64)> = vec![(TEAM_MORPH.to_owned(), team_id)];
    for resource in resources {
        if let Some(server_id) = resource.main_server_id {
            scopes.push((SERVER_MORPH.to_owned(), server_id));
        }
        if resource.is_application() {
            scopes.push((APPLICATION_MORPH.to_owned(), resource.id));
        }
    }
    scopes.dedup();
    let rows = image_scans::scope_policies(db, &scopes)
        .await
        .map_err(err)?;
    let row = |scope_type: &str, scope_id: i64| {
        rows.iter()
            .find(|r| r.scope_type.as_deref() == Some(scope_type) && r.scope_id == Some(scope_id))
            .cloned()
    };
    let instance = image_scans::instance_policy(db).await.map_err(err)?;
    let team = row(TEAM_MORPH, team_id);
    Ok(resources
        .iter()
        .map(|resource| {
            let mut levels = vec![("instance", instance.clone()), ("team", team.clone())];
            if let Some(server_id) = resource.main_server_id {
                levels.push(("server", row(SERVER_MORPH, server_id)));
            }
            if resource.is_application() {
                levels.push(("app", row(APPLICATION_MORPH, resource.id)));
            }
            (resource.key(), resolve_levels(&levels).count_medium.value)
        })
        .collect())
}

/// The level a rule row is edited for (`forScope()`, `inheritedFor()`).
#[derive(Debug, Clone, Copy)]
pub enum PolicyScope<'a> {
    Team(i64),
    Server { id: i64, team_id: i64 },
    Application(&'a ScanOwner),
}

impl PolicyScope<'_> {
    /// `(scope_type, scope_id)` of the rule row.
    pub fn key(&self) -> (&'static str, i64) {
        match self {
            PolicyScope::Team(id) => (TEAM_MORPH, *id),
            PolicyScope::Server { id, .. } => (SERVER_MORPH, *id),
            PolicyScope::Application(app) => (APPLICATION_MORPH, app.id),
        }
    }
}

/// `ImageScanPolicyResolver::forScope($scope)`: the row to edit for a level: the instance row for
/// `None`, else the existing row. `None` when there is no row yet (a new unsaved row upstream).
pub async fn for_scope(
    db: &Db,
    scope: Option<PolicyScope<'_>>,
) -> Result<Option<ImageScanPolicyRow>, String> {
    match scope {
        None => image_scans::instance_policy(db).await.map_err(err),
        Some(scope) => {
            let (scope_type, scope_id) = scope.key();
            image_scans::scope_policy(db, scope_type, scope_id)
                .await
                .map_err(err)
        }
    }
}

/// `ImageScanPolicyResolver::inheritedFor($scope)`: the values a level inherits from the levels
/// above it, each with the level it comes from.
pub async fn inherited_for(db: &Db, scope: PolicyScope<'_>) -> Result<ResolvedLevels, String> {
    let mut levels = vec![(
        "instance",
        image_scans::instance_policy(db).await.map_err(err)?,
    )];
    // A team has no team above it.
    let team_id = match scope {
        PolicyScope::Team(_) => None,
        PolicyScope::Server { team_id, .. } => Some(team_id),
        PolicyScope::Application(app) => app.team_id,
    };
    if let Some(team_id) = team_id {
        levels.push((
            "team",
            image_scans::scope_policy(db, TEAM_MORPH, team_id)
                .await
                .map_err(err)?,
        ));
    }
    if let PolicyScope::Application(app) = scope
        && let Some(server_id) = app.main_server_id
    {
        levels.push((
            "server",
            image_scans::scope_policy(db, SERVER_MORPH, server_id)
                .await
                .map_err(err)?,
        ));
    }
    Ok(resolve_levels(&levels))
}

/// `ImageScanPolicyResolver::offState($resource, $scanServer)`: why the resource is not scanned
/// now: `server` (scanning is off for its server), `application` (the app turned its scan off),
/// or `None` when it is scanned. Every surface that shows results uses this rule.
/// `scan_server_id` `None` is the main server of the resource.
pub async fn off_state(
    db: &Db,
    resource: &ScanOwner,
    scan_server_id: Option<i64>,
) -> Result<Option<&'static str>, String> {
    let server_on = match scan_server_id.or(resource.main_server_id) {
        Some(server_id) => image_scans::server_scan_settings(db, server_id)
            .await
            .map_err(err)?
            .is_some_and(|s| s.is_image_scan_enabled),
        None => false,
    };
    Ok(off_state_of(resource, server_on))
}

/// [`off_state`] when the caller knows whether scanning is on for the scan server.
pub fn off_state_of(resource: &ScanOwner, server_scans: bool) -> Option<&'static str> {
    if !server_scans {
        Some("server")
    } else if resource.is_application() && !resource.is_image_scan_enabled() {
        Some("application")
    } else {
        None
    }
}

/// `ImageScanPolicyResolver::isAllowedFor($resource)`: an app did not turn its scan off. Reads no
/// rules and no server, so a caller can stop before it resolves them.
pub fn is_allowed_for(resource: &ScanOwner) -> bool {
    !resource.is_application() || resource.is_image_scan_enabled()
}

/// `ImageScanPolicyResolver::isScanOn($resource)`.
pub async fn is_scan_on(db: &Db, resource: &ScanOwner) -> Result<bool, String> {
    Ok(off_state(db, resource, None).await?.is_none())
}

#[cfg(test)]
mod tests {
    use coolify_db::records::Record;
    use serde_json::json;

    use super::*;

    fn row(
        gate: Option<&str>,
        on_error: Option<&str>,
        medium: Option<bool>,
    ) -> Option<ImageScanPolicyRow> {
        Some(ImageScanPolicyRow {
            id: 1,
            uuid: "p".to_owned(),
            scope_type: None,
            scope_id: None,
            gate_mode: gate.map(str::to_owned),
            on_scan_error: on_error.map(str::to_owned),
            count_medium: medium,
            created_at: None,
            updated_at: None,
        })
    }

    fn owner(morph: &'static str, settings: Option<Record>) -> ScanOwner {
        ScanOwner {
            morph,
            id: 5,
            uuid: "u".to_owned(),
            team_id: Some(1),
            main_server_id: Some(2),
            service_id: None,
            record: Record::new(),
            settings,
        }
    }

    #[test]
    fn defaults_without_rows() {
        let resolved = resolve_levels(&[("instance", None), ("team", None)]);
        assert_eq!(
            resolved.gate_mode,
            Sourced {
                value: GateMode::Off,
                source: "instance"
            }
        );
        assert_eq!(
            resolved.on_scan_error,
            Sourced {
                value: OnScanError::Deploy,
                source: "instance"
            }
        );
        assert_eq!(
            resolved.count_medium,
            Sourced {
                value: false,
                source: "instance"
            }
        );
    }

    #[test]
    fn a_lower_level_wins_and_null_inherits() {
        let resolved = resolve_levels(&[
            ("instance", row(Some("on"), Some("deploy"), Some(false))),
            ("team", row(None, Some("block"), None)),
            ("server", row(Some("strict"), None, None)),
            ("app", row(None, None, Some(true))),
        ]);
        assert_eq!(
            resolved.gate_mode,
            Sourced {
                value: GateMode::Strict,
                source: "server"
            }
        );
        assert_eq!(
            resolved.on_scan_error,
            Sourced {
                value: OnScanError::Block,
                source: "team"
            }
        );
        assert_eq!(
            resolved.count_medium,
            Sourced {
                value: true,
                source: "app"
            }
        );
    }

    #[test]
    fn an_app_can_turn_a_rule_off_again() {
        let resolved = resolve_levels(&[
            ("instance", row(Some("strict"), Some("block"), Some(true))),
            ("app", row(Some("off"), Some("deploy"), Some(false))),
        ]);
        assert_eq!(
            resolved.gate_mode,
            Sourced {
                value: GateMode::Off,
                source: "app"
            }
        );
        assert_eq!(resolved.on_scan_error.value, OnScanError::Deploy);
        assert!(!resolved.count_medium.value);
    }

    #[test]
    fn enum_values_and_labels() {
        assert_eq!(GateMode::from_value("strict"), Some(GateMode::Strict));
        assert_eq!(GateMode::from_value("nope"), None);
        assert_eq!(GateMode::On.label(), "On");
        assert_eq!(GateMode::Off.description(), None);
        assert_eq!(OnScanError::from_value("block"), Some(OnScanError::Block));
        assert_eq!(OnScanError::Deploy.label(), "Deploy anyway");
        assert_eq!(OnScanError::Block.label(), "Block the deploy");
    }

    #[test]
    fn off_state_names_the_level_that_turned_scanning_off() {
        let settings = |on: bool| json!({"is_image_scan_enabled": on}).as_object().cloned();
        let app = owner(APPLICATION_MORPH, settings(true));
        assert_eq!(off_state_of(&app, true), None);
        assert_eq!(off_state_of(&app, false), Some("server"));
        let off = owner(APPLICATION_MORPH, settings(false));
        assert_eq!(off_state_of(&off, true), Some("application"));
        assert_eq!(off_state_of(&off, false), Some("server"));
        assert!(!is_allowed_for(&off));
        // An application without settings is not scanned.
        assert_eq!(
            off_state_of(&owner(APPLICATION_MORPH, None), true),
            Some("application")
        );
        // Other resources have no switch of their own.
        let database = owner(r"App\Models\StandalonePostgresql", None);
        assert_eq!(off_state_of(&database, true), None);
        assert!(is_allowed_for(&database));
    }
}
