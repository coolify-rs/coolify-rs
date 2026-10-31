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

/// `password_hash($password, PASSWORD_BCRYPT, ['cost' => $cost])`: a `$2y$` hash.
pub fn hash_bcrypt(password: &str, cost: u32) -> Option<String> {
    bcrypt::hash_with_result(password, cost)
        .ok()
        .map(|h| h.format_for_version(bcrypt::Version::TwoY))
}

/// `password_hash($password, PASSWORD_ARGON2ID, ['memory_cost' => .., 'time_cost' => ..])`
/// (one thread, 16 byte salt, 32 byte output: PHP's defaults).
pub fn hash_argon2id(password: &str, memory_cost: u32, time_cost: u32) -> Option<String> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let params = argon2::Params::new(memory_cost, time_cost, 1, Some(32)).ok()?;
    let hasher = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    hasher
        .hash_password(password.as_bytes(), &salt)
        .ok()
        .map(|h| h.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_php() {
        let bcrypt = hash_bcrypt("secret-password", 4).unwrap();
        assert!(bcrypt.starts_with("$2y$04$"));
        assert!(verify("secret-password", &bcrypt));
        let argon = hash_argon2id("secret-password", 8192, 1).unwrap();
        assert!(argon.starts_with("$argon2id$v=19$m=8192,t=1,p=1$"));
    }

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
