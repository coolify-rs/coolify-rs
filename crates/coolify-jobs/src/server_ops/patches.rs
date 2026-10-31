//! `CheckUpdates`, `ServerPatchCheckJob` and `UpdatePackage`.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine as _;
use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ColumnValue};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::proxy::{queue_activity, run_activity};
use super::{MARKS, Srv, shell_arg};
use crate::Jobs;

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|_| unreachable!())
}

const DNF_UPGRADES: &str = r#"LANG=C dnf repoquery -q --refresh --upgrades --latest-limit 1 --qf "%{name} %{arch} %{evr} %{repoid}\n""#;
const APT_SIMULATE: &str = "LANG=C apt-get -s upgrade 2>/dev/null";
const APT_SIMULATE_PHASED: &str =
    "LANG=C apt-get -s -o APT::Get::Always-Include-Phased-Updates=true upgrade 2>/dev/null";
const APT_HELD: &str = "apt-mark showhold 2>/dev/null";
const APK_SIMULATE: &str = "LANG=C apk upgrade -s 2>/dev/null";
const RPM_INSTALLED: &str = r#"rpm -qa --qf "%{NAME} %{ARCH} %{VERSION}-%{RELEASE}\n""#;
const PACMAN_CHECK: &str = r#"sh -c 'if command -v checkupdates >/dev/null 2>&1 && command -v fakeroot >/dev/null 2>&1; then checkupdates; rc=$?; if [ $rc -eq 2 ]; then exit 0; fi; exit $rc; fi; db=$(mktemp -d) || exit 1; chmod 755 "$db"; ln -s /var/lib/pacman/local "$db/local"; if ! LANG=C pacman -Sy --dbpath "$db" --logfile /dev/null >/dev/null; then rm -rf "$db"; exit 1; fi; pacman -Qu --dbpath "$db" 2>/dev/null; rm -rf "$db"; exit 0'"#;
const DNF_SECURITY: &str = "sh -c 'LANG=C dnf updateinfo list --security -q 2>/dev/null; exit 0'";
const ZYPPER_SECURITY: &str =
    "sh -c 'LANG=C zypper -n -x patch --dry-run --category security 2>/dev/null; exit 0'";
const REBOOT_CHECK: &str = r#"sh -c 'if [ -f /var/run/reboot-required ]; then echo REBOOT=yes; exit 0; fi; if command -v needrestart >/dev/null 2>&1; then k=$(needrestart -b 2>/dev/null | sed -n "s/^NEEDRESTART-KSTA: *//p"); case "$k" in 2|3) echo REBOOT=yes; exit 0;; 1) echo REBOOT=no; exit 0;; esac; fi; if command -v dnf >/dev/null 2>&1; then o=$(LANG=C dnf needs-restarting -r 2>/dev/null); case "$o" in *"Reboot is required"*) echo REBOOT=yes; exit 0;; *"should not be necessary"*) echo REBOOT=no; exit 0;; esac; fi; if command -v zypper >/dev/null 2>&1; then zypper -q needs-rebooting >/dev/null 2>&1; rc=$?; if [ $rc -eq 102 ]; then echo REBOOT=yes; exit 0; fi; if [ $rc -eq 0 ]; then echo REBOOT=no; exit 0; fi; fi; if [ -d /var/lib/dpkg ]; then echo REBOOT=no; exit 0; fi; echo REBOOT=unknown'"#;

type RiskTable = &'static [(&'static str, &'static [&'static str])];

