//! `App\Services\ImageScan\DockerfileBaseImage`: reads the base image of the stage a Dockerfile
//! build produces (spec 7.4).

use std::sync::LazyLock;

use coolify_remote::text::php_trim;
use regex::Regex;

use super::super::image_ref::ImageRef;

/// `DockerfileBaseImage::MAX_STAGES`.
const MAX_STAGES: usize = 100;

/// PCRE `\s` without the UTF-8 mode.
const SPACE: &str = r"[ \t\n\x0B\x0C\r]";
const NOT_SPACE: &str = r"[^ \t\n\x0B\x0C\r]";

/// `/\A#\s*([A-Za-z]+)\s*=\s*(\S+)\s*\z/`: a parser directive such as `# escape=\``.
static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\A#{SPACE}*([A-Za-z]+){SPACE}*={SPACE}*({NOT_SPACE}+){SPACE}*\z"
    ))
    .unwrap_or_else(|_| unreachable!())
});

/// `/<<-?(["']?)([A-Za-z_][A-Za-z0-9_]*)\1/`: the delimiter of a heredoc. The back reference is
/// written as three alternatives (double quotes, single quotes, no quotes).
static HEREDOC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"<<-?(?:"([A-Za-z_][A-Za-z0-9_]*)"|'([A-Za-z_][A-Za-z0-9_]*)'|([A-Za-z_][A-Za-z0-9_]*))"#,
    )
    .unwrap_or_else(|_| unreachable!())
});

/// One `FROM` instruction: its alias in lower case, and its image as written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stage {
    name: Option<String>,
    image: String,
}

/// `DockerfileBaseImage::finalStage($dockerfile, $target)`: the image in the `FROM` of the
/// final stage (or of the build target), with stage aliases resolved through their own `FROM`.
/// `None` for `scratch`, a build argument in the reference, or a reference that is not valid.
pub fn final_stage(dockerfile: &str, target: Option<&str>) -> Option<String> {
    let stages = stages(dockerfile);
    let mut index = stages.len().checked_sub(1)?;
    // `filled($target)`.
    if let Some(target) = target.filter(|t| !t.trim().is_empty()) {
        let wanted = php_trim(target).to_ascii_lowercase();
        index = stages
            .iter()
            .position(|stage| stage.name.as_deref() == Some(wanted.as_str()))?;
    }
    resolve(&stages, index)
}

/// `DockerfileBaseImage::resolve($stages, $index)`.
fn resolve(stages: &[Stage], mut index: usize) -> Option<String> {
    for _ in 0..=MAX_STAGES {
        let image = &stages.get(index)?.image;
        if image.contains('$') {
            return None;
        }
        let lower = image.to_ascii_lowercase();
        let alias = stages
            .get(..index)?
            .iter()
            .rposition(|earlier| earlier.name.as_deref() == Some(lower.as_str()));
        match alias {
            Some(earlier) => index = earlier,
            None => break,
        }
    }

    let image = &stages.get(index)?.image;
    if image.eq_ignore_ascii_case("scratch") {
        return None;
    }
    ImageRef::parse(image).ok()?;
    Some(image.clone())
}

/// `preg_split('/\r\n|\n|\r/', $dockerfile)`.
fn lines(dockerfile: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = dockerfile;
    while let Some(at) = rest.find(['\n', '\r']) {
        lines.push(&rest[..at]);
        let width = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
        rest = &rest[at + width..];
    }
    lines.push(rest);
    lines
}

/// `DockerfileBaseImage::stages($dockerfile)`: the `FROM` instructions in order. Skips comments
/// and heredoc bodies, joins continuation lines, and drops flags such as `--platform=…`.
fn stages(dockerfile: &str) -> Vec<Stage> {
    let lines = lines(dockerfile);
    let escape = escape_character(&lines);
    let mut stages = Vec::new();
    let mut instruction = String::new();
    let mut heredocs: Vec<String> = Vec::new();

    for line in &lines {
        if let Some(delimiter) = heredocs.first() {
            if php_trim(line) == delimiter.as_str() {
                heredocs.remove(0);
            }
            continue;
        }

        let trimmed = php_trim(line);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(before) = trimmed.strip_suffix(escape) {
            instruction.push_str(before);
            instruction.push(' ');
            continue;
        }

        instruction.push_str(trimmed);
        heredocs = add_instruction(&instruction, &mut stages);
        instruction.clear();
    }

    if !instruction.is_empty() {
        add_instruction(&instruction, &mut stages);
    }

    stages.truncate(MAX_STAGES);
    stages
}

