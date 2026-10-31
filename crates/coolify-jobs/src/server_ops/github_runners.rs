//! `InstallSysbox` and `GithubRunnerContainer::cleanupCommands`.
//!
//! Provisioning runners and everything else that talks to GitHub (`ProvisionGithubRunnerJob`, `GithubRunnerApi`,
//! `ReconcileGithubRunnersJob`, the `workflow_job` webhook) is not ported.

use base64::Engine as _;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::Srv;
use super::proxy::{queue_activity, run_activity};

const RUNTIME: &str = "sysbox-runc";
const DAEMON_CONFIG: &str = "/etc/docker/daemon.json";
const SECRETS_DIRECTORY: &str = "/data/coolify/github-runners";
/// `config('constants.github_runner.sysbox.version')`.
pub const SYSBOX_VERSION: &str = "0.7.1";

fn checksum(architecture: &str) -> &'static str {
    match architecture {
        "amd64" => "9d6d5484f980d0a17f86c492c1262015c2afb66280bdb97215b79fde6a0261c5",
        _ => "04ca894ae0b53f0fa54eaacc173ce40363c9a95ea5450f773716a84ef650a69b",
    }
}

/// `InstallSysbox::installedCheckCommand`.
const INSTALLED_CHECK: &str = "systemctl is-active --quiet sysbox-mgr sysbox-fs && test -x /usr/bin/fusermount3 && docker info --format '{{range $name, $runtime := .Runtimes}}{{$name}} {{end}}'";

/// `InstallSysbox::isInstalled`.
pub async fn is_installed(srv: &Srv) -> bool {
    let runtimes = srv
        .run_quiet(&[INSTALLED_CHECK.to_string()], "sysbox.check")
        .await
        .unwrap_or_default();
    runtimes.trim().split(' ').any(|r| r == RUNTIME)
}

/// `InstallSysbox::isKernelSupported`: 5.12 or newer.
fn kernel_supported(kernel: &str) -> bool {
    let mut parts = kernel.split('.');
    let number = |p: Option<&str>| -> Option<u32> {
        let digits: String = p?.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    match (number(parts.next()), number(parts.next())) {
        (Some(major), Some(minor)) => (major, minor) >= (5, 12),
        _ => false,
    }
}

/// `json_encode($config, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)`.
fn pretty(value: &Value) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    if value.serialize(&mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `InstallSysbox::daemonConfigWithNetworkSettings`.
pub fn daemon_config(current: Option<&str>, bridge: &str) -> Result<Option<String>, String> {
    let invalid = || {
        format!("{DAEMON_CONFIG} is not a valid JSON object. Fix it, then install Sysbox again.")
    };
    let text = current.unwrap_or_default();
    let mut config: Map<String, Value> = if text.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str(text) {
            Ok(Value::Object(m)) => m,
            _ => return Err(invalid()),
        }
    };
    let filled = |c: &Map<String, Value>, k: &str| match c.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        _ => true,
    };
    let (has_bip, has_pools) = (
        filled(&config, "bip"),
        filled(&config, "default-address-pools"),
    );
    if has_bip && has_pools {
        return Ok(None);
    }
    if !has_bip {
        let pattern =
            Regex::new(r"\A(\d{1,3}(?:\.\d{1,3}){3}) \d{1,3}(?:\.\d{1,3}){3}/(\d{1,2})\z")
                .map_err(|e| e.to_string())?;
        let captures = pattern
            .captures(bridge.trim())
            .ok_or("Could not read the address of the default Docker bridge network.")?;
        config.insert(
            "bip".into(),
            json!(format!("{}/{}", &captures[1], &captures[2])),
        );
    }
    if !has_pools {
        let mut pools: Vec<Value> = (17..=31)
            .map(|o| json!({"base": format!("172.{o}.0.0/16"), "size": 16}))
            .collect();
        pools.push(json!({"base": "192.168.0.0/16", "size": 20}));
        config.insert("default-address-pools".into(), Value::Array(pools));
    }
    Ok(Some(format!("{}\n", pretty(&Value::Object(config)))))
}

