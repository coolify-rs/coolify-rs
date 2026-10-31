//! `Service::extraFields()` and the part of `Project\Service\StackForm::mount` that shapes them.
//! The upstream `switch` is a table here: the first case whose image test passes wins.

use coolify_db::records::Record;
use serde_json::{Value, json};

/// One entry of `$data` in a `case`.
struct Spec {
    label: &'static str,
    /// The variables the field reads, the first one present wins.
    keys: &'static [&'static str],
    /// `if ($variable)`: left out when the variable does not exist.
    guarded: bool,
    /// `'key' => 'LITERAL'` with `'value' => 'literal'`: not read from a variable.
    literal: Option<(&'static str, &'static str)>,
    rules: Option<&'static str>,
    password: bool,
    helper: Option<&'static str>,
    sort_order: Option<i64>,
}

const fn f(
    label: &'static str,
    keys: &'static [&'static str],
    rules: Option<&'static str>,
    password: bool,
) -> Spec {
    Spec {
        label,
        keys,
        guarded: true,
        literal: None,
        rules,
        password,
        helper: None,
        sort_order: None,
    }
}

/// A field without the `if ($variable)` guard.
const fn always(
    label: &'static str,
    keys: &'static [&'static str],
    rules: Option<&'static str>,
    password: bool,
) -> Spec {
    Spec {
        label,
        keys,
        guarded: false,
        literal: None,
        rules,
        password,
        helper: None,
        sort_order: None,
    }
}

const fn literal(
    label: &'static str,
    key: &'static str,
    value: &'static str,
    password: bool,
) -> Spec {
    Spec {
        label,
        keys: &[],
        guarded: false,
        literal: Some((key, value)),
        rules: Some("required"),
        password,
        helper: None,
        sort_order: None,
    }
}

const R: Option<&str> = Some("required");
const RE: Option<&str> = Some("required|email");
const RU: Option<&str> = Some("required|url");
const RB: Option<&str> = Some("required|string|in:true,false");

/// How a `case` tests the image name (the part before the first `:`).
enum Test {
    Contains(&'static [&'static str]),
    /// `$image->is('registry')`.
    Is(&'static str),
    /// `isGrafanaServerImage($application->image)`.
    Grafana,
}

type Case = (Test, &'static str, &'static [Spec]);

const CONTAINS_DEFAULT: &[Spec] = &[
    f("User", &["SERVICE_USER_ADMIN"], R, false),
    f("Password", &["SERVICE_PASSWORD_ADMIN"], R, true),
    f("Email", &["ADMIN_EMAIL"], RE, false),
];

