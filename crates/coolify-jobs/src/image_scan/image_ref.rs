//! `App\Services\ImageScan\ImageRef`: a validated image reference, split by the Docker reference
//! grammar. The first path component is a registry only when it contains `.` or `:` or is
//! `localhost`; Docker Hub names get `library/`.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

/// `ValidationPatterns::DOCKER_IMAGE_NAME_PATTERN` without its length lookahead (1 to 255,
/// checked in [`is_valid_docker_image_name`]).
static DOCKER_IMAGE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\A(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z",
    )
});
/// `ValidationPatterns::DOCKER_IMAGE_LITERAL_TAG_PATTERN`, also `ImageRef::isValidTag()`.
static LITERAL_TAG: LazyLock<Regex> =
    LazyLock::new(|| re(r"\A[A-Za-z0-9_][A-Za-z0-9._-]{0,127}\z"));
static DIGEST: LazyLock<Regex> = LazyLock::new(|| re(r"\Asha256:[a-f0-9]{64}\z"));
static WRONG_DIGEST_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i):sha256?:"));
static NAME_AT_DIGEST: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\A(.+)@sha256:([a-f0-9]{64})\z"));
static OFFICIAL_REPOSITORY: LazyLock<Regex> = LazyLock::new(|| re(r"\Alibrary/[^/]+\z"));

/// Laravel `blank()` of a string.
fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

/// `ValidationPatterns::isValidDockerImageName($value)`: a repository name without a tag. A
/// blank value is valid.
pub fn is_valid_docker_image_name(value: &str) -> bool {
    blank(value) || ((1..=255).contains(&value.len()) && DOCKER_IMAGE_NAME.is_match(value))
}

/// The `DockerImageFormat` rule without variable tags (`new DockerImageFormat`).
fn passes_docker_image_format(value: &str) -> bool {
    if WRONG_DIGEST_SEPARATOR.is_match(value) {
        return false;
    }
    let (name, tag) = match NAME_AT_DIGEST.captures(value) {
        Some(c) => (c.get(1).map_or("", |m| m.as_str()), None),
        None => {
            let after_slash = value.rfind('/').map_or(0, |i| i + 1);
            match value[after_slash..].find(':') {
                Some(i) => (
                    &value[..after_slash + i],
                    Some(&value[after_slash + i + 1..]),
                ),
                None => (value, None),
            }
        }
    };
    is_valid_docker_image_name(name) && tag.is_none_or(|t| blank(t) || LITERAL_TAG.is_match(t))
}

/// `InvalidArgumentException` of `ImageRef`: the message is `Invalid image reference.`,
/// `Invalid image digest.` or `Invalid image tag.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidImageRef(pub &'static str);

impl fmt::Display for InvalidImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidImageRef {}

/// A validated image reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// `docker.io` for Docker Hub.
    pub registry: String,
    /// With `library/` for Docker Hub official images.
    pub repository: String,
    pub tag: Option<String>,
    /// `sha256:{64 hex}`.
    pub digest: Option<String>,
}

impl ImageRef {
    /// `ImageRef::parse($ref)`.
    pub fn parse(reference: &str) -> Result<Self, InvalidImageRef> {
        if reference.is_empty() || !passes_docker_image_format(reference) {
            return Err(InvalidImageRef("Invalid image reference."));
        }
        let mut name = reference;
        let mut tag = None;
        let mut digest = None;
        if let Some((before, after)) = reference.split_once('@') {
            name = before;
            if !Self::is_valid_digest(after) {
                return Err(InvalidImageRef("Invalid image digest."));
            }
            digest = Some(after.to_owned());
        } else if let Some(colon) = reference.rfind(':')
            && reference.rfind('/').is_none_or(|slash| colon > slash)
        {
            name = &reference[..colon];
            let candidate = &reference[colon + 1..];
            if !Self::is_valid_tag(candidate) {
                return Err(InvalidImageRef("Invalid image tag."));
            }
            tag = Some(candidate.to_owned());
        }

        let mut registry = "docker.io";
        let mut repository = name.to_owned();
        if let Some((first, rest)) = name.split_once('/')
            && (first.contains('.') || first.contains(':') || first == "localhost")
        {
            registry = first;
            repository = rest.to_owned();
        }
        if registry == "index.docker.io" {
            registry = "docker.io";
        }
        if registry == "docker.io" && !repository.contains('/') {
            repository = format!("library/{repository}");
        }
        Ok(Self {
            registry: registry.to_owned(),
            repository,
            tag,
            digest,
        })
    }

    /// `ImageRef::isValidTag($tag)`.
    pub fn is_valid_tag(tag: &str) -> bool {
        LITERAL_TAG.is_match(tag)
    }

    /// `ImageRef::isValidDigest($digest)`.
    pub fn is_valid_digest(digest: &str) -> bool {
        DIGEST.is_match(digest)
    }

    /// `isDockerOfficial()`.
    pub fn is_docker_official(&self) -> bool {
        self.registry == "docker.io" && OFFICIAL_REPOSITORY.is_match(&self.repository)
    }

