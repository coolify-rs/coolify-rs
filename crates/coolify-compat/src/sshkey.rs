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
    /// `PrivateKey::generateNewKeyPair('rsa', 4096)`: what a key rotation creates for hosts that
    /// do not accept Ed25519, such as FIPS hosts.
    Rsa4096,
}

/// `generateSSHKey()`: Ed25519 in OpenSSH format, RSA in PKCS#1 PEM. RSA keys get 4096 bits
/// (v4.7): GitHub rejects new RSA SSH keys smaller than 3072 bits.
pub fn generate(kind: KeyKind) -> Option<String> {
    let mut rng = rand::rngs::OsRng;
    match kind {
        KeyKind::Ed25519 => PrivateKey::random(&mut rng, ssh_key::Algorithm::Ed25519)
            .ok()?
            .to_openssh(ssh_key::LineEnding::LF)
            .ok()
            .map(|pem| pem.to_string()),
        KeyKind::Rsa | KeyKind::Rsa4096 => {
            use rsa::pkcs1::EncodeRsaPrivateKey;
            rsa::RsaPrivateKey::new(&mut rng, 4096)
                .ok()?
                .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
                .ok()
                .map(|pem| pem.to_string())
        }
    }
}

/// The public key data of an unencrypted private key in a supported format.
fn key_data(private_key: &str) -> Option<KeyData> {
    let key = private_key.trim();
    if key.contains("BEGIN OPENSSH PRIVATE KEY") {
        let parsed = PrivateKey::from_openssh(key).ok()?;
        if parsed.is_encrypted() {
            return None;
        }
        Some(parsed.public_key().key_data().clone())
    } else if key.contains("BEGIN RSA PRIVATE KEY") {
        rsa_key_data(&rsa::RsaPrivateKey::from_pkcs1_pem(key).ok()?)
    } else if key.contains("BEGIN PRIVATE KEY") {
        rsa_key_data(&rsa::RsaPrivateKey::from_pkcs8_pem(key).ok()?)
    } else {
        None
    }
}

/// `PrivateKey::validationError($privateKey, $strict)` (v4.7.5): why a private key cannot be
/// used, or `None` when it is valid. Without `strict`, only checks that the key can be loaded.
///
/// phpseclib loads PuTTY and PEM DSA keys and then refuses them in strict mode; this crate does
/// not load them, so they are recognized by their header to give upstream's message.
pub fn validation_error(private_key: &str, strict: bool) -> Option<&'static str> {
    const INVALID: &str = "The private key is invalid. Paste an unencrypted private key, not a public key or a certificate.";
    const PUTTY: &str = "PuTTY and other key formats are not supported. Paste the key in OpenSSH or PEM format. It starts with \"-----BEGIN\".";
    const DSA: &str = "DSA keys are not supported. Use an Ed25519 or RSA key.";
    let trimmed = private_key.trim();
    if strict && trimmed.starts_with("PuTTY-User-Key-File-") {
        return Some(PUTTY);
    }
    if strict && trimmed.starts_with("-----BEGIN DSA PRIVATE KEY-----") {
        return Some(DSA);
    }
    let Some(data) = key_data(private_key) else {
        return Some(INVALID);
    };
    if !strict {
        return None;
    }
    if !trimmed.starts_with("-----BEGIN ") {
        return Some(PUTTY);
    }
    match data {
        KeyData::Dsa(_) => Some(DSA),
        // `$key->getLength() < 2048`: the bit length of the modulus.
        KeyData::Rsa(rsa) if rsa_bits(&rsa) < 2048 => {
            Some("RSA keys must have at least 2048 bits.")
        }
        KeyData::Rsa(_) | KeyData::Ed25519(_) | KeyData::Ecdsa(_) => None,
        _ => Some(
            "This key type is not supported. Use an Ed25519, ECDSA (nistp256, nistp384, or nistp521), or RSA key.",
        ),
    }
}

fn rsa_bits(key: &RsaPublicKey) -> usize {
    let bytes = key.n.as_positive_bytes().unwrap_or_default();
    match bytes.first() {
        Some(first) => bytes.len() * 8 - first.leading_zeros() as usize,
        None => 0,
    }
}

