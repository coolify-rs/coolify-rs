//! A managed server as jobs need it: connection details and the settings
//! that decide whether commands may run on it.

use coolify_compat::DecryptError;
use coolify_db::private_keys;
use coolify_db::records::Record;
use coolify_db::servers::ServerRow;
use coolify_remote::{PrivateKey, Target};
use serde_json::Value;

use crate::Jobs;

/// Upstream `Server::PLACEHOLDER_IPS`.
const PLACEHOLDER_IPS: &[&str] = &["1.2.3.4", "0.0.0.0", "::"];

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub id: i64,
    pub team_id: i64,
    pub uuid: String,
    pub ip: String,
    pub user: String,
    pub port: i64,
    pub private_key_id: Option<i64>,
    pub settings: Record,
    pub transferred: bool,
}

pub fn str_of(r: &Record, k: &str) -> String {
    match r.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Laravel `DecryptException` messages.
fn decrypt_message(e: &DecryptError) -> &'static str {
    match e {
        DecryptError::InvalidPayload => "The payload is invalid.",
        DecryptError::InvalidMac => "The MAC is invalid.",
        DecryptError::DecryptionFailed => "Could not decrypt the data.",
    }
}

impl ServerInfo {
    pub fn from_row(row: &ServerRow) -> Self {
        let r = &row.record;
        Self {
            id: row.id,
            team_id: row.team_id,
            uuid: row.uuid.clone(),
            ip: str_of(r, "ip"),
            user: str_of(r, "user"),
            port: r.get("port").and_then(Value::as_i64).unwrap_or(22),
            private_key_id: r.get("private_key_id").and_then(Value::as_i64),
            settings: row.settings.clone().unwrap_or_default(),
            transferred: r
                .get("server_metadata")
                .and_then(|m| m.pointer("/transfer/status"))
                .and_then(Value::as_str)
                == Some("transferred"),
        }
    }

    pub fn setting_bool(&self, key: &str) -> Option<bool> {
        self.settings.get(key).and_then(Value::as_bool)
    }

    pub fn is_build_server(&self) -> bool {
        match self.settings.get("server_role").and_then(Value::as_str) {
            Some(role) => role == "build",
            None => self.setting_bool("is_build_server") == Some(true),
        }
    }

    pub fn has_placeholder_ip(&self) -> bool {
        self.ip.is_empty() || PLACEHOLDER_IPS.contains(&self.ip.as_str())
    }

    /// `skipServer()`.
    pub fn skipped(&self) -> bool {
        self.has_placeholder_ip() || self.setting_bool("force_disabled") == Some(true)
    }

    /// `isFunctional()`.
    pub fn is_functional(&self) -> bool {
        self.setting_bool("is_reachable") == Some(true)
            && self.setting_bool("is_usable") == Some(true)
            && self.setting_bool("force_disabled") == Some(false)
            && !self.has_placeholder_ip()
    }

    pub fn is_swarm(&self) -> bool {
        self.setting_bool("is_swarm_manager") == Some(true)
            || self.setting_bool("is_swarm_worker") == Some(true)
    }

    pub fn is_non_root(&self) -> bool {
        self.user != "root"
    }

    pub fn docker_version(&self) -> Option<&str> {
        self.settings.get("docker_version").and_then(Value::as_str)
    }

    /// `PrivateKey::where('team_id', $server->team_id)->findOrFail($server->private_key_id)`, decrypted.
    pub(crate) async fn target(&self, jobs: &Jobs) -> Result<Target, String> {
        let key_id = self.private_key_id.unwrap_or_default();
        let key = private_keys::find_by_id(&jobs.0.db, self.team_id, key_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("No query results for model [App\\Models\\PrivateKey] {key_id}")
            })?;
        let enc = jobs
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let pem = enc
            .decrypt_string(&key.private_key_encrypted)
            .map_err(|e| decrypt_message(&e).to_owned())?;
        Ok(Target {
            server_uuid: self.uuid.clone(),
            host: self.ip.clone(),
            port: self.port,
            user: self.user.clone(),
            key: PrivateKey::new(key.uuid, pem),
            connection_timeout: self
                .settings
                .get("connection_timeout")
                .and_then(Value::as_i64),
            cloudflare_tunnel: self.setting_bool("is_cloudflare_tunnel") == Some(true),
            force_disabled: self.setting_bool("force_disabled") == Some(true),
        })
    }
}
