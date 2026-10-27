//! Port of upstream `App\Traits\SshRetryable`.

use std::time::Duration;

/// Substrings (compared case-insensitively, not as regexes, exactly as upstream
/// does with `str_contains`) that mark an SSH connection failure.
const RETRYABLE_PATTERNS: &[&str] = &[
    "kex_exchange_identification",
    "Connection reset by peer",
    "Connection refused",
    "Connection timed out",
    "Connection closed by remote host",
    "ssh_exchange_identification",
    "Bad file descriptor",
    "Broken pipe",
    "No route to host",
    "Network is unreachable",
    "Host is down",
    "No buffer space available",
    "Connection reset by",
    "Permission denied, please try again",
    "Received disconnect from",
    "Disconnected from",
    "Connection to .* closed",
    "ssh: connect to host .* port .*: Connection",
    "Lost connection",
    "Timeout, server not responding",
    "Cannot assign requested address",
    "Network is down",
    "Host key verification failed",
    "Operation timed out",
    "Connection closed unexpectedly",
    "Remote host closed connection",
    "Authentication failed",
    "Too many authentication failures",
    "SSH command failed with exit code: 255",
];

/// Upstream `constants.ssh.max_retries` (attempts in total).
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

pub fn is_retryable_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    RETRYABLE_PATTERNS
        .iter()
        .any(|p| lower.contains(&p.to_lowercase()))
}

/// SSH exits with 255 for its own errors; any other positive code came from the
/// remote command, and retrying would replay commands that already ran.
pub fn is_retryable_failure(code: Option<i32>, message: &str) -> bool {
    if let Some(c) = code
        && c > 0
        && c != 255
    {
        return false;
    }
    is_retryable_error(message)
}

/// `min(base * multiplier^attempt, max)` with upstream's 2 s, ×2, 30 s.
pub fn retry_delay(attempt: u32) -> Duration {
    let secs = 2u64.saturating_mul(2u64.saturating_pow(attempt)).min(30);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cases from upstream tests/Unit/SshRetryMechanismTest.php.
    #[test]
    fn classifies_errors() {
        for e in [
            "kex_exchange_identification: read: Connection reset by peer",
            "Connection refused",
            "Connection timed out",
            "ssh_exchange_identification: Connection closed by remote host",
            "Broken pipe",
            "No route to host",
            "Network is unreachable",
            "SSH command failed with exit code: 255",
        ] {
            assert!(is_retryable_error(e), "{e}");
        }
        for e in [
            "Command not found",
            "Permission denied",
            "File not found",
            "Syntax error",
            "Invalid argument",
        ] {
            assert!(!is_retryable_error(e), "{e}");
        }
        // The regex-looking entries are literal substrings upstream.
        assert!(!is_retryable_error("Connection to 1.2.3.4 closed"));
    }

    #[test]
    fn remote_command_failures_are_not_retried() {
        assert!(!is_retryable_failure(
            Some(1),
            "upload failed: Connection refused"
        ));
        assert!(is_retryable_failure(Some(255), "Connection refused"));
        assert!(is_retryable_failure(None, "Connection refused"));
        assert!(is_retryable_failure(Some(0), "Broken pipe"));
    }

    #[test]
    fn backoff() {
        let secs: Vec<u64> = (0..6).map(|a| retry_delay(a).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 30, 30]);
    }
}
