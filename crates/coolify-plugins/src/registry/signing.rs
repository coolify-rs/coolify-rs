//! Ed25519 signatures over `index.json` (ADR 0010, section 7).
//!
//! The registry's CI signs the exact bytes of `index.json` and publishes the signature next to it as
//! `index.json.sig`, a small JSON document naming the key. coolify-rs trusts only the public keys compiled
//! into it, each until its `not_after` date, so a key can be rotated by shipping its successor before it
//! expires. In development mode more keys can be trusted with `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`.

use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// A public key coolify-rs accepts signatures from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub id: String,
    pub key: VerifyingKey,
    /// The last day (UTC) signatures by this key are accepted.
    pub not_after: NaiveDate,
}

/// Production registry keys, compiled in.
///
/// TODO(registry): no production key exists yet. The maintainers generate one with
/// `coolify-rs plugin keygen`, keep the secret half only in the registry repository's Actions secrets,
/// and add the public half here as `key("<id>", "<hex>", "<not_after>")`. Until then no index verifies
/// unless a key is added in development mode.
const PRODUCTION_KEYS: &[(&str, &str, &str)] = &[];

/// FOR TESTS AND LOCAL DEVELOPMENT ONLY. Its secret half is committed to this repository
/// (`crates/coolify-plugins/tests/fixtures/registry-dev.key`), so anyone can sign with it. It is never
/// trusted by default; tests trust it explicitly, and a developer can trust it in development mode by
/// passing [`DEV_KEY_ENV`] in `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`.
pub const DEV_KEY_ID: &str = "coolify-rs-dev-insecure";
pub const DEV_PUBLIC_KEY: &str = "dda8378d4cb93e1d432a7f77da35c35c58ecca29d13cc5551492af69016e493c";
pub const DEV_KEY_NOT_AFTER: &str = "2099-12-31";
/// [`development_key`] in the format of `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`.
pub const DEV_KEY_ENV: &str = "coolify-rs-dev-insecure:dda8378d4cb93e1d432a7f77da35c35c58ecca29d13cc5551492af69016e493c:2099-12-31";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("the signature file is not valid: {0}")]
    Malformed(String),
    #[error("the index is signed by key {0:?}, which this coolify-rs does not trust")]
    UnknownKey(String),
    #[error("the index is signed by key {id:?}, which expired on {not_after}")]
    ExpiredKey { id: String, not_after: NaiveDate },
    #[error("the signature does not match the index")]
    Mismatch,
}

/// `index.json.sig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureFile {
    pub algorithm: String,
    pub key_id: String,
    /// Base64 of the 64-byte Ed25519 signature.
    pub signature: String,
}

/// The secret half of a registry key, as `coolify-rs plugin keygen` writes it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretKeyFile {
    pub key_id: String,
    /// Hex of the 32-byte Ed25519 seed.
    pub secret_key: String,
}

impl std::fmt::Debug for SecretKeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKeyFile")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// The public half, ready to compile in or to pass in `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKeyFile {
    pub key_id: String,
    /// Hex of the 32-byte Ed25519 public key.
    pub public_key: String,
    pub not_after: NaiveDate,
}

impl PublicKeyFile {
    pub fn trusted(&self) -> Result<TrustedKey, String> {
        key(&self.key_id, &self.public_key, &self.not_after.to_string())
    }
}

fn key(id: &str, public_hex: &str, not_after: &str) -> Result<TrustedKey, String> {
    if id.is_empty() || id.len() > 64 || id.contains([':', ',']) || id.trim() != id {
        return Err(format!("invalid key id {id:?}"));
    }
    let bytes: [u8; 32] = hex::decode(public_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("key {id}: the public key must be 64 hex digits"))?;
    let key = VerifyingKey::from_bytes(&bytes)
        .map_err(|_| format!("key {id}: not an Ed25519 public key"))?;
    let not_after = NaiveDate::parse_from_str(not_after, "%Y-%m-%d")
        .map_err(|_| format!("key {id}: not_after must be YYYY-MM-DD"))?;
    Ok(TrustedKey {
        id: id.to_owned(),
        key,
        not_after,
    })
}