const APPLICATION_CASES: &[Case] = &[
    (
        Test::Contains(&["drizzle-team/gateway"]),
        "Drizzle",
        &[always(
            "Master Password",
            &["SERVICE_PASSWORD_DRIZZLE"],
            R,
            true,
        )],
    ),
    (
        Test::Contains(&["castopod"]),
        "Castopod",
        &[Spec {
            helper: Some(
                "If you want to use https, set this to 0. Variable name: CP_DISABLE_HTTPS",
            ),
            ..f("Disable HTTPS", &["CP_DISABLE_HTTPS"], R, false)
        }],
    ),
    (
        Test::Contains(&["label-studio"]),
        "Label Studio",
        &[
            f("Username", &["LABEL_STUDIO_USERNAME"], R, false),
            f("Password", &["SERVICE_PASSWORD_LABELSTUDIO"], R, true),
        ],
    ),
    (
        Test::Contains(&["litellm"]),
        "Litellm",
        &[
            f("Username", &["SERVICE_USER_UI"], R, false),
            f("Password", &["SERVICE_PASSWORD_UI"], R, true),
        ],
    ),
    (
        Test::Contains(&["langfuse"]),
        "Langfuse",
        &[
            f("Admin Email", &["LANGFUSE_INIT_USER_EMAIL"], RE, false),
            f("Admin Password", &["SERVICE_PASSWORD_LANGFUSE"], R, true),
        ],
    ),
    (
        Test::Contains(&["invoiceninja"]),
        "Invoice Ninja",
        &[
            always("Email", &["IN_USER_EMAIL"], RE, false),
            always("Password", &["SERVICE_PASSWORD_INVOICENINJAUSER"], R, true),
        ],
    ),
    (
        Test::Contains(&["argilla"]),
        "Argilla",
        &[
            always("API Key", &["SERVICE_PASSWORD_APIKEY"], R, true),
            always("Username", &["ARGILLA_USERNAME"], R, false),
            always("Password", &["SERVICE_PASSWORD_ARGILLA"], R, true),
        ],
    ),
    (
        Test::Contains(&["rabbitmq"]),
        "RabbitMQ",
        &[
            f("Host Port Binding", &["PORT"], R, false),
            f("Username", &["SERVICE_USER_RABBITMQ"], R, false),
            f("Password", &["SERVICE_PASSWORD_RABBITMQ"], R, true),
        ],
    ),
    (
        Test::Is("registry"),
        "Docker Registry",
        &[
            f("Registry User", &["SERVICE_USER_REGISTRY"], R, false),
            f("Registry Password", &["SERVICE_PASSWORD_REGISTRY"], R, true),
        ],
    ),
    (
        Test::Contains(&["tolgee"]),
        "Tolgee",
        &[
            literal(
                "Admin User",
                "TOLGEE_AUTHENTICATION_INITIAL_USERNAME",
                "admin",
                false,
            ),
            f("Admin Password", &["SERVICE_PASSWORD_TOLGEE"], R, true),
        ],
    ),
    (
        Test::Contains(&["logto"]),
        "Logto",
        &[
            f("Endpoint", &["LOGTO_ENDPOINT"], RU, false),
            f("Admin Endpoint", &["LOGTO_ADMIN_ENDPOINT"], RU, false),
        ],
    ),
    (
        Test::Contains(&["unleash-server"]),
        "Unleash",
        &[
            literal("Admin User", "SERVICE_USER_UNLEASH", "admin", false),
            f("Admin Password", &["SERVICE_PASSWORD_UNLEASH"], R, true),
        ],
    ),
    (
        Test::Grafana,
        "Grafana",
        &[
            literal("Admin User", "GF_SECURITY_ADMIN_USER", "admin", false),
            f("Admin Password", &["SERVICE_PASSWORD_GRAFANA"], R, true),
        ],
    ),
    (
        Test::Contains(&["elasticsearch"]),
        "Elasticsearch",
        &[f(
            "Password (default user: elastic)",
            &["SERVICE_PASSWORD_ELASTICSEARCH"],
            R,
            true,
        )],
    ),
    (
        Test::Contains(&["directus"]),
        "Directus",
        &[
            f("Admin Email", &["ADMIN_EMAIL"], RE, false),
            f("Admin Password", &["SERVICE_PASSWORD_ADMIN"], R, true),
        ],
    ),
    (
        Test::Contains(&["kong"]),
        "Supabase",
        &[
            f("Dashboard User", &["SERVICE_USER_ADMIN"], R, false),
            f("Dashboard Password", &["SERVICE_PASSWORD_ADMIN"], R, true),
        ],
    ),
    (
        Test::Contains(&["minio", "pgsty/silo"]),
        "MinIO",
        &[
            f("Console URL", &["MINIO_BROWSER_REDIRECT_URL"], RU, false),
            f("S3 API URL", &["MINIO_SERVER_URL"], RU, false),
            f(
                "Admin User",
                &["SERVICE_USER_MINIO", "MINIO_ROOT_USER"],
                R,
                false,
            ),
            f(
                "Admin Password",
                &["SERVICE_PASSWORD_MINIO", "MINIO_ROOT_PASSWORD"],
                R,
                true,
            ),
        ],
    ),
    (
        Test::Contains(&["garage"]),
        "Garage",
        &[
            f("S3 API URL", &["GARAGE_S3_API_URL"], RU, false),
            f("Web URL", &["GARAGE_WEB_URL"], RU, false),
            f("Admin URL", &["GARAGE_ADMIN_URL"], RU, false),
            f(
                "Admin Token",
                &["GARAGE_ADMIN_TOKEN", "SERVICE_PASSWORD_GARAGE"],
                R,
                true,
            ),
            f(
                "RPC Secret",
                &[
                    "GARAGE_RPC_SECRET",
                    "SERVICE_HEX_64_RPCSECRET",
                    "SERVICE_HEX_32_RPCSECRET",
                ],
                R,
                true,
            ),
            f(
                "Metrics Token",
                &["GARAGE_METRICS_TOKEN", "SERVICE_PASSWORD_GARAGEMETRICS"],
                R,
                true,
            ),
        ],
    ),
    (
        Test::Contains(&["weblate"]),
        "Weblate",
        &[
            f("Admin Email", &["WEBLATE_ADMIN_EMAIL"], RE, false),
            f("Admin Password", &["SERVICE_PASSWORD_WEBLATE"], R, true),
        ],
    ),
    (
        Test::Contains(&["meilisearch"]),
        "Meilisearch",
        &[f("API Key", &["SERVICE_PASSWORD_MEILISEARCH"], None, true)],
    ),
    (
        Test::Contains(&["linkding"]),
        "Linkding",
        &[
            f("Superuser Name", &["SERVICE_USER_LINKDING"], None, false),
            f(
                "Superuser Password",
                &["SERVICE_PASSWORD_LINKDING"],
                None,
                true,
            ),
        ],
    ),
    (
        Test::Contains(&["ghost"]),
        "Ghost",
        &[
            f("Mail Password", &["MAIL_OPTIONS_AUTH_PASS"], None, true),
            f("Mail User", &["MAIL_OPTIONS_AUTH_USER"], None, false),
            f("Mail Secure", &["MAIL_OPTIONS_SECURE"], None, false),
            f("Mail Port", &["MAIL_OPTIONS_PORT"], None, false),
            f("Mail Service", &["MAIL_OPTIONS_SERVICE"], None, false),
            f("Mail Host", &["MAIL_OPTIONS_HOST"], None, false),
        ],
    ),
    (
        Test::Contains(&["vaultwarden"]),
        "Vaultwarden",
        &[
            f("Database URL", &["DATABASE_URL"], None, false),
            f("Admin Password", &["SERVICE_PASSWORD_64_ADMIN"], None, true),
            f("Signup Allowed", &["SIGNUP_ALLOWED"], RB, false),
            f("Push Enabled", &["PUSH_ENABLED"], RB, false),
            f("Push Installation ID", &["PUSH_SERVICE_ID"], None, false),
            f("Push Installation Key", &["PUSH_SERVICE_KEY"], None, true),
        ],
    ),
    (
        Test::Contains(&["gitlab/gitlab"]),
        "GitLab",
        &[
            f("Root Password", &["SERVICE_PASSWORD_GITLAB"], R, true),
            literal("Root User", "GITLAB_ROOT_USER", "root", true),
        ],
    ),
    (
        Test::Contains(&["code-server"]),
        "Code Server",
        &[
            f(
                "Password",
                &["SERVICE_PASSWORD_64_PASSWORDCODESERVER"],
                R,
                true,
            ),
            f(
                "Sudo Password",
                &["SERVICE_PASSWORD_SUDOCODESERVER"],
                R,
                true,
            ),
        ],
    ),
    (
        Test::Contains(&["elestio/strapi"]),
        "Strapi",
        &[
            f("License", &["STRAPI_LICENSE"], None, false),
            f("Node Environment", &["NODE_ENV"], None, false),
        ],
    ),
    (
        Test::Contains(&["marckohlbrugge/sessy"]),
        "Sessy",
        &[
            f("HTTP Auth Username", &["SERVICE_USER_SESSY"], R, false),
            f("HTTP Auth Password", &["SERVICE_PASSWORD_SESSY"], R, true),
        ],
    ),
    (
        Test::Contains(&["coollabsio/openclaw"]),
        "Openclaw",
        &[
            f("Username", &["SERVICE_USER_OPENCLAW"], None, false),
            f("Password", &["SERVICE_PASSWORD_OPENCLAW"], None, true),
            f(
                "Gateway Token",
                &["SERVICE_PASSWORD_64_GATEWAYTOKEN"],
                None,
                true,
            ),
        ],
    ),
    (
        Test::Contains(&["coollabsio/jean-server"]),
        "",
        &[
            Spec {
                sort_order: Some(1),
                helper: Some(
                    "Token required to access Jean Server. Variable name: SERVICE_PASSWORD_64_JEAN",
                ),
                ..f("Token", &["SERVICE_PASSWORD_64_JEAN"], R, true)
            },
            Spec {
                sort_order: Some(2),
                helper: Some(
                    "Comma-separated additional browser origins. Same-origin access is always allowed. Variable name: JEAN_ALLOWED_ORIGINS",
                ),
                ..f(
                    "Allowed Origins",
                    &["JEAN_ALLOWED_ORIGINS"],
                    Some("nullable|string"),
                    false,
                )
            },
        ],
    ),
];

