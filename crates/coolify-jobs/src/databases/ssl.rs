//! `SslHelper`: EC certificates as upstream makes them with OpenSSL: a P-521 CA
//! (`secp521r1`) that signs P-256 (`prime256v1`) server certificates with SHA-512.
//! The X.509 structure is written here; `Server::ensureCaCertificate`.

use base64::Engine as _;
use chrono::{Datelike, Duration as ChronoDuration, NaiveDateTime, Timelike, Utc};
use coolify_db::database_parts::{self as parts, NewCertificate, NewFileVolume, SslCertificate};
use coolify_db::databases::{DatabaseRow, Kind};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use p256::pkcs8::{EncodePrivateKey as _, LineEnding};
use p521::ecdsa::signature::Signer as _;
use rand::Rng as _;
use sha1::Digest as _;

use super::common::{CONFIGURATION_DIR, b64};
use crate::Jobs;
use crate::server::ServerInfo;

const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_COUNTRY: &[u8] = &[0x55, 0x04, 0x06];
const OID_STATE: &[u8] = &[0x55, 0x04, 0x08];
const OID_ORGANIZATION: &[u8] = &[0x55, 0x04, 0x0a];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P521: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x23];
const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const OID_SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const OID_CLIENT_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

fn der_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let bytes: Vec<u8> = len
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        let mut out = vec![0x80 | bytes.len() as u8];
        out.extend(bytes);
        out
    }
}

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(der_len(value.len()));
    out.extend_from_slice(value);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid(bytes: &[u8]) -> Vec<u8> {
    tlv(0x06, bytes)
}

fn bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut v = vec![0];
    v.extend_from_slice(bytes);
    tlv(0x03, &v)
}

fn integer(bytes: &[u8]) -> Vec<u8> {
    let trimmed: Vec<u8> = {
        let mut b: &[u8] = bytes;
        while b.len() > 1 && b[0] == 0 {
            b = &b[1..];
        }
        b.to_vec()
    };
    let mut v = Vec::new();
    if trimmed.first().is_some_and(|b| b & 0x80 != 0) {
        v.push(0);
    }
    v.extend(trimmed);
    tlv(0x02, &v)
}

/// Generalized or UTC time as X.509 requires by year.
fn time(t: NaiveDateTime) -> Vec<u8> {
    let (tag, text) = if t.year() >= 2050 {
        (
            0x18,
            format!(
                "{:04}{:02}{:02}{:02}{:02}{:02}Z",
                t.year(),
                t.month(),
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            ),
        )
    } else {
        (
            0x17,
            format!(
                "{:02}{:02}{:02}{:02}{:02}{:02}Z",
                t.year() % 100,
                t.month(),
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            ),
        )
    };
    tlv(tag, text.as_bytes())
}

/// The subject `openssl_csr_new` builds from upstream's array: `CN`, `O`, `C`, `ST`.
fn subject(common_name: &str) -> Vec<u8> {
    let rdn =
        |o: &[u8], tag: u8, value: &str| tlv(0x31, &seq(&[oid(o), tlv(tag, value.as_bytes())]));
    let mut parts = vec![rdn(OID_COMMON_NAME, 0x0c, common_name)];
    parts.push(rdn(OID_ORGANIZATION, 0x0c, "Coolify"));
    parts.push(rdn(OID_COUNTRY, 0x13, "XX"));
    parts.push(rdn(OID_STATE, 0x0c, "Default"));
    tlv(0x30, &parts.concat())
}

fn extension(o: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(o)];
    if critical {
        parts.push(tlv(0x01, &[0xff]));
    }
    parts.push(tlv(0x04, &value));
    seq(&parts)
}

