//! WebAuthn registration as `laravel/passkeys` (`web-auth/webauthn-lib`) checks it, and the
//! `credential` JSON it stores. Attestation is `none`, so the only statement accepted is `none`.

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// `Base64UrlSafe::encodeUnpadded`.
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Unpadded or padded base64url.
pub fn from_b64(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .ok()
}

/// A CBOR value, as far as attestation objects need it.
#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Other,
}

impl Cbor {
    fn get(&self, key: &str) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m
                .iter()
                .find(|(k, _)| matches!(k, Cbor::Text(t) if t == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn get_int(&self, key: i64) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m
                .iter()
                .find(|(k, _)| matches!(k, Cbor::Int(i) if *i == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Decodes one CBOR item; returns it and how many bytes it took.
fn decode_cbor(b: &[u8], depth: usize) -> Option<(Cbor, usize)> {
    if depth > 16 {
        return None;
    }
    let first = *b.first()?;
    let major = first >> 5;
    let info = first & 0x1f;
    let (arg, mut pos) = match info {
        0..=23 => (u64::from(info), 1),
        24 => (u64::from(*b.get(1)?), 2),
        25 => (
            u64::from(u16::from_be_bytes(b.get(1..3)?.try_into().ok()?)),
            3,
        ),
        26 => (
            u64::from(u32::from_be_bytes(b.get(1..5)?.try_into().ok()?)),
            5,
        ),
        27 => (u64::from_be_bytes(b.get(1..9)?.try_into().ok()?), 9),
        _ => return None,
    };
    let len = usize::try_from(arg).ok();
    Some(match major {
        0 => (Cbor::Int(i64::try_from(arg).ok()?), pos),
        1 => (Cbor::Int(-1 - i64::try_from(arg).ok()?), pos),
        2 => {
            let end = pos.checked_add(len?)?;
            (Cbor::Bytes(b.get(pos..end)?.to_vec()), end)
        }
        3 => {
            let end = pos.checked_add(len?)?;
            (
                Cbor::Text(String::from_utf8(b.get(pos..end)?.to_vec()).ok()?),
                end,
            )
        }
        4 => {
            let mut items = Vec::new();
            for _ in 0..len? {
                let (v, used) = decode_cbor(b.get(pos..)?, depth + 1)?;
                items.push(v);
                pos += used;
            }
            (Cbor::Array(items), pos)
        }
        5 => {
            let mut items = Vec::new();
            for _ in 0..len? {
                let (k, used) = decode_cbor(b.get(pos..)?, depth + 1)?;
                pos += used;
                let (v, used) = decode_cbor(b.get(pos..)?, depth + 1)?;
                pos += used;
                items.push((k, v));
            }
            (Cbor::Map(items), pos)
        }
        7 => (Cbor::Other, pos),
        _ => return None,
    })
}

/// What a registration has to match.
#[derive(Debug, Clone)]
pub struct Expected<'a> {
    /// The challenge of the options, base64url.
    pub challenge: &'a str,
    pub rp_id: &'a str,
    pub origins: &'a [String],
    /// The user handle of the options, base64url.
    pub user_handle: &'a str,
}

/// A checked credential.
#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    /// `passkeys.credential_id`.
    pub credential_id: String,
    /// `passkeys.credential` in upstream's key order.
    pub credential: Value,
}

/// `AuthenticatorAttestationResponseValidator::check` for the browser's `credential`. Errors are
/// one message, as `InvalidPasskeyException` shows them.
pub fn verify_registration(
    credential: &Value,
    expected: &Expected<'_>,
) -> Result<Registered, String> {
    const FAILED: &str = "Unable to register passkey. Please try again.";
    let fail = || FAILED.to_string();
    let response = credential.get("response").ok_or_else(fail)?;
    let field = |k: &str| {
        response
            .get(k)
            .and_then(Value::as_str)
            .and_then(from_b64)
            .ok_or_else(fail)
    };
    let client_data_raw = field("clientDataJSON")?;
    let attestation_raw = field("attestationObject")?;
    let client: Value = serde_json::from_slice(&client_data_raw).map_err(|_| fail())?;
    if client.get("type").and_then(Value::as_str) != Some("webauthn.create") {
        return Err(fail());
    }
    let challenge = client
        .get("challenge")
        .and_then(Value::as_str)
        .and_then(from_b64);
    if challenge.is_none() || challenge != from_b64(expected.challenge) {
        return Err(fail());
    }
    let origin = client.get("origin").and_then(Value::as_str).unwrap_or("");
    if !expected.origins.iter().any(|o| o == origin) {
        return Err(fail());
    }
    let (attestation, _) = decode_cbor(&attestation_raw, 0).ok_or_else(fail)?;
    if !matches!(attestation.get("fmt"), Some(Cbor::Text(f)) if f == "none") {
        return Err(fail());
    }
    if !matches!(attestation.get("attStmt"), Some(Cbor::Map(m)) if m.is_empty()) {
        return Err(fail());
    }
    let Some(Cbor::Bytes(auth)) = attestation.get("authData") else {
        return Err(fail());
    };
    // rpIdHash(32) flags(1) counter(4) aaguid(16) credIdLen(2) credId COSE key
    if auth.len() < 55 {
        return Err(fail());
    }
    let rp_hash: [u8; 32] = Sha256::digest(expected.rp_id.as_bytes()).into();
    if auth[..32] != rp_hash {
        return Err(fail());
    }
    let flags = auth[32];
    let (user_present, user_verified, backup_eligible, backup_state, attested) = (
        flags & 0x01 != 0,
        flags & 0x04 != 0,
        flags & 0x08 != 0,
        flags & 0x10 != 0,
        flags & 0x40 != 0,
    );
    if !user_present || !user_verified || !attested {
        return Err(fail());
    }
    let counter = u32::from_be_bytes(auth[33..37].try_into().map_err(|_| fail())?);
    let aaguid = uuid_text(&auth[37..53]);
    let id_len = usize::from(u16::from_be_bytes(
        auth[53..55].try_into().map_err(|_| fail())?,
    ));
    let id_end = 55usize.checked_add(id_len).ok_or_else(fail)?;
    let credential_id = auth.get(55..id_end).ok_or_else(fail)?;
    let (cose, used) = decode_cbor(auth.get(id_end..).ok_or_else(fail)?, 0).ok_or_else(fail)?;
    let cose_raw = &auth[id_end..id_end + used];
    // Only the algorithms offered in the options: ES256 and RS256.
    if !matches!(cose.get_int(3), Some(Cbor::Int(-7 | -257))) {
        return Err(fail());
    }
    let transports: Vec<Value> = response
        .get("transports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let credential_id = b64(credential_id);
    Ok(Registered {
        credential_id: credential_id.clone(),
        credential: json!({
            "publicKeyCredentialId": credential_id,
            "type": "public-key",
            "transports": transports,
            "attestationType": "none",
            "trustPath": [],
            "aaguid": aaguid,
            "credentialPublicKey": b64(cose_raw),
            "userHandle": expected.user_handle,
            "counter": counter,
            "backupEligible": backup_eligible,
            "backupStatus": backup_state,
            "uvInitialized": user_verified,
        }),
    })
}

/// What an assertion has to match: `PublicKeyCredentialRequestOptions` and the relying party.
#[derive(Debug, Clone)]
pub struct AssertionExpected<'a> {
    /// The challenge of the options, base64url.
    pub challenge: &'a str,
    pub rp_id: &'a str,
    pub origins: &'a [String],
}

/// `RSASSA-PKCS1-v1_5` with SHA-256 for a public key given as big-endian modulus and exponent.
pub fn rs256_verify(n: &[u8], e: &[u8], message: &[u8], signature: &[u8]) -> bool {
    use rsa::{BigUint, Pkcs1v15Sign, RsaPublicKey};
    /// `DigestInfo` of SHA-256.
    const PREFIX: [u8; 19] = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];
    let Ok(key) = RsaPublicKey::new(BigUint::from_bytes_be(n), BigUint::from_bytes_be(e)) else {
        return false;
    };
    let scheme = Pkcs1v15Sign {
        hash_len: Some(32),
        prefix: PREFIX.to_vec().into_boxed_slice(),
    };
    let hashed: [u8; 32] = Sha256::digest(message).into();
    key.verify(scheme, &hashed, signature).is_ok()
}

/// The signature of an assertion against a COSE key: ES256 and RS256, as registration accepts.
fn cose_verify(cose: &Cbor, message: &[u8], signature: &[u8]) -> bool {
    match cose.get_int(3) {
        Some(Cbor::Int(-7)) => {
            use p256::ecdsa::signature::Verifier;
            use p256::ecdsa::{Signature, VerifyingKey};
            let (Some(Cbor::Bytes(x)), Some(Cbor::Bytes(y))) = (cose.get_int(-2), cose.get_int(-3))
            else {
                return false;
            };
            let mut point = vec![0x04];
            point.extend_from_slice(x);
            point.extend_from_slice(y);
            let (Ok(key), Ok(sig)) = (
                VerifyingKey::from_sec1_bytes(&point),
                Signature::from_der(signature),
            ) else {
                return false;
            };
            key.verify(message, &sig).is_ok()
        }
        Some(Cbor::Int(-257)) => {
            let (Some(Cbor::Bytes(n)), Some(Cbor::Bytes(e))) = (cose.get_int(-1), cose.get_int(-2))
            else {
                return false;
            };
            rs256_verify(n, e, message, signature)
        }
        _ => false,
    }
}

/// `AuthenticatorAssertionResponseValidator::check`: returns the stored `credential` JSON with
/// the new counter and backup state, in its key order.
pub fn verify_assertion(
    credential: &Value,
    stored: &Value,
    expected: &AssertionExpected<'_>,
) -> Result<Value, String> {
    const FAILED: &str = "Unable to verify passkey. Please try again.";
    let fail = || FAILED.to_string();
    let response = credential.get("response").ok_or_else(fail)?;
    let field = |k: &str| {
        response
            .get(k)
            .and_then(Value::as_str)
            .and_then(from_b64)
            .ok_or_else(fail)
    };
    let client_data_raw = field("clientDataJSON")?;
    let auth = field("authenticatorData")?;
    let signature = field("signature")?;
    let client: Value = serde_json::from_slice(&client_data_raw).map_err(|_| fail())?;
    if client.get("type").and_then(Value::as_str) != Some("webauthn.get") {
        return Err(fail());
    }
    let challenge = client
        .get("challenge")
        .and_then(Value::as_str)
        .and_then(from_b64);
    if challenge.is_none() || challenge != from_b64(expected.challenge) {
        return Err(fail());
    }
    let origin = client.get("origin").and_then(Value::as_str).unwrap_or("");
    if !expected.origins.iter().any(|o| o == origin) {
        return Err(fail());
    }
    // rpIdHash(32) flags(1) counter(4)
    if auth.len() < 37 {
        return Err(fail());
    }
    let rp_hash: [u8; 32] = Sha256::digest(expected.rp_id.as_bytes()).into();
    if auth[..32] != rp_hash {
        return Err(fail());
    }
    let flags = auth[32];
    let (user_present, user_verified, backup_eligible, backup_state) = (
        flags & 0x01 != 0,
        flags & 0x04 != 0,
        flags & 0x08 != 0,
        flags & 0x10 != 0,
    );
    // `userVerification: required`; a backed-up credential must be backup eligible.
    if !user_present || !user_verified || (backup_state && !backup_eligible) {
        return Err(fail());
    }
    // The handle a discoverable credential returns has to be the stored one.
    let stored_handle = stored.get("userHandle").and_then(Value::as_str);
    if let Some(handle) = response
        .get("userHandle")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        && from_b64(handle) != stored_handle.and_then(from_b64)
    {
        return Err(fail());
    }
    let key = stored
        .get("credentialPublicKey")
        .and_then(Value::as_str)
        .and_then(from_b64)
        .ok_or_else(fail)?;
    let (cose, _) = decode_cbor(&key, 0).ok_or_else(fail)?;
    let mut signed = auth.clone();
    signed.extend_from_slice(&Sha256::digest(&client_data_raw));
    if !cose_verify(&cose, &signed, &signature) {
        return Err(fail());
    }
    // `CounterChecker`: a counter in use has to grow, or the authenticator was cloned.
    let counter = u32::from_be_bytes(auth[33..37].try_into().map_err(|_| fail())?);
    let old = stored
        .get("counter")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if (counter != 0 || old != 0) && u64::from(counter) <= old {
        return Err(fail());
    }
    let mut updated = stored.clone();
    updated["counter"] = json!(counter);
    updated["backupEligible"] = json!(backup_eligible);
    updated["backupStatus"] = json!(backup_state);
    updated["uvInitialized"] = json!(true);
    Ok(updated)
}

fn uuid_text(b: &[u8]) -> String {
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `Aaguids::labelFor`.
pub fn authenticator_label(aaguid: &str) -> Option<&'static str> {
    const LABELS: &[(&str, &str)] = &[
        ("a11a5faa-9f32-4b8c-8c5d-2f7d13e8c942", "AliasVault"),
        (
            "ea9b8d66-4d01-1d21-3ce4-b6b48cb575d4",
            "Google Password Manager",
        ),
        ("adce0002-35bc-c60a-648b-0b25f1f05503", "Chrome on Mac"),
        ("08987058-cadc-4b81-b6e1-30de50dcbe96", "Windows Hello"),
        ("9ddd1817-af5a-4672-a2b9-3e3dd95000a9", "Windows Hello"),
        ("6028b017-b1d4-4c02-b4b3-afcdafc96bb2", "Windows Hello"),
        (
            "dd4ec289-e01d-41c9-bb89-70fa845d4bf2",
            "iCloud Keychain (Managed)",
        ),
        ("531126d6-e717-415c-9320-3d9aa6981239", "Dashlane"),
        ("bada5566-a7aa-401f-bd96-45619a55120d", "1Password"),
        ("b84e4048-15dc-4dd0-8640-f4f60813c8af", "NordPass"),
        ("0ea242b4-43c4-4a1b-8b17-dd6d0b6baec6", "Keeper"),
        ("891494da-2c90-4d31-a9cd-4eab0aed1309", "Sésame"),
        ("f3809540-7f14-49c1-a8b3-8f813b225541", "Enpass"),
        ("b5397666-4885-aa6b-cebf-e52262a439a2", "Chromium Browser"),
        ("771b48fd-d3d4-4f74-9232-fc157ab0507a", "Edge on Mac"),
        ("39a5647e-1853-446c-a1f6-a79bae9f5bc7", "IDmelon"),
        ("d548826e-79b4-db40-a3d8-11116f7e8349", "Bitwarden"),
        ("fbfc3007-154e-4ecc-8c0b-6e020557d7bd", "Apple Passwords"),
        ("53414d53-554e-4700-0000-000000000000", "Samsung Pass"),
        ("66a0ccb3-bd6a-191f-ee06-e375c50b9846", "Thales Bio iOS SDK"),
        (
            "8836336a-f590-0921-301d-46427531eee6",
            "Thales Bio Android SDK",
        ),
        (
            "cd69adb5-3c7a-deb9-3177-6800ea6cb72a",
            "Thales PIN Android SDK",
        ),
        ("17290f1e-c212-34d0-1423-365d729f09d9", "Thales PIN iOS SDK"),
        ("50726f74-6f6e-5061-7373-50726f746f6e", "Proton Pass"),
        ("fdb141b2-5d84-443e-8a35-4698c205a502", "KeePassXC"),
        ("eaecdef2-1c31-5634-8639-f1cbd9c00a08", "KeePassDX"),
        (
            "cc45f64e-52a2-451b-831a-4edd8022a202",
            "ToothPic Passkey Provider",
        ),
        ("bfc748bb-3429-4faa-b9f9-7cfa9f3b76d0", "iPasswords"),
        ("b35a26b2-8f6e-4697-ab1d-d44db4da28c6", "Zoho Vault"),
        ("b78a0a55-6ef8-d246-a042-ba0f6d55050c", "LastPass"),
        ("de503f9c-21a4-4f76-b4b7-558eb55c6f89", "Devolutions"),
        ("22248c4c-7a12-46e2-9a41-44291b373a4d", "LogMeOnce"),
        (
            "a10c6dd9-465e-4226-8198-c7c44b91c555",
            "Kaspersky Password Manager",
        ),
        ("d350af52-0351-4ba2-acd3-dfeeadc3f764", "pwSafe"),
        (
            "d3452668-01fd-4c12-926c-83a4204853aa",
            "Microsoft Password Manager",
        ),
        ("6d212b28-a2c1-4638-b375-5932070f62e9", "initial"),
        ("d49b2120-b865-4191-8cea-be84a52b0485", "Heimlane Vault"),
        ("e8b7f4a2-c3d5-e6f7-890a-b1c2d3e4f567", "Sherlocked"),
        (
            "d9be9d39-e6a6-4c28-a581-32b044d986e4",
            "Sticky Password Manager",
        ),
        ("70617373-7761-6c6c-6669-646f32303236", "Passwall"),
        (
            "c9cadfc9-89a9-489e-a25a-c7e86a4d5f15",
            "Burp Suite Navigation Recorder",
        ),
    ];
    LABELS.iter().find(|(k, _)| *k == aaguid).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // From a registration through the reference with Chrome's virtual authenticator.
    const ATTESTATION: &str = "o2NmbXRkbm9uZWdhdHRTdG10oGhhdXRoRGF0YVikSZYN5YgOjGh0NBcPZHZgW4_krrmihjLHmVzzuoMdl2NFAAAAAQECAwQFBgcIAQIDBAUGBwgAIPVW8mrE-KRw-jqsdh4v7ZfoAttbZ5KTrdzldoukFLaIpQECAyYgASFYINRf4WJc5sKkhjITr9BtQ91EabjRsL0PgAT5eTWhMcehIlggYhkC7D8_T9shItHteKux4Gdlry-bS7ab0L9laqNLksI";
    const CLIENT: &str = "eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIiwiY2hhbGxlbmdlIjoicldtRXUxNW4yUDhGOGIzUy1qTUFFWmVJWnFzSU03WnEtRGVSQlpuMzlvWSIsIm9yaWdpbiI6Imh0dHA6Ly9sb2NhbGhvc3Q6MTgwMDAiLCJjcm9zc09yaWdpbiI6ZmFsc2V9";

    fn credential() -> Value {
        json!({"response": {"clientDataJSON": CLIENT, "attestationObject": ATTESTATION, "transports": ["internal"]}})
    }

    #[test]
    fn registers_like_the_reference() {
        let origins = vec!["http://localhost:18000".to_string()];
        let handle = "NjI2MTRjYjJiZmZhZjNjM2M1NzE3Y2YzNDE0M2I4ODQyMzZlNzA4NzM1YWY5MjM2YWIxMTI0MDVjNjYxMjZjYg";
        let r = verify_registration(
            &credential(),
            &Expected {
                challenge: "rWmEu15n2P8F8b3S-jMAEZeIZqsIM7Zq-DeRBZn39oY",
                rp_id: "localhost",
                origins: &origins,
                user_handle: handle,
            },
        )
        .unwrap();
        assert_eq!(
            r.credential_id,
            "9VbyasT4pHD6Oqx2Hi_tl-gC21tnkpOt3OV2i6QUtog"
        );
        // The row the reference stored for this registration.
        assert_eq!(
            r.credential.to_string(),
            format!(
                "{{\"publicKeyCredentialId\":\"9VbyasT4pHD6Oqx2Hi_tl-gC21tnkpOt3OV2i6QUtog\",\"type\":\"public-key\",\"transports\":[\"internal\"],\"attestationType\":\"none\",\"trustPath\":[],\"aaguid\":\"01020304-0506-0708-0102-030405060708\",\"credentialPublicKey\":\"pQECAyYgASFYINRf4WJc5sKkhjITr9BtQ91EabjRsL0PgAT5eTWhMcehIlggYhkC7D8_T9shItHteKux4Gdlry-bS7ab0L9laqNLksI\",\"userHandle\":\"{handle}\",\"counter\":1,\"backupEligible\":false,\"backupStatus\":false,\"uvInitialized\":true}}"
            )
        );
    }

    #[test]
    fn refuses_a_wrong_ceremony() {
        let origins = vec!["http://localhost:18000".to_string()];
        let exp = |challenge, rp_id, origins| Expected {
            challenge,
            rp_id,
            origins,
            user_handle: "h",
        };
        let good = "rWmEu15n2P8F8b3S-jMAEZeIZqsIM7Zq-DeRBZn39oY";
        assert!(verify_registration(&credential(), &exp("AAAA", "localhost", &origins)).is_err());
        assert!(verify_registration(&credential(), &exp(good, "other.test", &origins)).is_err());
        assert!(verify_registration(&credential(), &exp(good, "localhost", &[])).is_err());
        assert!(verify_registration(&json!({}), &exp(good, "localhost", &origins)).is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(
            authenticator_label("bada5566-a7aa-401f-bd96-45619a55120d"),
            Some("1Password")
        );
        assert_eq!(
            authenticator_label("01020304-0506-0708-0102-030405060708"),
            None
        );
    }
}