const DATABASE_CASES: &[Case] = &[
    (
        Test::Contains(&["postgres"]),
        "PostgreSQL",
        &[
            f(
                "User",
                &["SERVICE_USER_POSTGRES", "SERVICE_USER_POSTGRESQL"],
                R,
                false,
            ),
            f(
                "Password",
                &["SERVICE_PASSWORD_POSTGRES", "SERVICE_PASSWORD_POSTGRESQL"],
                R,
                true,
            ),
            f(
                "Database Name",
                &["POSTGRESQL_DATABASE", "POSTGRES_DB"],
                R,
                false,
            ),
        ],
    ),
    (
        Test::Contains(&["mysql"]),
        "MySQL",
        &[
            f(
                "User",
                &["SERVICE_USER_MYSQL", "SERVICE_USER_WORDPRESS", "MYSQL_USER"],
                R,
                false,
            ),
            f(
                "Password",
                &[
                    "SERVICE_PASSWORD_MYSQL",
                    "SERVICE_PASSWORD_WORDPRESS",
                    "MYSQL_PASSWORD",
                    "SERVICE_PASSWORD_64_MYSQL",
                ],
                R,
                true,
            ),
            f(
                "Root Password",
                &[
                    "SERVICE_PASSWORD_MYSQLROOT",
                    "SERVICE_PASSWORD_ROOT",
                    "SERVICE_PASSWORD_64_MYSQLROOT",
                ],
                R,
                true,
            ),
            f("Database Name", &["MYSQL_DATABASE"], R, false),
        ],
    ),
    (
        Test::Contains(&["mariadb"]),
        "MariaDB",
        &[
            f(
                "User",
                &[
                    "SERVICE_USER_MARIADB",
                    "SERVICE_USER_WORDPRESS",
                    "SERVICE_USER_MYSQL",
                    "MYSQL_USER",
                ],
                R,
                false,
            ),
            f(
                "Password",
                &[
                    "SERVICE_PASSWORD_MARIADB",
                    "SERVICE_PASSWORD_WORDPRESS",
                    "_APP_DB_PASS",
                    "MYSQL_PASSWORD",
                ],
                R,
                true,
            ),
            f(
                "Root Password",
                &[
                    "SERVICE_PASSWORD_MARIADBROOT",
                    "SERVICE_PASSWORD_ROOT",
                    "_APP_DB_ROOT_PASS",
                    "MYSQL_ROOT_PASSWORD",
                ],
                R,
                true,
            ),
            f(
                "Database Name",
                &[
                    "SERVICE_DATABASE_MARIADB",
                    "SERVICE_DATABASE_WORDPRESS",
                    "_APP_DB_SCHEMA",
                    "MYSQL_DATABASE",
                ],
                R,
                false,
            ),
        ],
    ),
];

