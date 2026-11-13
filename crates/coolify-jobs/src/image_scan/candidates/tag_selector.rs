//! `App\Services\ImageScan\TagSelector`: picks newer version tags of an image from a registry tag
//! list (spec 7.2 and the candidate rules of spec 11).
//!
//! A tag is `prefix + numeric version + suffix` (`v1.2.3`, `15.0-alpine`). Candidates keep the
//! prefix, the suffix, and the number of version parts of the current tag. Never uses Grype's
//! suggested version.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::image_scan::image_ref::ImageRef;

/// `TagSelector::MAX_TAGS`: the tags of a list that are read.
pub const MAX_TAGS: usize = 10_000;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

/// `TagSelector::PRE_RELEASE`.
static PRE_RELEASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:\A|[._-])(?:rc|beta|alpha|dev|nightly|edge|preview|canary)\d*(?:\z|[._-])")
});
/// Cosign tags of a registry (`sha256-….sig`, `.att`, `.sbom`).
static SIGNATURE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r"\.(sig|att|sbom)\z"));
static VERSION_TAG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\A(?P<prefix>[^0-9]*)(?P<version>[0-9]{1,9}(?:\.[0-9]{1,9})*)(?P<suffix>(?:[-_+].*)?)\z")
});
static PREFIX_END: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)[-_.v]\z"));
static SHA_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\Asha[-_]?\z"));

/// What [`select`] returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// The newest tag of the same major. When the current tag is floating, the newest floating
    /// tag of the line, else the newest exact tag.
    pub line: Option<String>,
    /// For an exact tag with three or more version parts (`8.0.40`), the newest tag of the same
    /// major and minor (`8.0.45`), the smallest change; `None` when it is the `line` tag.
    pub patch: Option<String>,
    /// The newest tag of a higher major, for information only.
    pub major: Option<String>,
    /// Other tags extend the current tag (`15-alpine` with `15.4-alpine`).
    pub floating: bool,
}

/// A tag split into `prefix + version + suffix`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTag {
    prefix: String,
    parts: Vec<u32>,
    suffix: String,
}

/// `TagSelector::select($currentTag, $tags)`. Everything is `None` for a tag that is not a
/// version.
pub fn select<T: AsRef<str>>(current_tag: &str, tags: &[T]) -> Selection {
    let Some(current) = parse(current_tag) else {
        return Selection::default();
    };

    // The tags with the prefix and the suffix of the current tag, in the order of the list.
    let mut same_kind: Vec<(&str, Vec<u32>)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for tag in tags.iter().take(MAX_TAGS) {
        let tag = tag.as_ref();
        let Some(parsed) = parse(tag) else {
            continue;
        };
        if parsed.prefix == current.prefix && parsed.suffix == current.suffix && seen.insert(tag) {
            same_kind.push((tag, parsed.parts));
        }
    }

    let length = current.parts.len();
    let floating = same_kind
        .iter()
        .any(|(_, parts)| parts.len() > length && parts[..length] == current.parts[..]);

    let same_shape: Vec<&(&str, Vec<u32>)> = same_kind
        .iter()
        .filter(|(_, parts)| parts.len() == length)
        .collect();
    let mut line = newest(same_shape.iter().copied().filter(|(_, parts)| {
        parts[0] == current.parts[0] && compare(parts, &current.parts) != Ordering::Less
    }));
    if line.as_deref() == Some(current_tag) && !floating {
        line = None;
    }

    let mut patch = None;
    if !floating && length >= 3 {
        patch = newest(same_shape.iter().copied().filter(|(_, parts)| {
            parts[0] == current.parts[0]
                && parts[1] == current.parts[1]
                && compare(parts, &current.parts) == Ordering::Greater
        }));
        if patch == line {
            patch = None;
        }
    }

    let higher_major = |entry: &&(&str, Vec<u32>)| entry.1[0] > current.parts[0];
    let major = newest(same_shape.iter().copied().filter(higher_major))
        .or_else(|| newest(same_kind.iter().filter(higher_major)));

    Selection {
        line,
        patch,
        major,
        floating,
    }
}

/// `TagSelector::isVersion($tag)`: whether a tag is a version that other tags can be compared
/// with (`sha-3f2a1c`, `main`, `latest` are not).
pub fn is_version(tag: &str) -> bool {
    parse(tag).is_some()
}

