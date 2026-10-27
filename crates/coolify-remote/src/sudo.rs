//! Port of upstream `parseCommandsByLineForSudo` (`bootstrap/helpers/sudo.php`).
//!
//! Commands for a server whose SSH user is not root get `sudo` inserted line by
//! line. The rules are heuristics over shell text, so they are reproduced as
//! they are (including their quirks) rather than improved: a script must elevate
//! the same commands on either implementation.

use std::sync::LazyLock;

use regex::Regex;

use crate::text::php_trim;

/// PCRE `\s` without the `u` flag.
const WS: &str = r"[\t\n\x0B\x0C\r ]";

const BASH_KEYWORDS: &[&str] = &[
    "cd", "command", "declare", "echo", "export", "local", "readonly", "return", "true", "if",
    "fi", "for", "done", "while", "until", "case", "esac", "select", "then", "else", "elif",
    "break", "continue", "do",
];

const SYSTEM_PATHS: &[&str] = &[
    "/var", "/etc", "/usr", "/opt", "/sys", "/proc", "/dev", "/bin", "/sbin", "/lib", "/lib64",
    "/boot", "/root", "/home", "/media", "/mnt", "/srv", "/run",
];

fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", WS)).unwrap_or_else(|_| unreachable!("static pattern"))
}

static NEGATION: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*!\s+"));
static NEGATION_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(\s*(?:!\s+)+)"));
static IF_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(\s*if\s+(?:!\s+)*)"));
static KEYWORDS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    BASH_KEYWORDS
        .iter()
        .map(|k| re(&format!(r"^{}(\s|;|$)", regex::escape(k))))
        .collect()
});
static MKDIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?s)^(sudo mkdir -p ([^\t\n\x0B\x0C\r ;&|<>]+))({WS}*(?:(?:&&|;).*)?)$"
    ))
    .unwrap_or_else(|_| unreachable!("static pattern"))
});
static SINGLE_SUDO_SHELL: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*sudo (?:ba)?sh -c '(?:[^']|'\\'')*'\s*$"));

/// Rewrites each line of a non-root server's script to run elevated.
pub fn parse_commands_by_line_for_sudo(commands: &[String], user: &str) -> Vec<String> {
    commands
        .iter()
        .map(|line| prefix_line(line))
        .map(|line| add_ownership_after_mkdir(&line, user))
        .map(|line| elevate_operators(&line))
        .collect()
}

fn prefix_line(line: &str) -> String {
    let trimmed = php_trim(line);
    if trimmed.starts_with('#') {
        return line.to_owned();
    }
    // Negation belongs to the shell, before the elevated command.
    if NEGATION.is_match(line) {
        return NEGATION_PREFIX.replacen(line, 1, "${1}sudo ").into_owned();
    }
    for (keyword, pattern) in BASH_KEYWORDS.iter().zip(KEYWORDS.iter()) {
        if pattern.is_match(trimmed) {
            if *keyword == "if" {
                return IF_PREFIX.replacen(line, 1, "${1}sudo ").into_owned();
            }
            return line.to_owned();
        }
    }
    format!("sudo {line}")
}

fn should_change_ownership(path: &str) -> bool {
    let path = php_trim(path);
    if SYSTEM_PATHS
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
    {
        return false;
    }
    path.starts_with("/data/coolify") || path.starts_with("/tmp/coolify")
}

fn add_ownership_after_mkdir(line: &str, user: &str) -> String {
    let Some(m) = MKDIR.captures(line) else {
        return line.to_owned();
    };
    let path = &m[2];
    if !should_change_ownership(path) {
        return line.to_owned();
    }
    // No sudo here: the && rule adds it.
    format!(
        "{} && find {path} -user root -exec chown -h {user}:{user} {{}} + && chmod o-rwx {path}{}",
        &m[1],
        m.get(3).map_or("", |g| g.as_str())
    )
}

