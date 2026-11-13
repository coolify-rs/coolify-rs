//! The HEALTHCHECK of a Dockerfile, read like Docker does (bootstrap/helpers/shared.php, Coolify
//! v4.7): `dockerfileHealthcheckInstruction()`, `dockerfileHealthcheckOptions()` and
//! `parseDockerfileInterval()`.

use std::sync::LazyLock;

use regex::Regex;

/// PHP `trim()`.
fn php_trim(s: &str) -> &str {
    s.trim_matches([' ', '\n', '\r', '\t', '\x0B', '\0'])
}

/// `preg_split('/\R/', $text)`.
fn split_lines(text: &str) -> Vec<&str> {
    static NEWLINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\r\n|[\n\x0B\x0C\r\u{85}\u{2028}\u{2029}]").unwrap_or_else(|_| unreachable!())
    });
    NEWLINE.split(text).collect()
}

struct Stage {
    name: Option<String>,
    parent: String,
    healthcheck: Option<String>,
}

/// `dockerfileHealthcheckInstruction($dockerfile, $target)`: the HEALTHCHECK instruction that
/// applies to the image built from the Dockerfile.
///
/// Docker builds the target stage, or the last stage when no target is set. A stage keeps the
/// HEALTHCHECK of an earlier stage that it starts FROM, and the last HEALTHCHECK of a stage wins.
/// `None` when no HEALTHCHECK applies or when it is HEALTHCHECK NONE.
pub fn dockerfile_healthcheck_instruction(
    dockerfile: &str,
    target: Option<&str>,
) -> Option<String> {
    // The "# escape=`" parser directive (Windows Dockerfiles) is valid only in the comment lines at the top of the file.
    static ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?im)\A(?:[ \t]*#[ \t]*[a-z]+[ \t]*=[^\r\n]*(?:\r\n|[\n\x0B\x0C\r\u{85}\u{2028}\u{2029}]))*?[ \t]*#[ \t]*escape[ \t]*=[ \t]*([\\`])[ \t]*$",
        )
        .unwrap_or_else(|_| unreachable!())
    });
    static FROM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?")
            .unwrap_or_else(|_| unreachable!())
    });
    static HEALTHCHECK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^HEALTHCHECK(\s|$)").unwrap_or_else(|_| unreachable!()));
    static NONE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^HEALTHCHECK(?:\s+--\S+)*\s+NONE$").unwrap_or_else(|_| unreachable!())
    });
    let escape = ESCAPE
        .captures(dockerfile)
        .and_then(|c| c.get(1))
        .map_or("\\", |m| m.as_str());

    let mut instructions: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in split_lines(dockerfile) {
        let line = php_trim(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let continues = line.ends_with(escape);
        let part = if continues {
            &line[..line.len() - escape.len()]
        } else {
            line
        };
        current = php_trim(&format!("{current} {part}")).to_owned();
        if !continues {
            instructions.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        instructions.push(current);
    }

    let mut stages: Vec<Stage> = Vec::new();
    for instruction in instructions {
        if let Some(c) = FROM.captures(&instruction) {
            stages.push(Stage {
                name: c.get(2).map(|m| m.as_str().to_lowercase()),
                parent: c
                    .get(1)
                    .map(|m| m.as_str().to_lowercase())
                    .unwrap_or_default(),
                healthcheck: None,
            });
        } else if HEALTHCHECK.is_match(&instruction)
            && let Some(stage) = stages.last_mut()
        {
            stage.healthcheck = Some(instruction);
        }
    }
    if stages.is_empty() {
        return None;
    }

    let target = target
        .map(php_trim)
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase);
    let mut index = match &target {
        None => Some(stages.len() - 1),
        Some(t) => stages.iter().rposition(|s| s.name.as_ref() == Some(t)),
    };
    while let Some(i) = index {
        let stage = &stages[i];
        if let Some(healthcheck) = &stage.healthcheck {
            return (!NONE.is_match(healthcheck)).then(|| healthcheck.clone());
        }
        index = stages[..i]
            .iter()
            .rposition(|candidate| candidate.name.as_ref() == Some(&stage.parent));
    }
    None
}