/// `subjectAltName`: `DNS:x` and `IP:x` entries.
fn alt_names(names: &[String]) -> Vec<u8> {
    let mut items = Vec::new();
    for name in names {
        let Some((kind, value)) = name.split_once(':') else {
            continue;
        };
        match kind {
            "IP" => match value.parse::<std::net::IpAddr>() {
                Ok(std::net::IpAddr::V4(v4)) => items.push(tlv(0x87, &v4.octets())),
                Ok(std::net::IpAddr::V6(v6)) => items.push(tlv(0x87, &v6.octets())),
                Err(_) => {}
            },
            _ => items.push(tlv(0x82, value.as_bytes())),
        }
    }
    seq(&items)
}

/// The `subject` of a certificate in DER: serial, signature algorithm, issuer, validity, subject.
fn subject_of(cert_der: &[u8]) -> Option<Vec<u8>> {
    fn read(d: &[u8]) -> Option<(u8, usize, usize)> {
        let tag = *d.first()?;
        let first = *d.get(1)?;
        if first < 0x80 {
            Some((tag, 2, first as usize))
        } else {
            let n = (first & 0x7f) as usize;
            let mut len = 0usize;
            for b in d.get(2..2 + n)? {
                len = (len << 8) | *b as usize;
            }
            Some((tag, 2 + n, len))
        }
    }
    let (_, h, _) = read(cert_der)?;
    let tbs = cert_der.get(h..)?;
    let (_, h, _) = read(tbs)?;
    let mut rest = tbs.get(h..)?;
    // version [0] is optional.
    let (tag, h, l) = read(rest)?;
    if tag == 0xa0 {
        rest = rest.get(h + l..)?;
    }
    for _ in 0..4 {
        // serial, signature, issuer, validity
        let (_, h, l) = read(rest)?;
        rest = rest.get(h + l..)?;
    }
    let (_, h, l) = read(rest)?;
    rest.get(..h + l).map(<[u8]>::to_vec)
}

fn pem_body(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}

fn certificate_pem(der: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// A P-521 signing key from upstream's PEM (`openssl_pkey_export`: PKCS#8) or a SEC1 PEM.
fn ca_signing_key(pem: &str) -> Result<p521::ecdsa::SigningKey, String> {
    use p521::pkcs8::DecodePrivateKey as _;
    let secret = p521::SecretKey::from_pkcs8_pem(pem)
        .or_else(|_| p521::SecretKey::from_sec1_pem(pem))
        .map_err(|e| format!("cannot read the CA key: {e}"))?;
    p521::ecdsa::SigningKey::from_bytes(&secret.to_bytes()).map_err(|e| e.to_string())
}

/// The certificate and private key PEMs.
struct Generated {
    certificate: String,
    private_key: String,
    subject_alternative_names: Vec<String>,
}

fn serial() -> Vec<u8> {
    // `random_int(1, PHP_INT_MAX)`.
    let n: i64 = rand::thread_rng().gen_range(1..=i64::MAX);
    n.to_be_bytes().to_vec()
}

#[allow(clippy::too_many_arguments)]
fn build_tbs(
    issuer: Vec<u8>,
    subject: Vec<u8>,
    public_key_info: Vec<u8>,
    public_point: &[u8],
    now: NaiveDateTime,
    days: i64,
    is_ca: bool,
    alt: &[String],
) -> Vec<u8> {
    let mut extensions = vec![
        extension(
            OID_BASIC_CONSTRAINTS,
            true,
            if is_ca {
                seq(&[tlv(0x01, &[0xff]), integer(&[0])])
            } else {
                seq(&[])
            },
        ),
        extension(
            OID_KEY_USAGE,
            true,
            // keyCertSign, cRLSign / digitalSignature, keyAgreement
            if is_ca {
                tlv(0x03, &[0x01, 0x06])
            } else {
                tlv(0x03, &[0x03, 0x88])
            },
        ),
    ];
    if !is_ca {
        extensions.push(extension(
            OID_EXT_KEY_USAGE,
            false,
            seq(&[oid(OID_SERVER_AUTH), oid(OID_CLIENT_AUTH)]),
        ));
    }
    let key_id = sha1::Sha1::digest(public_point);
    extensions.push(extension(OID_SUBJECT_KEY_ID, false, tlv(0x04, &key_id)));
    if !is_ca {
        extensions.push(extension(OID_SUBJECT_ALT_NAME, false, alt_names(alt)));
    }
    seq(&[
        tlv(0xa0, &integer(&[2])),
        integer(&serial()),
        seq(&[oid(OID_ECDSA_SHA512)]),
        issuer,
        seq(&[time(now), time(now + ChronoDuration::days(days))]),
        subject,
        public_key_info,
        tlv(0xa3, &seq(&extensions)),
    ])
}

fn sign(tbs: Vec<u8>, key: &p521::ecdsa::SigningKey) -> Vec<u8> {
    let signature: p521::ecdsa::Signature = key.sign(&tbs);
    seq(&[
        tbs,
        seq(&[oid(OID_ECDSA_SHA512)]),
        bit_string(signature.to_der().as_bytes()),
    ])
}

/// `generateSslCertificate(isCaCertificate: true)`.
fn generate_ca(common_name: &str, days: i64) -> Result<Generated, String> {
    let secret = p521::SecretKey::random(&mut rand_core::OsRng);
    let point = secret.public_key().to_encoded_point(false);
    let spki = seq(&[
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_P521)]),
        bit_string(point.as_bytes()),
    ]);
    let name = subject(common_name);
    let now = Utc::now().naive_utc();
    let tbs = build_tbs(
        name.clone(),
        name,
        spki,
        point.as_bytes(),
        now,
        days,
        true,
        &[],
    );
    let key = p521::ecdsa::SigningKey::from_bytes(&secret.to_bytes()).map_err(|e| e.to_string())?;
    let der = sign(tbs, &key);
    let private_key = secret
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    Ok(Generated {
        certificate: certificate_pem(&der),
        private_key,
        subject_alternative_names: Vec::new(),
    })
}