/// `parse()`.
fn parse(tag: &str) -> Option<ParsedTag> {
    if !ImageRef::is_valid_tag(tag)
        || tag.starts_with("sha256-")
        || SIGNATURE_SUFFIX.is_match(tag)
        || PRE_RELEASE.is_match(tag)
    {
        return None;
    }
    let captures = VERSION_TAG.captures(tag)?;
    let prefix = captures.name("prefix").map_or("", |m| m.as_str());
    if !prefix.is_empty() && (!PREFIX_END.is_match(prefix) || SHA_PREFIX.is_match(prefix)) {
        return None;
    }
    let parts = captures
        .name("version")?
        .as_str()
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    Some(ParsedTag {
        prefix: prefix.to_owned(),
        parts,
        suffix: captures
            .name("suffix")
            .map_or("", |m| m.as_str())
            .to_owned(),
    })
}

/// `newest()`: the tag with the highest version; the first one of equal versions.
fn newest<'a, 'b: 'a>(tags: impl Iterator<Item = &'a (&'b str, Vec<u32>)>) -> Option<String> {
    let mut newest: Option<&(&str, Vec<u32>)> = None;
    for entry in tags {
        if newest.is_none_or(|best| compare(&entry.1, &best.1) == Ordering::Greater) {
            newest = Some(entry);
        }
    }
    newest.map(|(tag, _)| (*tag).to_owned())
}

