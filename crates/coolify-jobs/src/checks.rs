//! Pure parts of server validation: output parsing and the messages upstream
//! stores in `servers.validation_logs`.

use std::sync::LazyLock;

use regex::Regex;

/// `SUPPORTED_OS` (bootstrap/helpers/constants.php).
const SUPPORTED_OS: &[&str] = &[
    "ubuntu debian raspbian pop",
    "centos fedora rhel ol rocky amzn almalinux",
    "sles opensuse-leap opensuse-tumbleweed",
    "arch",
    "alpine",
];

/// `config('constants.docker.minimum_required_version')`.
pub const MINIMUM_DOCKER_VERSION: &str = "24.0";

const DOCKER_DOCS: &str = "https://docs.docker.com/engine/install/#server";
const OPENSSH_DOCS: &str = "https://coolify.io/docs/knowledge-base/server/openssh";

/// PHP truthiness of a string: `""` and `"0"` are false.
pub fn php_truthy(s: Option<&str>) -> bool {
    s.is_some_and(|v| !v.is_empty() && v != "0")
}

fn php_trim(s: &str) -> &str {
    coolify_remote::text::php_trim(s)
}

/// `Str::before`: the whole string when the needle is missing.
fn before<'a>(s: &'a str, needle: &str) -> &'a str {
    s.find(needle).map_or(s, |i| &s[..i])
}

/// `Str::after`: the whole string when the needle is missing.
fn after<'a>(s: &'a str, needle: &str) -> &'a str {
    s.find(needle).map_or(s, |i| &s[i + needle.len()..])
}

/// `Server::supportedOsFromOsRelease`: the matching SUPPORTED_OS group, from
/// `ID` first, then each `ID_LIKE` token.
pub fn supported_os_from_os_release(os_release: Option<&str>) -> Option<&'static str> {
    let mut id: Option<String> = None;
    let mut id_like: Option<String> = None;
    for line in os_release.unwrap_or_default().split('\n') {
        let item = php_trim(line);
        let value = after(item, "=").to_lowercase().replace(['"', '\''], "");
        match before(item, "=") {
            "ID" => id = Some(value),
            "ID_LIKE" => id_like = Some(value),
            _ => {}
        }
    }
    let like = id_like.unwrap_or_default();
    let candidates = id
        .into_iter()
        .chain(like.split(' ').map(str::to_owned))
        .filter(|c| php_truthy(Some(c)));
    for candidate in candidates {
        let matches: Vec<&str> = SUPPORTED_OS
            .iter()
            .copied()
            .filter(|g| g.contains(candidate.as_str()))
            .collect();
        if let [only] = matches.as_slice() {
            return Some(only);
        }
    }
    None
}

static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+\.\d+(?:\.\d+)?").unwrap_or_else(|_| unreachable!()));

/// PHP `(int)` of a digit string, saturating like PHP on overflow.
fn php_int(digits: &str) -> i64 {
    digits
        .parse()
        .unwrap_or(if digits.is_empty() { 0 } else { i64::MAX })
}

/// `parseDockerEngineVersion`: `major.minor.patch` from the first version-looking token.
pub fn parse_docker_engine_version(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    if php_trim(raw).is_empty() {
        return None;
    }
    let m = VERSION.find(raw)?;
    let mut parts = m.as_str().split('.');
    let mut next = || parts.next().map_or(0, php_int);
    Some(format!("{}.{}.{}", next(), next(), next()))
}

/// `dockerEngineVersionFromJson`: `Server.Version` of `docker version --format json`.
pub fn docker_engine_version_from_json(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    if php_trim(raw).is_empty() {
        return None;
    }
    let decoded: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !(decoded.is_object() || decoded.is_array()) {
        return None;
    }
    let version = decoded.get("Server")?.get("Version")?.as_str()?;
    parse_docker_engine_version(Some(version))
}

