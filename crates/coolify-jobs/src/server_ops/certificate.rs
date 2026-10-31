//! `SslHelper::generateSslCertificate` for the CA certificate of a server,
//! `SslHelper::caCertificateFileCommands`, `Server::generateCaCertificate`
//! and `Server::ensureCaCertificate`.
//!
//! The certificate has what upstream's OpenSSL configuration asks for: an ECDSA
//! P-521 key, sha512, `CN = Coolify CA Certificate`, organization `Coolify`, country `XX`,
//! state `Default`, critical `CA:TRUE, pathlen:0`, critical `keyCertSign, cRLSign`
//! and a subject key identifier, valid for ten years.

use base64::Engine as _;
use chrono::{Duration, NaiveDateTime, Utc};
use coolify_db::server_ops::{self as ops, NewCertificate};
use p521::ecdsa::{Signature, SigningKey, signature::Signer};
use p521::pkcs8::{EncodePrivateKey, LineEnding};
use sha2::{Digest, Sha256};

use super::{BASE_CONFIG_DIR, Srv};
use crate::Jobs;

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend([0x81, len as u8]);
    } else {
        out.extend([0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

fn oid(bytes: &[u8]) -> Vec<u8> {
    der(0x06, bytes)
}

const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_O: &[u8] = &[0x55, 0x04, 0x0a];
const OID_C: &[u8] = &[0x55, 0x04, 0x06];
const OID_ST: &[u8] = &[0x55, 0x04, 0x08];
const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_SECP521R1: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x23];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];

fn rdn(attr: &[u8], tag: u8, value: &str) -> Vec<u8> {
    der(0x31, &seq(&[oid(attr), der(tag, value.as_bytes())]))
}

fn time(t: NaiveDateTime) -> Vec<u8> {
    use chrono::Datelike;
    if t.year() < 2050 {
        der(0x17, t.format("%y%m%d%H%M%SZ").to_string().as_bytes())
    } else {
        der(0x18, t.format("%Y%m%d%H%M%SZ").to_string().as_bytes())
    }
}

fn extension(id: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(der(0x01, &[0xff]));
    }
    parts.push(der(0x04, value));
    seq(&parts)
}