/// `compare()`: compares two versions part by part; missing parts count as 0.
fn compare(a: &[u32], b: &[u32]) -> Ordering {
    let length = a.len().max(b.len());
    let part = |parts: &[u32], i: usize| parts.get(i).copied().unwrap_or(0);
    (0..length)
        .map(|i| part(a, i).cmp(&part(b, i)))
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(tag: &str) -> Option<String> {
        Some(tag.to_owned())
    }

    /// `it('selects the newest tag of the line and the newest higher major')`.
    #[test]
    fn selects_the_newest_tag_of_the_line_and_the_newest_higher_major() {
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, &str, Vec<&str>, Option<&str>, Option<&str>, bool)> = vec![
            (
                "exact tag with suffix",
                "15.0-alpine",
                vec![
                    "15.11-alpine",
                    "15.15-alpine",
                    "16.4-alpine",
                    "15.16-rc1-alpine",
                    "15-alpine",
                ],
                Some("15.15-alpine"),
                Some("16.4-alpine"),
                false,
            ),
            (
                "floating tag keeps the floating line tag",
                "15-alpine",
                vec![
                    "15.11-alpine",
                    "15.15-alpine",
                    "16.4-alpine",
                    "15.16-rc1-alpine",
                    "15-alpine",
                    "16-alpine",
                ],
                Some("15-alpine"),
                Some("16-alpine"),
                true,
            ),
            (
                "floating minor tag moves to the newest minor",
                "1.27",
                vec!["1.27", "1.27.3", "1.28", "1.28.0", "1.29", "1.29.1", "2.0"],
                Some("1.29"),
                Some("2.0"),
                true,
            ),
            (
                "other suffixes are another line",
                "15.0",
                vec!["15.11-alpine", "15.12-bookworm", "15.10"],
                Some("15.10"),
                None,
                false,
            ),
            (
                "v prefix is kept",
                "v1.2.3",
                vec!["v1.2.4", "v1.10.0", "1.11.0", "v2.0.0", "v1.9.9-beta.1"],
                Some("v1.10.0"),
                Some("v2.0.0"),
                false,
            ),
            (
                "no newer tag",
                "15.15-alpine",
                vec!["15.11-alpine", "15.15-alpine"],
                None,
                None,
                false,
            ),
            (
                "pre-release words are dropped",
                "2.1.0",
                vec![
                    "2.2.0-rc1",
                    "2.2.0-beta",
                    "2.2.0-alpha.2",
                    "2.2.0-dev",
                    "2.2.0-nightly",
                    "2.2.0-edge",
                    "2.2.0-preview",
                    "2.2.0-canary",
                    "3.0.0-RC2",
                ],
                None,
                None,
                false,
            ),
            (
                "git sha tag",
                "sha-3f2a1c",
                vec!["sha-3f2a1c", "sha-4a5b6c", "1.0.0"],
                None,
                None,
                false,
            ),
            (
                "branch tag",
                "main",
                vec!["main", "develop", "1.0.0"],
                None,
                None,
                false,
            ),
            (
                "latest tag",
                "latest",
                vec!["latest", "1.0.0", "2.0.0"],
                None,
                None,
                false,
            ),
            (
                "GHCR cosign tags are ignored",
                "2024.10.1",
                vec![
                    "2024.10.1",
                    "2024.10.4",
                    "sha256-abc.sig",
                    "sha256-0123456789abcdef.att",
                    "2024.11.0.sbom",
                    "2025.1.0",
                ],
                Some("2024.10.4"),
                Some("2025.1.0"),
                false,
            ),
            (
                "invalid tags are ignored",
                "1.0",
                vec!["1.1", "1.2 ", "-1.3", "1.4$(id)", "..1.5"],
                Some("1.1"),
                None,
                false,
            ),
        ];
        for (name, current, tags, line, major, floating) in cases {
            let selected = select(current, &tags);
            assert_eq!(selected.line.as_deref(), line, "line of: {name}");
            assert_eq!(selected.major.as_deref(), major, "major of: {name}");
            assert_eq!(selected.floating, floating, "floating of: {name}");
        }
    }

    /// `it('selects the newest patch of the same minor before the newest tag of the line')`.
    #[test]
    fn selects_the_newest_patch_of_the_same_minor_before_the_newest_tag_of_the_line() {
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, &str, Vec<&str>, Option<&str>, Option<&str>)> = vec![
            (
                "patch and newer minor",
                "8.0.40",
                vec!["8.0.40", "8.0.41", "8.0.45", "8.4.13", "9.7.4"],
                Some("8.0.45"),
                Some("8.4.13"),
            ),
            (
                "v prefix and suffix are kept",
                "v1.2.3-alpine",
                vec!["v1.2.9-alpine", "v1.2.10", "v1.5.0-alpine"],
                Some("v1.2.9-alpine"),
                Some("v1.5.0-alpine"),
            ),
            (
                "the newest patch is the line tag",
                "1.27.1",
                vec!["1.27.1", "1.27.4"],
                None,
                Some("1.27.4"),
            ),
            (
                "no newer patch",
                "8.0.45",
                vec!["8.0.45", "8.4.13"],
                None,
                Some("8.4.13"),
            ),
            (
                "two version parts have no patch line",
                "15.4",
                vec!["15.4", "15.13"],
                None,
                Some("15.13"),
            ),
            (
                "a floating tag has no patch line",
                "1.27",
                vec!["1.27", "1.27.3", "1.28", "1.28.0"],
                None,
                Some("1.28"),
            ),
        ];
        for (name, current, tags, patch, line) in cases {
            let selected = select(current, &tags);
            assert_eq!(selected.patch.as_deref(), patch, "patch of: {name}");
            assert_eq!(selected.line.as_deref(), line, "line of: {name}");
        }
    }

    /// `it('selects tags for an image of a registry with a port')`.
    #[test]
    fn selects_tags_for_an_image_of_a_registry_with_a_port() {
        let reference = ImageRef::parse("localhost:5000/team/app:1.2").unwrap();
        assert_eq!(reference.tag.as_deref(), Some("1.2"));
        assert_eq!(reference.qualified_name(), "localhost:5000/team/app");
        assert_eq!(
            select(reference.tag.as_deref().unwrap(), &["1.2", "1.3", "2.0"]),
            Selection {
                line: some("1.3"),
                patch: None,
                major: some("2.0"),
                floating: false,
            }
        );
    }

    /// `it('reads at most 10,000 tags')`.
    #[test]
    fn reads_at_most_10_000_tags() {
        let mut tags: Vec<String> = (1..=10_000).map(|patch| format!("1.0.{patch}")).collect();
        tags.push("1.0.99999".to_owned());
        assert_eq!(MAX_TAGS, 10_000);
        assert_eq!(select("1.0.0", &tags).line, some("1.0.10000"));
    }

    #[test]
    fn a_version_is_a_prefix_numbers_and_a_suffix() {
        for tag in [
            "1",
            "1.2.3",
            "v1.2.3",
            "V2",
            "15.0-alpine",
            "release-1.4",
            "1.2_3",
            "2024.10.1",
        ] {
            assert!(is_version(tag), "{tag}");
        }
        for tag in [
            "latest",
            "main",
            "sha-3f2a1c",
            "sha256-abc.sig",
            "1.0.0.sbom",
            "1.0-rc1",
            "1.0-BETA",
            "nightly-1.2",
            "alpine3.20",
            "1234567890",
            "",
            "1.2 ",
        ] {
            assert!(!is_version(tag), "{tag}");
        }
    }

    #[test]
    fn the_major_falls_back_to_a_tag_of_another_shape() {
        // No tag of a higher major has three parts: the newest of the same prefix and suffix.
        let selected = select("1.2.3", &["1.2.3", "2.0", "2.1", "3"]);
        assert_eq!(selected.major, some("3"));
        // Equal versions: the first tag of the list wins.
        assert_eq!(select("1.2", &["1.3", "1.03"]).line, some("1.3"));
        assert_eq!(
            compare(&[1, 2], &[1, 2, 0]),
            Ordering::Equal,
            "missing parts are 0"
        );
        assert_eq!(compare(&[1, 10], &[1, 9, 9]), Ordering::Greater);
    }
}