/// `CheckUpdates::RISKY_PACKAGES`.
fn risky(package_manager: &str) -> RiskTable {
    match package_manager {
        "apt" => &[
            (
                "docker",
                &[
                    "docker-ce",
                    "docker-ce-cli",
                    "docker-ce-rootless-extras",
                    "docker-buildx-plugin",
                    "docker-compose-plugin",
                    "containerd.io",
                    "containerd",
                    "runc",
                    "docker.io",
                ],
            ),
            (
                "kernel",
                &[
                    "linux-image-*",
                    "linux-modules-*",
                    "linux-generic*",
                    "linux-virtual*",
                    "linux-aws*",
                    "linux-azure*",
                    "linux-gcp*",
                    "linux-oracle*",
                    "linux-kvm*",
                    "linux-raspi*",
                ],
            ),
            (
                "system",
                &[
                    "libc6",
                    "libc-bin",
                    "systemd",
                    "systemd-sysv",
                    "libsystemd0",
                    "udev",
                    "dbus",
                    "openssh-server",
                    "openssh-sftp-server",
                ],
            ),
        ],
        "dnf" => &[
            (
                "docker",
                &[
                    "docker-ce",
                    "docker-ce-cli",
                    "docker-ce-rootless-extras",
                    "docker-buildx-plugin",
                    "docker-compose-plugin",
                    "containerd.io",
                    "containerd",
                    "runc",
                    "moby-engine",
                    "docker",
                ],
            ),
            (
                "kernel",
                &[
                    "kernel",
                    "kernel-core",
                    "kernel-modules",
                    "kernel-modules-core",
                    "kernel-modules-extra",
                    "kernel-uek",
                    "kernel-uek-core",
                    "kernel-uek-modules",
                ],
            ),
            (
                "system",
                &[
                    "glibc",
                    "glibc-common",
                    "glibc-langpack-*",
                    "systemd",
                    "systemd-libs",
                    "systemd-udev",
                    "dbus",
                    "dbus-broker",
                    "openssh-server",
                ],
            ),
        ],
        "zypper" => &[
            (
                "docker",
                &[
                    "docker",
                    "docker-ce",
                    "containerd",
                    "runc",
                    "docker-compose",
                    "docker-buildx",
                ],
            ),
            (
                "kernel",
                &[
                    "kernel-default",
                    "kernel-default-base",
                    "kernel-azure",
                    "kernel-kvmsmall",
                ],
            ),
            (
                "system",
                &[
                    "glibc",
                    "systemd",
                    "libsystemd0",
                    "udev",
                    "dbus-1",
                    "openssh",
                    "openssh-server",
                ],
            ),
        ],
        "pacman" => &[
            (
                "docker",
                &[
                    "docker",
                    "containerd",
                    "runc",
                    "docker-compose",
                    "docker-buildx",
                ],
            ),
            (
                "kernel",
                &["linux", "linux-lts", "linux-zen", "linux-hardened"],
            ),
            (
                "system",
                &["glibc", "systemd", "systemd-libs", "dbus", "openssh"],
            ),
        ],
        "apk" => &[
            (
                "docker",
                &[
                    "docker",
                    "docker-engine",
                    "docker-cli",
                    "docker-openrc",
                    "docker-cli-compose",
                    "docker-cli-buildx",
                    "containerd",
                    "runc",
                ],
            ),
            ("kernel", &["linux-lts", "linux-virt", "linux-edge"]),
            ("system", &["musl", "openrc", "openssh", "openssh-server"]),
        ],
        _ => &[],
    }
}

/// `riskFor`: `fnmatch` with `*` only.
pub fn risk_for(package_manager: &str, package: &str) -> Option<&'static str> {
    for (risk, patterns) in risky(package_manager) {
        for p in *patterns {
            let hit = match p.strip_suffix('*') {
                Some(prefix) => package.starts_with(prefix),
                None => *p == package,
            };
            if hit {
                return Some(risk);
            }
        }
    }
    None
}

static APT_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(.+?)/(\S+)\s+(\S+)\s+(\S+)\s+\[upgradable from: (.+?)\]"));
static PACMAN_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\S+)\s+(\S+)\s+->\s+(\S+)\n?\z"));
static APK_LINE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(.+)-([0-9]\S*) (\S+) \{\S+\} \([^)]+\) \[upgradable from: .+?-([0-9][^\]]+)\]\n?\z")
});

fn result(updates: Vec<Value>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("total_updates".into(), json!(updates.len()));
    m.insert("updates".into(), Value::Array(updates));
    m
}

/// `parseAptOutput`.
pub fn parse_apt_output(output: &str) -> Map<String, Value> {
    let updates = output
        .split('\n')
        .filter(|l| !l.is_empty() && *l != "0" && !l.contains("Listing..."))
        .filter_map(|l| {
            let c = APT_LINE.captures(l)?;
            Some(json!({
                "package": &c[1], "repository": &c[2], "new_version": &c[3],
                "architecture": &c[4], "current_version": &c[5],
            }))
        })
        .collect();
    result(updates)
}

/// `parsePacmanOutput`.
pub fn parse_pacman_output(output: &str) -> Map<String, Value> {
    let mut updates = Vec::new();
    let mut unparsed = Vec::new();
    for l in output.split('\n').filter(|l| !l.is_empty() && *l != "0") {
        if let Some(c) = PACMAN_LINE.captures(l) {
            updates.push(json!({
                "package": &c[1], "current_version": &c[2], "new_version": &c[3],
                "architecture": "unknown", "repository": "unknown",
            }));
        } else {
            unparsed.push(json!(l));
        }
    }
    let mut r = result(updates);
    if !unparsed.is_empty() {
        r.insert("unparsed_lines".into(), Value::Array(unparsed));
    }
    r
}

