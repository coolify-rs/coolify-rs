//! Identifiers.

use rand::Rng;

/// Team ids are `bigint` in `teams`, but `personal_access_tokens.team_id` is
/// a string column upstream; parsing happens at the persistence boundary.
pub type TeamId = i64;

/// Team 0 is the instance-admin ("Root") team.
pub const ROOT_TEAM_ID: TeamId = 0;

/// Upstream `new_public_id()` (`bootstrap/helpers/shared.php:130-135`):
/// `Str::lower(Str::random(24))`, i.e. 24 lowercase alphanumerics.
pub fn new_public_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ids_match_upstream_shape() {
        let id = new_public_id();
        assert_eq!(id.len(), 24);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_ne!(new_public_id(), id);
    }
}