/// `PrivateKey::publicKeyBody()`: the key type and key data, without a comment. Lines in
/// authorized_keys are found by this value.
pub fn public_key_body(private_key: &str) -> Option<String> {
    let public = inspect(private_key)?.public_key;
    let parts: Vec<&str> = public.split_whitespace().take(2).collect();
    Some(parts.join(" "))
}

/// `PrivateKey::authorizedKeysLine()`: the authorized_keys line that Coolify adds. The comment
/// names the instance and the key, and does not contain the word "coolify", which old install
/// scripts remove. `instance` is `config('app.id') ?: config('app.url')`.
pub fn authorized_keys_line(private_key: &str, key_uuid: &str, instance: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(instance.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!(
        "{} cpk:{}:{key_uuid}",
        public_key_body(private_key)?,
        &hash[..12]
    ))
}

/// `AuthorizedKeys::fingerprint($publicKeyBody)`: the SHA256 fingerprint in the format of
/// `private_keys.fingerprint`, or `None` when the key cannot be read.
pub fn public_key_fingerprint(public_key_body: &str) -> Option<String> {
    let public = PublicKey::from_openssh(public_key_body.trim()).ok()?;
    Some(
        public
            .fingerprint(HashAlg::Sha256)
            .to_string()
            .trim_start_matches("SHA256:")
            .to_string(),
    )
}

/// Parses a private key and derives the public key and fingerprint.
/// Returns `None` for anything upstream would reject as invalid (and for the
/// formats listed in the module docs).
pub fn inspect(private_key: &str) -> Option<KeyInfo> {
    let data = key_data(private_key)?;
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

    // `PrivateKey::validationError()` at v4.7.5 (upstream tests/Unit/PrivateKeyValidationTest).
    #[test]
    fn validation_errors_follow_upstream() {
        let ed = generate(KeyKind::Ed25519).expect("generate");
        assert_eq!(validation_error(&ed, true), None);
        assert_eq!(validation_error(&format!("\n  {ed}"), true), None);
        let invalid = "The private key is invalid. Paste an unencrypted private key, not a public key or a certificate.";
        assert_eq!(validation_error("garbage", true), Some(invalid));
        assert_eq!(validation_error("garbage", false), Some(invalid));
        assert_eq!(
            validation_error(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68",
                true
            ),
            Some(invalid)
        );
        assert_eq!(
            validation_error(
                "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\n",
                true
            ),
            Some(
                "PuTTY and other key formats are not supported. Paste the key in OpenSSH or PEM format. It starts with \"-----BEGIN\"."
            )
        );
        assert_eq!(
            validation_error(
                "-----BEGIN DSA PRIVATE KEY-----\nMIIB\n-----END DSA PRIVATE KEY-----",
                true
            ),
            Some("DSA keys are not supported. Use an Ed25519 or RSA key.")
        );
    }

    #[test]
    fn short_rsa_keys_are_refused_only_in_strict_mode() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let short = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024)
            .expect("generate")
            .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
            .expect("pem")
            .to_string();
        assert_eq!(
            validation_error(&short, true),
            Some("RSA keys must have at least 2048 bits.")
        );
        // A server transfer accepts any key that can be loaded.
        assert_eq!(validation_error(&short, false), None);
        let rsa = generate(KeyKind::Rsa).expect("generate");
        assert_eq!(validation_error(&rsa, true), None);
    }

    #[test]
    fn authorized_keys_line_names_the_instance_and_the_key() {
        let ed = generate(KeyKind::Ed25519).expect("generate");
        let body = public_key_body(&ed).expect("body");
        let parts: Vec<&str> = body.split(' ').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "ssh-ed25519");
        let line = authorized_keys_line(&ed, "key-uuid", "https://coolify.example").expect("line");
        // sha256("https://coolify.example") is not needed here: the comment has 12 hex characters.
        let comment = line.strip_prefix(&format!("{body} ")).expect("comment");
        let fields: Vec<&str> = comment.split(':').collect();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], "cpk");
        assert_eq!(fields[1].len(), 12);
        assert!(fields[1].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(fields[2], "key-uuid");
        assert!(!line.to_lowercase().contains("coolify"));
        // The fingerprint of the body is the fingerprint of the key.
        assert_eq!(
            public_key_fingerprint(&body),
            inspect(&ed).map(|i| i.fingerprint)
        );
        assert_eq!(public_key_fingerprint("not a key"), None);
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
