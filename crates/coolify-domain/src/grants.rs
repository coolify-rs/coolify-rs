//! Scoped access grants of team members and API tokens (upstream 4.6.0).
//!
//! `app/Enums/AccessLevel.php`, `app/Enums/AccessScope.php`, `app/Services/Access/Grant.php` and
//! `app/Services/Access/AccessGrants.php`. The resolver that turns grant UUIDs into ids is in
//! `coolify_db::access`; this module has no I/O.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

/// `AccessLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessLevel {
    Read,
    ReadSensitive,
    Deploy,
    Write,
    Delete,
    Root,
}

impl AccessLevel {
    /// `AccessLevel::cases()`.
    pub const ALL: [AccessLevel; 6] = [
        Self::Read,
        Self::ReadSensitive,
        Self::Deploy,
        Self::Write,
        Self::Delete,
        Self::Root,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadSensitive => "read:sensitive",
            Self::Deploy => "deploy",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Root => "root",
        }
    }

    /// `AccessLevel::tryFrom`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// `includes()`: the levels a grant of this level also gives. Write includes ReadSensitive
    /// because a user who can change a resource can read its secrets back.
    pub fn includes(self) -> &'static [AccessLevel] {
        match self {
            Self::Root => &Self::ALL,
            Self::Write => &[Self::Write, Self::Deploy, Self::ReadSensitive, Self::Read],
            Self::Delete => &[Self::Delete, Self::Read],
            Self::Deploy => &[Self::Deploy, Self::Read],
            Self::ReadSensitive => &[Self::ReadSensitive, Self::Read],
            Self::Read => &[Self::Read],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::ReadSensitive => "Read secrets",
            Self::Deploy => "Deploy",
            Self::Write => "Write",
            Self::Delete => "Delete",
            Self::Root => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Read => "See resources, logs, and settings. Secrets stay hidden.",
            Self::ReadSensitive => "Also see environment variables and other secrets.",
            Self::Deploy => "Deploy, start, stop, and restart.",
            Self::Write => "Change settings and create resources.",
            Self::Delete => "Delete resources.",
            Self::Root => "Every level, with no limits.",
        }
    }

    /// `parseMany`: unknown values are ignored, duplicates dropped, order kept.
    pub fn parse_many<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<AccessLevel> {
        let mut levels = Vec::new();
        for v in values {
            if let Some(l) = Self::parse(v)
                && !levels.contains(&l)
            {
                levels.push(l);
            }
        }
        levels
    }
}

/// `AccessScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessScope {
    Team,
    Project,
    Environment,
    Resource,
}

impl AccessScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Project => "project",
            Self::Environment => "environment",
            Self::Resource => "resource",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "team" => Some(Self::Team),
            "project" => Some(Self::Project),
            "environment" => Some(Self::Environment),
            "resource" => Some(Self::Resource),
            _ => None,
        }
    }
}

/// `Grant`: a scope (team, project, environment or one resource, by UUID) and its levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub scope: AccessScope,
    pub uuid: Option<String>,
    pub levels: Vec<AccessLevel>,
}

/// PHP `(string)` of a decoded JSON scalar.
fn php_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(true) => Some("1".into()),
        Value::Bool(false) | Value::Null => Some(String::new()),
        _ => None,
    }
}

impl Grant {
    pub fn team(levels: Vec<AccessLevel>) -> Self {
        Self {
            scope: AccessScope::Team,
            uuid: None,
            levels,
        }
    }

    /// `Grant::fromArray`.
    pub fn from_value(data: &Value) -> Option<Self> {
        let obj = data.as_object()?;
        let scope = obj
            .get("scope")
            .and_then(php_string)
            .and_then(|s| AccessScope::parse(&s))?;
        let uuid = if scope == AccessScope::Team {
            None
        } else {
            match obj.get("uuid") {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                _ => return None,
            }
        };
        let levels = match obj.get("levels") {
            Some(Value::Array(items)) => {
                let strings: Vec<String> = items.iter().filter_map(php_string).collect();
                AccessLevel::parse_many(strings.iter().map(String::as_str))
            }
            Some(Value::Object(map)) => {
                let strings: Vec<String> = map.values().filter_map(php_string).collect();
                AccessLevel::parse_many(strings.iter().map(String::as_str))
            }
            _ => Vec::new(),
        };
        Some(Self {
            scope,
            uuid,
            levels,
        })
    }