/// What `dockerfileHealthcheckOptions()` returns. An option is `None` when it is missing or not valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HealthcheckOptions {
    pub interval: Option<i64>,
    pub timeout: Option<i64>,
    pub start_period: Option<i64>,
    pub retries: Option<i64>,
}

/// `dockerfileHealthcheckOptions($instruction)`: the options of a HEALTHCHECK instruction like
/// Docker reads them: only the `--name=value` flags before CMD count. Docker uses its default for
/// a zero interval, timeout, or retries (30s, 30s, 3), so those are returned instead of 0.
pub fn dockerfile_healthcheck_options(instruction: &str) -> HealthcheckOptions {
    static FLAGS_PART: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)^HEALTHCHECK((?:\s+--[a-z-]+=(?:"[^"]*"|'[^']*'|\S+))*)"#)
            .unwrap_or_else(|_| unreachable!())
    });
    static FLAG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)--([a-z-]+)=("[^"]*"|'[^']*'|\S+)"#).unwrap_or_else(|_| unreachable!())
    });
    static RETRIES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\+?\d{1,9}$").unwrap_or_else(|_| unreachable!()));
    let flags_part = FLAGS_PART
        .captures(instruction)
        .and_then(|c| c.get(1))
        .map_or("", |m| m.as_str());
    let mut values: Vec<(String, String)> = Vec::new();
    for c in FLAG.captures_iter(flags_part) {
        let (name, value) = (&c[1], &c[2]);
        // `preg_replace('/^(["\'])(.*)\1$/', '$2', $value)`.
        let unquoted = if value.len() >= 2
            && (value.starts_with('"') || value.starts_with('\''))
            && value.ends_with(&value[..1])
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        match values.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = unquoted.to_owned(),
            None => values.push((name.to_owned(), unquoted.to_owned())),
        }
    }
    let value = |name: &str| {
        values
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, v)| v.as_str())
    };
    let interval = parse_dockerfile_interval(value("interval"));
    let timeout = parse_dockerfile_interval(value("timeout"));
    let retries = RETRIES
        .is_match(value("retries"))
        .then(|| value("retries").trim_start_matches('+').parse::<i64>().ok())
        .flatten();
    HealthcheckOptions {
        interval: if interval == Some(0) {
            Some(30)
        } else {
            interval
        },
        timeout: if timeout == Some(0) {
            Some(30)
        } else {
            timeout
        },
        start_period: parse_dockerfile_interval(value("start-period")),
        retries: if retries == Some(0) { Some(3) } else { retries },
    }
}

/// The largest value of the integer columns.
const MAX_SECONDS: i64 = 2_147_483_647;

