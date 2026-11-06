//! What the `resources` and `deploy` capabilities reach (ADR 0011).
//!
//! The REST API's team-scoped queries and serializers live in `coolify-api`, which depends on this crate, so
//! the host reaches them through these traits: `coolify-api` implements them and hands them to
//! [`Host::connect`](crate::Host::connect). The host decides the team from the call, never from what the
//! plugin passes, and checks the grant, the team's consent and the rate limit before calling them.

use std::future::Future;
use std::pin::Pin;

/// A boxed future, so the traits stay object safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A kind of resource, as named in the manifest (`RESOURCE_KINDS`) and the WIT `kind` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// Projects, and their environments.
    Projects,
    Servers,
    Applications,
    Services,
    Databases,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 5] = [
        Self::Projects,
        Self::Servers,
        Self::Applications,
        Self::Services,
        Self::Databases,
    ];

    /// The manifest name, which is also the API path segment.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Servers => "servers",
            Self::Applications => "applications",
            Self::Services => "services",
            Self::Databases => "databases",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One read, always for the team of the call the host is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceQuery {
    /// `GET /api/v1/<kind>`.
    All(ResourceKind),
    /// `GET /api/v1/<kind>/<uuid>`.
    One(ResourceKind, String),
    /// `GET /api/v1/projects/<uuid>/environments`.
    Environments(String),
}

/// Reads the team's resources as the REST API answers a token of the team with `read` and without
/// `read:sensitive`.
pub trait ResourceSource: Send + Sync + 'static {
    /// The JSON body the API would answer, `None` when the team has no such resource (a 404), or an error
    /// for the plugin.
    fn read<'a>(
        &'a self,
        team_id: i64,
        query: &'a ResourceQuery,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}

/// On whose authority a plugin deploys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A request to the plugin's routes: the caller's token, so a plugin route can never deploy what the
    /// caller could not deploy through the API.
    Caller {
        user_id: i64,
        abilities: Vec<String>,
    },
    /// An event, which has no caller: the team admin who granted `deploy` to the plugin, re-checked on
    /// every deployment.
    Grantor { user_id: i64 },
}

/// A deployment a plugin asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRequest {
    pub plugin: String,
    /// The team of the call.
    pub team_id: i64,
    pub actor: Actor,
    /// The application's uuid.
    pub uuid: String,
    pub force: bool,
}

/// Queues deployments through the same code path as `POST /api/v1/deploy`.
pub trait Deployer: Send + Sync + 'static {
    /// The API's JSON answer, or why the deployment was refused.
    fn deploy<'a>(&'a self, request: &'a DeployRequest) -> BoxFuture<'a, Result<String, String>>;
}