    /// `Grant::listFrom`: a stored JSON column (or decoded value) to grants; invalid items dropped.
    pub fn list_from_str(stored: &str) -> Vec<Self> {
        match serde_json::from_str::<Value>(stored) {
            Ok(v) => Self::list_from(&v),
            Err(_) => Vec::new(),
        }
    }

    pub fn list_from(stored: &Value) -> Vec<Self> {
        // A JSON string column that holds encoded JSON (double encoding) is decoded once more.
        if let Value::String(s) = stored {
            return Self::list_from_str(s);
        }
        let items: Vec<&Value> = match stored {
            Value::Array(a) => a.iter().collect(),
            Value::Object(o) => o.values().collect(),
            _ => return Vec::new(),
        };
        items.into_iter().filter_map(Self::from_value).collect()
    }

    /// `toArray()`.
    pub fn to_value(&self) -> Value {
        json!({
            "scope": self.scope.as_str(),
            "uuid": self.uuid,
            "levels": self.levels.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
        })
    }

    /// `listToArray`, JSON-encoded like Laravel's `json` cast (`json_encode`, slashes escaped).
    pub fn list_to_json(grants: &[Grant]) -> String {
        let v = Value::Array(grants.iter().map(Grant::to_value).collect());
        crate::grants::php_json_encode(&v)
    }
}

/// `json_encode` with PHP's default escaping of `/`.
pub fn php_json_encode(v: &Value) -> String {
    serde_json::to_string(v)
        .unwrap_or_else(|_| "[]".into())
        .replace('/', "\\/")
}

/// Expanded levels of one or more grants.
pub type LevelSet = BTreeSet<AccessLevel>;

/// `AccessGrants::expand`.
pub fn expand(levels: &[AccessLevel]) -> LevelSet {
    levels
        .iter()
        .flat_map(|l| l.includes().iter().copied())
        .collect()
}

/// `AccessGrants`: the grants of one actor in one team, with UUIDs resolved to ids.
/// A grant on a parent applies to its children (team > project > environment > resource);
/// the levels of all matching grants add up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessGrants {
    pub team_levels: LevelSet,
    pub project_levels: BTreeMap<i64, LevelSet>,
    pub environment_levels: BTreeMap<i64, LevelSet>,
    pub resource_levels: BTreeMap<String, LevelSet>,
    /// Environment id => project id, for granted environments and parents of granted resources.
    pub environment_projects: BTreeMap<i64, i64>,
    /// Resource UUID => environment id.
    pub resource_environments: BTreeMap<String, i64>,
    /// Environments inside granted projects.
    pub project_environment_ids: Vec<i64>,
}

impl AccessGrants {
    pub fn unrestricted() -> Self {
        Self {
            team_levels: expand(&[AccessLevel::Root]),
            ..Default::default()
        }
    }

    /// True when a team-wide grant exists, so every project in the team is visible.
    pub fn covers_whole_team(&self) -> bool {
        !self.team_levels.is_empty()
    }

    /// True when the layer has grants below the team, so checks need the target's location.
    pub fn has_scoped_grants(&self) -> bool {
        !self.project_levels.is_empty()
            || !self.environment_levels.is_empty()
            || !self.resource_levels.is_empty()
    }

    pub fn allows(
        &self,
        level: AccessLevel,
        project_id: Option<i64>,
        environment_id: Option<i64>,
        resource_uuid: Option<&str>,
    ) -> bool {
        if self.team_levels.contains(&level) {
            return true;
        }
        if let Some(p) = project_id
            && self
                .project_levels
                .get(&p)
                .is_some_and(|s| s.contains(&level))
        {
            return true;
        }
        if let Some(e) = environment_id
            && self
                .environment_levels
                .get(&e)
                .is_some_and(|s| s.contains(&level))
        {
            return true;
        }
        resource_uuid.is_some_and(|u| {
            self.resource_levels
                .get(u)
                .is_some_and(|s| s.contains(&level))
        })
    }

