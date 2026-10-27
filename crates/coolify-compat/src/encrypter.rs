//! Laravel `Illuminate\Encryption\Encrypter` compatible AES-256-CBC encryption.
//!
//! Upstream stores encrypted model attributes (Laravel `encrypted` casts) as
//! `base64(json({"iv","value","mac","tag"}))` where:
//!
//! * `iv` is base64 of a random 16-byte IV,
//! * `value` is base64 of the AES-256-CBC (PKCS#7) ciphertext,
//! * `mac` is hex HMAC-SHA256 over `iv_b64 || value_b64` keyed with APP_KEY,
//! * `tag` is empty for CBC ciphers.
//!
//! The key is `APP_KEY`, usually written as `base64:<32 bytes>`. Laravel also
//! accepts `APP_PREVIOUS_KEYS` for rotation; [`Encrypter::with_previous_keys`]
//! mirrors that: encryption always uses the current key, decryption tries each.

use aes::Aes256;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::php_serialize;

type HmacSha256 = Hmac<Sha256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const KEY_LEN: usize = 32;
const IV_LEN: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EncrypterKeyError {
    #[error("APP_KEY is empty")]
    Empty,
    #[error("APP_KEY is not valid base64")]
    InvalidBase64,
    #[error("APP_KEY must be {KEY_LEN} bytes for AES-256-CBC, got {0}")]
    WrongLength(usize),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecryptError {
    #[error("the payload is invalid")]
    InvalidPayload,
    #[error("the MAC is invalid")]
    InvalidMac,
    #[error("could not decrypt the data")]
    DecryptionFailed,
}

/// Key material is deliberately not `Debug`-printable.
#[derive(Clone)]
struct Key([u8; KEY_LEN]);

impl Key {
    fn parse(raw: &str) -> Result<Self, EncrypterKeyError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(EncrypterKeyError::Empty);
        }
        let bytes = match raw.strip_prefix("base64:") {
            Some(encoded) => B64
                .decode(encoded)
                .map_err(|_| EncrypterKeyError::InvalidBase64)?,
            None => raw.as_bytes().to_vec(),
        };
        let arr: [u8; KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| EncrypterKeyError::WrongLength(bytes.len()))?;
        Ok(Self(arr))
    }
}

