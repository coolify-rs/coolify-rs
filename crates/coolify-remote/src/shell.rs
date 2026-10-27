//! Quoting for values placed into remote POSIX shell scripts.

/// PHP `escapeshellarg` on Unix: wraps in single quotes and turns each `'` into `'\''`.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quotes() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("$(rm -rf /); `x`"), "'$(rm -rf /); `x`'");
    }

    #[test]
    fn round_trips_through_sh() {
        let nasty = "a'b\"c$d`e\\f\ng ; | & * ?";
        let Ok(out) = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", quote(nasty)))
            .output()
        else {
            return;
        };
        assert_eq!(String::from_utf8_lossy(&out.stdout), nasty);
    }
}