fn elevate_operators(line: &str) -> String {
    if SINGLE_SUDO_SHELL.is_match(line) {
        return line.to_owned();
    }
    let complex_pipe = line.contains(" | sh")
        || line.contains(" | bash")
        || line.contains(" sh -c ")
        || (line.contains(" | ") && (line.contains("||") || line.contains("&&")));

    if complex_pipe && let Some(rest) = line.strip_prefix("sudo ") {
        return format!("sudo bash -c '{}'", rest.replace('\'', r"'\''"));
    }

    let mut line = line.to_owned();
    if line.contains("$(") {
        line = line.replace("$(", "$(sudo ");
    }
    if !complex_pipe && line.contains("||") {
        line = line.replace("||", "|| sudo");
    }
    if !complex_pipe && line.contains("&&") {
        line = line.replace("&&", "&& sudo");
    }
    if !complex_pipe {
        line = add_sudo_after_unquoted_pipes(&line);
    }
    line
}

/// Puts sudo after each ` | ` that the shell sees as a pipe; pipes inside quotes stay.
fn add_sudo_after_unquoted_pipes(line: &str) -> String {
    let b = line.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 16);
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote.is_none() && b[i..].starts_with(b" | ") {
            out.extend_from_slice(if b[i + 3..].starts_with(b"sudo ") {
                b" | "
            } else {
                b" | sudo "
            });
            i += 3;
            continue;
        }
        out.push(c);
        if c == b'\\' && quote != Some(b'\'') && i + 1 < b.len() {
            i += 1;
            out.push(b[i]);
        } else if quote.is_none() && (c == b'\'' || c == b'"') {
            quote = Some(c);
        } else if Some(c) == quote {
            quote = None;
        }
        i += 1;
    }
    // Only ASCII was inserted, at ASCII positions.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(lines: &[&str]) -> Vec<String> {
        let v: Vec<String> = lines.iter().map(|s| (*s).to_owned()).collect();
        parse_commands_by_line_for_sudo(&v, "ubuntu")
    }
    fn one(line: &str) -> String {
        p(&[line]).remove(0)
    }

    // Cases from upstream tests/Unit/ParseCommandsByLineForSudoTest.php.
    #[test]
    fn upstream_single_line_cases() {
        let cases: &[(&str, &str)] = &[
            (
                "curl https://releases.rancher.com/install-docker/27.3.sh | sh || curl https://get.docker.com | sh",
                "sudo bash -c 'curl https://releases.rancher.com/install-docker/27.3.sh | sh || curl https://get.docker.com | sh'",
            ),
            (
                "docker exec 'db-name-uuid' pg_dump --username 'postgres' 'app' > '/backups/db-name.dump'",
                "sudo docker exec 'db-name-uuid' pg_dump --username 'postgres' 'app' > '/backups/db-name.dump'",
            ),
            (
                "curl https://example.com/script.sh | bash",
                "sudo bash -c 'curl https://example.com/script.sh | bash'",
            ),
            (
                "curl https://example.com | sh && echo \"done\"",
                "sudo bash -c 'curl https://example.com | sh && echo \"done\"'",
            ),
            (
                "curl https://example.com | sh -c 'echo \"test\"'",
                "sudo bash -c 'curl https://example.com | sh -c '\\''echo \"test\"'\\'''",
            ),
            ("apt-get update", "sudo apt-get update"),
            (
                "mkdir -p /foo && chown ubuntu /foo",
                "sudo mkdir -p /foo && sudo chown ubuntu /foo",
            ),
            (
                "command -v docker || echo \"not found\"",
                "command -v docker || sudo echo \"not found\"",
            ),
            (
                "cat file | grep pattern",
                "sudo cat file | sudo grep pattern",
            ),
            ("echo $(whoami)", "echo $(sudo whoami)"),
            ("cd /var/www", "cd /var/www"),
            ("echo \"test\"", "echo \"test\""),
            ("command -v docker", "command -v docker"),
            ("true", "true"),
            ("if command -v docker", "if sudo command -v docker"),
            (
                "if ! docker inspect coolify-proxy; then",
                "if ! sudo docker inspect coolify-proxy; then",
            ),
            (
                "\tif  !  docker inspect coolify-proxy; then",
                "\tif  !  sudo docker inspect coolify-proxy; then",
            ),
            (
                "    if ! docker ps -a --format \"{{.Names}}\" | grep -q \"^coolify-proxy$\"; then",
                "    if ! sudo docker ps -a --format \"{{.Names}}\" | sudo grep -q \"^coolify-proxy$\"; then",
            ),
            (
                "! docker inspect coolify-proxy",
                "! sudo docker inspect coolify-proxy",
            ),
            (
                "if ! ! docker inspect coolify-proxy; then",
                "if ! ! sudo docker inspect coolify-proxy; then",
            ),
            ("echo \"! docker\"", "echo \"! docker\""),
            ("fi", "fi"),
            (
                "mkdir -p /data/coolify/logs",
                "sudo mkdir -p /data/coolify/logs && sudo find /data/coolify/logs -user root -exec chown -h ubuntu:ubuntu {} + && sudo chmod o-rwx /data/coolify/logs",
            ),
            (
                "mkdir -p /tmp/coolify/cache",
                "sudo mkdir -p /tmp/coolify/cache && sudo find /tmp/coolify/cache -user root -exec chown -h ubuntu:ubuntu {} + && sudo chmod o-rwx /tmp/coolify/cache",
            ),
            (
                "mkdir -p /data/coolify/proxy > /dev/null 2>&1 || true",
                "sudo mkdir -p /data/coolify/proxy > /dev/null 2>&1 || sudo true",
            ),
            ("mkdir -p /var/log", "sudo mkdir -p /var/log"),
            (
                "docker ps || echo $(date)",
                "sudo docker ps || sudo echo $(sudo date)",
            ),
            (
                "curl https://example.com | sh -s -- --arg1 --arg2",
                "sudo bash -c 'curl https://example.com | sh -s -- --arg1 --arg2'",
            ),
            ("ifconfig eth0", "sudo ifconfig eth0"),
            ("ifup eth0", "sudo ifup eth0"),
            (
                "find /var -name \"*.log\"",
                "sudo find /var -name \"*.log\"",
            ),
            ("file /tmp/test", "sudo file /tmp/test"),
            ("docker ps", "sudo docker ps"),
            ("fortune", "sudo fortune"),
            ("formail -s procmail", "sudo formail -s procmail"),
            (
                "if [ -f /tmp/test ]; then",
                "if sudo [ -f /tmp/test ]; then",
            ),
            ("for i in 1 2 3; do", "for i in 1 2 3; do"),
            ("do", "do"),
            (
                "docker ps --format '{{.Names}} | {{.Image}}' | grep app",
                "sudo docker ps --format '{{.Names}} | {{.Image}}' | sudo grep app",
            ),
            (
                "sh -c 'docker exec '\\''db'\\'' pg_dump '\\''app'\\'' > '\\''/data/coolify/backups/app.dmp'\\'' && echo it'\\''s done | cat'",
                "sudo sh -c 'docker exec '\\''db'\\'' pg_dump '\\''app'\\'' > '\\''/data/coolify/backups/app.dmp'\\'' && echo it'\\''s done | cat'",
            ),
            (
                "docker run --rm -v $HOME/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock coolify-helper:1.0.0 bash -c 'exec 9>/root/.docker/buildx/coolify-railpack.lock; if flock -s 9; then :; else exit $?; fi; docker buildx prune --builder coolify-railpack -af; status=$?; if flock -n -x 9; then DOCKER_CONFIG=/root/.docker docker buildx stop coolify-railpack >/dev/null 2>&1; fi; exit $status' 2>/dev/null || true",
                "sudo docker run --rm -v $HOME/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock coolify-helper:1.0.0 bash -c 'exec 9>/root/.docker/buildx/coolify-railpack.lock; if flock -s 9; then :; else exit $?; fi; docker buildx prune --builder coolify-railpack -af; status=$?; if flock -n -x 9; then DOCKER_CONFIG=/root/.docker docker buildx stop coolify-railpack >/dev/null 2>&1; fi; exit $status' 2>/dev/null || sudo true",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(one(input), *expected, "input: {input}");
        }
    }

    #[test]
    fn upstream_block_cases() {
        assert_eq!(
            p(&[
                "case $1 in",
                "    start)",
                "        systemctl start service",
                "        ;;",
                "esac"
            ]),
            [
                "case $1 in",
                "sudo     start)",
                "sudo         systemctl start service",
                "sudo         ;;",
                "esac"
            ]
        );
        assert_eq!(
            p(&[
                "if [ -f /tmp/file ]; then",
                "    cat /tmp/file",
                "else",
                "    touch /tmp/file",
                "fi"
            ]),
            [
                "if sudo [ -f /tmp/file ]; then",
                "sudo     cat /tmp/file",
                "else",
                "sudo     touch /tmp/file",
                "fi"
            ]
        );
        assert_eq!(
            p(&[
                "# This is a comment",
                "    # Indented comment",
                "apt-get update"
            ]),
            [
                "# This is a comment",
                "    # Indented comment",
                "sudo apt-get update"
            ]
        );
        assert_eq!(
            p(&[
                "until [ -f /tmp/ready ]; do",
                "    echo \"Waiting...\"",
                "    sleep 1",
                "done"
            ]),
            [
                "until [ -f /tmp/ready ]; do",
                "    echo \"Waiting...\"",
                "sudo     sleep 1",
                "done"
            ]
        );
        assert_eq!(p(&["   ", ""]).len(), 2);
        assert!(p(&[]).is_empty());
    }

    #[test]
    fn mkdir_followed_by_more_commands() {
        for sep in [" &&", ";"] {
            let out = one(&format!(
                "mkdir -p /tmp/coolify/x/dynamic{sep} ls -1 /tmp/coolify/x/dynamic | head -n 1000"
            ));
            assert!(
                out.contains(
                    "find /tmp/coolify/x/dynamic -user root -exec chown -h ubuntu:ubuntu {} + && "
                ),
                "{out}"
            );
            assert!(
                out.contains(&format!("chmod o-rwx /tmp/coolify/x/dynamic{sep} ")),
                "{out}"
            );
            assert!(out.ends_with("head -n 1000'"), "{out}");
        }
    }

    #[test]
    fn proxy_loop_runs_like_upstream() {
        let lines = [
            "for i in {1..15}; do",
            "    if ! docker ps -a --format \"{{.Names}}\" | grep -q \"^coolify-proxy$\"; then",
            "        echo \"Container removed successfully.\"",
            "        break",
            "    fi",
            "    echo \"Waiting for container to be removed... ($i/15)\"",
            "done",
        ];
        let script = format!(
            "sudo() {{ \"$@\"; }}\ndocker() {{ printf '%s\\n' \"$CONTAINERS\"; }}\n{}",
            p(&lines).join("\n")
        );
        for (containers, expected) in [
            ("", "Container removed successfully.\n".to_owned()),
            (
                "another-container",
                "Container removed successfully.\n".to_owned(),
            ),
            (
                "coolify-proxy",
                (1..=15)
                    .map(|n| format!("Waiting for container to be removed... ({n}/15)\n"))
                    .collect(),
            ),
        ] {
            let Ok(out) = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .env("CONTAINERS", containers)
                .output()
            else {
                return; // no bash on this machine
            };
            assert!(out.status.success());
            assert_eq!(String::from_utf8_lossy(&out.stderr), "");
            assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
        }
    }
}
