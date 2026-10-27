//! Private key handling compatible with upstream's phpseclib usage
//! (`PrivateKey::validatePrivateKey`, `extractPublicKeyFromPrivate`,
//! `generateFingerprint`, `formatPrivateKey`).
//!
//! Supported inputs: unencrypted OpenSSH private keys (Ed25519, ECDSA, RSA)
//! and RSA keys in PKCS#1 or PKCS#8 PEM. phpseclib accepts more exotic
//! formats (PuTTY, PKCS#8 EC); those are rejected here (KNOWN_LIMITATIONS).

use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use ssh_key::public::{KeyData, RsaPublicKey};
use ssh_key::{HashAlg, Mpint, PrivateKey, PublicKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// `getPublicKey()->toString('OpenSSH', ['comment' => ''])`: note the
    /// trailing space phpseclib leaves for the empty comment.
    pub public_key: String,
    /// `getFingerprint('sha256')`: unpadded base64, no `SHA256:` prefix.
    pub fingerprint: String,
}

/// `formatPrivateKey`: trim, then ensure exactly one trailing newline.
pub fn format_private_key(key: &str) -> String {
    format!("{}\n", key.trim())
}

/// Parses a private key and derives the public key and fingerprint.
/// Returns `None` for anything upstream would reject as invalid (and for the
/// formats listed in the module docs).
pub fn inspect(private_key: &str) -> Option<KeyInfo> {
    let key = private_key.trim();
    let data = if key.contains("BEGIN OPENSSH PRIVATE KEY") {
        let parsed = PrivateKey::from_openssh(key).ok()?;
        if parsed.is_encrypted() {
            return None;
        }
        parsed.public_key().key_data().clone()
    } else if key.contains("BEGIN RSA PRIVATE KEY") {
        rsa_key_data(&rsa::RsaPrivateKey::from_pkcs1_pem(key).ok()?)?
    } else if key.contains("BEGIN PRIVATE KEY") {
        rsa_key_data(&rsa::RsaPrivateKey::from_pkcs8_pem(key).ok()?)?
    } else {
        return None;
    };
    let public = PublicKey::new(data, "");
    let openssh = public.to_openssh().ok()?;
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    Some(KeyInfo {
        public_key: format!("{} ", openssh.trim_end()),
        fingerprint: fingerprint.trim_start_matches("SHA256:").to_string(),
    })
}

fn rsa_key_data(key: &rsa::RsaPrivateKey) -> Option<KeyData> {
    key.validate().ok()?;
    let e = Mpint::from_positive_bytes(&key.e().to_bytes_be()).ok()?;
    let n = Mpint::from_positive_bytes(&key.n().to_bytes_be()).ok()?;
    Some(KeyData::Rsa(RsaPublicKey { e, n }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_upstream() {
        assert_eq!(
            format_private_key("  -----BEGIN X-----\nabc\n-----END X-----\n\n"),
            "-----BEGIN X-----\nabc\n-----END X-----\n"
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(inspect("garbage"), None);
        assert_eq!(
            inspect("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"),
            None
        );
    }
}
