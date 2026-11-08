//! Team roles and API token abilities.

use std::fmt;

/// `team_user.role` (`app/Enums/Role.php`): member < admin < owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Member,
    Admin,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member" => Some(Self::Member),
            "admin" => Some(Self::Admin),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }

    /// `User::isAdminOfTeam`: admin or owner.
    pub fn is_admin(self) -> bool {
        self >= Self::Admin
    }
}

/// Token abilities accepted by upstream (`app/Livewire/Security/ApiTokens.php:123`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ability {
    Read,
    ReadSensitive,
    Write,
    WriteSensitive,
    Deploy,
    /// Its own ability since upstream 4.6.0 (`add_access_grants_to_team_members_and_tokens`).
    Delete,
    Root,
}

impl Ability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadSensitive => "read:sensitive",
            Self::Write => "write",
            Self::WriteSensitive => "write:sensitive",
            Self::Deploy => "deploy",
            Self::Delete => "delete",
            Self::Root => "root",
        }
    }

    /// Abilities a team member (non-admin) may not hold
    /// (`EnsureTokenBelongsToCurrentTeamMember`, `ApiAbility::MEMBER_DISALLOWED_ABILITIES`).
    pub const ELEVATED: [Ability; 5] = [
        Ability::Root,
        Ability::Write,
        Ability::WriteSensitive,
        Ability::Deploy,
        Ability::ReadSensitive,
    ];
}

impl fmt::Display for Ability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The ability list stored on a token (JSON array in `abilities`).
///
/// Values are kept as strings so unknown abilities written by newer upstream
/// versions are preserved and simply never match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Abilities(Vec<String>);

impl Abilities {
    pub fn new(values: Vec<String>) -> Self {
        Self(values)
    }

    /// Parses the `abilities` column. Sanctum stores a JSON array; NULL means none.
    pub fn from_column(raw: Option<&str>) -> Self {
        Self(
            raw.and_then(|r| serde_json::from_str::<Vec<String>>(r).ok())
                .unwrap_or_default(),
        )
    }

    /// Sanctum `PersonalAccessToken::can`: `*` or an exact match.
    pub fn can(&self, ability: Ability) -> bool {
        self.0.iter().any(|a| a == "*" || a == ability.as_str())
    }

    /// Elevated abilities held, in upstream's reporting order.
    pub fn elevated(&self) -> Vec<Ability> {
        Ability::ELEVATED
            .into_iter()
            .filter(|a| self.can(*a))
            .collect()
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// Outcome of an `api.ability:<list>` check (`ApiAbility::handle`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbilityCheck {
    Allowed,
    Missing(Vec<Ability>),
}

/// `root` passes everything; otherwise any one of `required` must be held.
pub fn check_any(abilities: &Abilities, required: &[Ability]) -> AbilityCheck {
    if abilities.can(Ability::Root) || required.iter().any(|a| abilities.can(*a)) {
        AbilityCheck::Allowed
    } else {
        AbilityCheck::Missing(required.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ability_column() {
        let a = Abilities::from_column(Some(r#"["read","write"]"#));
        assert!(a.can(Ability::Read) && a.can(Ability::Write));
        assert!(!a.can(Ability::Deploy));
        assert_eq!(Abilities::from_column(None), Abilities::default());
        assert_eq!(Abilities::from_column(Some("[]")), Abilities::default());
        assert!(Abilities::from_column(Some(r#"["*"]"#)).can(Ability::Deploy));
        assert!(Abilities::from_column(Some(r#"["read:sensitive"]"#)).can(Ability::ReadSensitive));
    }

    #[test]
    fn ability_semantics_match_upstream() {
        let write = Abilities::from_column(Some(r#"["write"]"#));
        // A write-only token cannot call read routes; only root is a superset.
        assert_eq!(
            check_any(&write, &[Ability::Read]),
            AbilityCheck::Missing(vec![Ability::Read])
        );
        let root = Abilities::from_column(Some(r#"["root"]"#));
        assert_eq!(check_any(&root, &[Ability::Deploy]), AbilityCheck::Allowed);
        // `api.ability:read,write` = either.
        assert_eq!(
            check_any(&write, &[Ability::Read, Ability::Write]),
            AbilityCheck::Allowed
        );
    }

    #[test]
    fn elevated_order() {
        let a = Abilities::from_column(Some(r#"["deploy","write","read"]"#));
        assert_eq!(a.elevated(), vec![Ability::Write, Ability::Deploy]);
    }

    #[test]
    fn roles() {
        assert!(Role::parse("owner").unwrap().is_admin());
        assert!(Role::parse("admin").unwrap().is_admin());
        assert!(!Role::parse("member").unwrap().is_admin());
        assert_eq!(Role::parse("superuser"), None);
    }
}
