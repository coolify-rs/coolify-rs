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

/// Key types offered by "New private key" (upstream `generateSSHKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Ed25519,
    Rsa,
}

/// `generateSSHKey()`: Ed25519 in OpenSSH format, RSA (phpseclib's default
/// 2048 bits) in PKCS#1 PEM.
pub fn generate(kind: KeyKind) -> Option<String> {
    let mut rng = rand::rngs::OsRng;
    match kind {
        KeyKind::Ed25519 => PrivateKey::random(&mut rng, ssh_key::Algorithm::Ed25519)
            .ok()?
            .to_openssh(ssh_key::LineEnding::LF)
            .ok()
            .map(|pem| pem.to_string()),
        KeyKind::Rsa => {
            use rsa::pkcs1::EncodeRsaPrivateKey;
            rsa::RsaPrivateKey::new(&mut rng, 2048)
                .ok()?
                .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
                .ok()
                .map(|pem| pem.to_string())
        }
    }
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
    #[test]
    fn generated_keys_are_accepted() {
        for kind in [super::KeyKind::Ed25519, super::KeyKind::Rsa] {
            let pem = super::generate(kind).expect("generate");
            assert!(super::inspect(&pem).is_some(), "{kind:?} key not accepted");
        }
    }

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