fn pem(label: &str, data: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// `TraefikAcmeService::expirationDate`: `notAfter` of the base64 PEM in acme.json.
pub fn acme_expiration(encoded: &str) -> Option<String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let body = text
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let compact: String = body.split_whitespace().collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()?;
    let (0x30, start, _) = der_element(&data, 0)? else {
        return None;
    };
    let (0x30, tbs, _) = der_element(&data, start)? else {
        return None;
    };
    let mut at = tbs;
    if der_element(&data, at)?.0 == 0xa0 {
        at = der_element(&data, at)?.2;
    }
    for _ in 0..3 {
        at = der_element(&data, at)?.2;
    }
    let (0x30, validity, _) = der_element(&data, at)? else {
        return None;
    };
    let not_before_end = der_element(&data, validity)?.2;
    let (tag, s, e) = der_element(&data, not_before_end)?;
    let raw = std::str::from_utf8(&data[s..e]).ok()?;
    let format = if tag == 0x17 {
        "%y%m%d%H%M%SZ"
    } else {
        "%Y%m%d%H%M%SZ"
    };
    let t = NaiveDateTime::parse_from_str(raw, format).ok()?;
    Some(t.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// `openssl_x509_read` + `openssl_x509_export`: the first certificate of the text in canonical PEM.
pub fn export_certificate(text: &str) -> Option<String> {
    let body = text
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let compact: String = body.split_whitespace().collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()?;
    subject_public_key_info(&data)?;
    Some(pem("CERTIFICATE", &data))
}

/// One DER element at `at`: `(tag, content start, end)`.
fn der_element(data: &[u8], at: usize) -> Option<(u8, usize, usize)> {
    let tag = *data.get(at)?;
    let first = *data.get(at + 1)? as usize;
    let (len, start) = if first < 0x80 {
        (first, at + 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *data.get(at + 2 + i)? as usize;
        }
        (len, at + 2 + n)
    };
    let end = start.checked_add(len)?;
    (end <= data.len()).then_some((tag, start, end))
}

/// The `subjectPublicKeyInfo` element of a DER certificate.
fn subject_public_key_info(cert: &[u8]) -> Option<&[u8]> {
    let (0x30, cert_start, _) = der_element(cert, 0)? else {
        return None;
    };
    let (0x30, tbs_start, tbs_end) = der_element(cert, cert_start)? else {
        return None;
    };
    let mut at = tbs_start;
    // version (optional, [0]), serial, signature, issuer, validity, subject.
    if der_element(cert, at)?.0 == 0xa0 {
        at = der_element(cert, at)?.2;
    }
    for _ in 0..5 {
        at = der_element(cert, at)?.2;
    }
    let (tag, _, end) = der_element(cert, at)?;
    (tag == 0x30 && end <= tbs_end).then(|| &cert[at..end])
}

/// `openssl_x509_check_private_key` for the keys Coolify generates (ECDSA P-521).
pub fn certificate_matches_key(certificate_pem: &str, key_pem: &str) -> bool {
    let Some(body) = certificate_pem
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)
        .and_then(|b| b.split("-----END CERTIFICATE-----").next())
    else {
        return false;
    };
    let compact: String = body.split_whitespace().collect();
    let Ok(der_cert) = base64::engine::general_purpose::STANDARD.decode(compact) else {
        return false;
    };
    let Some(spki) = subject_public_key_info(&der_cert) else {
        return false;
    };
    public_key_der(key_pem).is_some_and(|d| d == spki)
}

/// The SubjectPublicKeyInfo of a private key in PEM, for what `openssl_x509_check_private_key` accepts:
/// EC P-521, P-384, P-256 (PKCS#8 or SEC1), RSA (PKCS#8 or PKCS#1) and Ed25519 (PKCS#8).
fn public_key_der(key_pem: &str) -> Option<Vec<u8>> {
    use p521::pkcs8::{DecodePrivateKey, EncodePublicKey};
    macro_rules! ec {
        ($curve:ident) => {
            if let Some(secret) = $curve::SecretKey::from_pkcs8_pem(key_pem)
                .ok()
                .or_else(|| $curve::SecretKey::from_sec1_pem(key_pem).ok())
            {
                return secret
                    .public_key()
                    .to_public_key_der()
                    .ok()
                    .map(|d| d.as_bytes().to_vec());
            }
        };
    }
    ec!(p521);
    ec!(p384);
    ec!(p256);
    use rsa::pkcs1::DecodeRsaPrivateKey;
    if let Some(secret) = rsa::RsaPrivateKey::from_pkcs8_pem(key_pem)
        .ok()
        .or_else(|| rsa::RsaPrivateKey::from_pkcs1_pem(key_pem).ok())
    {
        return secret
            .to_public_key()
            .to_public_key_der()
            .ok()
            .map(|d| d.as_bytes().to_vec());
    }
    ed25519_public_key_der(key_pem)
}

/// RFC 8410: a PKCS#8 Ed25519 key holds the seed; the public key is derived from it.
fn ed25519_public_key_der(key_pem: &str) -> Option<Vec<u8>> {
    let body = key_pem
        .split("-----BEGIN PRIVATE KEY-----")
        .nth(1)?
        .split("-----END PRIVATE KEY-----")
        .next()?;
    let compact: String = body.split_whitespace().collect();
    let der_key = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()?;
    // SEQUENCE { INTEGER 0, SEQUENCE { OID 1.3.101.112 }, OCTET STRING { OCTET STRING seed } }
    const PREFIX: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    let seed: &[u8; 32] = der_key.strip_prefix(PREFIX)?.get(..32)?.try_into().ok()?;
    let keypair = ssh_key::private::Ed25519Keypair::from_seed(seed);
    let mut spki = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    spki.extend_from_slice(keypair.public.as_ref());
    Some(spki)
}

#[cfg(test)]
mod key_match_tests {
    use super::*;

    /// Keys and self-signed certificates from `openssl req -x509 -newkey`.
    fn matches(key_type: &[&str]) -> bool {
        let dir = std::env::temp_dir().join(format!("coolify-ca-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).expect("dir");
        let (key, cert) = (dir.join("k.pem"), dir.join("c.pem"));
        let mut args = vec!["req", "-x509", "-nodes", "-subj", "/CN=x", "-days", "1"];
        args.extend_from_slice(key_type);
        args.extend_from_slice(&[
            "-keyout",
            key.to_str().expect("path"),
            "-out",
            cert.to_str().expect("path"),
        ]);
        let ok = std::process::Command::new("openssl")
            .args(&args)
            .output()
            .expect("openssl")
            .status
            .success();
        assert!(ok, "openssl failed for {key_type:?}");
        let (k, c) = (
            std::fs::read_to_string(&key).expect("key"),
            std::fs::read_to_string(&cert).expect("cert"),
        );
        let _ = std::fs::remove_dir_all(&dir);
        certificate_matches_key(&c, &k)
    }

    #[test]
    fn accepts_every_key_type() {
        assert!(matches(&[
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-521"
        ]));
        assert!(matches(&[
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-384"
        ]));
        assert!(matches(&[
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256"
        ]));
        assert!(matches(&["-newkey", "rsa:2048"]));
        assert!(matches(&["-newkey", "ed25519"]));
    }

    #[test]
    fn rejects_another_key() {
        let (cert, _, _) = new_ca_certificate("x", 1).expect("cert");
        let (_, key, _) = new_ca_certificate("y", 1).expect("key");
        assert!(!certificate_matches_key(&cert, &key));
    }
}

/// A new self-signed CA certificate: `(certificate PEM, private key PEM, valid until)`.
pub fn new_ca_certificate(
    common_name: &str,
    validity_days: i64,
) -> Result<(String, String, NaiveDateTime), String> {
    let mut rng = rand_core_rng();
    let key = SigningKey::random(&mut rng);
    let secret = p521::SecretKey::from_bytes(&key.to_bytes()).map_err(|e| e.to_string())?;
    let key_pem = secret
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    let public = secret.public_key();
    let point = {
        use p521::elliptic_curve::sec1::ToEncodedPoint;
        public.to_encoded_point(false).as_bytes().to_vec()
    };
    let now = Utc::now().naive_utc();
    let until = now + Duration::days(validity_days);
    let sig_alg = seq(&[oid(OID_ECDSA_SHA512)]);
    let name = seq(&[
        rdn(OID_CN, 0x0c, common_name),
        rdn(OID_O, 0x0c, "Coolify"),
        rdn(OID_C, 0x13, "XX"),
        rdn(OID_ST, 0x0c, "Default"),
    ]);
    let mut serial = [0u8; 8];
    serial[..].copy_from_slice(&rand::random::<u64>().to_be_bytes());
    serial[0] &= 0x7f;
    let spki = seq(&[
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_SECP521R1)]),
        der(0x03, &[&[0u8][..], &point].concat()),
    ]);
    let key_id = &Sha256::digest(&point)[..20];
    let extensions = der(
        0xa3,
        &seq(&[
            extension(
                OID_BASIC_CONSTRAINTS,
                true,
                &seq(&[der(0x01, &[0xff]), der(0x02, &[0x00])]),
            ),
            // keyCertSign and cRLSign.
            extension(OID_KEY_USAGE, true, &der(0x03, &[0x01, 0x06])),
            extension(OID_SUBJECT_KEY_ID, false, &der(0x04, key_id)),
        ]),
    );
    let tbs = seq(&[
        der(0xa0, &der(0x02, &[0x02])),
        der(0x02, &serial),
        sig_alg.clone(),
        name.clone(),
        seq(&[time(now), time(until)]),
        name,
        spki,
        extensions,
    ]);
    let signature: Signature = key.sign(&tbs);
    let sig_der = signature.to_der();
    let cert = seq(&[
        tbs,
        sig_alg,
        der(0x03, &[&[0u8][..], sig_der.as_bytes()].concat()),
    ]);
    Ok((pem("CERTIFICATE", &cert), key_pem, until))
}