/// `parseApkOutput`.
pub fn parse_apk_output(output: &str) -> Map<String, Value> {
    let updates = output
        .split('\n')
        .filter(|l| !l.is_empty() && *l != "0")
        .filter_map(|l| {
            let c = APK_LINE.captures(l)?;
            Some(json!({
                "package": &c[1], "new_version": &c[2], "architecture": &c[3], "current_version": &c[4],
            }))
        })
        .collect();
    result(updates)
}

/// `parseDnfOutput`.
pub fn parse_dnf_output(output: &str, installed: &str) -> Map<String, Value> {
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    for l in installed.split('\n') {
        let p: Vec<&str> = l.split_whitespace().collect();
        if p.len() == 3 {
            versions.insert(format!("{}.{}", p[0], p[1]), p[2].to_string());
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    for l in output.split('\n') {
        let p: Vec<&str> = l.split_whitespace().collect();
        if p.len() != 4 {
            continue;
        }
        let key = format!("{}.{}", p[0], p[1]);
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.insert(
            key.clone(),
            json!({
                "package": p[0],
                "new_version": p[2].strip_prefix("0:").unwrap_or(p[2]),
                "repository": p[3],
                "architecture": p[1],
                "current_version": versions.get(&key).map_or("unknown", String::as_str),
            }),
        );
    }
    result(order.iter().filter_map(|k| map.remove(k)).collect())
}

/// `parseZypperOutput` for the `update` elements of `zypper -tx list-updates`.
pub fn parse_zypper_output(output: &str) -> Map<String, Value> {
    if !output.contains("<stream") && !output.contains("<?xml") {
        let mut m = Map::new();
        m.insert("total_updates".into(), json!(0));
        m.insert("updates".into(), json!([]));
        m.insert("error".into(), json!("Failed to parse XML output"));
        return m;
    }
    static UPDATE: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?s)<update\b([^>]*?)(?:/>|>(.*?)</update>)"#));
    static SUMMARY: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<summary>(.*?)</summary>"));
    static DESCRIPTION: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?s)<description>(.*?)</description>"));
    static SOURCE: LazyLock<Regex> = LazyLock::new(|| re(r#"<source\b[^>]*\balias="([^"]*)""#));
    let attr = |attrs: &str, name: &str| {
        re(&format!(r#"\b{}="([^"]*)""#, regex::escape(name)))
            .captures(attrs)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default()
    };
    let mut updates = Vec::new();
    for c in UPDATE.captures_iter(output) {
        let attrs = c.get(1).map_or("", |m| m.as_str());
        let body = c.get(2).map_or("", |m| m.as_str());
        let sub = |r: &Regex| {
            r.captures(body)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .unwrap_or_default()
        };
        updates.push(json!({
            "package": attr(attrs, "name"),
            "new_version": attr(attrs, "edition"),
            "current_version": attr(attrs, "edition-old"),
            "architecture": attr(attrs, "arch"),
            "repository": sub(&SOURCE),
            "summary": sub(&SUMMARY),
            "description": sub(&DESCRIPTION),
        }));
    }
    result(updates)
}

/// `parseDnfSecurityPackages`.
pub fn parse_dnf_security(output: &str) -> Vec<String> {
    static TOKEN: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(.+)-[^-/]+-[^-/]+\.[A-Za-z0-9_]+\n?\z"));
    let mut out: Vec<String> = Vec::new();
    for t in output.split_whitespace() {
        if let Some(c) = TOKEN.captures(t) {
            let name = c[1].to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// `parseZypperSecurityPackages`.
pub fn parse_zypper_security(output: &str) -> Vec<String> {
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<to-upgrade>(.*?)</to-upgrade>"));
    static SOLVABLE: LazyLock<Regex> = LazyLock::new(|| re(r"<solvable\b[^>]*>"));
    static NAME: LazyLock<Regex> = LazyLock::new(|| re(r#"\bname="([^"]+)""#));
    let mut out: Vec<String> = Vec::new();
    for b in BLOCK.captures_iter(output) {
        for s in SOLVABLE.find_iter(&b[1]) {
            if s.as_str().contains("kind=\"package\"")
                && let Some(n) = NAME.captures(s.as_str())
                && !out.iter().any(|x| x == &n[1])
            {
                out.push(n[1].to_string());
            }
        }
    }
    out
}

/// `parseRebootStatus`.
fn parse_reboot(output: &str) -> Value {
    if output.contains("REBOOT=yes") {
        json!(true)
    } else if output.contains("REBOOT=no") {
        json!(false)
    } else {
        Value::Null
    }
}

/// `parseAptHeldPackages`.
pub fn parse_apt_held(held: &str, upgrade: &str, phased: &str) -> Vec<(String, &'static str)> {
    static SUMMARY: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\d+ upgraded,"));
    static INST: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^Inst ([^\s:]+)"));
    static HELD: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([^\s:]+)"));
    let installs = |o: &str| {
        SUMMARY.is_match(o).then(|| {
            INST.captures_iter(o)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
    };
    let mut out: Vec<(String, &'static str)> = Vec::new();
    if let (Some(a), Some(b)) = (installs(upgrade), installs(phased)) {
        for p in b {
            if !a.contains(&p) {
                out.push((p, "phased"));
            }
        }
    }
    for c in HELD.captures_iter(held) {
        let name = c[1].to_string();
        out.retain(|(p, _)| *p != name);
        out.push((name, "held"));
    }
    out
}

/// `separateHeldUpdates`.
fn separate_held(
    mut out: Map<String, Value>,
    held: &[(String, &'static str)],
) -> Map<String, Value> {
    let updates = out
        .get("updates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut kept = Vec::new();
    let mut held_updates = Vec::new();
    for u in updates {
        let name = u.get("package").and_then(Value::as_str).unwrap_or_default();
        if let Some((_, reason)) = held.iter().find(|(p, _)| p == name) {
            let mut h = u.clone();
            if let Some(m) = h.as_object_mut() {
                m.insert("held_reason".into(), json!(reason));
            }
            held_updates.push(h);
        } else {
            kept.push(u);
        }
    }
    out.insert("total_updates".into(), json!(kept.len()));
    out.insert("updates".into(), Value::Array(kept));
    out.insert("held_updates".into(), Value::Array(held_updates));
    // Laravel's order: updates, total_updates, held_updates are assigned on the existing keys.
    let mut ordered = Map::new();
    for k in ["total_updates", "updates", "held_updates"] {
        if let Some(v) = out.shift_remove(k) {
            ordered.insert(k.into(), v);
        }
    }
    ordered.extend(out);
    ordered
}

/// `addUpdateDetails`.
fn add_details(
    mut out: Map<String, Value>,
    pm: &str,
    security: Option<&[String]>,
) -> Map<String, Value> {
    let knows = pm == "apt" || security.is_some();
    let updates: Vec<Value> = out
        .get("updates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut u| {
            let package = u
                .get("package")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let sec = if pm == "apt" {
                u.get("repository")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .split(',')
                    .any(|s| s.ends_with("-security"))
            } else {
                security.is_some_and(|s| s.contains(&package))
            };
            if let Some(m) = u.as_object_mut() {
                m.insert("security".into(), json!(sec));
                m.insert(
                    "risk".into(),
                    risk_for(pm, &package).map_or(Value::Null, |r| json!(r)),
                );
            }
            u
        })
        .collect();
    let count = updates
        .iter()
        .filter(|u| u.get("security") == Some(&Value::Bool(true)))
        .count();
    out.insert("updates".into(), Value::Array(updates));
    out.insert(
        "security_updates".into(),
        if knows { json!(count) } else { Value::Null },
    );
    out
}

/// `CheckUpdates::handle`.
pub async fn check_updates(srv: &Srv) -> Map<String, Value> {
    let mut os_id = "unknown".to_string();
    let mut package_manager: Option<&'static str> = None;
    let result: Result<Map<String, Value>, String> = async {
        if !srv.info.is_functional() {
            let mut m = Map::new();
            m.insert(
                "error".into(),
                json!("Server is not reachable or not ready."),
            );
            return Ok(m);
        }
        let release = srv
            .run(&["cat /etc/os-release".to_string()], "patch.os")
            .await?
            .unwrap_or_default();
        let mut id = String::new();
        for l in release.split('\n') {
            if let Some((k, v)) = l.split_once('=')
                && k == "ID"
            {
                id = v.trim_matches('"').to_string();
            }
        }
        os_id = id.clone();
        let os_type = match id.as_str() {
            "manjaro" | "manjaro-arm" | "endeavouros" => "arch",
            "pop" | "linuxmint" | "zorin" => "ubuntu",
            "fedora-asahi-remix" => "fedora",
            o => o,
        }
        .to_string();
        package_manager = match os_type.as_str() {
            "arch" => Some("pacman"),
            "alpine" => Some("apk"),
            "ubuntu" | "debian" | "raspbian" => Some("apt"),
            "centos" | "fedora" | "rhel" | "ol" | "rocky" | "almalinux" | "amzn" => Some("dnf"),
            "sles" | "opensuse-leap" | "opensuse-tumbleweed" => Some("zypper"),
            _ => None,
        };
        let run = |c: &str| {
            let c = c.to_string();
            async move {
                srv.run(&[c], "patch.check")
                    .await
                    .map(Option::unwrap_or_default)
            }
        };
        let quiet = |c: &str| {
            let c = c.to_string();
            async move { srv.run_quiet(&[c], "patch.check").await.unwrap_or_default() }
        };
        let mut security: Option<Vec<String>> = None;
        let out = match package_manager {
            Some("zypper") => {
                let o = parse_zypper_output(&run("LANG=C zypper -tx list-updates").await?);
                security = Some(parse_zypper_security(&quiet(ZYPPER_SECURITY).await));
                o
            }
            Some("dnf") => {
                let output = run(DNF_UPGRADES).await?;
                let installed = run(RPM_INSTALLED).await?;
                let o = parse_dnf_output(&output, &installed);
                security = Some(parse_dnf_security(&quiet(DNF_SECURITY).await));
                o
            }
            Some("apt") => {
                run("apt-get update -qq").await?;
                let listing =
                    parse_apt_output(&run("LANG=C apt list --upgradable 2>/dev/null").await?);
                let held = parse_apt_held(
                    &quiet(APT_HELD).await,
                    &quiet(APT_SIMULATE).await,
                    &quiet(APT_SIMULATE_PHASED).await,
                );
                separate_held(listing, &held)
            }
            Some("pacman") => parse_pacman_output(&run(PACMAN_CHECK).await?),
            Some("apk") => {
                run("apk update -q").await?;
                let listing =
                    parse_apk_output(&run("LANG=C apk list --upgradable 2>/dev/null").await?);
                let simulation = quiet(APK_SIMULATE).await;
                let mut held = Vec::new();
                if re(r"(?m)^OK: ").is_match(&simulation) {
                    let upgrading: Vec<String> = re(r"(?m)^\(\s*\d+/\d+\) Upgrading (\S+) \(")
                        .captures_iter(&simulation)
                        .map(|c| c[1].to_string())
                        .collect();
                    for u in listing
                        .get("updates")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let p = u.get("package").and_then(Value::as_str).unwrap_or_default();
                        if !upgrading.iter().any(|x| x == p) {
                            held.push((p.to_string(), "held"));
                        }
                    }
                }
                separate_held(listing, &held)
            }
            _ => {
                let mut m = Map::new();
                m.insert("osId".into(), json!(os_id));
                m.insert("error".into(), json!("Unsupported package manager"));
                m.insert("package_manager".into(), Value::Null);
                return Ok(m);
            }
        };
        let pm = package_manager.unwrap_or_default();
        let mut out = add_details(out, pm, security.as_deref());
        out.insert(
            "reboot_required".into(),
            parse_reboot(&quiet(REBOOT_CHECK).await),
        );
        out.insert("osId".into(), json!(os_id));
        out.insert("package_manager".into(), json!(pm));
        Ok(out)
    }
    .await;
    result.unwrap_or_else(|e| {
        let mut m = Map::new();
        m.insert("osId".into(), json!(os_id));
        m.insert(
            "package_manager".into(),
            package_manager.map_or(Value::Null, |p| json!(p)),
        );
        m.insert("error".into(), json!(e));
        m
    })
}

/// `ServerPatchCheckJob::saveResult`.
async fn save_result(srv: &Srv, data: &Map<String, Value>) -> Result<(), String> {
    let error = data
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stored: Value = if error.is_none() {
        Value::Object(data.clone())
    } else {
        let mut m = Map::new();
        for k in ["osId", "package_manager"] {
            if let Some(v) = data.get(k) {
                m.insert(k.into(), v.clone());
            }
        }
        Value::Object(m)
    };
    let text = coolify_compat::php_json::encode(&stored);
    ops::save_settings_json(
        srv.db(),
        srv.id(),
        "patch_check_result",
        Some(&text),
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    servers::update_settings(
        srv.db(),
        srv.id(),
        &[
            ("patch_check_error", ColumnValue::Text(error)),
            ("patch_checked_at", ColumnValue::Time(Jobs::now())),
        ],
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// `ServerPatchCheckJob::handle`.
pub async fn server_patch_check(jobs: &Jobs, server_id: i64, manual: bool) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    if srv.info.transferred {
        return Ok(());
    }
    if !srv.info.is_functional() {
        if manual {
            let mut m = Map::new();
            m.insert(
                "error".into(),
                json!("Server is not reachable or not ready."),
            );
            save_result(&srv, &m).await?;
        }
        return Ok(());
    }
    let previous_error = srv
        .info
        .settings
        .get("patch_check_error")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let data = check_updates(&srv).await;
    save_result(&srv, &data).await?;
    let notify = if data.get("error").is_some_and(|e| !e.is_null()) {
        // Only when the state changes from ok to error, not on every check of a broken server.
        previous_error.is_none_or(|e| e.trim().is_empty())
    } else {
        !manual
            && data
                .get("total_updates")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 0
    };
    if notify
        && let Some((team_id, server)) =
            crate::notifications::lookup::server(srv.db(), server_id).await
    {
        crate::notifications::notify(
            srv.db(),
            jobs.0.encrypter.as_ref(),
            team_id,
            crate::notifications::Notification::ServerPatchCheck {
                server,
                patch_data: Value::Object(data),
            },
        )
        .await;
    }
    Ok(())
}

/// `ServerPatchCheckJob::runningCacheKey`.
pub fn running_key(server_uuid: &str) -> String {
    format!("server-patch-check-running:{server_uuid}")
}

/// `ServerPatchCheckJob::dispatch`: a job after `delay` that saves its failure and removes the running mark.
/// `WithoutOverlapping`: a check of a server that is already checked is dropped.
fn dispatch_patch_check(jobs: &Jobs, server_id: i64, server_uuid: String, delay: Duration) {
    let jobs = jobs.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let lock = format!("server-patch-check-{server_uuid}");
        if !MARKS.add(lock.clone(), Duration::from_secs(600)) {
            return;
        }
        if let Err(e) = server_patch_check(&jobs, server_id, true).await {
            tracing::warn!(server_id, error = %e, "patch check failed");
            if let Ok(srv) = Srv::load(&jobs, server_id).await {
                let mut m = Map::new();
                m.insert("error".into(), json!(e));
                let _ = save_result(&srv, &m).await;
            }
        }
        MARKS.forget(&lock);
        MARKS.forget(&running_key(&server_uuid));
    });
}

/// `ServerPatchCheckJob::dispatchManual`.
pub fn dispatch_manual(jobs: &Jobs, server_id: i64, server_uuid: &str) {
    MARKS.put(
        running_key(server_uuid),
        String::new(),
        Duration::from_secs(15 * 60),
    );
    dispatch_patch_check(jobs, server_id, server_uuid.to_string(), Duration::ZERO);
}

/// `UpdatePackage::commands`.
pub fn update_commands(pm: Option<&str>, escaped: Option<&str>) -> Option<Vec<String>> {
    let apt = "-y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";
    Some(match pm? {
        "apt" => vec![
            "env DEBIAN_FRONTEND=noninteractive apt-get update -qq".to_string(),
            match escaped {
                None => format!("env DEBIAN_FRONTEND=noninteractive apt-get {apt} upgrade"),
                Some(p) => format!(
                    "env DEBIAN_FRONTEND=noninteractive apt-get {apt} install --only-upgrade {p}"
                ),
            },
        ],
        "dnf" => vec![match escaped {
            None => "dnf upgrade -y".to_string(),
            Some(p) => format!("dnf upgrade -y {p}"),
        }],
        "zypper" => vec![match escaped {
            None => "zypper --non-interactive update".to_string(),
            Some(p) => format!("zypper --non-interactive update {p}"),
        }],
        "pacman" => {
            if escaped.is_some() {
                return None;
            }
            vec!["pacman -Syu --noconfirm".to_string()]
        }
        "apk" => vec![
            "apk update".to_string(),
            match escaped {
                None => "apk upgrade".to_string(),
                Some(p) => format!("apk upgrade {p}"),
            },
        ],
        _ => return None,
    })
}

/// `UpdatePackage::handle`: the error it returns, or the id of the activity that runs the update.
/// `FinishServerPackagesUpdate` follows the activity: the audit entry, then a new check.
pub async fn update_package(
    srv: &Srv,
    package: Option<&str>,
    package_manager: Option<&str>,
    all: bool,
) -> Result<i64, String> {
    if !srv.info.is_functional() {
        return Err("Server is not reachable or not ready.".to_string());
    }
    if !all && package.is_none_or(str::is_empty) {
        return Err("Package name required when 'all' is false.".to_string());
    }
    let mut escaped = None;
    if let (Some(p), false) = (package, all) {
        if !p
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | ':' | '-'))
        {
            return Err("Invalid package name. Package names can only contain alphanumeric characters, hyphens, underscores, periods, plus signs, and colons.".to_string());
        }
        escaped = Some(shell_arg(p));
    }
    if !all && package_manager == Some("pacman") {
        return Err(
            "This operating system supports only a full system upgrade. Use \"Update all packages\"."
                .to_string(),
        );
    }
    let commands = update_commands(package_manager, escaped.as_deref())
        .ok_or_else(|| "OS not supported".to_string())?;
    let update_id = coolify_domain::new_public_id();
    let activity = queue_activity(srv, "patch.update", Some(&update_id)).await?;
    let (jobs, id, uuid, team_id) = (
        srv.jobs.clone(),
        srv.id(),
        srv.row.uuid.clone(),
        srv.info.team_id,
    );
    let packages = package.filter(|_| !all).map(str::to_string);
    let manager = package_manager.map(str::to_string);
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let result = run_activity(&s, activity, &commands, "patch.update").await;
        tracing::info!(
            target: "audit",
            event = "ui.server.packages_update_finished",
            team_id,
            server_uuid = %uuid,
            packages = packages.as_deref().unwrap_or("all"),
            package_manager = manager.as_deref().unwrap_or_default(),
            activity_id = activity,
            result = if result.is_ok() { "success" } else { "failed" },
            "packages update finished"
        );
        dispatch_manual(&jobs, id, &uuid);
    });
    Ok(activity)
}

/// `RebootServer::COMMAND`.
const REBOOT_COMMAND: &str = r#"sh -c 'if command -v systemd-run >/dev/null 2>&1; then systemd-run --on-active=5 --timer-property=AccuracySec=1s systemctl reboot >/dev/null 2>&1; else nohup sh -c "sleep 5; reboot" >/dev/null 2>&1 </dev/null & fi; echo REBOOT_SCHEDULED'"#;

/// `RebootServer::handle`.
pub async fn reboot_server(srv: &Srv) -> Result<(), String> {
    if srv.info.transferred {
        return Err("This server is managed by another Coolify instance.".to_string());
    }
    if !srv.info.is_functional() {
        return Err("Server is not reachable or not ready.".to_string());
    }
    let output = srv
        .run(&[REBOOT_COMMAND.to_string()], "patch.reboot")
        .await?
        .unwrap_or_default();
    if !output.contains("REBOOT_SCHEDULED") {
        return Err("The reboot could not be started.".to_string());
    }
    dispatch_patch_check(
        &srv.jobs,
        srv.id(),
        srv.row.uuid.clone(),
        Duration::from_secs(5 * 60),
    );
    Ok(())
}

const APT_CONFIG_FILE: &str = "/etc/apt/apt.conf.d/52coolify-unattended-upgrades";
const APT_TIMER_DROP_IN_DIRECTORY: &str = "/etc/systemd/system/apt-daily-upgrade.timer.d";
const DNF_CONFIG_FILE: &str = "/etc/dnf/automatic.conf";
const DNF_TIMER_SELECTION: &str = "t=dnf-automatic.timer; if [ -e /usr/lib/systemd/system/dnf5-automatic.timer ]; then t=dnf5-automatic.timer; fi; ";

/// `ConfigureAutomaticSecurityUpdates::aptConfig`.
const APT_CONFIG: &str = r#"// Managed by Coolify: install security updates only.
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
#clear Unattended-Upgrade::Allowed-Origins;
#clear Unattended-Upgrade::Origins-Pattern;
Unattended-Upgrade::Origins-Pattern {
    "origin=Debian,codename=${distro_codename}-security,label=Debian-Security";
    "origin=Debian,codename=${distro_codename},label=Debian-Security";
    "origin=Ubuntu,archive=${distro_codename}-security";
    "origin=UbuntuESMApps,archive=${distro_codename}-apps-security";
    "origin=UbuntuESM,archive=${distro_codename}-infra-security";
};
"#;

/// `ConfigureAutomaticSecurityUpdates::TIME_PATTERN`.
pub fn valid_update_time(time: &str) -> bool {
    let b = time.as_bytes();
    // PCRE's `$` also accepts a final newline, which `trim()` already removed.
    b.len() == 5
        && b[2] == b':'
        && matches!(
            (b[0], b[1]),
            (b'0' | b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
        )
        && matches!(b[3], b'0'..=b'5')
        && b[4].is_ascii_digit()
}

/// `ConfigureAutomaticSecurityUpdates::supportFor`.
pub fn auto_updates_supported(package_manager: Option<&str>) -> bool {
    matches!(package_manager, Some("apt" | "dnf"))
}

/// `ConfigureAutomaticSecurityUpdates::commands`.
pub fn auto_update_commands(package_manager: &str, enabled: bool, time: &str) -> Vec<String> {
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let drop_in_text =
        format!("[Timer]\nOnCalendar=\nOnCalendar=*-*-* {time}\nRandomizedDelaySec=0\n");
    let drop_in = format!("{APT_TIMER_DROP_IN_DIRECTORY}/coolify.conf");
    match (package_manager, enabled) {
        ("apt", true) => vec![
            "sh -c 'command -v unattended-upgrade >/dev/null 2>&1 || { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y unattended-upgrades; }'".to_string(),
            format!("mkdir -p {APT_TIMER_DROP_IN_DIRECTORY}"),
            format!("echo '{}' | base64 -d | tee {APT_CONFIG_FILE} > /dev/null", b64(APT_CONFIG)),
            format!("echo '{}' | base64 -d | tee {drop_in} > /dev/null", b64(&drop_in_text)),
            format!("chmod 644 {APT_CONFIG_FILE} {drop_in}"),
            "systemctl daemon-reload".to_string(),
            "systemctl enable --now apt-daily.timer apt-daily-upgrade.timer".to_string(),
        ],
        ("apt", false) => vec![
            format!("rm -f {APT_CONFIG_FILE} {drop_in}"),
            "systemctl daemon-reload".to_string(),
        ],
        ("dnf", true) => vec![
            "dnf install -y dnf-automatic".to_string(),
            format!(
                "sh -c 'set -e; f={DNF_CONFIG_FILE}; touch $f; sed -i -E \"/^[[:space:]]*(upgrade_type|apply_updates)[[:space:]]*=/d\" $f; grep -q \"^\\[commands\\]\" $f || printf \"\\n[commands]\\n\" >> $f; sed -i -e \"/^\\[commands\\]/a upgrade_type = security\" -e \"/^\\[commands\\]/a apply_updates = yes\" $f; chmod 644 $f'"
            ),
            format!(
                "sh -c 'set -e; {DNF_TIMER_SELECTION}mkdir -p /etc/systemd/system/$t.d; echo {} | base64 -d > /etc/systemd/system/$t.d/coolify.conf; chmod 644 /etc/systemd/system/$t.d/coolify.conf; systemctl daemon-reload; systemctl enable --now $t'",
                b64(&drop_in_text)
            ),
        ],
        _ => vec![format!(
            "sh -c 'set -e; {DNF_TIMER_SELECTION}if [ -e /usr/lib/systemd/system/$t ]; then systemctl disable --now $t; fi; rm -f /etc/systemd/system/$t.d/coolify.conf; systemctl daemon-reload'"
        )],
    }
}

/// `ConfigureAutomaticSecurityUpdates::handle`.
pub async fn configure_auto_updates(
    srv: &Srv,
    package_manager: &str,
    enabled: bool,
    time: &str,
) -> Result<(), String> {
    srv.run(
        &auto_update_commands(package_manager, enabled, time),
        "patch.auto_updates",
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_parser() {
        let o = parse_apt_output(
            "Listing... Done\nlibc6/noble-security 2.0-1 amd64 [upgradable from: 1.0-1]\n",
        );
        assert_eq!(o["total_updates"], json!(1));
        let d = add_details(o, "apt", None);
        assert_eq!(d["updates"][0]["security"], json!(true));
        assert_eq!(d["updates"][0]["risk"], json!("system"));
        assert_eq!(d["security_updates"], json!(1));
    }

    #[test]
    fn held_packages() {
        let h = parse_apt_held(
            "foo\n",
            "1 upgraded,\nInst a [1]\n",
            "2 upgraded,\nInst a [1]\nInst b [2]\n",
        );
        assert_eq!(
            h,
            vec![("b".to_string(), "phased"), ("foo".to_string(), "held")]
        );
    }

    #[test]
    fn other_parsers() {
        assert_eq!(
            parse_pacman_output("a 1 -> 2\nxx")["unparsed_lines"],
            json!(["xx"])
        );
        assert_eq!(
            parse_dnf_security("RLSA-1 Important/Sec. openssl-1:3.0.7-27.el9.x86_64"),
            vec!["openssl"]
        );
        assert_eq!(risk_for("apt", "linux-image-6"), Some("kernel"));
        assert_eq!(update_commands(Some("pacman"), Some("'x'")), None);
    }

    #[test]
    fn maintenance_time() {
        assert!(valid_update_time("03:00"));
        assert!(valid_update_time("23:59"));
        assert!(!valid_update_time("24:00"));
        assert!(!valid_update_time("3:00"));
        assert!(!valid_update_time("12:60"));
    }
}