/// `generateSslCertificate(...)` of a server certificate, signed by the CA.
fn generate_leaf(
    common_name: &str,
    server_ip: &str,
    ca_certificate: &str,
    ca_key: &str,
    days: i64,
) -> Result<Generated, String> {
    let secret = p256::SecretKey::random(&mut rand_core::OsRng);
    let point = secret.public_key().to_encoded_point(false);
    let spki = seq(&[
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_P256)]),
        bit_string(point.as_bytes()),
    ]);
    // array_unique(array_merge(["DNS:$commonName"], [..., "$type:$ip"]))
    let mut names = vec![format!("DNS:{common_name}")];
    if !server_ip.is_empty() {
        let kind = if server_ip.parse::<std::net::IpAddr>().is_ok() {
            "IP"
        } else {
            "DNS"
        };
        let entry = format!("{kind}:{server_ip}");
        if !names.contains(&entry) {
            names.push(entry);
        }
    }
    let ca_der = pem_body(ca_certificate).ok_or("cannot read the CA certificate")?;
    let issuer = subject_of(&ca_der).ok_or("cannot read the CA subject")?;
    let now = Utc::now().naive_utc();
    let tbs = build_tbs(
        issuer,
        subject(common_name),
        spki,
        point.as_bytes(),
        now,
        days,
        false,
        &names,
    );
    let der = sign(tbs, &ca_signing_key(ca_key)?);
    let private_key = secret
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    Ok(Generated {
        certificate: certificate_pem(&der),
        private_key,
        subject_alternative_names: names,
    })
}

/// `SslHelper::caCertificateFileCommands`.
pub fn ca_certificate_file_commands(certificate: &str) -> Vec<String> {
    let dir = "/data/coolify/ssl/";
    vec![
        format!("mkdir -p {dir}"),
        format!("chown -R 9999:root {dir}"),
        format!("chmod -R 700 {dir}"),
        format!("rm -rf {dir}/coolify-ca.crt"),
        format!(
            "echo '{}' | base64 -d | tee {dir}/coolify-ca.crt > /dev/null",
            b64(certificate)
        ),
        format!("chmod 644 {dir}/coolify-ca.crt"),
    ]
}