#[derive(Clone)]
pub struct Encrypter {
    current: Key,
    previous: Vec<Key>,
    /// The `APP_KEY` string as configured (`config('app.key')`), for HMACs keyed with it.
    app_key: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for Encrypter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encrypter")
            .field("previous_keys", &self.previous.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Payload {
    iv: String,
    value: String,
    mac: String,
    #[serde(default)]
    tag: Option<String>,
}

impl Encrypter {
    /// Builds an encrypter from an `APP_KEY` value (`base64:...` or raw).
    pub fn new(app_key: &str) -> Result<Self, EncrypterKeyError> {
        Ok(Self {
            current: Key::parse(app_key)?,
            previous: Vec::new(),
            app_key: zeroize::Zeroizing::new(app_key.to_owned()),
        })
    }

    /// `hash_hmac('sha256', $data, config('app.key'))`, lower-case hex.
    pub fn hmac_with_app_key(&self, data: &[u8]) -> String {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(self.app_key.as_bytes()) else {
            return String::new();
        };
        mac.update(data);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Adds `APP_PREVIOUS_KEYS` (comma separated, as Laravel reads them).
    pub fn with_previous_keys(mut self, previous: &str) -> Result<Self, EncrypterKeyError> {
        for raw in previous.split(',').filter(|k| !k.trim().is_empty()) {
            self.previous.push(Key::parse(raw)?);
        }
        Ok(self)
    }

    /// Equivalent of `Crypt::encryptString` (what `encrypted` casts use).
    pub fn encrypt_string(&self, plaintext: &str) -> String {
        self.encrypt_bytes(plaintext.as_bytes())
    }

    /// Equivalent of the `encrypt()` helper (`$serialize = true`) for a string:
    /// the plaintext is PHP-serialized (`s:<bytes>:"...";`) before encryption.
    pub fn encrypt_serialized(&self, plaintext: &str) -> String {
        self.encrypt_bytes(format!("s:{}:\"{}\";", plaintext.len(), plaintext).as_bytes())
    }

    /// Equivalent of `Crypt::decryptString`.
    pub fn decrypt_string(&self, payload: &str) -> Result<String, DecryptError> {
        let bytes = self.decrypt_bytes(payload)?;
        String::from_utf8(bytes).map_err(|_| DecryptError::DecryptionFailed)
    }

    /// Equivalent of the `decrypt()` helper with `$unserialize = true` for
    /// payloads holding a PHP-serialized string. Falls back to the raw string
    /// when the plaintext is not a serialized PHP string, which is what
    /// upstream data written by `encryptString` looks like.
    pub fn decrypt_maybe_serialized(&self, payload: &str) -> Result<String, DecryptError> {
        let plain = self.decrypt_string(payload)?;
        Ok(php_serialize::unserialize_string(&plain).unwrap_or(plain))
    }

    fn encrypt_bytes(&self, plaintext: &[u8]) -> String {
        let mut iv = [0u8; IV_LEN];
        rand::thread_rng().fill_bytes(&mut iv);
        let ciphertext = Aes256CbcEnc::new(&self.current.0.into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        let iv_b64 = B64.encode(iv);
        let value_b64 = B64.encode(ciphertext);
        let mac = hex::encode(compute_mac(&self.current, &iv_b64, &value_b64));
        let payload = Payload {
            iv: iv_b64,
            value: value_b64,
            mac,
            tag: Some(String::new()),
        };
        // Serializing a struct of strings cannot fail.
        let json = serde_json::to_string(&payload).unwrap_or_default();
        B64.encode(json)
    }

    fn decrypt_bytes(&self, payload: &str) -> Result<Vec<u8>, DecryptError> {
        let json = B64
            .decode(payload.trim())
            .map_err(|_| DecryptError::InvalidPayload)?;
        let payload: Payload =
            serde_json::from_slice(&json).map_err(|_| DecryptError::InvalidPayload)?;
        let iv = B64
            .decode(&payload.iv)
            .map_err(|_| DecryptError::InvalidPayload)?;
        if iv.len() != IV_LEN {
            return Err(DecryptError::InvalidPayload);
        }
        let mac = hex::decode(&payload.mac).map_err(|_| DecryptError::InvalidPayload)?;
        let ciphertext = B64
            .decode(&payload.value)
            .map_err(|_| DecryptError::InvalidPayload)?;

        // Laravel tries the current key first, then each previous key.
        let key = std::iter::once(&self.current)
            .chain(self.previous.iter())
            .find(|key| {
                let expected = compute_mac(key, &payload.iv, &payload.value);
                bool::from(expected.as_slice().ct_eq(&mac))
            })
            .ok_or(DecryptError::InvalidMac)?;

        let iv: [u8; IV_LEN] = iv
            .as_slice()
            .try_into()
            .map_err(|_| DecryptError::InvalidPayload)?;
        Aes256CbcDec::new(&key.0.into(), &iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(&ciphertext)
            .map_err(|_| DecryptError::DecryptionFailed)
    }
}

fn compute_mac(key: &Key, iv_b64: &str, value_b64: &str) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&key.0).unwrap_or_else(|_| {
        // HMAC accepts keys of any length; this branch is unreachable.
        unreachable!("HMAC-SHA256 accepts any key length")
    });
    mac.update(iv_b64.as_bytes());
    mac.update(value_b64.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "base64:hPAFGDXpvHW0ZnwoP/YKkC1WJ+9Bv2jZCwc1hGGaI3s=";

    #[test]
    fn round_trips() {
        let enc = Encrypter::new(KEY).unwrap();
        let payload = enc.encrypt_string("s3cr3t value ✓");
        assert_eq!(enc.decrypt_string(&payload).unwrap(), "s3cr3t value ✓");
    }

    #[test]
    fn serialized_round_trip() {
        let enc = Encrypter::new(KEY).unwrap();
        let payload = enc.encrypt_serialized(r#"{"server_uuid":"abc"}"#);
        assert_eq!(
            enc.decrypt_string(&payload).unwrap(),
            r#"s:21:"{"server_uuid":"abc"}";"#
        );
        assert_eq!(
            enc.decrypt_maybe_serialized(&payload).unwrap(),
            r#"{"server_uuid":"abc"}"#
        );
    }

    #[test]
    fn rejects_tampered_mac() {
        let enc = Encrypter::new(KEY).unwrap();
        let payload = enc.encrypt_string("x");
        let json = B64.decode(&payload).unwrap();
        let mut p: Payload = serde_json::from_slice(&json).unwrap();
        p.value = B64.encode(b"0123456789abcdef");
        let forged = B64.encode(serde_json::to_vec(&p).unwrap());
        assert_eq!(enc.decrypt_string(&forged), Err(DecryptError::InvalidMac));
    }

    #[test]
    fn rejects_wrong_key() {
        let a = Encrypter::new(KEY).unwrap();
        let b = Encrypter::new("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap();
        let payload = a.encrypt_string("x");
        assert_eq!(b.decrypt_string(&payload), Err(DecryptError::InvalidMac));
    }

    #[test]
    fn previous_keys_decrypt_old_payloads() {
        let old = Encrypter::new(KEY).unwrap();
        let payload = old.encrypt_string("rotated");
        let new = Encrypter::new("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .unwrap()
            .with_previous_keys(KEY)
            .unwrap();
        assert_eq!(new.decrypt_string(&payload).unwrap(), "rotated");
    }

    #[test]
    fn key_validation() {
        assert_eq!(Encrypter::new("").unwrap_err(), EncrypterKeyError::Empty);
        assert_eq!(
            Encrypter::new("base64:c2hvcnQ=").unwrap_err(),
            EncrypterKeyError::WrongLength(5)
        );
        // A raw 32-character key is accepted as bytes, like Laravel.
        assert!(Encrypter::new("0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn debug_does_not_leak_key() {
        let enc = Encrypter::new(KEY).unwrap();
        assert!(!format!("{enc:?}").contains("hPAF"));
    }
}