/// `parseDockerfileInterval($duration)`: a Dockerfile HEALTHCHECK duration (Go duration syntax,
/// for example "1m30s" or "1.5m") in whole seconds. A value without a unit is read as seconds. A
/// positive value below one second becomes 1, so it does not read as zero. `None` when the value
/// is not a valid duration or does not fit the integer column.
pub fn parse_dockerfile_interval(duration: &str) -> Option<i64> {
    static PART: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+(?:\.\d*)?|\.\d+)(ns|us|\u{b5}s|\u{3bc}s|ms|s|m|h)")
            .unwrap_or_else(|_| unreachable!())
    });
    let duration = duration.strip_prefix('+').unwrap_or(duration);
    if !duration.is_empty() && duration.bytes().all(|b| b.is_ascii_digit()) {
        return duration.parse::<i64>().ok().filter(|v| *v <= MAX_SECONDS);
    }
    let mut seconds = 0.0_f64;
    let mut matched = String::new();
    for c in PART.captures_iter(duration) {
        matched.push_str(&c[0]);
        let value: f64 = c[1]
            .parse()
            .ok()
            .or_else(|| format!("{}0", &c[1]).parse().ok())?;
        let unit = match &c[2] {
            "ns" => 1e-9,
            "us" | "\u{b5}s" | "\u{3bc}s" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            _ => 3600.0,
        };
        seconds += value * unit;
    }
    if matched.is_empty() || matched != duration {
        return None;
    }
    if seconds > 0.0 && seconds < 1.0 {
        return Some(1);
    }
    // Round away float noise (0.3m = 17.999...s), then drop the fraction: the columns store whole seconds.
    let seconds = ((seconds * 1e6).round() / 1e6).floor();
    (seconds <= MAX_SECONDS as f64).then_some(seconds as i64)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    pub(crate) const MULTI_STAGE_DOCKERFILE: &str = "FROM node:20-alpine AS builder\nRUN npm ci\n\nFROM node:20-alpine AS api\nCOPY --from=builder /app/dist ./dist\nHEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \\\n    CMD node -e \"require('http').get('http://localhost:3000/health')\"\nCMD [\"node\", \"dist/main.js\"]\n\n# === Stage: worker ===\nFROM node:20-alpine AS worker\nCOPY --from=builder /app/dist ./dist\n# No HEALTHCHECK - this stage has no HTTP server\nCMD [\"node\", \"dist/worker.js\"]";

    /// "finds the HEALTHCHECK that applies to the built image".
    #[test]
    fn finds_the_healthcheck_that_applies_to_the_built_image() {
        let cases: [(&str, &str, Option<&str>, Option<&str>); 9] = [
            (
                "last stage when no target is set",
                MULTI_STAGE_DOCKERFILE,
                None,
                None,
            ),
            (
                "HEALTHCHECK only in a builder stage",
                "FROM node AS build\nHEALTHCHECK CMD true\nFROM nginx\nCOPY --from=build /a /b",
                None,
                None,
            ),
            (
                "inherited from a parent stage",
                "FROM node AS base\nHEALTHCHECK --interval=10s CMD true\nFROM base AS app\nCMD run",
                Some("app"),
                Some("HEALTHCHECK --interval=10s CMD true"),
            ),
            (
                "HEALTHCHECK NONE overrides the parent stage",
                "FROM node AS base\nHEALTHCHECK CMD true\nFROM base AS worker\nHEALTHCHECK NONE",
                Some("worker"),
                None,
            ),
            (
                "last HEALTHCHECK of a stage wins",
                "FROM node\nHEALTHCHECK --interval=1s CMD a\nHEALTHCHECK --interval=2s CMD b",
                None,
                Some("HEALTHCHECK --interval=2s CMD b"),
            ),
            (
                "FROM with platform flag",
                "FROM --platform=linux/amd64 node AS app\nhealthcheck --retries=2 CMD true",
                Some("app"),
                Some("healthcheck --retries=2 CMD true"),
            ),
            (
                "target that does not exist",
                "FROM node AS app\nHEALTHCHECK CMD true",
                Some("missing"),
                None,
            ),
            (
                "comment mentions HEALTHCHECK",
                "FROM node\n# HEALTHCHECK is not needed\nCMD run",
                None,
                None,
            ),
            ("empty Dockerfile", "", None, None),
        ];
        for (name, dockerfile, target, expected) in cases {
            assert_eq!(
                dockerfile_healthcheck_instruction(dockerfile, target).as_deref(),
                expected,
                "{name}"
            );
        }
        // The target is compared without case and surrounding spaces.
        assert!(
            dockerfile_healthcheck_instruction(MULTI_STAGE_DOCKERFILE, Some(" API "))
                .is_some_and(|i| i.starts_with("HEALTHCHECK --interval=30s --timeout=5s"))
        );
        assert_eq!(
            dockerfile_healthcheck_instruction(MULTI_STAGE_DOCKERFILE, Some("worker")),
            None
        );
    }

    /// "reads Dockerfile syntax around HEALTHCHECK like Docker".
    #[test]
    fn reads_dockerfile_syntax_around_healthcheck_like_docker() {
        let cases: [(&str, &str, Option<&str>); 7] = [
            (
                "escape directive",
                "# escape=`\nFROM scratch\nHEALTHCHECK --interval=24s `\n  --timeout=6s CMD /probe",
                Some("HEALTHCHECK --interval=24s --timeout=6s CMD /probe"),
            ),
            (
                "escape directive after syntax directive",
                "# syntax=docker/dockerfile:1\n# escape=`\nFROM scratch\nHEALTHCHECK --interval=49s `\n  CMD /probe",
                Some("HEALTHCHECK --interval=49s CMD /probe"),
            ),
            (
                "escape comment after an instruction",
                "FROM scratch\n# escape=`\nHEALTHCHECK --interval=50s \\\n  CMD /probe",
                Some("HEALTHCHECK --interval=50s CMD /probe"),
            ),
            (
                "backslash in command with backtick escape",
                "# escape=`\nFROM scratch\nHEALTHCHECK CMD c:\\probe.exe\\",
                Some("HEALTHCHECK CMD c:\\probe.exe\\"),
            ),
            (
                "continuation with trailing space",
                "FROM scratch\nHEALTHCHECK --interval=48s \\  \n  CMD /probe",
                Some("HEALTHCHECK --interval=48s CMD /probe"),
            ),
            (
                "CRLF line endings",
                "FROM scratch\r\nHEALTHCHECK --interval=27s \\\r\n  CMD /probe\r\n",
                Some("HEALTHCHECK --interval=27s CMD /probe"),
            ),
            (
                "NONE with flags",
                "FROM scratch\nHEALTHCHECK --interval=44s NONE",
                None,
            ),
        ];
        for (name, dockerfile, expected) in cases {
            assert_eq!(
                dockerfile_healthcheck_instruction(dockerfile, None).as_deref(),
                expected,
                "{name}"
            );
        }
    }

    /// "converts a Docker duration to whole seconds".
    #[test]
    fn converts_a_docker_duration_to_whole_seconds() {
        let cases: [(&str, Option<i64>); 18] = [
            ("30s", Some(30)),
            ("30", Some(30)),
            ("2m", Some(120)),
            ("1h2m3s", Some(3723)),
            ("1.5m", Some(90)),
            (".5m", Some(30)),
            ("0.3m1.2s", Some(19)),
            ("1500ms", Some(1)),
            ("2000000\u{b5}s", Some(2)),
            ("3000000000ns", Some(3)),
            ("+30s", Some(30)),
            ("0s", Some(0)),
            ("500ms", Some(1)),
            ("-30s", None),
            ("5x", None),
            ("5sec", None),
            ("", None),
            ("3000000h", None),
        ];
        for (duration, expected) in cases {
            assert_eq!(
                parse_dockerfile_interval(duration),
                expected,
                "{duration:?}"
            );
        }
        assert_eq!(parse_dockerfile_interval("2147483648"), None);
        assert_eq!(parse_dockerfile_interval("2147483647"), Some(2_147_483_647));
        assert_eq!(parse_dockerfile_interval("1."), None);
        assert_eq!(parse_dockerfile_interval("1.s"), Some(1));
    }

    fn options(
        interval: Option<i64>,
        timeout: Option<i64>,
        start_period: Option<i64>,
        retries: Option<i64>,
    ) -> HealthcheckOptions {
        HealthcheckOptions {
            interval,
            timeout,
            start_period,
            retries,
        }
    }

    /// "reads only the HEALTHCHECK flags before CMD, like Docker".
    #[test]
    fn reads_only_the_healthcheck_flags_before_cmd() {
        let cases: [(&str, HealthcheckOptions); 8] = [
            (
                r#"HEALTHCHECK --interval=10s --timeout=3s --start-period=1m --retries=7 CMD ["/probe"]"#,
                options(Some(10), Some(3), Some(60), Some(7)),
            ),
            (
                r#"HEALTHCHECK CMD ["/probe"]"#,
                options(None, None, None, None),
            ),
            (
                r#"HEALTHCHECK --interval="18s" --timeout='4s' CMD /probe"#,
                options(Some(18), Some(4), None, None),
            ),
            (
                "HEALTHCHECK --interval=19s CMD /probe --timeout=99s --retries=9",
                options(Some(19), None, None, None),
            ),
            (
                "HEALTHCHECK --interval=0s --timeout=0 --start-period=0s --retries=0 CMD /probe",
                options(Some(30), Some(30), Some(0), Some(3)),
            ),
            (
                "HEALTHCHECK --start-interval=46s CMD /probe",
                options(None, None, None, None),
            ),
            (
                "HEALTHCHECK --retries=+4 CMD /probe",
                options(None, None, None, Some(4)),
            ),
            (
                "HEALTHCHECK --retries=1.5 CMD /probe",
                options(None, None, None, None),
            ),
        ];
        for (instruction, expected) in cases {
            assert_eq!(
                dockerfile_healthcheck_options(instruction),
                expected,
                "{instruction}"
            );
        }
    }
}