    /// `withTag($tag)`: the same repository with another tag and no digest.
    pub fn with_tag(&self, tag: &str) -> Result<Self, InvalidImageRef> {
        if !Self::is_valid_tag(tag) {
            return Err(InvalidImageRef("Invalid image tag."));
        }
        Ok(Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: Some(tag.to_owned()),
            digest: None,
        })
    }

    /// `qualifiedName()`: registry and repository without tag or digest, for example
    /// `docker.io/library/nginx`.
    pub fn qualified_name(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }
}

/// `__toString()`: the short form Docker shows (`nginx:1.27`, `grafana/grafana`,
/// `ghcr.io/a/b@sha256:…`).
impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = if self.is_docker_official() {
            self.repository["library/".len()..].to_owned()
        } else if self.registry == "docker.io" {
            self.repository.clone()
        } else {
            self.qualified_name()
        };
        match (&self.digest, &self.tag) {
            (Some(digest), _) => write!(f, "{name}@{digest}"),
            (None, Some(tag)) => write!(f, "{name}:{tag}"),
            (None, None) => f.write_str(&name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:4cc92b64ca74ffca4c94d4c8fcc01f217731375300be14d0b6eeb2e82eaa110a";

    fn parsed(r: &str) -> ImageRef {
        ImageRef::parse(r).unwrap()
    }

    #[test]
    fn docker_hub_official_images_get_library() {
        let r = parsed("nginx:1.27");
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.tag.as_deref(), Some("1.27"));
        assert_eq!(r.digest, None);
        assert!(r.is_docker_official());
        assert_eq!(r.qualified_name(), "docker.io/library/nginx");
        assert_eq!(r.to_string(), "nginx:1.27");
        assert_eq!(parsed("nginx").to_string(), "nginx");
        assert_eq!(parsed("library/nginx:1").to_string(), "nginx:1");
    }

    #[test]
    fn docker_hub_user_images() {
        let r = parsed("grafana/grafana");
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "grafana/grafana");
        assert_eq!(r.tag, None);
        assert!(!r.is_docker_official());
        assert_eq!(r.to_string(), "grafana/grafana");
        assert_eq!(
            parsed("index.docker.io/grafana/grafana:11").to_string(),
            "grafana/grafana:11"
        );
        assert_eq!(parsed("docker.io/nginx").repository, "library/nginx");
    }

    #[test]
    fn other_registries() {
        let r = parsed("ghcr.io/coollabsio/coolify:4.0.0");
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "coollabsio/coolify");
        assert_eq!(r.to_string(), "ghcr.io/coollabsio/coolify:4.0.0");

        let port = parsed("localhost:5000/app");
        assert_eq!(port.registry, "localhost:5000");
        assert_eq!(port.repository, "app");
        assert_eq!(port.tag, None);
        assert_eq!(port.to_string(), "localhost:5000/app");

        let local = parsed("localhost/app:dev");
        assert_eq!(local.registry, "localhost");
        assert_eq!(local.tag.as_deref(), Some("dev"));

        // A first component without a dot or a colon is part of the repository.
        let nested = parsed("team/app/web:1");
        assert_eq!(nested.registry, "docker.io");
        assert_eq!(nested.repository, "team/app/web");
    }

    #[test]
    fn digests() {
        let r = parsed(&format!("ghcr.io/a/b@{DIGEST}"));
        assert_eq!(r.digest.as_deref(), Some(DIGEST));
        assert_eq!(r.tag, None);
        assert_eq!(r.to_string(), format!("ghcr.io/a/b@{DIGEST}"));
        assert_eq!(
            parsed(&format!("nginx@{DIGEST}"))
                .with_tag("1.28")
                .unwrap()
                .to_string(),
            "nginx:1.28"
        );
    }

    #[test]
    fn refuses_invalid_references() {
        for bad in [
            "",
            "nginx:",
            "NGINX",
            "nginx:1 2",
            "nginx; rm -rf /",
            "nginx:$(id)",
            "nginx:${TAG}",
            "nginx:sha256:abc",
            "nginx@sha256:abc",
            "nginx@md5:abc",
            "-nginx",
            "nginx:-tag",
            "ghcr.io/a/b:tag@sha256",
        ] {
            assert!(ImageRef::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            ImageRef::parse(""),
            Err(InvalidImageRef("Invalid image reference."))
        );
        assert_eq!(
            parsed("nginx").with_tag("bad tag"),
            Err(InvalidImageRef("Invalid image tag."))
        );
    }

    #[test]
    fn tag_and_digest_checks() {
        assert!(ImageRef::is_valid_tag("1.27-alpine"));
        assert!(ImageRef::is_valid_tag("_x"));
        assert!(!ImageRef::is_valid_tag(".x"));
        assert!(!ImageRef::is_valid_tag(&"a".repeat(129)));
        assert!(ImageRef::is_valid_digest(DIGEST));
        assert!(!ImageRef::is_valid_digest(&DIGEST.to_uppercase()));
    }
}