/// `Collection::put`: replaces in place, or appends.
fn put<K: PartialEq, V>(list: &mut Vec<(K, V)>, key: K, value: V) {
    if let Some(entry) = list.iter_mut().find(|(k, _)| *k == key) {
        entry.1 = value;
    } else {
        list.push((key, value));
    }
}

/// A field as `StackForm::$fields` holds it.
#[derive(Debug, Clone)]
pub struct StackField {
    pub service_name: String,
    pub key: String,
    pub name: String,
    pub value: Option<String>,
    pub rules: String,
    pub is_password: bool,
    pub custom_helper: Option<String>,
    pub sort_order: Option<i64>,
    pub is_locked: bool,
}

impl StackField {
    pub fn to_json(&self) -> Value {
        json!({
            "serviceName": self.service_name,
            "key": self.key,
            "name": self.name,
            "value": self.value,
            "isPassword": self.is_password,
            "rules": self.rules,
            "customHelper": self.custom_helper,
            "isLocked": self.is_locked,
        })
    }
}

/// `isGrafanaServerImage()`: optionally prefixed by a registry host, and without the tag.
fn is_grafana_server_image(image: &str) -> bool {
    let without_digest = image.split('@').next().unwrap_or(image);
    let mut name = without_digest;
    if let Some((first, rest)) = name.split_once('/')
        && (first.contains('.') || first.contains(':') || first == "localhost")
    {
        name = rest;
    }
    let name = match name.rsplit_once(':') {
        Some((n, tag)) if !tag.contains('/') => n,
        _ => name,
    };
    matches!(
        name,
        "grafana/grafana" | "grafana/grafana-oss" | "grafana/grafana-enterprise"
    )
}