/// `InstallSysbox::installCommands`.
pub fn install_commands(architecture: &str, config: Option<&str>) -> Vec<String> {
    let version = SYSBOX_VERSION;
    let package = format!("/tmp/sysbox-ce_{version}_{architecture}.deb");
    let url = format!(
        "https://github.com/nestybox/sysbox/releases/download/v{version}/sysbox-ce_{version}.linux_{architecture}.deb"
    );
    let apt = "DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300";
    let mut c = vec![format!("echo 'Installing Sysbox {version}.'")];
    if let Some(config) = config {
        c.push(format!("echo 'Saving the current Docker network settings in {DAEMON_CONFIG} so that Docker does not restart.'"));
        c.push("mkdir -p /etc/docker".to_string());
        c.push(format!(
            "cp {DAEMON_CONFIG} {DAEMON_CONFIG}.before-sysbox 2>/dev/null || true"
        ));
        c.push(format!(
            "echo {} | base64 -d | tee {DAEMON_CONFIG} > /dev/null",
            base64::engine::general_purpose::STANDARD.encode(config)
        ));
    }
    c.extend([
        "echo 'Downloading the Sysbox package.'".to_string(),
        format!("curl -fsSL -o {package} {url}"),
        format!("echo '{}  {package}' | sha256sum -c -", checksum(architecture)),
        "echo 'Installing the Sysbox package. Docker reloads its configuration, running containers are not restarted.'".to_string(),
        format!("{apt} update"),
        format!("{apt} install -y fuse3 {package}"),
        format!("rm -f {package}"),
        "systemctl is-active --quiet sysbox-mgr sysbox-fs".to_string(),
        "test -x /usr/bin/fusermount3".to_string(),
        format!("docker info --format '{{{{range $name, $runtime := .Runtimes}}}}{{{{$name}}}} {{{{end}}}}' | grep -qw {RUNTIME}"),
        "echo 'Sysbox is installed.'".to_string(),
    ]);
    c
}

/// `InstallSysbox::handle`: the id of the activity.
pub async fn install_sysbox(srv: &Srv) -> Result<i64, String> {
    let one = |c: &str| {
        let c = c.to_string();
        async move {
            srv.run(&[c], "sysbox.install")
                .await
                .map(Option::unwrap_or_default)
        }
    };
    if one("command -v apt-get || true").await?.trim().is_empty() {
        return Err("Automatic Sysbox installation is only available on Debian and Ubuntu servers. Install Sysbox manually: https://github.com/nestybox/sysbox/blob/master/docs/user-guide/install-package.md".to_string());
    }
    let architecture = one("dpkg --print-architecture").await?.trim().to_string();
    if !matches!(architecture.as_str(), "amd64" | "arm64") {
        return Err(format!(
            "Sysbox packages are not available for the {architecture} architecture."
        ));
    }
    let kernel = one("uname -r").await?.trim().to_string();
    if !kernel_supported(&kernel) {
        return Err(format!(
            "Sysbox needs Linux kernel 5.12 or newer. This server runs {kernel}."
        ));
    }
    let current = one(&format!("cat {DAEMON_CONFIG} 2>/dev/null || true")).await?;
    let bridge =
        one("docker network inspect bridge --format '{{range .IPAM.Config}}{{.Gateway}} {{.Subnet}}{{end}}'")
            .await?;
    let config = daemon_config(Some(&current), &bridge)?;
    let commands = install_commands(&architecture, config.as_deref());
    let activity = queue_activity(srv, "sysbox.install", None).await?;
    let (jobs, id) = (srv.jobs.clone(), srv.id());
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let _ = run_activity(&s, activity, &commands, "sysbox.install").await;
    });
    Ok(activity)
}

/// `GithubRunnerContainer::cleanupCommands`.
pub fn cleanup_commands(execution_uuid: &str) -> Vec<String> {
    let name = format!("coolify-runner-{execution_uuid}");
    vec![
        format!("docker rm -f -v {name} {name}-dind > /dev/null 2>&1 || true"),
        format!(
            "docker volume rm {name}-sock {name}-work {name}-externals > /dev/null 2>&1 || true"
        ),
        format!("docker network rm {name} > /dev/null 2>&1 || true"),
        format!(
            "rm -f {SECRETS_DIRECTORY}/{execution_uuid}.{} {SECRETS_DIRECTORY}/{execution_uuid}-job-started.sh",
            concat!("en", "v")
        ),
    ]
}

/// `CleanupGithubRunnerJob`, without `GithubRunnerApi::deleteRunner` and the next provisioning.
pub fn spawn_cleanup(srv: &Srv, execution_uuid: String) {
    if srv.info.transferred {
        return;
    }
    let (jobs, id) = (srv.jobs.clone(), srv.id());
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        s.run_quiet(&cleanup_commands(&execution_uuid), "github_runner.cleanup")
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels() {
        assert!(kernel_supported("6.8.0-45-generic"));
        assert!(kernel_supported("5.12.0"));
        assert!(!kernel_supported("5.4.0"));
        assert!(!kernel_supported("nope"));
    }

    #[test]
    fn daemon_settings() {
        assert_eq!(
            daemon_config(
                Some(r#"{"bip":"1.2.3.4/16","default-address-pools":[{"base":"x"}]}"#),
                ""
            ),
            Ok(None)
        );
        let text = daemon_config(
            Some(r#"{"log-driver":"json-file"}"#),
            "172.17.0.1 172.17.0.0/16",
        )
        .ok()
        .flatten()
        .unwrap_or_default();
        assert!(text.starts_with(
            "{\n    \"log-driver\": \"json-file\",\n    \"bip\": \"172.17.0.1/16\",\n"
        ));
        assert!(text.ends_with("}\n"));
        assert!(daemon_config(Some("[]"), "").is_err());
    }
}
