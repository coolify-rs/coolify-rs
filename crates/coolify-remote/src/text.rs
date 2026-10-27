//! PHP string helpers whose exact behaviour shows up in command output.

/// PHP `trim()`: strips `" \t\n\r\0\x0B"` only, not Unicode whitespace.
pub fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B'))
}

/// Upstream `sanitize_utf8_text`: `mb_convert_encoding($text, 'UTF-8', 'UTF-8')`,
/// which replaces each invalid sequence with mbstring's substitute character `?`.
pub fn sanitize_utf8(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                out.push('?');
                let skip = e.error_len().unwrap_or(after.len());
                rest = &after[skip..];
            }
        }
    }
}

/// How `instant_remote_process` turns stdout into its return value: trimmed,
/// and the literal `null` means no output.
pub fn command_output(stdout: &[u8]) -> Option<String> {
    let text = sanitize_utf8(stdout);
    let trimmed = php_trim(&text);
    (trimmed != "null").then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_like_php() {
        assert_eq!(php_trim("\x0B\0 a b\r\n"), "a b");
        assert_eq!(php_trim("\u{a0}x\u{a0}"), "\u{a0}x\u{a0}");
    }

    #[test]
    fn replaces_invalid_utf8() {
        assert_eq!(sanitize_utf8(b"ok \xff\xfe end"), "ok ?? end");
        assert_eq!(sanitize_utf8("é".as_bytes()), "é");
        assert_eq!(sanitize_utf8(b"cut \xc3"), "cut ?");
    }

    #[test]
    fn null_output() {
        assert_eq!(command_output(b"null\n"), None);
        assert_eq!(command_output(b"  hi \n"), Some("hi".into()));
        assert_eq!(command_output(b""), Some(String::new()));
    }
}
