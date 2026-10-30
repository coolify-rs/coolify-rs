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
