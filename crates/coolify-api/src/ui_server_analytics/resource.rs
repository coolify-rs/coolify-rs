//! `Services\TrafficResource` with `Analytics::teamResources` and `Analytics::appMeta`.

use coolify_db::Db;
use coolify_db::traffic::TrafficResourceRow;
use coolify_jobs::compose_parser::labels::traefik_safe_service_name_segment;
use coolify_jobs::compose_parser::php::parse_url;
use serde_json::Value;
use std::collections::HashMap;

use super::client::{candidate_owner_uuids, key_belongs_to};

/// Normal previews use `{uuid}-pr-{id}`, compose previews `{uuid}-{id}` and `{uuid}-{id}-{service}`.
fn preview_suffix(suffix: &str) -> Option<(i64, Option<&str>)> {
    let rest = suffix.strip_prefix("pr-").unwrap_or(suffix);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id = rest.get(..digits)?.parse().ok()?;
    match &rest[digits..] {
        "" => Some((id, None)),
        tail => Some((id, Some(tail.strip_prefix('-').filter(|s| !s.is_empty())?))),
    }
}

/// `firstHost`.
pub fn first_host(domains: Option<&str>) -> Option<String> {
    let first = domains.unwrap_or_default().split(',').next()?.trim();
    if first.is_empty() {
        return None;
    }
    parse_url(first)?.host.filter(|h| !h.is_empty() && h != "0")
}

