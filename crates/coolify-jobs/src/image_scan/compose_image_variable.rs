//! `App\Services\ImageScan\ComposeImageVariable`: an image of a compose file that a variable
//! sets, in one of the two forms that a security update can change through the Coolify
//! environment variable: the whole image (`${N8N_IMAGE}`, `${N8N_IMAGE:-n8nio/n8n:1.0}`) or the
//! tag only (`n8nio/n8n:${N8N_VERSION:-1.0}`). Other forms (two variables, a variable in the
//! name) are not supported.

use std::sync::LazyLock;

use coolify_remote::text::php_trim;
use regex::Regex;

const VARIABLE: &str = r"\$\{(?P<key>[A-Za-z_][A-Za-z0-9_]*)(?:(?P<op>:?[-?])(?P<default>[^}]*))?\}|\$(?P<bare>[A-Za-z_][A-Za-z0-9_]*)";

static WHOLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\A(?:{VARIABLE})\z")).unwrap_or_else(|_| unreachable!())
});
static TAG_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\A(?P<prefix>[^$:@\s]+(?::[0-9]+)?(?:/[^$:@\s]+)*):(?:{VARIABLE})\z"
    ))
    .unwrap_or_else(|_| unreachable!())
});

/// A compose image set by one variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeImageVariable {
    /// The name of the variable.
    pub key: String,
    /// The variable sets only the tag; `prefix` is the image name before it.
    pub tag_only: bool,
    pub prefix: String,
    /// The default of `${KEY:-default}` or `${KEY-default}`.
    pub default: Option<String>,
}

impl ComposeImageVariable {
    /// `ComposeImageVariable::parse($image)`: `None` for an image without a variable, or with a
    /// variable in a form that Coolify cannot change.
    pub fn parse(image: &str) -> Option<Self> {
        let image = php_trim(image);
        if let Some(c) = WHOLE.captures(image) {
            return Some(Self::from(&c, false, ""));
        }
        let c = TAG_ONLY.captures(image)?;
        let prefix = c.name("prefix").map_or("", |m| m.as_str());
        Some(Self::from(&c, true, prefix))
    }

    /// `ComposeImageVariable::hasVariable($image)`: whether the image names a variable at all
    /// (supported or not).
    pub fn has_variable(image: &str) -> bool {
        image.contains('$')
    }

    /// `resolve($value)`: the image that Compose starts when the variable has this value (`None`:
    /// not set, so the default applies).
    pub fn resolve(&self, value: Option<&str>) -> Option<String> {
        let value = value
            .filter(|v| !v.is_empty())
            .or(self.default.as_deref())
            .filter(|v| !v.is_empty())?;
        Some(if self.tag_only {
            format!("{}:{value}", self.prefix)
        } else {
            value.to_owned()
        })
    }

    /// `valueFor($current, $tag)`: the value of the variable that starts the image with this tag.
    /// `current` is the image it starts now.
    pub fn value_for(&self, current: &str, tag: &str) -> String {
        if self.tag_only {
            return tag.to_owned();
        }
        let name = match current.rfind(':') {
            Some(colon) if current.rfind('/').is_none_or(|slash| colon > slash) => {
                &current[..colon]
            }
            _ => current,
        };
        format!("{name}:{tag}")
    }

    fn from(c: &regex::Captures<'_>, tag_only: bool, prefix: &str) -> Self {
        let group = |name: &str| c.name(name).map_or("", |m| m.as_str());
        let key = if group("bare").is_empty() {
            group("key")
        } else {
            group("bare")
        };
        let op = group("op");
        Self {
            key: key.to_owned(),
            tag_only,
            prefix: prefix.to_owned(),
            default: (!op.is_empty() && op.ends_with('-')).then(|| group("default").to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_whole_image_variable() {
        let v = ComposeImageVariable::parse(" ${N8N_IMAGE} ").unwrap();
        assert_eq!(v.key, "N8N_IMAGE");
        assert!(!v.tag_only);
        assert_eq!(v.prefix, "");
        assert_eq!(v.default, None);
        assert_eq!(v.resolve(None), None);
        assert_eq!(v.resolve(Some("")), None);
        assert_eq!(
            v.resolve(Some("n8nio/n8n:1.1")).as_deref(),
            Some("n8nio/n8n:1.1")
        );
        assert_eq!(v.value_for("n8nio/n8n:1.1", "1.2"), "n8nio/n8n:1.2");
        assert_eq!(
            v.value_for("localhost:5000/n8n", "1.2"),
            "localhost:5000/n8n:1.2"
        );

        let bare = ComposeImageVariable::parse("$N8N_IMAGE").unwrap();
        assert_eq!(bare.key, "N8N_IMAGE");
        assert_eq!(bare.default, None);

        let with_default = ComposeImageVariable::parse("${N8N_IMAGE:-n8nio/n8n:1.0}").unwrap();
        assert_eq!(with_default.default.as_deref(), Some("n8nio/n8n:1.0"));
        assert_eq!(with_default.resolve(None).as_deref(), Some("n8nio/n8n:1.0"));
        assert_eq!(
            with_default.resolve(Some("n8nio/n8n:2")).as_deref(),
            Some("n8nio/n8n:2")
        );
        assert_eq!(
            ComposeImageVariable::parse("${N8N_IMAGE-n8nio/n8n:1.0}")
                .unwrap()
                .default
                .as_deref(),
            Some("n8nio/n8n:1.0")
        );
        // `:?` is an error message, not a default.
        assert_eq!(
            ComposeImageVariable::parse("${N8N_IMAGE:?required}")
                .unwrap()
                .default,
            None
        );
    }

    #[test]
    fn parses_a_tag_variable() {
        let v = ComposeImageVariable::parse("n8nio/n8n:${N8N_VERSION:-1.0}").unwrap();
        assert_eq!(v.key, "N8N_VERSION");
        assert!(v.tag_only);
        assert_eq!(v.prefix, "n8nio/n8n");
        assert_eq!(v.default.as_deref(), Some("1.0"));
        assert_eq!(v.resolve(None).as_deref(), Some("n8nio/n8n:1.0"));
        assert_eq!(v.resolve(Some("1.5")).as_deref(), Some("n8nio/n8n:1.5"));
        assert_eq!(v.value_for("n8nio/n8n:1.5", "1.6"), "1.6");

        let port = ComposeImageVariable::parse("localhost:5000/team/app:$TAG").unwrap();
        assert_eq!(port.prefix, "localhost:5000/team/app");
        assert_eq!(port.key, "TAG");
    }

    #[test]
    fn refuses_other_forms() {
        for image in [
            "nginx:1.27",
            "",
            "${REGISTRY}/app:${TAG}",
            "${REGISTRY}/app",
            "app-${NAME}:1",
            "nginx@${DIGEST}",
            "nginx:1.${MINOR}",
            "nginx:${TAG}-alpine",
        ] {
            assert_eq!(ComposeImageVariable::parse(image), None, "{image}");
        }
        assert!(ComposeImageVariable::has_variable("app-${NAME}:1"));
        assert!(!ComposeImageVariable::has_variable("nginx:1.27"));
    }
}