/// The compiled-in production keys.
pub fn production_keys() -> Vec<TrustedKey> {
    PRODUCTION_KEYS
        .iter()
        .filter_map(|(id, public, not_after)| match key(id, public, not_after) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::error!(error = %e, "a compiled-in registry key is invalid");
                None
            }
        })
        .collect()
}

/// The development key. See [`DEV_KEY_ID`]: never trust it in production.
pub fn development_key() -> TrustedKey {
    key(DEV_KEY_ID, DEV_PUBLIC_KEY, DEV_KEY_NOT_AFTER)
        .unwrap_or_else(|e| unreachable!("the development key is valid: {e}"))
}

/// Parses `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`: `id:hex-public-key:YYYY-MM-DD`, separated by commas.
pub fn parse_keys(text: &str) -> Result<Vec<TrustedKey>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let parts: Vec<&str> = item.split(':').collect();
            match parts.as_slice() {
                [id, public, not_after] => key(id, public, not_after),
                _ => Err(format!(
                    "{item:?}: each key must be id:hex-public-key:YYYY-MM-DD"
                )),
            }
        })
        .collect()
}

/// Signs `bytes`, returning the contents of the `.sig` file.
pub fn sign(bytes: &[u8], secret: &SecretKeyFile) -> Result<Vec<u8>, String> {
    let signing = signing_key(secret)?;
    let file = SignatureFile {
        algorithm: "ed25519".into(),
        key_id: secret.key_id.clone(),
        signature: base64::engine::general_purpose::STANDARD.encode(signing.sign(bytes).to_bytes()),
    };
    let mut out = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    out.push(b'\n');
    Ok(out)
}

