//! `App\Services\EnvironmentCanvas`, `ProjectCanvas` and `ServerCanvas`: the canvas payloads.
//! Cards hold summary fields only, never environment variables, passwords, keys or connection URLs.

use std::collections::{HashMap, HashSet};

use coolify_db::canvas::{self as db, APPLICATION_MORPH, Resource, SERVICE_MORPH};
use coolify_db::records::Record;
use coolify_db::services::{SubKind, SubRow};
use coolify_jobs::services::status::container_status_of;
use coolify_jobs::services::templates;
use serde_json::{Map, Value, json};

use crate::response::ApiError;
use crate::routes::applications::status_getter;
use crate::state::AppState;

pub const EMPTY_ZONE: &str = "empty";

pub(crate) fn text<'a>(r: &'a Record, key: &str) -> &'a str {
    r.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn int(r: &Record, key: &str) -> i64 {
    match r.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn flag(r: Option<&Value>) -> bool {
    r.is_some_and(crate::serialize::php_truthy)
}

/// `[...$node, ...$other]`: a key that exists keeps its place.
pub(crate) fn spread(node: &mut Map<String, Value>, other: Value) {
    if let Value::Object(m) = other {
        for (k, v) in m {
            node.insert(k, v);
        }
    }
}

/// `route('project.resource.index', ...)`.
pub(crate) fn environment_page(base: &str, project: &str, environment: &str) -> String {
    format!("{base}/project/{project}/environment/{environment}")
}

/// A loaded set of top-level resources with the relations the canvas reads.
pub struct Loaded {
    pub resources: Vec<Resource>,
    pub environments: HashMap<i64, Record>,
    pub projects: HashMap<i64, Record>,
    pub servers: HashMap<i64, Record>,
    destinations: HashMap<(String, i64), i64>,
    additional: HashMap<i64, Vec<(i64, Option<String>)>>,
    service_applications: HashMap<i64, Vec<Record>>,
    service_databases: HashMap<i64, Vec<Record>>,
}

impl Loaded {
    pub fn environment(&self, r: &Resource) -> Option<&Record> {
        self.environments.get(&r.environment_id())
    }

    pub fn project(&self, r: &Resource) -> Option<&Record> {
        self.environment(r)
            .and_then(|e| self.projects.get(&int(e, "project_id")))
    }

    fn destination_server(&self, r: &Resource) -> Option<&Record> {
        let key = (
            text(&r.row, "destination_type").to_owned(),
            int(&r.row, "destination_id"),
        );
        self.destinations
            .get(&key)
            .and_then(|id| self.servers.get(id))
    }

    /// `$service->server` for a service, `$resource->destination?->server` otherwise.
    pub fn main_server(&self, r: &Resource) -> Option<&Record> {
        if r.morph == SERVICE_MORPH {
            self.servers.get(&int(&r.row, "server_id"))
        } else {
            self.destination_server(r)
        }
    }

    /// `$service->server ?? $service->destination?->server`, or the destination server.
    fn card_server(&self, r: &Resource) -> Option<&Record> {
        self.main_server(r).or_else(|| self.destination_server(r))
    }

    pub fn morph_key_uuid(&self) -> HashMap<(String, i64), String> {
        self.resources
            .iter()
            .map(|r| ((r.morph.to_owned(), r.id()), r.uuid().to_owned()))
            .collect()
    }
}

/// `ServerCanvas::loadResources` with the eager loads, for environments of known projects.
pub async fn load(state: &AppState, environments: &[Record]) -> Result<Loaded, ApiError> {
    let ids: Vec<i64> = environments.iter().map(|e| int(e, "id")).collect();
    let resources = db::resources(&state.db, &ids).await?;
    let project_ids: Vec<i64> = environments.iter().map(|e| int(e, "project_id")).collect();
    let projects = db::projects_by_id(&state.db, &project_ids).await?;
    let destinations = db::destination_servers(&state.db).await?;
    let app_ids: Vec<i64> = resources
        .iter()
        .filter(|r| r.morph == APPLICATION_MORPH)
        .map(Resource::id)
        .collect();
    let service_ids: Vec<i64> = resources
        .iter()
        .filter(|r| r.morph == SERVICE_MORPH)
        .map(Resource::id)
        .collect();
    let additional = db::additional_servers(&state.db, &app_ids).await?;
    let mut server_ids: Vec<i64> = additional.values().flatten().map(|(id, _)| *id).collect();
    for r in &resources {
        let key = (
            text(&r.row, "destination_type").to_owned(),
            int(&r.row, "destination_id"),
        );
        server_ids.extend(destinations.get(&key));
        if r.morph == SERVICE_MORPH {
            server_ids.push(int(&r.row, "server_id"));
        }
    }
    let servers = db::servers_by_id(&state.db, &server_ids).await?;
    Ok(Loaded {
        resources,
        environments: environments
            .iter()
            .map(|e| (int(e, "id"), e.clone()))
            .collect(),
        projects,
        servers,
        destinations,
        additional,
        service_applications: db::service_children(&state.db, "service_applications", &service_ids)
            .await?,
        service_databases: db::service_children(&state.db, "service_databases", &service_ids)
            .await?,
    })
}

/// `EnvironmentCanvas::filterPositions`: the given cards, with integer coordinates.
pub fn filter_positions(positions: Option<&Value>, uuids: &HashSet<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(Value::Object(m)) = positions {
        for (k, v) in m {
            if uuids.contains(k.as_str())
                && let Some(p) = zone_position(Some(v))
            {
                out.insert(k.clone(), p);
            }
        }
    }
    out
}

fn numeric(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

/// `EnvironmentCanvas::zonePosition`.
pub fn zone_position(saved: Option<&Value>) -> Option<Value> {
    let saved = saved?.as_object()?;
    Some(json!({"x": numeric(saved.get("x"))?, "y": numeric(saved.get("y"))?}))
}

/// An empty PHP array is `[]` in JSON, a filled one is an object.
pub fn php_map(m: Map<String, Value>) -> Value {
    if m.is_empty() {
        json!([])
    } else {
        Value::Object(m)
    }
}

/// `EnvironmentCanvas::zone`.
pub fn zone(server: &Record, can_read_sensitive: bool, base: &str) -> Map<String, Value> {
    let settings = server.get("settings").and_then(Value::as_object);
    let z = json!({
        "id": server.get("uuid"),
        "name": server.get("name"),
        "ip": if can_read_sensitive { server.get("ip").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "is_reachable": flag(settings.and_then(|s| s.get("is_reachable"))),
        "is_usable": flag(settings.and_then(|s| s.get("is_usable"))),
        "link": format!("{base}/server/{}", text(server, "uuid")),
    });
    match z {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `EnvironmentCanvas::emptyZone`.
pub fn empty_zone() -> Value {
    json!({"id": EMPTY_ZONE, "name": "Empty environments", "icon": "service"})
}

/// `EnvironmentCanvas::projectZones`: the zone with most of each project's cards.
pub fn project_zones(nodes: &[Map<String, Value>]) -> HashMap<String, String> {
    let mut counts: Vec<(String, Vec<(String, usize)>)> = Vec::new();
    for node in nodes {
        let Some(zone) = node
            .get("zone")
            .and_then(Value::as_str)
            .filter(|z| !z.is_empty())
        else {
            continue;
        };
        let project = node
            .get("project")
            .and_then(|p| p.get("uuid"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let at = counts
            .iter()
            .position(|(p, _)| p == project)
            .unwrap_or_else(|| {
                counts.push((project.to_owned(), Vec::new()));
                counts.len() - 1
            });
        let zones = &mut counts[at].1;
        match zones.iter_mut().find(|(z, _)| z == zone) {
            Some((_, n)) => *n += 1,
            None => zones.push((zone.to_owned(), 1)),
        }
    }
    counts
        .into_iter()
        .filter_map(|(project, zones)| {
            let mut best: Option<(String, usize)> = None;
            for (zone, count) in zones {
                if best.as_ref().is_none_or(|(_, c)| count > *c) {
                    best = Some((zone, count));
                }
            }
            best.map(|(zone, _)| (project, zone))
        })
        .collect()
}

/// `EnvironmentCanvas::emptyNode`.
pub fn empty_node(
    project: &Record,
    environment: Option<&Record>,
    zone: &str,
    context: &str,
    can_add: bool,
    base: &str,
) -> Map<String, Value> {
    let project_uuid = text(project, "uuid");
    let page = match environment {
        Some(e) => environment_page(base, project_uuid, text(e, "uuid")),
        None => format!("{base}/project/{project_uuid}"),
    };
    let add = can_add && environment.is_some();
    let id = environment.map_or(project_uuid, |e| text(e, "uuid"));
    let node = json!({
        "id": format!("empty:{id}"),
        "type": "empty",
        "name": if environment.is_some() { "No resources yet" } else { "No environments yet" },
        "subtitle": if add {
            "Click to add a resource".to_owned()
        } else {
            format!("Open to see this {}", if environment.is_some() { "environment" } else { "project" })
        },
        "status": null,
        "zone": zone,
        "extra_zones": [],
        "urls": [],
        "children": [],
        "link": if add {
            format!(
                "{base}/canvas?project={project_uuid}&environment={}",
                environment.map_or("", |e| text(e, "uuid"))
            )
        } else {
            page.clone()
        },
        "context": context,
        "context_link": page,
        "project": {"uuid": project.get("uuid"), "name": project.get("name")},
        "environment": environment.map(|e| json!({"uuid": e.get("uuid"), "name": e.get("name")})),
    });
    match node {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `splitUrls`.
fn split_urls(fqdn: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in fqdn.unwrap_or_default().split(',') {
        let url = url.trim();
        if !url.is_empty() && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
        }
    }
    out
}

/// `subtitle`.
fn subtitle(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && *p != "0")
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `resource_logo_url`, `resource_logo`: logos of the "New resource" page. The upstream URL has a
/// `?v=` of the file's modification time; the files are built into the frontend here.
fn named_logo(base: &str, name: &str, dark: bool) -> Value {
    json!({
        "src": format!("{base}/svgs/resources/{name}.svg"),
        "dark": if dark { Value::String(format!("{base}/svgs/resources/{name}-dark.svg")) } else { Value::Null },
        "fallback": null,
        "padded": false,
        "favicon": null,
    })
}

/// `resource_favicon_url`.
fn favicon(fqdn: Option<&str>) -> Option<String> {
    let domain = fqdn?.split(',').map(str::trim).find(|u| !u.is_empty())?;
    let (scheme, rest) = domain.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    if host.is_empty() || host.contains('*') {
        return None;
    }
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{scheme}://{host}{port}/favicon.ico"))
}

fn application_logo(r: &Resource, base: &str) -> Value {
    let pack = r.str("build_pack").unwrap_or_default();
    let source = r.str("source_type").unwrap_or_default();
    let mut logo = match pack {
        "dockerimage" => named_logo(base, "docker-image", false),
        "dockerfile" => named_logo(base, "dockerfile", false),
        "dockercompose" => named_logo(base, "docker-compose", false),
        _ if source == "App\\Models\\GithubApp" => named_logo(base, "github-app", true),
        _ if source == "App\\Models\\GitlabApp" => named_logo(base, "gitlab-app", false),
        _ => named_logo(base, "public-repo", false),
    };
    let basic_auth = flag(r.row.get("is_http_basic_auth_enabled"));
    if let Some(m) = logo.as_object_mut() {
        let fav = if basic_auth {
            None
        } else {
            favicon(r.str("fqdn"))
        };
        m.insert("favicon".into(), json!(fav));
    }
    logo
}

fn service_logo(r: &Resource, base: &str) -> Value {
    let name = r.name();
    let kind = r
        .str("service_type")
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| name.rsplit_once('-').map_or(name, |x| x.0));
    let key = templates::resolve_service_template_key(kind);
    let logo = templates::get_service_templates()
        .get(&key)
        .and_then(|t| t.get("logo"))
        .and_then(Value::as_str);
    match logo.filter(|l| !l.is_empty() && !l.ends_with("default.webp")) {
        None => Value::Null,
        Some(l) => {
            let (src, cdn, _) = crate::ui_services::logo_urls(Some(l));
            json!({"src": format!("{base}{src}"), "dark": null, "fallback": cdn, "padded": true, "favicon": null})
        }
    }
}

fn database_logo(engine: &str, base: &str) -> Value {
    match engine {
        "postgresql" => named_logo(base, "postgres", false),
        "keydb" | "dragonfly" => named_logo(base, engine, true),
        "mysql" | "mariadb" | "mongodb" | "redis" | "clickhouse" | "sqlite" => {
            named_logo(base, engine, false)
        }
        _ => Value::Null,
    }
}

const fn database_name(engine: &str) -> &str {
    match engine.as_bytes() {
        b"postgresql" => "PostgreSQL",
        b"redis" => "Redis",
        b"mongodb" => "MongoDB",
        b"mysql" => "MySQL",
        b"mariadb" => "MariaDB",
        b"keydb" => "KeyDB",
        b"dragonfly" => "Dragonfly",
        b"clickhouse" => "ClickHouse",
        b"sqlite" => "SQLite",
        _ => engine,
    }
}

fn build_pack_name(pack: &str) -> String {
    match pack {
        "nixpacks" => "Nixpacks".into(),
        "railpack" => "Railpack".into(),
        "static" => "Static".into(),
        "dockerfile" => "Dockerfile".into(),
        "dockercompose" => "Docker Compose".into(),
        "dockerimage" => "Docker Image".into(),
        other => coolify_db::audit::headline(other),
    }
}

/// `Application::status()`: with additional servers, the status over all of them.
fn application_status(value: Option<&str>, additional: &[(i64, Option<String>)]) -> String {
    if additional.is_empty() {
        return status_getter(value);
    }
    let value = value.unwrap_or_default();
    let main_status = value.split(':').next().unwrap_or_default();
    let main_health = value.split_once(':').map_or(value, |(_, h)| h);
    let (mut status, mut health) = (main_status, main_health);
    for (_, pivot) in additional {
        let pivot = pivot.as_deref().unwrap_or_default();
        let server_status = pivot.split(':').next().unwrap_or_default();
        if main_status != server_status {
            status = "degraded";
        }
        if let Some((_, server_health)) = pivot.split_once(':')
            && main_health != server_health
        {
            health = "unhealthy";
        }
    }
    format!("{status}:{health}")
}

pub struct Graph {
    pub zones: Vec<Value>,
    pub nodes: Vec<Map<String, Value>>,
    pub links: Vec<Value>,
}

fn sub_rows(kind: SubKind, rows: Option<&Vec<Record>>) -> Vec<SubRow> {
    rows.map(|list| {
        list.iter()
            .map(|r| SubRow {
                kind,
                id: int(r, "id"),
                uuid: text(r, "uuid").to_owned(),
                name: text(r, "name").to_owned(),
                raw: r.clone(),
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `EnvironmentCanvas::graph`: zones, cards and links. Links with an end outside are left out.
pub async fn graph(
    state: &AppState,
    l: &Loaded,
    links: &[Record],
    can_read_sensitive: bool,
    base: &str,
) -> Result<Graph, ApiError> {
    let service_uuids: Vec<String> = l
        .resources
        .iter()
        .filter(|r| r.morph == SERVICE_MORPH)
        .map(|r| r.uuid().to_owned())
        .collect();
    let starting: HashSet<String> = db::starting_services(&state.db, &service_uuids)
        .await?
        .into_iter()
        .collect();

    // `$servers->put($server->id, $server)`: first seen first.
    let mut servers: Vec<&Record> = Vec::new();
    let mut nodes = Vec::with_capacity(l.resources.len());
    for r in &l.resources {
        let server = l.card_server(r);
        let zone = server
            .and_then(|s| s.get("uuid"))
            .cloned()
            .unwrap_or(Value::Null);
        let project_uuid = l.project(r).map_or("", |p| text(p, "uuid"));
        let environment_uuid = l.environment(r).map_or("", |e| text(e, "uuid"));
        let path = |kind: &str| {
            format!(
                "{base}/project/{project_uuid}/environment/{environment_uuid}/{kind}/{}",
                r.uuid()
            )
        };
        let mut shown: Vec<&Record> = server.into_iter().collect();
        let mut node = if r.morph == APPLICATION_MORPH {
            let pivots = l.additional.get(&r.id()).map_or(&[][..], Vec::as_slice);
            let extra: Vec<&Record> = pivots
                .iter()
                .filter_map(|(i, _)| l.servers.get(i))
                .collect();
            let mut extra_zones: Vec<&str> = Vec::new();
            for s in &extra {
                let uuid = text(s, "uuid");
                if Some(uuid) != server.map(|m| text(m, "uuid")) && !extra_zones.contains(&uuid) {
                    extra_zones.push(uuid);
                }
            }
            shown.extend(extra.iter().copied());
            let pack = r.str("build_pack").unwrap_or_default();
            let detail = if pack == "dockerimage" {
                [
                    r.str("docker_registry_image_name").unwrap_or_default(),
                    r.str("docker_registry_image_tag").unwrap_or_default(),
                ]
                .into_iter()
                .filter(|p| !p.is_empty() && *p != "0")
                .collect::<Vec<_>>()
                .join(":")
            } else {
                r.str("git_branch").unwrap_or_default().to_owned()
            };
            json!({
                "id": r.uuid(),
                "type": "application",
                "name": r.name(),
                "status": application_status(r.str("status"), pivots),
                "zone": zone,
                "extra_zones": extra_zones,
                "urls": split_urls(r.str("fqdn")),
                "kind": r.row.get("build_pack"),
                "subtitle": subtitle(&[&build_pack_name(pack), &detail]),
                "children": [],
                "link": path("application"),
                "logo": application_logo(r, base),
            })
        } else if r.morph == SERVICE_MORPH {
            let apps = l.service_applications.get(&r.id());
            let dbs = l.service_databases.get(&r.id());
            let child = |c: &Record, kind: &str| json!({"name": c.get("name"), "type": kind, "status": text(c, "status")});
            let mut children: Vec<Value> = apps
                .into_iter()
                .flatten()
                .map(|c| child(c, "application"))
                .collect();
            children.extend(dbs.into_iter().flatten().map(|c| child(c, "database")));
            let count = children.len();
            let mut urls: Vec<String> = Vec::new();
            for app in apps.into_iter().flatten() {
                for u in split_urls(app.get("fqdn").and_then(Value::as_str)) {
                    if !urls.contains(&u) {
                        urls.push(u);
                    }
                }
            }
            let status = if starting.contains(r.uuid()) {
                "starting:unhealthy".to_owned()
            } else {
                let mut subs = sub_rows(SubKind::Application, apps);
                subs.extend(sub_rows(SubKind::Database, dbs));
                container_status_of(&subs).unwrap_or_default()
            };
            json!({
                "id": r.uuid(),
                "type": "service",
                "name": r.name(),
                "status": status,
                "zone": zone,
                "extra_zones": [],
                "urls": urls,
                "kind": null,
                "subtitle": subtitle(&["Service", &format!("{count} {}", if count == 1 { "container" } else { "containers" })]),
                "children": children,
                "link": path("service"),
                "logo": service_logo(r, base),
            })
        } else {
            let engine = r.engine.unwrap_or_default();
            json!({
                "id": r.uuid(),
                "type": "database",
                "name": r.name(),
                "status": status_getter(r.str("status")),
                "zone": zone,
                "extra_zones": [],
                "urls": [],
                "kind": engine,
                "subtitle": subtitle(&[database_name(engine), r.str("image").unwrap_or_default()]),
                "children": [],
                "link": path("database"),
                "logo": database_logo(engine, base),
            })
        };
        for s in shown {
            if !servers.iter().any(|known| int(known, "id") == int(s, "id")) {
                servers.push(s);
            }
        }
        if let Some(m) = node.as_object_mut() {
            let only: HashSet<&str> = [r.uuid()].into_iter().collect();
            let saved = l
                .environment(r)
                .map(|e| filter_positions(e.get("canvas_positions"), &only))
                .and_then(|mut p| p.remove(r.uuid()));
            m.insert("saved".into(), saved.unwrap_or(Value::Null));
        }
        if let Value::Object(m) = node {
            nodes.push(m);
        }
    }

    let by_morph = l.morph_key_uuid();
    let uuid_of = |t: &str, id: i64| by_morph.get(&(t.to_owned(), id));
    let links = links
        .iter()
        .filter_map(|link| {
            let source = uuid_of(text(link, "source_type"), int(link, "source_id"))?;
            let target = uuid_of(text(link, "target_type"), int(link, "target_id"))?;
            Some(link_payload(link, source, target))
        })
        .collect();

    Ok(Graph {
        zones: servers
            .iter()
            .map(|s| Value::Object(zone(s, can_read_sensitive, base)))
            .collect(),
        nodes,
        links,
    })
}

/// `EnvironmentCanvas::linkPayload`.
pub fn link_payload(link: &Record, source: &str, target: &str) -> Value {
    json!({"id": link.get("uuid"), "source": source, "target": target, "label": link.get("label")})
}

fn owner(l: &Loaded, r: &Resource) -> Value {
    let p = l.project(r);
    let e = l.environment(r);
    json!({
        "project": {"uuid": p.and_then(|p| p.get("uuid")), "name": p.and_then(|p| p.get("name"))},
        "environment": {"uuid": e.and_then(|e| e.get("uuid")), "name": e.and_then(|e| e.get("name"))},
    })
}

fn context_link(l: &Loaded, r: &Resource, base: &str) -> String {
    environment_page(
        base,
        l.project(r).map_or("", |p| text(p, "uuid")),
        l.environment(r).map_or("", |e| text(e, "uuid")),
    )
}

/// `EnvironmentCanvas::payload`.
pub async fn environment_payload(
    state: &AppState,
    project: &Record,
    environment: &Record,
    can_read_sensitive: bool,
    can_edit: bool,
    base: &str,
) -> Result<Value, ApiError> {
    let environments = vec![environment.clone()];
    let l = load(state, &environments).await?;
    let links = db::links(&state.db, &[int(environment, "id")]).await?;
    let g = graph(state, &l, &links, can_read_sensitive, base).await?;
    let uuids: HashSet<&str> = l.resources.iter().map(Resource::uuid).collect();
    Ok(json!({
        "project": {
            "uuid": project.get("uuid"),
            "name": project.get("name"),
            "link": format!("{base}/project/{}", text(project, "uuid")),
        },
        "environment": {
            "uuid": environment.get("uuid"),
            "name": environment.get("name"),
            "link": environment_page(base, text(project, "uuid"), text(environment, "uuid")),
        },
        "can_edit": can_edit,
        "zones": g.zones,
        "nodes": g.nodes,
        "links": g.links,
        "positions": php_map(filter_positions(environment.get("canvas_positions"), &uuids)),
    }))
}

/// `ProjectCanvas::emptyNodes`.
fn empty_project_nodes(
    project: &Record,
    environments: &[&Record],
    used: &HashSet<i64>,
    zone: &str,
    can_add: bool,
    base: &str,
) -> Vec<Map<String, Value>> {
    if environments.is_empty() {
        return vec![empty_node(
            project,
            None,
            zone,
            "No environments",
            false,
            base,
        )];
    }
    environments
        .iter()
        .filter(|e| !used.contains(&int(e, "id")))
        .map(|e| empty_node(project, Some(e), zone, text(e, "name"), can_add, base))
        .collect()
}

/// `ProjectCanvas::teamPayload`.
pub async fn projects_payload(
    state: &AppState,
    team_id: i64,
    can_read_sensitive: bool,
    can_edit: bool,
    base: &str,
) -> Result<Value, ApiError> {
    let projects = db::team_projects(&state.db, team_id).await?;
    let project_ids: Vec<i64> = projects.iter().map(|p| int(p, "id")).collect();
    let environments = db::project_environments(&state.db, &project_ids).await?;
    let env_ids: Vec<i64> = environments.iter().map(|e| int(e, "id")).collect();
    let l = load(state, &environments).await?;
    let links = db::links(&state.db, &env_ids).await?;
    let g = graph(state, &l, &links, can_read_sensitive, base).await?;
    let used: HashSet<i64> = l.resources.iter().map(Resource::environment_id).collect();
    let mut nodes = Vec::new();
    for (node, r) in g.nodes.into_iter().zip(&l.resources) {
        let mut node = node;
        spread(
            &mut node,
            json!({
                "zone": l.project(r).and_then(|p| p.get("uuid")),
                "extra_zones": [],
                "context": l.environment(r).and_then(|e| e.get("name")),
                "context_link": context_link(&l, r, base),
            }),
        );
        spread(&mut node, owner(&l, r));
        nodes.push(node);
    }
    for p in &projects {
        let own: Vec<&Record> = environments
            .iter()
            .filter(|e| int(e, "project_id") == int(p, "id"))
            .collect();
        nodes.extend(empty_project_nodes(
            p,
            &own,
            &used,
            text(p, "uuid"),
            can_edit,
            base,
        ));
    }
    Ok(json!({
        "title": "Projects",
        "title_link": format!("{base}/projects"),
        "can_edit": can_edit,
        "can_move": false,
        "can_move_groups": can_edit,
        "can_move_zones": can_edit,
        "layout": "server",
        "zones": projects.iter().map(|p| json!({
            "id": p.get("uuid"),
            "name": p.get("name"),
            "icon": "projects",
            "link": format!("{base}/project/{}", text(p, "uuid")),
            "position": zone_position(p.get("canvas_position")),
        })).collect::<Vec<_>>(),
        "nodes": nodes,
        "links": g.links,
        "positions": [],
    }))
}

/// `ProjectCanvas::payload`.
pub async fn project_payload(
    state: &AppState,
    project: &Record,
    can_read_sensitive: bool,
    can_edit: bool,
    base: &str,
) -> Result<Value, ApiError> {
    let environments = db::project_environments(&state.db, &[int(project, "id")]).await?;
    let env_ids: Vec<i64> = environments.iter().map(|e| int(e, "id")).collect();
    let l = load(state, &environments).await?;
    let links = db::links(&state.db, &env_ids).await?;
    let g = graph(state, &l, &links, can_read_sensitive, base).await?;
    let mut nodes = Vec::new();
    for (node, r) in g.nodes.into_iter().zip(&l.resources) {
        let mut node = node;
        spread(
            &mut node,
            json!({
                "context": l.environment(r).and_then(|e| e.get("name")),
                "context_link": context_link(&l, r, base),
            }),
        );
        spread(&mut node, owner(&l, r));
        nodes.push(node);
    }
    let fallback = g
        .zones
        .first()
        .and_then(|z| z.get("id"))
        .and_then(Value::as_str)
        .unwrap_or(EMPTY_ZONE)
        .to_owned();
    let zone = project_zones(&nodes)
        .remove(text(project, "uuid"))
        .unwrap_or(fallback);
    let used: HashSet<i64> = l.resources.iter().map(Resource::environment_id).collect();
    let own: Vec<&Record> = environments.iter().collect();
    let empty = empty_project_nodes(project, &own, &used, &zone, can_edit, base);
    let mut zones = g.zones;
    if !empty.is_empty() && zone == EMPTY_ZONE {
        zones.push(empty_zone());
    }
    nodes.extend(empty);
    Ok(json!({
        "title": project.get("name"),
        "title_link": format!("{base}/project/{}", text(project, "uuid")),
        "can_edit": can_edit,
        "can_move": false,
        "can_move_groups": can_edit,
        "layout": "server",
        "zones": zones,
        "nodes": nodes,
        "links": g.links,
        "positions": [],
    }))
}

/// `ServerCanvas::details`.
fn details(server: &Record) -> Value {
    let meta = server.get("server_metadata").and_then(Value::as_object);
    let get = |k: &str| meta.and_then(|m| m.get(k)).cloned().unwrap_or(Value::Null);
    let as_int = |k: &str| match get(k) {
        Value::Null => Value::Null,
        v => numeric(Some(&v)).map_or(json!(0), |n| json!(n)),
    };
    let proxy = server.get("proxy").and_then(Value::as_object);
    let proxy_type = proxy.and_then(|p| p.get("type")).and_then(Value::as_str);
    let settings = server.get("settings").and_then(Value::as_object);
    let live = flag(settings.and_then(|s| s.get("is_reachable")))
        && flag(settings.and_then(|s| s.get("is_metrics_enabled")));
    json!({
        "os": get("os"),
        "arch": get("arch"),
        "cpus": as_int("cpus"),
        "memory_bytes": as_int("memory_bytes"),
        "uptime_since": get("uptime_since"),
        "proxy": match proxy_type {
            Some(t) if !t.is_empty() && t != "NONE" => json!({
                "type": t,
                "status": proxy.and_then(|p| p.get("status")),
            }),
            _ => Value::Null,
        },
        "usage_endpoint": if live {
            Value::String(format!("/servers/{}/canvas/usage", text(server, "uuid")))
        } else {
            Value::Null
        },
    })
}

/// `ServerCanvas::payload`.
pub async fn server_payload(
    state: &AppState,
    team_id: i64,
    servers: &[Record],
    single: bool,
    can_read_sensitive: bool,
    can_edit: bool,
    base: &str,
) -> Result<Value, ApiError> {
    let environments = db::team_environments(&state.db, team_id).await?;
    let env_ids: Vec<i64> = environments.iter().map(|e| int(e, "id")).collect();
    let l = load(state, &environments).await?;
    let links = db::links(&state.db, &env_ids).await?;
    let g = graph(state, &l, &links, can_read_sensitive, base).await?;
    let server_uuids: Vec<&str> = servers.iter().map(|s| text(s, "uuid")).collect();

    let mut nodes: Vec<Map<String, Value>> = Vec::new();
    for (node, r) in g.nodes.into_iter().zip(&l.resources) {
        let main = node
            .get("zone")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let extras: Vec<String> = node
            .get("extra_zones")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let shown_main = server_uuids.contains(&main.as_str());
        let shown_extra = extras.iter().find(|u| server_uuids.contains(&u.as_str()));
        if !shown_main && shown_extra.is_none() {
            continue;
        }
        let mut node = node;
        if !shown_main {
            node.insert("zone".into(), json!(shown_extra));
        }
        let p = l.project(r);
        let e = l.environment(r);
        spread(
            &mut node,
            json!({
                "context": format!(
                    "{} / {}",
                    p.map_or("", |p| text(p, "name")),
                    e.map_or("", |e| text(e, "name")),
                ),
                "context_link": context_link(&l, r, base),
            }),
        );
        spread(&mut node, owner(&l, r));
        nodes.push(node);
    }
    let shown: HashSet<String> = nodes
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let used: HashSet<i64> = l.resources.iter().map(Resource::environment_id).collect();
    let fallback = server_uuids.first().copied().unwrap_or(EMPTY_ZONE);
    let zone_of = project_zones(&nodes);
    let mut empty = Vec::new();
    if !single {
        for e in &environments {
            if used.contains(&int(e, "id")) {
                continue;
            }
            let Some(p) = l.projects.get(&int(e, "project_id")) else {
                continue;
            };
            let zone = zone_of
                .get(text(p, "uuid"))
                .map_or(fallback, String::as_str);
            let context = format!("{} / {}", text(p, "name"), text(e, "name"));
            empty.push(empty_node(p, Some(e), zone, &context, can_edit, base));
        }
    }
    let filled: HashSet<String> = nodes
        .iter()
        .chain(&empty)
        .filter_map(|n| n.get("zone").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut bare = Vec::new();
    for s in servers {
        let uuid = text(s, "uuid");
        if !filled.contains(uuid) {
            bare.push(json!({
                "id": format!("empty-server:{uuid}"),
                "type": "empty",
                "name": "No resources yet",
                "subtitle": "Open to see this server",
                "status": null,
                "zone": uuid,
                "extra_zones": [],
                "urls": [],
                "children": [],
                "link": format!("{base}/server/{uuid}"),
                "context": null,
                "project": null,
                "environment": null,
            }));
        }
    }
    let mut zones: Vec<Value> = servers
        .iter()
        .map(|s| {
            let mut z = zone(s, can_read_sensitive, base);
            z.insert("details".into(), details(s));
            z.insert(
                "position".into(),
                if single {
                    Value::Null
                } else {
                    json!(zone_position(s.get("canvas_position")))
                },
            );
            Value::Object(z)
        })
        .collect();
    if empty
        .iter()
        .any(|n| n.get("zone").and_then(Value::as_str) == Some(EMPTY_ZONE))
    {
        zones.push(empty_zone());
    }
    let mut all: Vec<Value> = nodes.into_iter().map(Value::Object).collect();
    all.extend(empty.into_iter().map(Value::Object));
    all.extend(bare);
    let only = single.then(|| servers.first()).flatten();
    Ok(json!({
        "title": only.map_or(json!("Servers"), |s| json!(s.get("name"))),
        "title_link": only.map(|s| format!("{base}/server/{}", text(s, "uuid"))),
        "containers_endpoint": only.map(|s| format!("/servers/{}/canvas/containers", text(s, "uuid"))),
        "can_edit": can_edit,
        "can_move": false,
        "can_move_groups": can_edit,
        "can_move_zones": can_edit && !single,
        "layout": "server",
        "zones": zones,
        "nodes": all,
        "links": g.links.into_iter().filter(|link| {
            let end = |k: &str| link.get(k).and_then(Value::as_str).unwrap_or_default();
            shown.contains(end("source")) && shown.contains(end("target"))
        }).collect::<Vec<_>>(),
        "positions": [],
    }))
}

/// `ServerCanvas::dashboardPayload`.
pub async fn dashboard_payload(
    state: &AppState,
    team_id: i64,
    servers: &[Record],
    can_read_sensitive: bool,
    can_edit: bool,
    base: &str,
) -> Result<Value, ApiError> {
    let mut payload = server_payload(
        state,
        team_id,
        servers,
        false,
        can_read_sensitive,
        can_edit,
        base,
    )
    .await?;
    let ids: Vec<i64> = servers.iter().map(|s| int(s, "id")).collect();
    let active = db::dashboard_deployments(&state.db, &ids, true).await?;
    let recent = db::dashboard_deployments(&state.db, &ids, false).await?;
    let app_ids: Vec<i64> = active
        .iter()
        .chain(&recent)
        .map(|d| int(d, "application_id"))
        .collect();
    let uuids = db::application_uuids(&state.db, &app_ids).await?;
    let uuid_of = |d: &Record| uuids.get(&int(d, "application_id"));
    let summary = |d: &Record| {
        let pr = int(d, "pull_request_id");
        json!({
            "status": d.get("status"),
            "link": d.get("deployment_url"),
            "pull_request_id": if pr == 0 { Value::Null } else { json!(pr) },
        })
    };
    let mut by_uuid: HashMap<String, Value> = HashMap::new();
    for d in &active {
        if let Some(u) = uuid_of(d) {
            by_uuid.entry(u.clone()).or_insert_with(|| summary(d));
        }
    }
    let iso = |v: Option<&Value>| match v.and_then(Value::as_str) {
        Some(s) => {
            let parsed = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f");
            parsed.map_or(Value::Null, |d| {
                json!(d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string())
            })
        }
        None => Value::Null,
    };
    let deployments: Vec<Value> = active
        .iter()
        .chain(&recent)
        .map(|d| {
            let mut row = json!({
                "id": d.get("deployment_uuid"),
                "application": uuid_of(d),
                "name": d.get("application_name"),
                "server": d.get("server_name"),
                "created_at": iso(d.get("created_at")),
                "finished_at": iso(d.get("finished_at")),
            });
            if let Some(m) = row.as_object_mut() {
                spread(m, summary(d));
            }
            row
        })
        .collect();
    if let Some(m) = payload.as_object_mut() {
        m.insert("title".into(), json!("Dashboard"));
        m.insert("title_link".into(), json!(base));
        if let Some(Value::Array(nodes)) = m.get_mut("nodes") {
            for n in nodes.iter_mut() {
                let hit = n.get("type").and_then(Value::as_str) == Some("application");
                let found = hit
                    .then(|| {
                        n.get("id")
                            .and_then(Value::as_str)
                            .and_then(|i| by_uuid.get(i))
                    })
                    .flatten()
                    .cloned();
                if let (Some(deployment), Some(node)) = (found, n.as_object_mut()) {
                    node.insert("deployment".into(), deployment);
                }
            }
        }
        m.insert("deployments".into(), json!(deployments));
    }
    Ok(payload)
}