/// The mount path of a kind's certificate files.
pub fn certificate_mount(kind: Kind) -> &'static str {
    match kind {
        Kind::Postgresql => "/var/lib/postgresql/certs",
        Kind::Mysql | Kind::Mariadb => "/etc/mysql/certs",
        Kind::Redis => "/etc/redis/certs",
        Kind::Keydb => "/etc/keydb/certs",
        Kind::Dragonfly => "/etc/dragonfly/certs",
        Kind::Mongodb => "/etc/mongo/certs",
        Kind::Clickhouse | Kind::Sqlite => "",
    }
}

fn mount_files(mount: &str) -> Vec<String> {
    ["server.crt", "server.key", "server.pem"]
        .iter()
        .map(|f| format!("{mount}/{f}"))
        .collect()
}

impl Jobs {
    /// `Server::ensureCaCertificate`: the CA row, created (and written to the server) when missing.
    pub async fn ensure_ca_certificate(
        &self,
        server: &ServerInfo,
    ) -> Result<(String, String), String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let decrypt = |c: &SslCertificate| -> Result<(String, String), String> {
            Ok((
                enc.decrypt_string(&c.ssl_certificate)
                    .map_err(|_| "The MAC is invalid.".to_owned())?,
                enc.decrypt_string(&c.ssl_private_key)
                    .map_err(|_| "The MAC is invalid.".to_owned())?,
            ))
        };
        if let Some(c) = parts::ca_certificate(&self.0.db, server.id)
            .await
            .map_err(|e| e.to_string())?
        {
            return decrypt(&c);
        }
        // `generateCaCertificate`
        let g = generate_ca("Coolify CA Certificate", 10 * 365)?;
        let now = Jobs::now();
        parts::store_certificate(
            &self.0.db,
            &NewCertificate {
                certificate_encrypted: &enc.encrypt_string(&g.certificate),
                private_key_encrypted: &enc.encrypt_string(&g.private_key),
                resource_type: None,
                resource_id: None,
                server_id: server.id,
                configuration_dir: None,
                mount_path: None,
                valid_until: now + ChronoDuration::days(10 * 365),
                is_ca_certificate: true,
                common_name: "Coolify CA Certificate",
                subject_alternative_names: &g.subject_alternative_names,
            },
            now,
        )
        .await
        .map_err(|e| e.to_string())?;
        if let Ok(t) = server.target(self).await {
            let commands = ca_certificate_file_commands(&g.certificate);
            let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
            self.remote()
                .run_quiet(&t, &refs, coolify_remote::RunOptions::op("ssl.ca"))
                .await;
        }
        Ok((g.certificate, g.private_key))
    }

    /// `SslHelper::generateSslCertificate` for a database: the row and its file storages
    /// (`server.pem` for MongoDB, `server.crt` + `server.key` otherwise).
    pub async fn generate_database_certificate(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
        ca: &(String, String),
    ) -> Result<(), String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let configuration_dir = format!("{CONFIGURATION_DIR}/{}", d.uuid);
        let mount = certificate_mount(d.kind);
        let g = generate_leaf(&d.uuid, &server.ip, &ca.0, &ca.1, 365)?;
        let now = Jobs::now();
        parts::store_certificate(
            &self.0.db,
            &NewCertificate {
                certificate_encrypted: &enc.encrypt_string(&g.certificate),
                private_key_encrypted: &enc.encrypt_string(&g.private_key),
                resource_type: Some(d.kind.morph()),
                resource_id: Some(d.id),
                server_id: server.id,
                configuration_dir: Some(&configuration_dir),
                mount_path: Some(mount),
                valid_until: now + ChronoDuration::days(365),
                is_ca_certificate: false,
                common_name: &d.uuid,
                subject_alternative_names: &g.subject_alternative_names,
            },
            now,
        )
        .await
        .map_err(|e| e.to_string())?;
        parts::delete_file_storages_at(&self.0.db, d.kind.morph(), d.id, &mount_files(mount))
            .await
            .map_err(|e| e.to_string())?;
        let mut files: Vec<(String, String, String, &str)> = Vec::new();
        if d.kind == Kind::Mongodb {
            files.push((
                format!("{configuration_dir}/ssl/server.pem"),
                format!("{mount}/server.pem"),
                format!("{}\n{}", g.certificate, g.private_key),
                "600",
            ));
        } else {
            files.push((
                format!("{configuration_dir}/ssl/server.crt"),
                format!("{mount}/server.crt"),
                g.certificate.clone(),
                "644",
            ));
            files.push((
                format!("{configuration_dir}/ssl/server.key"),
                format!("{mount}/server.key"),
                g.private_key.clone(),
                "600",
            ));
        }
        for (fs_path, mount_path, content, chmod) in files {
            let row = parts::create_file_storage(
                &self.0.db,
                d.kind.morph(),
                d.id,
                &NewFileVolume {
                    fs_path,
                    mount_path,
                    content_encrypted: Some(enc.encrypt_string(&content)),
                    is_directory: false,
                    is_host_file: false,
                    chmod: Some(chmod.to_owned()),
                },
                now,
            )
            .await
            .map_err(|e| e.to_string())?;
            // `created` hook: ServerStorageSaveJob.
            if let Err(e) = self.save_file_storage(&row, d, server).await {
                tracing::warn!(database = %d.uuid, error = %e, "certificate file could not be written");
            }
        }
        Ok(())
    }

    /// The SSL part of a `Start*` action: the commands, and the rows it changes.
    pub(super) async fn ssl_start_commands(
        &self,
        d: &DatabaseRow,
        server: &ServerInfo,
    ) -> Result<Vec<String>, String> {
        let dir = format!("{CONFIGURATION_DIR}/{}", d.uuid);
        if !d.bool("enable_ssl") {
            parts::delete_resource_certificates(&self.0.db, d.kind.morph(), d.id)
                .await
                .map_err(|e| e.to_string())?;
            parts::delete_file_storages_at(
                &self.0.db,
                d.kind.morph(),
                d.id,
                &mount_files(certificate_mount(d.kind)),
            )
            .await
            .map_err(|e| e.to_string())?;
            return Ok(vec![format!("rm -rf {dir}/ssl")]);
        }
        let mut commands = vec![
            "echo 'Setting up SSL for this database.'".to_owned(),
            format!("mkdir -p {dir}/ssl"),
        ];
        let ca = self
            .ensure_ca_certificate(server)
            .await
            .map_err(|_| MISSING_CA.to_owned())?;
        commands.extend(ca_certificate_file_commands(&ca.0));
        let existing = parts::resource_certificate(&self.0.db, d.kind.morph(), d.id)
            .await
            .map_err(|e| e.to_string())?;
        if existing.is_none() {
            commands.push(
                "echo 'No SSL certificate found, generating new SSL certificate for this database.'"
                    .to_owned(),
            );
            self.generate_database_certificate(d, server, &ca).await?;
        }
        Ok(commands)
    }
}

/// `DatabaseStartException::missingCaCertificate`.
pub const MISSING_CA: &str =
    "The CA certificate of the server is missing. It could not be generated.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_and_leaf_certificates_parse_as_der() {
        let ca = generate_ca("Coolify CA Certificate", 3650).unwrap();
        assert!(ca.private_key.starts_with("-----BEGIN PRIVATE KEY-----"));
        let leaf =
            generate_leaf("abc", "192.0.2.1", &ca.certificate, &ca.private_key, 365).unwrap();
        assert_eq!(
            leaf.subject_alternative_names,
            vec!["DNS:abc".to_owned(), "IP:192.0.2.1".to_owned()]
        );
        let der = pem_body(&leaf.certificate).unwrap();
        assert_eq!(der[0], 0x30);
        assert_eq!(subject_of(&der).unwrap(), subject("abc"),);
    }
}