/// Leading integer of a string, as PHP's `(int)` cast reads it.
fn php_int_prefix(s: &str) -> i64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    let (sign, digits) = match t.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    sign * php_int(&digits[..end])
}

/// `checkMinimumDockerEngineVersion`: the version when its major is new enough.
pub fn check_minimum_docker_engine_version(version: Option<&str>) -> Option<&str> {
    let major = php_int_prefix(before(version.unwrap_or_default(), "."));
    let required = php_int_prefix(before(MINIMUM_DOCKER_VERSION, "."));
    if major < required { None } else { version }
}

/// `htmlspecialchars($s, ENT_QUOTES, 'UTF-8')`.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Which upstream entry point's wording to use. The API runs the
/// `ValidateServer` action; the web UI runs Livewire `ValidateAndInstall`
/// (without installing), whose messages differ slightly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wording {
    Api,
    Ui,
}

pub const TRANSFERRED: &str =
    "This server was transferred to another Coolify instance and cannot be revalidated here.";

pub fn unreachable_message(error: &str) -> String {
    format!(
        "Server is not reachable. Please validate your configuration and connection.<br>Check this <a target=\"_blank\" class=\"text-black underline dark:text-white\" href=\"{OPENSSH_DOCS}\">documentation</a> for further help. <br><br><div class=\"text-error\">Error: {}</div>",
        html_escape(error)
    )
}

fn docs_link(w: Wording) -> String {
    match w {
        Wording::Api => format!(
            "<a target=\"_blank\" class=\"text-black underline dark:text-white\" href=\"{DOCKER_DOCS}\">documentation</a>"
        ),
        Wording::Ui => format!(
            "<a target=\"_blank\" class=\"underline\" href=\"{DOCKER_DOCS}\">documentation</a>"
        ),
    }
}

pub fn os_message(w: Wording) -> String {
    format!(
        "Server OS type is not supported. Please install Docker manually before continuing: {}.",
        docs_link(w)
    )
}

pub fn prerequisites_message(w: Wording, missing: &[&str]) -> String {
    let list = missing.join(", ");
    match w {
        Wording::Api => format!(
            "Prerequisites ({list}) are not installed. Please install them before continuing or use the validation with installation endpoint."
        ),
        Wording::Ui => format!(
            "Prerequisites ({list}) are not installed. Please install them before continuing."
        ),
    }
}

pub fn docker_missing_message(w: Wording) -> String {
    format!(
        "Docker Engine is not installed. Please install Docker manually before continuing: {}.",
        docs_link(w)
    )
}