fn passes(test: &Test, image_name: &str, image: &str) -> bool {
    match test {
        Test::Contains(needles) => needles.iter().any(|n| image_name.contains(n)),
        Test::Is(v) => image_name == *v,
        Test::Grafana => is_grafana_server_image(image),
    }
}

fn group_of(
    cases: &'static [Case],
    default: Option<(&'static str, &'static [Spec])>,
    image: &str,
) -> Option<(&'static str, &'static [Spec])> {
    let name = image.split(':').next().unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    cases
        .iter()
        .find(|(t, _, _)| passes(t, name, image))
        .map(|(_, group, specs)| (*group, *specs))
        .or(default)
}

/// `$this->environment_variables()->where('key', ...)->first()` over the service's rows.
fn find_row<'a>(
    rows: &'a [(Record, Option<String>)],
    keys: &[&str],
) -> Option<&'a (Record, Option<String>)> {
    keys.iter().find_map(|k| {
        rows.iter()
            .find(|(r, _)| r.get("key").and_then(Value::as_str) == Some(*k))
    })
}

/// `Service::extraFields()` followed by the shaping in `StackForm::mount`, in display order.
///
/// `rows` are the service's variables with their decrypted values.
pub fn stack_fields(
    application_images: &[String],
    database_images: &[String],
    rows: &[(Record, Option<String>)],
    hide_passwords: bool,
) -> Vec<StackField> {
    // `$fields->put($group, ...)`: a later resource of the same group replaces the earlier one.
    let mut groups: Vec<(&'static str, &'static [Spec])> = Vec::new();
    for image in application_images {
        if let Some((g, specs)) =
            group_of(APPLICATION_CASES, Some(("Admin", CONTAINS_DEFAULT)), image)
        {
            put(&mut groups, g, specs);
        }
    }
    for image in database_images {
        if let Some((g, specs)) = group_of(DATABASE_CASES, None, image) {
            put(&mut groups, g, specs);
        }
    }
    // `$this->fields->put($key, ...)`: keyed by variable.
    let mut fields: Vec<(String, StackField)> = Vec::new();
    for (group, specs) in groups {
        let mut data: Vec<(&str, StackField)> = Vec::new();
        for spec in specs {
            let row = find_row(rows, spec.keys);
            if spec.guarded && row.is_none() {
                continue;
            }
            let (key, mut value) = match spec.literal {
                Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                None => (
                    row.and_then(|(r, _)| r.get("key").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_owned(),
                    row.and_then(|(_, v)| v.clone()),
                ),
            };
            let mut is_locked = false;
            // The closing `map` of extraFields(): a `$SERVICE_*` value reads that variable.
            if let Some(reference) = value.as_deref().and_then(|v| v.strip_prefix('$'))
                && reference.starts_with("SERVICE_")
                && let Some((r, v)) = find_row(rows, &[reference])
            {
                if r.get("is_shown_once")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    value = None;
                    is_locked = true;
                } else {
                    value.clone_from(v);
                }
            }
            put(
                &mut data,
                spec.label,
                StackField {
                    service_name: group.to_owned(),
                    key,
                    name: spec.label.to_owned(),
                    value,
                    rules: spec.rules.unwrap_or("nullable").to_owned(),
                    is_password: spec.password,
                    custom_helper: spec.helper.map(str::to_owned),
                    sort_order: spec.sort_order,
                    is_locked,
                },
            );
        }
        for (_, field) in data {
            put(&mut fields, field.key.clone(), field);
        }
    }
    // `groupBy('serviceName')`, then `sortBy(sortOrder ?? (isPassword ? 1 : 0))` in each group.
    let mut grouped: Vec<(String, Vec<StackField>)> = Vec::new();
    for (_, field) in fields {
        match grouped.iter_mut().find(|(g, _)| *g == field.service_name) {
            Some((_, list)) => list.push(field),
            None => grouped.push((field.service_name.clone(), vec![field])),
        }
    }
    let mut out: Vec<StackField> = Vec::new();
    for (_, mut group) in grouped {
        group.sort_by_key(|f| f.sort_order.unwrap_or(i64::from(f.is_password)));
        out.extend(group);
    }
    // `lockedFieldKeys()`.
    let shown_once: Vec<String> = rows
        .iter()
        .filter(|(r, _)| {
            r.get("is_shown_once")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|(r, _)| r.get("key").and_then(Value::as_str).map(str::to_owned))
        .collect();
    out.into_iter()
        .map(|mut field| {
            field.is_locked = field.is_locked || shown_once.contains(&field.key);
            if field.is_locked || (hide_passwords && field.is_password) {
                field.value = None;
            }
            field
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, value: &str, once: bool) -> (Record, Option<String>) {
        let mut r = Record::new();
        r.insert("key".into(), json!(key));
        r.insert("is_shown_once".into(), json!(once));
        (r, Some(value.to_owned()))
    }

    #[test]
    fn default_case_sorts_passwords_last() {
        let rows = [
            row("SERVICE_PASSWORD_ADMIN", "pw", false),
            row("SERVICE_USER_ADMIN", "me", false),
            row("ADMIN_EMAIL", "a@b.c", false),
        ];
        let f = stack_fields(&["acme/app:1".into()], &[], &rows, false);
        let names: Vec<&str> = f.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["User", "Email", "Password"]);
        assert_eq!(f[0].service_name, "Admin");
    }

    #[test]
    fn database_cases_take_the_first_listed_key() {
        let rows = [
            row("MYSQL_USER", "b", false),
            row("SERVICE_USER_MYSQL", "a", false),
        ];
        let f = stack_fields(&[], &["mysql:8".into()], &rows, false);
        assert_eq!(f[0].key, "SERVICE_USER_MYSQL");
    }

    #[test]
    fn service_reference_of_a_locked_variable_locks_the_field() {
        let rows = [
            row("LABEL_STUDIO_USERNAME", "$SERVICE_USER_X", false),
            row("SERVICE_USER_X", "secret", true),
        ];
        let f = stack_fields(&["heartexlabs/label-studio:1".into()], &[], &rows, false);
        assert!(f[0].is_locked);
        assert_eq!(f[0].value, None);
    }

    #[test]
    fn grafana_server_images_only() {
        assert!(is_grafana_server_image("grafana/grafana:11"));
        assert!(is_grafana_server_image("ghcr.io/grafana/grafana-oss:11"));
        assert!(!is_grafana_server_image("grafana/loki:3"));
    }
}