/// `DockerfileBaseImage::instruction($instruction, $stages)`: adds a `FROM` to `stages`.
/// Returns the heredoc delimiters of any other instruction, whose bodies follow it.
fn add_instruction(instruction: &str, stages: &mut Vec<Stage>) -> Vec<String> {
    let parts: Vec<&str> = instruction
        .split([' ', '\t', '\n', '\x0B', '\x0C', '\r'])
        .filter(|part| !part.is_empty())
        .collect();
    let Some(keyword) = parts.first() else {
        return Vec::new();
    };

    if !keyword.eq_ignore_ascii_case("FROM") {
        return HEREDOC
            .captures_iter(instruction)
            .filter_map(|c| c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3)))
            .map(|m| m.as_str().to_owned())
            .collect();
    }

    let arguments: Vec<&str> = parts
        .iter()
        .skip(1)
        .filter(|part| !part.starts_with("--"))
        .copied()
        .collect();
    let Some(image) = arguments.first() else {
        return Vec::new();
    };

    let name = match (arguments.get(1), arguments.get(2)) {
        (Some(keyword), Some(name)) if keyword.eq_ignore_ascii_case("as") => {
            Some(name.to_ascii_lowercase())
        }
        _ => None,
    };
    stages.push(Stage {
        name,
        image: (*image).to_owned(),
    });
    Vec::new()
}