/// `composeDomains`: compose service name and its comma-separated domains.
fn compose_domains(json: Option<&str>) -> Vec<(String, String)> {
    let text = json.filter(|j| !j.is_empty() && *j != "0").unwrap_or("[]");
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .filter_map(|(name, config)| {
                let domain = config.get("domain")?.as_str()?;
                (!domain.trim().is_empty()).then(|| (name, domain.to_string()))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `matchesServiceName`.
fn matches_service_name(suffix: &str, service: &str) -> bool {
    !service.is_empty()
        && (suffix == service || suffix == traefik_safe_service_name_segment(service))
}

/// `composeDomainFor`.
fn compose_domain_for(json: Option<&str>, suffix: &str) -> Option<String> {
    compose_domains(json)
        .into_iter()
        .find(|(name, _)| matches_service_name(suffix, name))
        .and_then(|(_, domain)| first_host(Some(&domain)))
}

pub struct TrafficResource(pub TrafficResourceRow);

impl TrafficResource {
    /// `projectName`.
    pub fn project_name(&self) -> String {
        self.0
            .project_name
            .clone()
            .filter(|n| !n.is_empty() && n != "0")
            .unwrap_or_else(|| "Ungrouped".to_string())
    }

    /// `analyticsLink`.
    pub fn analytics_link(&self) -> Option<String> {
        let project = self.0.project_uuid.as_deref()?;
        let environment = self.0.environment_uuid.as_deref()?;
        let kind = if self.0.is_service {
            "service"
        } else {
            "application"
        };
        Some(format!(
            "/project/{project}/environment/{environment}/{kind}/{}/analytics",
            self.0.uuid
        ))
    }

    /// `primaryDomain`.
    pub fn primary_domain(&self) -> Option<String> {
        if self.0.is_service {
            return self
                .0
                .service_applications
                .iter()
                .find_map(|(_, fqdn)| first_host(fqdn.as_deref()));
        }
        first_host(self.0.fqdn.as_deref()).or_else(|| {
            compose_domains(self.0.docker_compose_domains.as_deref())
                .iter()
                .find_map(|(_, domain)| first_host(Some(domain)))
        })
    }

    /// `domainForKey`.
    async fn domain_for_key(&self, db: &Db, key: &str) -> Option<String> {
        let uuid = self.0.uuid.as_str();
        if !key_belongs_to(key, uuid) || key == uuid {
            return self.primary_domain();
        }
        let suffix = &key[uuid.len() + 1..];
        if self.0.is_service {
            return self
                .0
                .service_applications
                .iter()
                .find(|(name, _)| matches_service_name(suffix, name))
                .and_then(|(_, fqdn)| first_host(fqdn.as_deref()))
                .or_else(|| self.primary_domain());
        }
        if let Some((pull_request, service)) = preview_suffix(suffix)
            && let Ok(Some((fqdn, domains))) =
                coolify_db::traffic::preview_domains(db, self.0.id, pull_request).await
        {
            let host = match service {
                Some(service) => compose_domain_for(domains.as_deref(), service),
                None => first_host(fqdn.as_deref()),
            };
            if host.is_some() {
                return host;
            }
        }
        compose_domain_for(self.0.docker_compose_domains.as_deref(), suffix)
            .or_else(|| self.primary_domain())
    }
}

/// `appMeta`.
#[derive(Debug, Clone)]
pub struct AppMeta {
    pub uuid: String,
    pub name: String,
    pub domain: Option<String>,
    pub row_domain: Option<String>,
    pub link: Option<String>,
    pub is_service: bool,
}

/// Every Application and Service of the team: the only source used to name Sentinel keys,
/// so a key never resolves to another team's resource.
pub struct Resources {
    db: Db,
    pub items: Vec<TrafficResource>,
    by_uuid: HashMap<String, usize>,
    meta: HashMap<String, AppMeta>,
}

impl Resources {
    pub fn new(db: Db, rows: Vec<TrafficResourceRow>) -> Self {
        let items: Vec<TrafficResource> = rows.into_iter().map(TrafficResource).collect();
        let by_uuid = items
            .iter()
            .enumerate()
            .map(|(i, r)| (r.0.uuid.clone(), i))
            .collect();
        Self {
            db,
            items,
            by_uuid,
            meta: HashMap::new(),
        }
    }

    pub fn get(&self, uuid: &str) -> Option<&TrafficResource> {
        self.by_uuid.get(uuid).map(|i| &self.items[*i])
    }

    pub async fn app_meta(&mut self, key: &str) -> AppMeta {
        if let Some(meta) = self.meta.get(key) {
            return meta.clone();
        }
        let owner = candidate_owner_uuids(key)
            .into_iter()
            .find_map(|candidate| self.get(candidate));
        let meta = match owner {
            None => AppMeta {
                uuid: key.to_string(),
                name: key.to_string(),
                domain: None,
                row_domain: None,
                link: None,
                is_service: false,
            },
            Some(owner) => AppMeta {
                uuid: owner.0.uuid.clone(),
                name: owner.0.name.clone(),
                domain: owner.domain_for_key(&self.db, key).await,
                row_domain: owner.primary_domain(),
                link: owner.analytics_link(),
                is_service: owner.0.is_service,
            },
        };
        self.meta.insert(key.to_string(), meta.clone());
        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn application(fqdn: Option<&str>, compose: Option<&str>) -> TrafficResource {
        TrafficResource(TrafficResourceRow {
            id: 1,
            uuid: "app".to_string(),
            name: "App".to_string(),
            is_service: false,
            server_uuid: None,
            environment_uuid: Some("env".to_string()),
            project_uuid: Some("proj".to_string()),
            project_name: None,
            fqdn: fqdn.map(str::to_string),
            docker_compose_domains: compose.map(str::to_string),
            service_applications: vec![],
        })
    }

    #[test]
    fn preview_suffixes() {
        assert_eq!(preview_suffix("pr-12"), Some((12, None)));
        assert_eq!(preview_suffix("12-web"), Some((12, Some("web"))));
        assert_eq!(preview_suffix("web"), None);
        assert_eq!(preview_suffix("12-"), None);
        assert_eq!(preview_suffix("12x"), None);
    }

    #[test]
    fn first_host_reads_the_first_domain() {
        assert_eq!(
            first_host(Some(" https://a.example:8080/x , http://b.example ")),
            Some("a.example".to_string())
        );
        assert_eq!(first_host(Some("")), None);
        assert_eq!(first_host(None), None);
    }

    #[test]
    fn primary_domain_falls_back_to_compose() {
        let a = application(None, Some(r#"{"web":{"domain":"https://c.example"}}"#));
        assert_eq!(a.primary_domain(), Some("c.example".to_string()));
        assert_eq!(
            compose_domain_for(a.0.docker_compose_domains.as_deref(), "web"),
            Some("c.example".to_string())
        );
        assert_eq!(a.project_name(), "Ungrouped");
    }

    #[test]
    fn analytics_links_match_upstream_routes() {
        assert_eq!(
            application(None, None).analytics_link().as_deref(),
            Some("/project/proj/environment/env/application/app/analytics")
        );
    }
}
