//! `ApplicationDeploymentJob::validateDockerRegistryImageConfiguration()`: the registry image
//! name and tags of a deployment are checked before the job starts, because they reach shell
//! commands and image names. The forms and the API check them on save; rows saved before that,
//! or queued before an upgrade, are refused here.

use std::sync::LazyLock;

use regex::Regex;

/// `ValidationPatterns::DOCKER_IMAGE_NAME_PATTERN` without its length lookahead (1 to 255,
/// checked in [`is_valid_docker_image_name`]).
static DOCKER_IMAGE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z")
        .unwrap_or_else(|_| unreachable!())
});

/// `ValidationPatterns::DOCKER_IMAGE_TAG_PATTERN` without its length lookahead (1 to 128,
/// checked in [`is_valid_docker_image_tag`]). The possessive quantifiers of the PHP pattern
/// match the same strings here: the characters they keep from backtracking are either allowed
/// on their own or cannot be followed by `:`, `-` or `}`.
static DOCKER_IMAGE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:[A-Za-z0-9_]|\$(?:[A-Za-z_][A-Za-z0-9_]*|\{[A-Za-z_][A-Za-z0-9_]*(?::?-[A-Za-z0-9_.-]*)?\}))(?:[A-Za-z0-9_.-]|\$(?:[A-Za-z_][A-Za-z0-9_]*|\{[A-Za-z_][A-Za-z0-9_]*(?::?-[A-Za-z0-9_.-]*)?\}))*\z")
        .unwrap_or_else(|_| unreachable!())
});

/// Laravel `blank()` of a string column.
fn blank(value: Option<&str>) -> bool {
    value.is_none_or(|v| v.trim().is_empty())
}

/// `ValidationPatterns::isValidDockerImageName($value)`.
fn is_valid_docker_image_name(value: Option<&str>) -> bool {
    blank(value)
        || value.is_some_and(|v| (1..=255).contains(&v.len()) && DOCKER_IMAGE_NAME.is_match(v))
}

/// `ValidationPatterns::isValidDockerImageTag($value)` (variables allowed).
fn is_valid_docker_image_tag(value: Option<&str>) -> bool {
    blank(value)
        || value.is_some_and(|v| (1..=128).contains(&v.len()) && DOCKER_IMAGE_TAG.is_match(v))
}

/// `validateDockerRegistryImageConfiguration()`: the `DeploymentException` message, if any.
/// `preview_tag` is the tag of the queued deployment (`$this->dockerImagePreviewTag`), before
/// the preview's own tag fills it in.
pub(super) fn validate_docker_registry_image_configuration(
    name: Option<&str>,
    tag: Option<&str>,
    preview_tag: Option<&str>,
) -> Result<(), &'static str> {
    if !is_valid_docker_image_name(name) {
        return Err("Docker registry image name contains invalid characters.");
    }
    if !is_valid_docker_image_tag(tag) {
        return Err("Docker registry image tag contains invalid characters.");
    }
    if !is_valid_docker_image_tag(preview_tag) {
        return Err("Docker registry preview image tag contains invalid characters.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_docker_registry_image_configuration as validate;

    #[test]
    fn accepts_empty_and_valid_values() {
        assert_eq!(validate(None, None, None), Ok(()));
        assert_eq!(validate(Some(""), Some("  "), Some("")), Ok(()));
        assert_eq!(
            validate(
                Some("ghcr.io/acme/web-app"),
                Some("v1.2.3"),
                Some("pr-${PR_ID:-7}")
            ),
            Ok(())
        );
        assert_eq!(
            validate(Some("localhost:5000/app@sha256"), Some("$TAG"), None),
            Ok(())
        );
    }

    #[test]
    fn refuses_values_that_reach_the_shell() {
        assert_eq!(
            validate(Some("acme/app;id"), None, None),
            Err("Docker registry image name contains invalid characters.")
        );
        assert_eq!(
            validate(Some("Acme/App"), None, None),
            Err("Docker registry image name contains invalid characters.")
        );
        assert_eq!(
            validate(Some(&"a".repeat(256)), None, None),
            Err("Docker registry image name contains invalid characters.")
        );
        assert_eq!(
            validate(Some("acme/app"), Some("latest $(id)"), None),
            Err("Docker registry image tag contains invalid characters.")
        );
        assert_eq!(
            validate(None, Some(&"a".repeat(129)), None),
            Err("Docker registry image tag contains invalid characters.")
        );
        assert_eq!(
            validate(None, Some("latest"), Some(".pr")),
            Err("Docker registry preview image tag contains invalid characters.")
        );
    }
}
