//! Laravel Sanctum personal access token compatibility.
//!
//! Upstream issues tokens as `"{id}|{plaintext}"` and stores
//! `sha256_hex(plaintext)` in `personal_access_tokens.token`. Sanctum's
//! `findToken` accepts both the `id|plaintext` form (lookup by id, then a
//! constant-time hash comparison) and a bare plaintext (lookup by hash).

use rand::Rng;
use rand::distributions::Alphanumeric;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A bearer token split into its lookup parts. Never log `plaintext`.
#[derive(PartialEq, Eq)]
pub struct ParsedToken<'a> {
    pub id: Option<i64>,
    pub plaintext: &'a str,
}

impl std::fmt::Debug for ParsedToken<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedToken")
            .field("id", &self.id)
            .field("plaintext", &"<redacted>")
            .finish()
    }
}

/// Splits a bearer token the way Sanctum's `findToken` does.
///
/// Returns `None` for an empty token or an `id|...` prefix that is not an
/// integer (Sanctum's `find()` would simply miss in that case).
pub fn parse_bearer_token(token: &str) -> Option<ParsedToken<'_>> {
    if token.is_empty() {
        return None;
    }
    match token.split_once('|') {
        None => Some(ParsedToken {
            id: None,
            plaintext: token,
        }),
        Some((id, plaintext)) => {
            let id = id.parse::<i64>().ok()?;
            Some(ParsedToken {
                id: Some(id),
                plaintext,
            })
        }
    }
}

/// `hash('sha256', $plaintext)` as lowercase hex.
pub fn hash_token(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

/// Constant-time comparison of a stored hash with the hash of `plaintext`.
pub fn verify_token(stored_hash: &str, plaintext: &str) -> bool {
    let candidate = hash_token(plaintext);
    bool::from(candidate.as_bytes().ct_eq(stored_hash.as_bytes()))
}

/// Generates a plaintext token the way Sanctum 4 does: 40 random
/// alphanumerics followed by the CRC32B checksum of those characters
/// (8 lowercase hex chars), optionally prefixed by `sanctum.token_prefix`.
pub fn generate_plaintext(prefix: &str) -> String {
    let entropy: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(40)
        .map(char::from)
        .collect();
    format!("{prefix}{entropy}{:08x}", crc32b(entropy.as_bytes()))
}

/// PHP `hash('crc32b', ...)`: the standard IEEE CRC-32.
fn crc32b(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_id_and_plaintext() {
        let t = parse_bearer_token("12|abc|def").unwrap();
        assert_eq!(t.id, Some(12));
        assert_eq!(t.plaintext, "abc|def");
        let t = parse_bearer_token("plain").unwrap();
        assert_eq!(t.id, None);
        assert!(parse_bearer_token("x|abc").is_none());
        assert!(parse_bearer_token("").is_none());
    }

    #[test]
    fn hashes_like_php() {
        // php -r 'echo hash("sha256", "secret");'
        assert_eq!(
            hash_token("secret"),
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
        assert!(verify_token(&hash_token("secret"), "secret"));
        assert!(!verify_token(&hash_token("secret"), "Secret"));
    }

    #[test]
    fn crc32b_matches_php() {
        // php -r 'echo hash("crc32b", "The quick brown fox jumps over the lazy dog");'
        assert_eq!(
            crc32b(b"The quick brown fox jumps over the lazy dog"),
            0x414f_a339
        );
    }

    #[test]
    fn generated_tokens_have_sanctum_shape() {
        let t = generate_plaintext("");
        assert_eq!(t.len(), 48);
        let (entropy, checksum) = t.split_at(40);
        assert_eq!(checksum, format!("{:08x}", crc32b(entropy.as_bytes())));
    }

    #[test]
    fn debug_redacts_plaintext() {
        let t = parse_bearer_token("1|topsecret").unwrap();
        assert!(!format!("{t:?}").contains("topsecret"));
    }
}