/// `Server::dockerEngineVersionError`.
pub fn docker_version_message(stored_version: Option<&str>, user: &str) -> String {
    let required = before(MINIMUM_DOCKER_VERSION, ".");
    match stored_version {
        None => {
            let command = if user != "root" {
                "sudo docker version --format json"
            } else {
                "docker version --format json"
            };
            format!(
                "Coolify could not read the Docker Engine version. Run \"{command}\" on the server as the {} user. It must exit without an error and print only JSON. Text from shell startup files (for example ~/.bashrc) breaks the output.",
                html_escape(user)
            )
        }
        Some(v) => format!(
            "Docker Engine {v} is installed, but Coolify needs version {required} or newer. Please upgrade Docker: <a target=\"_blank\" class=\"underline\" href=\"{DOCKER_DOCS}\">documentation</a>."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cases from upstream tests/Unit/ServerSupportedOsTest.php.
    #[test]
    fn supported_os() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "ID=ubuntu\nID_LIKE=debian",
                Some("ubuntu debian raspbian pop"),
            ),
            ("ID=arch", Some("arch")),
            (
                "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"",
                Some("centos fedora rhel ol rocky amzn almalinux"),
            ),
            ("NAME=\"Omarchy\"\nID=omarchy\nID_LIKE=arch", Some("arch")),
            ("ID=manjaro\nID_LIKE=arch", Some("arch")),
            (
                "ID=linuxmint\nID_LIKE=\"ubuntu debian\"",
                Some("ubuntu debian raspbian pop"),
            ),
            (
                "ID=zorin\nID_LIKE='ubuntu debian'",
                Some("ubuntu debian raspbian pop"),
            ),
            ("ID=gentoo", None),
            ("ID=foo\nID_LIKE=bar", None),
            ("", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                supported_os_from_os_release(Some(input)),
                *expected,
                "{input}"
            );
        }
        assert_eq!(supported_os_from_os_release(None), None);
        // Substring matching, as upstream: "ian" is only in the Debian group.
        assert_eq!(
            supported_os_from_os_release(Some("ID=ian")),
            Some("ubuntu debian raspbian pop")
        );
        // Ambiguous substrings match nothing.
        assert_eq!(supported_os_from_os_release(Some("ID=e")), None);
    }

    #[test]
    fn docker_versions() {
        assert_eq!(
            parse_docker_engine_version(Some("28.0.1")),
            Some("28.0.1".into())
        );
        assert_eq!(
            parse_docker_engine_version(Some("v2.38")),
            Some("2.38.0".into())
        );
        assert_eq!(
            parse_docker_engine_version(Some("Docker version 27.03.1-ce, build x")),
            Some("27.3.1".into())
        );
        assert_eq!(parse_docker_engine_version(Some("  ")), None);
        assert_eq!(parse_docker_engine_version(Some("none")), None);
        assert_eq!(parse_docker_engine_version(None), None);

        let json =
            r#"{"Client":{"Version":"28.0.1"},"Server":{"Version":"28.0.1","ApiVersion":"1.48"}}"#;
        assert_eq!(
            docker_engine_version_from_json(Some(json)),
            Some("28.0.1".into())
        );
        assert_eq!(
            docker_engine_version_from_json(Some(&format!("Welcome!\n{json}"))),
            None
        );
        assert_eq!(
            docker_engine_version_from_json(Some(r#"{"Server":{"Version":28}}"#)),
            None
        );
        assert_eq!(
            docker_engine_version_from_json(Some(r#"{"Client":{}}"#)),
            None
        );

        assert_eq!(
            check_minimum_docker_engine_version(Some("24.0.0")),
            Some("24.0.0")
        );
        assert_eq!(check_minimum_docker_engine_version(Some("20.10.7")), None);
        assert_eq!(check_minimum_docker_engine_version(None), None);
    }

    #[test]
    fn messages() {
        assert_eq!(
            unreachable_message("user@h: Permission denied (publickey)."),
            "Server is not reachable. Please validate your configuration and connection.<br>Check this <a target=\"_blank\" class=\"text-black underline dark:text-white\" href=\"https://coolify.io/docs/knowledge-base/server/openssh\">documentation</a> for further help. <br><br><div class=\"text-error\">Error: user@h: Permission denied (publickey).</div>"
        );
        assert!(
            unreachable_message("<script>'&\"")
                .ends_with("Error: &lt;script&gt;&#039;&amp;&quot;</div>")
        );
        assert_eq!(
            prerequisites_message(Wording::Api, &["git", "jq"]),
            "Prerequisites (git, jq) are not installed. Please install them before continuing or use the validation with installation endpoint."
        );
        assert_eq!(
            docker_version_message(None, "ubuntu"),
            "Coolify could not read the Docker Engine version. Run \"sudo docker version --format json\" on the server as the ubuntu user. It must exit without an error and print only JSON. Text from shell startup files (for example ~/.bashrc) breaks the output."
        );
        assert_eq!(
            docker_version_message(Some("20.10.7"), "root"),
            "Docker Engine 20.10.7 is installed, but Coolify needs version 24 or newer. Please upgrade Docker: <a target=\"_blank\" class=\"underline\" href=\"https://docs.docker.com/engine/install/#server\">documentation</a>."
        );
    }
}