    /// Projects the actor can open: granted projects and parents of granted environments and
    /// resources.
    pub fn visible_project_ids(&self) -> Vec<i64> {
        let mut out: Vec<i64> = Vec::new();
        for id in self
            .project_levels
            .keys()
            .chain(self.environment_projects.values())
        {
            if !out.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    /// Environments the actor can open, including parents of granted resources.
    pub fn visible_environment_ids(&self) -> Vec<i64> {
        let mut out = self.full_environment_ids();
        for id in self.resource_environments.values() {
            if !out.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    /// Environments where every resource is visible (granted directly or through the project).
    pub fn full_environment_ids(&self) -> Vec<i64> {
        let mut out: Vec<i64> = Vec::new();
        for id in self
            .environment_levels
            .keys()
            .chain(self.project_environment_ids.iter())
        {
            if !out.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    pub fn resource_uuids(&self) -> Vec<String> {
        self.resource_levels.keys().cloned().collect()
    }

    /// `VisibleThroughAccessGrants` for one project row.
    pub fn project_visible(&self, project_id: i64) -> bool {
        self.covers_whole_team()
            || self.project_levels.contains_key(&project_id)
            || self.environment_projects.values().any(|p| *p == project_id)
    }

    /// `VisibleThroughAccessGrants` for one environment row.
    pub fn environment_visible(&self, environment_id: i64) -> bool {
        self.covers_whole_team()
            || self.environment_levels.contains_key(&environment_id)
            || self.project_environment_ids.contains(&environment_id)
            || self
                .resource_environments
                .values()
                .any(|e| *e == environment_id)
    }

    /// `VisibleThroughAccessGrants` for one resource row.
    pub fn resource_visible(&self, environment_id: Option<i64>, uuid: &str) -> bool {
        self.covers_whole_team()
            || environment_id.is_some_and(|e| {
                self.environment_levels.contains_key(&e)
                    || self.project_environment_ids.contains(&e)
            })
            || self.resource_levels.contains_key(uuid)
    }
}

/// Where a target sits in the team: `AccessResolver::locate`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    pub project_id: Option<i64>,
    pub environment_id: Option<i64>,
    pub resource_uuid: Option<String>,
}

impl Target {
    pub fn project(project_id: i64) -> Self {
        Self {
            project_id: Some(project_id),
            ..Default::default()
        }
    }

    /// An environment; its project comes from the actor's environment map.
    pub fn environment(environment_id: i64) -> Self {
        Self {
            environment_id: Some(environment_id),
            ..Default::default()
        }
    }

    /// A resource in an environment (application, service, standalone database).
    pub fn resource(environment_id: Option<i64>, uuid: &str) -> Self {
        Self {
            environment_id,
            resource_uuid: Some(uuid.to_string()),
            ..Default::default()
        }
    }
}

/// The access of one actor in one team: the layers of `AccessResolver::layersFor` (member
/// grants unless owner/admin, then the token scopes when the token has any). Every layer must
/// allow an action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    pub layers: Vec<AccessGrants>,
    /// Environment id => project id of every environment in the team, loaded when a layer has
    /// grants below the team (`locate()` reads the environment's project).
    pub environment_projects: HashMap<i64, i64>,
}

impl Access {
    /// Owners and admins with an unscoped token or a session: no layers, no limits.
    pub fn unrestricted() -> Self {
        Self::default()
    }

    pub fn needs_locations(&self) -> bool {
        self.layers.iter().any(AccessGrants::has_scoped_grants)
    }

    fn locate(&self, target: &Target) -> Option<(Option<i64>, Option<i64>)> {
        if target.environment_id.is_none() && target.project_id.is_some() {
            return Some((target.project_id, None));
        }
        let env = target.environment_id?;
        if !self.needs_locations() {
            return Some((target.project_id, Some(env)));
        }
        let project = self.environment_projects.get(&env).copied()?;
        Some((Some(project), Some(env)))
    }

    /// `AccessResolver::allows` (`User::canAccess`).
    pub fn allows(&self, level: AccessLevel, target: &Target) -> bool {
        let Some((project, env)) = self.locate(target) else {
            return false;
        };
        self.layers
            .iter()
            .all(|layer| layer.allows(level, project, env, target.resource_uuid.as_deref()))
    }

    /// `AccessResolver::canOpen` for a project.
    pub fn can_open_project(&self, project_id: i64) -> bool {
        self.layers.iter().all(|l| l.project_visible(project_id))
    }

    /// `AccessResolver::canOpen` for an environment.
    pub fn can_open_environment(&self, environment_id: i64) -> bool {
        if self.needs_locations() && !self.environment_projects.contains_key(&environment_id) {
            return false;
        }
        self.layers
            .iter()
            .all(|l| l.environment_visible(environment_id))
    }

    /// `AccessResolver::coversWholeTeam`.
    pub fn covers_whole_team(&self) -> bool {
        self.layers.iter().all(AccessGrants::covers_whole_team)
    }

    /// `AccessResolver::allowsTeamWide`.
    pub fn allows_team_wide(&self, level: AccessLevel) -> bool {
        self.layers
            .iter()
            .all(|l| l.allows(level, None, None, None))
    }

    /// Row filters of `VisibleThroughAccessGrants`.
    pub fn project_visible(&self, project_id: i64) -> bool {
        self.can_open_project(project_id)
    }

    pub fn environment_visible(&self, environment_id: i64) -> bool {
        self.layers
            .iter()
            .all(|l| l.environment_visible(environment_id))
    }

    pub fn resource_visible(&self, environment_id: Option<i64>, uuid: &str) -> bool {
        self.layers
            .iter()
            .all(|l| l.resource_visible(environment_id, uuid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer_with(project: Option<(i64, &[AccessLevel])>) -> AccessGrants {
        let mut g = AccessGrants::default();
        if let Some((p, levels)) = project {
            g.project_levels.insert(p, expand(levels));
            g.project_environment_ids = vec![10, 11];
        }
        g
    }

    #[test]
    fn includes_matches_upstream() {
        assert_eq!(
            expand(&[AccessLevel::Write]),
            [
                AccessLevel::Read,
                AccessLevel::ReadSensitive,
                AccessLevel::Deploy,
                AccessLevel::Write
            ]
            .into_iter()
            .collect()
        );
        assert!(!expand(&[AccessLevel::Write]).contains(&AccessLevel::Delete));
        assert_eq!(expand(&[AccessLevel::Root]).len(), 6);
        assert_eq!(
            expand(&[AccessLevel::Delete]),
            [AccessLevel::Read, AccessLevel::Delete]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn parse_many_ignores_unknown_and_duplicates() {
        assert_eq!(
            AccessLevel::parse_many(["read", "write:sensitive", "*", "read", "delete"]),
            vec![AccessLevel::Read, AccessLevel::Delete]
        );
    }

    #[test]
    fn grant_from_value() {
        let g = Grant::list_from_str(
            r#"[{"scope":"team","uuid":"x","levels":["read"]},{"scope":"project","levels":["write"]},{"scope":"project","uuid":"p1","levels":["write","bogus"]},{"scope":"nope"},"x"]"#,
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], Grant::team(vec![AccessLevel::Read]));
        assert_eq!(g[1].uuid.as_deref(), Some("p1"));
        assert_eq!(g[1].levels, vec![AccessLevel::Write]);
        assert_eq!(
            Grant::list_to_json(&g),
            r#"[{"scope":"team","uuid":null,"levels":["read"]},{"scope":"project","uuid":"p1","levels":["write"]}]"#
        );
        assert!(Grant::list_from_str("null").is_empty());
    }

    #[test]
    fn layers_must_all_allow() {
        let member = layer_with(Some((1, &[AccessLevel::Write])));
        let mut access = Access {
            layers: vec![member],
            environment_projects: HashMap::from([(10, 1), (11, 1), (20, 2)]),
        };
        let in_project = Target::resource(Some(10), "app");
        let elsewhere = Target::resource(Some(20), "other");
        assert!(access.allows(AccessLevel::Write, &in_project));
        assert!(access.allows(AccessLevel::Deploy, &in_project));
        assert!(!access.allows(AccessLevel::Delete, &in_project));
        assert!(!access.allows(AccessLevel::Read, &elsewhere));
        assert!(!access.covers_whole_team());
        assert!(!access.allows_team_wide(AccessLevel::Read));
        assert!(access.resource_visible(Some(10), "app"));
        assert!(!access.resource_visible(Some(20), "other"));
        assert!(access.can_open_project(1) && !access.can_open_project(2));
        // A token layer with only read on the team narrows it further.
        let token = AccessGrants {
            team_levels: expand(&[AccessLevel::Read]),
            ..AccessGrants::default()
        };
        access.layers.push(token);
        assert!(access.allows(AccessLevel::Read, &in_project));
        assert!(!access.allows(AccessLevel::Write, &in_project));
        // An unknown environment cannot be located.
        assert!(!access.allows(AccessLevel::Read, &Target::resource(Some(99), "x")));
    }

    #[test]
    fn owners_and_default_members() {
        let owner = Access::unrestricted();
        assert!(owner.allows(AccessLevel::Delete, &Target::resource(Some(1), "a")));
        assert!(owner.covers_whole_team());
        let team_read = AccessGrants {
            team_levels: expand(&[AccessLevel::Read]),
            ..AccessGrants::default()
        };
        let member = Access {
            layers: vec![team_read],
            environment_projects: HashMap::new(),
        };
        assert!(member.covers_whole_team());
        assert!(member.allows(AccessLevel::Read, &Target::resource(Some(1), "a")));
        assert!(!member.allows(AccessLevel::Write, &Target::resource(Some(1), "a")));
        assert!(member.resource_visible(Some(1), "a"));
    }
}
