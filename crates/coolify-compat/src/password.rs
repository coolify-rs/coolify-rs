//! Verification of upstream password hashes (Laravel `Hash::check`, bcrypt).

/// A syntactically valid bcrypt hash of a random string, used to keep the
/// timing of "unknown user" equal to "wrong password".
const DUMMY_HASH: &str = "$2y$10$Kqq2Sl7jiC./d2ulhasekOuxTRH1BvPABOJteAkjbJxdJdhftCMqe";

/// Checks `password` against a stored `$2y$`/`$2b$`/`$2a$` hash.
/// Malformed hashes verify as false (fail closed).
pub fn verify(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}

/// `Hash::make`: bcrypt with Laravel's default cost and the `$2y$` prefix.
pub fn hash(password: &str) -> Option<String> {
    bcrypt::hash_with_result(password, 10)
        .ok()
        .map(|h| h.format_for_version(bcrypt::Version::TwoY))
}

/// Spends the same time as a real verification and always returns false.
pub fn verify_dummy(password: &str) -> bool {
    let _ = bcrypt::verify(password, DUMMY_HASH);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_laravel_hashes() {
        // php -r 'echo password_hash("secret-password", PASSWORD_BCRYPT, ["cost" => 4]);'
        let hash = "$2y$04$3EUJ/TMAepQkPElj6gMovusWMWBd2XclldAGdQYIBGuB7QCBai9jK";
        assert!(verify("secret-password", hash));
        assert!(!verify("Secret-password", hash));
        assert!(!verify("x", "not-a-hash"));
        assert!(!verify_dummy("anything"));
    }
}
