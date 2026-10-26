//! Minimal reader for PHP `serialize()` output.
//!
//! Only string values (`s:<len>:"...";`) are supported: that is the only shape
//! the upstream `encrypt()` helper produces for the values coolify-rs reads.
//! Anything else is reported as unsupported rather than guessed at.

/// Returns the inner string if `input` is exactly one serialized PHP string.
/// The length prefix counts bytes, as PHP does.
pub fn unserialize_string(input: &str) -> Option<String> {
    let rest = input.strip_prefix("s:")?;
    let (len, rest) = rest.split_once(':')?;
    let len: usize = len.parse().ok()?;
    let rest = rest.strip_prefix('"')?;
    let bytes = rest.as_bytes();
    if bytes.len() != len + 2 || &bytes[len..] != b"\";" {
        return None;
    }
    String::from_utf8(bytes[..len].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strings() {
        assert_eq!(
            unserialize_string("s:5:\"hello\";").as_deref(),
            Some("hello")
        );
        assert_eq!(unserialize_string("s:0:\"\";").as_deref(), Some(""));
        // Multibyte: the length is in bytes.
        assert_eq!(unserialize_string("s:3:\"✓\";").as_deref(), Some("✓"));
        // Embedded quotes are fine because the length is authoritative.
        assert_eq!(unserialize_string("s:3:\"a\"b\";").as_deref(), Some("a\"b"));
    }

    #[test]
    fn rejects_other_shapes() {
        assert_eq!(unserialize_string("hello"), None);
        assert_eq!(unserialize_string("i:5;"), None);
        assert_eq!(unserialize_string("s:4:\"hello\";"), None);
        assert_eq!(unserialize_string("s:5:\"hello\""), None);
    }
}