fn signing_key(secret: &SecretKeyFile) -> Result<SigningKey, String> {
    let seed: [u8; 32] = hex::decode(secret.secret_key.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("the secret key must be 64 hex digits")?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The public half of a secret key.
pub fn public_half(secret: &SecretKeyFile, not_after: NaiveDate) -> Result<PublicKeyFile, String> {
    let signing = signing_key(secret)?;
    Ok(PublicKeyFile {
        key_id: secret.key_id.clone(),
        public_key: hex::encode(signing.verifying_key().as_bytes()),
        not_after,
    })
}

/// A new key pair from the operating system's random source.
pub fn generate(
    key_id: &str,
    not_after: NaiveDate,
) -> Result<(SecretKeyFile, PublicKeyFile), String> {
    use rand::RngCore as _;
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let secret = SecretKeyFile {
        key_id: key_id.to_owned(),
        secret_key: hex::encode(seed),
    };
    let public = public_half(&secret, not_after)?;
    public.trusted()?;
    Ok((secret, public))
}

/// Checks that `signature` (the `.sig` file) is a valid signature of exactly `bytes` by a key in `keys` that
/// has not expired at `now`. Returns that key.
pub fn verify<'k>(
    bytes: &[u8],
    signature: &[u8],
    keys: &'k [TrustedKey],
    now: DateTime<Utc>,
) -> Result<&'k TrustedKey, SignatureError> {
    let file: SignatureFile =
        serde_json::from_slice(signature).map_err(|e| SignatureError::Malformed(e.to_string()))?;
    if file.algorithm != "ed25519" {
        return Err(SignatureError::Malformed(format!(
            "unsupported algorithm {:?}",
            file.algorithm
        )));
    }
    let raw: [u8; 64] = base64::engine::general_purpose::STANDARD
        .decode(file.signature.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| {
            SignatureError::Malformed("the signature must be 64 bytes, base64".into())
        })?;
    let key = keys
        .iter()
        .find(|k| k.id == file.key_id)
        .ok_or_else(|| SignatureError::UnknownKey(file.key_id.clone()))?;
    if now.date_naive() > key.not_after {
        return Err(SignatureError::ExpiredKey {
            id: key.id.clone(),
            not_after: key.not_after,
        });
    }
    key.key
        .verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&raw))
        .map_err(|_| SignatureError::Mismatch)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(id: &str, not_after: &str) -> (SecretKeyFile, TrustedKey) {
        let date = NaiveDate::parse_from_str(not_after, "%Y-%m-%d").unwrap();
        let (secret, public) = generate(id, date).unwrap();
        (secret, public.trusted().unwrap())
    }

    fn now() -> DateTime<Utc> {
        "2026-11-06T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn signatures_verify_only_for_the_exact_bytes() {
        let (secret, key) = pair("test-1", "2030-01-01");
        let keys = vec![key];
        let index = br#"{"format":1}"#;
        let sig = sign(index, &secret).unwrap();
        assert_eq!(verify(index, &sig, &keys, now()).unwrap().id, "test-1");

        let tampered = br#"{"format":2}"#;
        assert_eq!(
            verify(tampered, &sig, &keys, now()).unwrap_err(),
            SignatureError::Mismatch
        );
        let mut extra = index.to_vec();
        extra.push(b'\n');
        assert_eq!(
            verify(&extra, &sig, &keys, now()).unwrap_err(),
            SignatureError::Mismatch
        );
    }

    #[test]
    fn unknown_and_expired_keys_are_refused() {
        let (secret, _) = pair("stranger", "2030-01-01");
        let (_, trusted) = pair("test-1", "2030-01-01");
        let index = b"{}";
        let sig = sign(index, &secret).unwrap();
        assert_eq!(
            verify(index, &sig, std::slice::from_ref(&trusted), now()).unwrap_err(),
            SignatureError::UnknownKey("stranger".into())
        );

        // A key with the right id but other key material is a mismatch, not a pass.
        let (impostor, _) = pair("test-1", "2030-01-01");
        let sig = sign(index, &impostor).unwrap();
        assert_eq!(
            verify(index, &sig, std::slice::from_ref(&trusted), now()).unwrap_err(),
            SignatureError::Mismatch
        );

        let (secret, old) = pair("old", "2026-11-05");
        let sig = sign(index, &secret).unwrap();
        let err = verify(index, &sig, std::slice::from_ref(&old), now()).unwrap_err();
        assert!(matches!(err, SignatureError::ExpiredKey { .. }), "{err}");
        // Still valid on its last day.
        let last_day = "2026-11-05T23:59:59Z".parse().unwrap();
        assert!(verify(index, &sig, std::slice::from_ref(&old), last_day).is_ok());
    }

    #[test]
    fn malformed_signature_files_are_refused() {
        let (_, key) = pair("test-1", "2030-01-01");
        let keys = vec![key];
        for bad in [
            &b"not json"[..],
            br#"{"algorithm":"rsa","key_id":"test-1","signature":""}"#,
            br#"{"algorithm":"ed25519","key_id":"test-1","signature":"AAAA"}"#,
        ] {
            let err = verify(b"{}", bad, &keys, now()).unwrap_err();
            assert!(matches!(err, SignatureError::Malformed(_)), "{err}");
        }
    }

    #[test]
    fn parses_extra_keys() {
        let keys = parse_keys(DEV_KEY_ENV).unwrap();
        assert_eq!(keys, vec![development_key()]);
        assert!(parse_keys("").unwrap().is_empty());
        for bad in [
            "id:abc:2030-01-01",
            "id-only",
            &format!("id:{DEV_PUBLIC_KEY}:tomorrow"),
        ] {
            assert!(parse_keys(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn no_production_key_is_invented() {
        // The production key is added by the registry maintainers; see PRODUCTION_KEYS.
        assert!(
            production_keys()
                .iter()
                .all(|k| k.id != DEV_KEY_ID && !k.id.contains("dev"))
        );
    }
}
