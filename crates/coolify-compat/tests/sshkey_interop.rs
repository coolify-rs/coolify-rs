//! Public keys and fingerprints must equal what upstream (phpseclib) computes.
//! The fixture holds throwaway keys and upstream's API output for them.

use coolify_compat::sshkey::{format_private_key, inspect};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    file: String,
    private_key: String,
    stored_private_key: String,
    public_key: String,
    fingerprint: String,
}

#[test]
fn matches_upstream_for_every_supported_format() {
    let fx: Fixture = serde_json::from_str(include_str!("fixtures/ssh_keys.json")).unwrap();
    assert_eq!(fx.cases.len(), 4);
    for c in &fx.cases {
        let info = inspect(&c.private_key).unwrap_or_else(|| panic!("{} rejected", c.file));
        assert_eq!(info.public_key, c.public_key, "{}", c.file);
        assert_eq!(info.fingerprint, c.fingerprint, "{}", c.file);
        assert_eq!(
            format_private_key(&c.private_key),
            c.stored_private_key,
            "{}",
            c.file
        );
    }
}