/// `DockerfileBaseImage::escapeCharacter($lines)`: the `# escape=` parser directive, which must
/// come before any other line.
fn escape_character(lines: &[&str]) -> char {
    for line in lines {
        let Some(directive) = DIRECTIVE.captures(php_trim(line)) else {
            break;
        };
        let name = directive.get(1).map_or("", |m| m.as_str());
        let value = directive.get(2).map_or("", |m| m.as_str());
        if name.eq_ignore_ascii_case("escape") {
            match value {
                "\\" => return '\\',
                "`" => return '`',
                _ => {}
            }
        }
    }
    '\\'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `it('reads the base image of the final stage')` of `DockerfileBaseImageTest`.
    #[test]
    fn reads_the_base_image_of_the_final_stage() {
        let digest = format!("node@sha256:{}", "a".repeat(64));
        let digest_dockerfile = format!("FROM {digest}\n");
        let cases: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "single stage",
                "FROM node:20.11-alpine\nWORKDIR /app\nCOPY . .\nCMD [\"node\", \"index.js\"]\n",
                Some("node:20.11-alpine"),
            ),
            (
                "multi-stage, last stage is an image",
                "FROM node:20 AS build\nRUN npm ci\nFROM nginx:1.27-alpine\nCOPY --from=build /app/dist /usr/share/nginx/html\n",
                Some("nginx:1.27-alpine"),
            ),
            (
                "multi-stage, alias chain",
                "FROM node:20 AS base\nFROM base AS build\nRUN npm ci\nFROM build\nCMD [\"node\"]\n",
                Some("node:20"),
            ),
            (
                "alias names compare without case",
                "FROM node:20 AS Build\nFROM build\n",
                Some("node:20"),
            ),
            (
                "platform flag and alias",
                "FROM --platform=$BUILDPLATFORM golang:1.23 AS b\nRUN go build\nFROM alpine:3.20\nCOPY --from=b /out /app\n",
                Some("alpine:3.20"),
            ),
            (
                "platform flag on the final stage",
                "FROM --platform=linux/amd64 alpine:3.20\n",
                Some("alpine:3.20"),
            ),
            (
                "alias of a stage with a platform flag",
                "FROM --platform=$BUILDPLATFORM golang:1.23 AS b\nFROM b\n",
                Some("golang:1.23"),
            ),
            ("ARG in FROM", "ARG BASE=node:20\nFROM ${BASE}\n", None),
            (
                "ARG without braces",
                "ARG VERSION=20\nFROM node:$VERSION\n",
                None,
            ),
            (
                "ARG reached through an alias",
                "ARG BASE=node:20\nFROM $BASE AS build\nFROM build\n",
                None,
            ),
            (
                "comments, lowercase from, and line continuations",
                "# syntax=docker/dockerfile:1\n# FROM ubuntu:22.04\nfrom node:20 \\\n  as build\nRUN echo \\\n  FROM debian:12\n  # FROM fedora:40\nfrom \\\n  build\n",
                Some("node:20"),
            ),
            (
                "heredoc body is not an instruction",
                "FROM alpine:3.20\nRUN <<EOF\nFROM debian:12\nEOF\n",
                Some("alpine:3.20"),
            ),
            (
                "scratch",
                "FROM golang:1.23 AS build\nFROM scratch\nCOPY --from=build /app /app\n",
                None,
            ),
            (
                "scratch through an alias",
                "FROM scratch AS base\nFROM base\n",
                None,
            ),
            ("invalid reference", "FROM Node:20!\n", None),
            (
                "digest reference",
                digest_dockerfile.as_str(),
                Some(digest.as_str()),
            ),
            (
                "registry with a port",
                "FROM localhost:5000/team/base:1.2 AS b\nFROM b\n",
                Some("localhost:5000/team/base:1.2"),
            ),
            ("no FROM", "RUN echo hi\n", None),
            ("empty", "", None),
            (
                "CRLF line endings",
                "FROM node:20 AS build\r\nFROM build\r\n",
                Some("node:20"),
            ),
        ];
        for (name, dockerfile, expected) in cases {
            assert_eq!(final_stage(dockerfile, None).as_deref(), expected, "{name}");
        }
    }

    /// `it('reads the base image of the build target stage')`.
    #[test]
    fn reads_the_base_image_of_the_build_target_stage() {
        let dockerfile = "FROM node:20 AS build\nFROM build AS dev\nFROM nginx:1.27 AS prod\n";
        assert_eq!(
            final_stage(dockerfile, Some("dev")).as_deref(),
            Some("node:20")
        );
        assert_eq!(
            final_stage(dockerfile, Some("PROD")).as_deref(),
            Some("nginx:1.27")
        );
        assert_eq!(final_stage(dockerfile, Some("missing")), None);
        // A blank target is the final stage.
        assert_eq!(
            final_stage(dockerfile, Some("  ")).as_deref(),
            Some("nginx:1.27")
        );
    }

    #[test]
    fn heredocs_with_quotes_and_dashes_are_skipped() {
        let dockerfile = "FROM alpine:3.20\nRUN <<-'EOF' cat && <<\"TWO\"\nFROM debian:12\n\tEOF\nFROM fedora:40\nTWO\nRUN echo done\n";
        assert_eq!(
            final_stage(dockerfile, None).as_deref(),
            Some("alpine:3.20")
        );
        // A quote that is not closed is no heredoc.
        assert!(add_instruction("RUN echo <<\"EOF", &mut Vec::new()).is_empty());
        assert_eq!(
            add_instruction("RUN cat <<A <<-B", &mut Vec::new()),
            vec!["A".to_owned(), "B".to_owned()]
        );
    }

    #[test]
    fn the_escape_directive_changes_the_continuation_character() {
        let dockerfile = "# escape=`\nFROM node:20 `\n  AS build\nFROM build\n";
        assert_eq!(final_stage(dockerfile, None).as_deref(), Some("node:20"));
        // With the directive a backslash does not continue the line.
        let windows = "# escape=`\nFROM mcr.microsoft.com/windows/nanoserver:ltsc2022\nWORKDIR C:\\\nRUN echo hi\n";
        assert_eq!(
            final_stage(windows, None).as_deref(),
            Some("mcr.microsoft.com/windows/nanoserver:ltsc2022")
        );
        // A directive after another line is a comment.
        assert_eq!(escape_character(&["FROM a", "# escape=`"]), '\\');
        assert_eq!(
            escape_character(&["# syntax=docker/dockerfile:1", "#Escape = `"]),
            '`'
        );
        assert_eq!(escape_character(&["# escape=x"]), '\\');
    }

    #[test]
    fn a_from_without_an_image_is_no_stage() {
        assert_eq!(final_stage("FROM --platform=linux/amd64\n", None), None);
        // The last instruction may end with a continuation.
        assert_eq!(
            final_stage("FROM node:20 \\", None).as_deref(),
            Some("node:20")
        );
    }

    #[test]
    fn only_the_first_hundred_stages_count() {
        let mut dockerfile = String::new();
        for i in 0..MAX_STAGES {
            dockerfile.push_str(&format!("FROM node:{i}\n"));
        }
        dockerfile.push_str("FROM nginx:1.27\n");
        assert_eq!(final_stage(&dockerfile, None).as_deref(), Some("node:99"));
    }
}
