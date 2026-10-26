//! Naming rules from `app/Support/ValidationPatterns.php`.

use std::sync::LazyLock;

use regex::Regex;

/// `NAME_PATTERN` (L13): letters (incl. Unicode), marks, numbers, whitespace
/// and `- _ . @ / & ( ) # , : +`.
pub static NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{L}\p{M}\p{N}\s\-_.@/&()#,:+]+$").unwrap_or_else(|_| unreachable!())
});

/// `DESCRIPTION_PATTERN` (L18).
pub static DESCRIPTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[\p{L}\p{M}\p{N}\s\-_.,!?()'"+=*@/&]+$"#).unwrap_or_else(|_| unreachable!())
});

pub const NAME_MIN: usize = 3;
pub const NAME_MAX: usize = 255;
pub const DESCRIPTION_MAX: usize = 255;

/// PHP `strip_tags`, as applied by `HasSafeStringAttribute` to `name` and
/// `description` on write. Inputs reaching it have passed the patterns above
/// (which exclude `<` and `>`), so in practice it only maps `null` to `""`;
/// tags are still removed defensively.
pub fn strip_tags(value: Option<&str>) -> String {
    let Some(v) = value else {
        return String::new();
    };
    let mut out = String::with_capacity(v.len());
    let mut in_tag = false;
    for c in v.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_pattern() {
        for ok in [
            "Proj One",
            "Ünïcødé 名前",
            "a-b_c.d@e/f&g(h)#i,j:k+l",
            "x\ty",
        ] {
            assert!(NAME_PATTERN.is_match(ok), "{ok}");
        }
        // Observed on the 4.5.0 reference: ✓ and <> are rejected.
        for bad in ["bad<name>", "Proj Två ✓", "semi;colon", "quote'"] {
            assert!(!NAME_PATTERN.is_match(bad), "{bad}");
        }
    }

    #[test]
    fn description_pattern() {
        assert!(DESCRIPTION_PATTERN.is_match("Stage é/ü"));
        assert!(DESCRIPTION_PATTERN.is_match(r#"It's "fine"! (ok?) a+b=c*d"#));
        assert!(!DESCRIPTION_PATTERN.is_match("no <tags>"));
        assert!(!DESCRIPTION_PATTERN.is_match("hash#tag"));
    }

    #[test]
    fn strip_tags_semantics() {
        assert_eq!(strip_tags(None), "");
        assert_eq!(strip_tags(Some("a<b>c</b>d")), "acd");
        assert_eq!(strip_tags(Some("plain")), "plain");
    }
}