fn rand_core_rng() -> p521::elliptic_curve::rand_core::OsRng {
    p521::elliptic_curve::rand_core::OsRng
}

/// `SslHelper::caCertificateFileCommands`.
pub fn ca_certificate_file_commands(certificate: &str) -> Vec<String> {
    let dir = format!("{BASE_CONFIG_DIR}/ssl/");
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate);
    vec![
        format!("mkdir -p {dir}"),
        format!("chown -R 9999:root {dir}"),
        format!("chmod -R 700 {dir}"),
        format!("rm -rf {dir}/coolify-ca.crt"),
        format!("echo '{encoded}' | base64 -d | tee {dir}/coolify-ca.crt > /dev/null"),
        format!("chmod 644 {dir}/coolify-ca.crt"),
    ]
}

/// `generateSslCertificate(commonName: 'Coolify CA Certificate', isCaCertificate: true, validityDays: 3650)`:
/// stores the row and returns the certificate.
pub async fn generate_ca_certificate(jobs: &Jobs, server_id: i64) -> Result<String, String> {
    let (certificate, key, until) = new_ca_certificate("Coolify CA Certificate", 10 * 365)?;
    let enc = jobs
        .0
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    ops::replace_server_certificate(
        &jobs.0.db,
        &NewCertificate {
            certificate: &enc.encrypt_string(&certificate),
            private_key: &enc.encrypt_string(&key),
            server_id,
            common_name: "Coolify CA Certificate",
            subject_alternative_names: "[]",
            valid_until: chrono::SubsecRound::trunc_subsecs(until, 0),
            is_ca_certificate: true,
        },
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(certificate)
}

/// `Server::generateCaCertificate`: the file on the server is written with `throwError: false`.
pub async fn regenerate_ca_certificate(srv: &Srv) -> Result<String, String> {
    let certificate = generate_ca_certificate(&srv.jobs, srv.id()).await?;
    let _ = srv
        .run_quiet(
            &ca_certificate_file_commands(&certificate),
            "server.ca_certificate",
        )
        .await;
    Ok(certificate)
}

/// `Server::ensureCaCertificate`.
pub async fn ensure_ca_certificate(srv: &Srv) -> Result<String, String> {
    if let Some((_, cipher)) = ops::ca_certificate(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?
    {
        let enc = srv
            .jobs
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        return enc.decrypt_string(&cipher).map_err(|e| e.to_string());
    }
    regenerate_ca_certificate(srv).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_certificate() {
        let (cert, key, _) = new_ca_certificate("Coolify CA Certificate", 3650).unwrap();
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(key.starts_with("-----BEGIN PRIVATE KEY-----\n"));
    }
}
