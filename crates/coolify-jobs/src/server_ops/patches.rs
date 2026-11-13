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
        // v4.7.5: the kernels, so the vulnerability list can tell an installed fix that waits
        // for a reboot from a kernel that does not run.
        out.insert(
            "kernels".into(),
            parse_kernels(&quiet(KERNELS_COMMAND).await).unwrap_or(Value::Null),
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
    let packages: Vec<String> = package
        .filter(|p| !p.is_empty())
        .map(|p| vec![p.to_owned()])
        .unwrap_or_default();
    update_packages(srv, &packages, package_manager, all).await
}

/// `UpdatePackage::handle` at v4.7.5: `$packages` is a list; it is ignored when `$all` is true.
pub async fn update_packages(
    srv: &Srv,
    packages: &[String],
    package_manager: Option<&str>,
    all: bool,
) -> Result<i64, String> {
    if !srv.info.is_functional() {
        return Err("Server is not reachable or not ready.".to_string());
    }
    if !all && packages.is_empty() {
        return Err("Package name required when 'all' is false.".to_string());
    }
    let escaped = if all {
        None
    } else {
        Some(escaped_packages(packages)?)
    };
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
    let packages = (!all).then(|| unique_packages(packages).join(","));
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
        scan_host_packages_after_update(&jobs, id).await;
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

/// `array_values(array_unique($packages))`.
fn unique_packages(packages: &[String]) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for package in packages {
        if !unique.contains(package) {
            unique.push(package.clone());
        }
    }
    unique
}

/// The package arguments of `UpdatePackage::handle`: only characters that are valid in package
/// names of all supported package managers; no leading hyphen, so a name is never read as an
/// option. Several escaped names are separated by spaces.
pub fn escaped_packages(packages: &[String]) -> Result<String, String> {
    let unique = unique_packages(packages);
    for package in &unique {
        let mut chars = package.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | ':' | '-'));
        if !valid {
            return Err("Invalid package name. Package names can only contain alphanumeric characters, hyphens, underscores, periods, plus signs, and colons.".to_string());
        }
    }
    Ok(unique
        .iter()
        .map(|p| shell_arg(p))
        .collect::<Vec<_>>()
        .join(" "))
}

/// `RemoveUnusedPackages::commands()` (v4.7.5): apt autoremove also removes old kernels. dnf
/// autoremove does not, so old install-only packages (kernels) are removed separately; dnf fails
/// when there are none. zypper removes old kernels with purge-kernels. `None` when the package
/// manager is not supported.
pub fn remove_unused_commands(package_manager: Option<&str>) -> Option<Vec<String>> {
    let commands: &[&str] = match package_manager {
        Some("apt") => &["env DEBIAN_FRONTEND=noninteractive apt-get -y autoremove"],
        Some("dnf") => &[
            "dnf autoremove -y",
            "sh -c 'dnf remove -y --oldinstallonly || true'",
        ],
        Some("zypper") => &["zypper --non-interactive purge-kernels"],
        _ => return None,
    };
    Some(commands.iter().map(|c| (*c).to_owned()).collect())
}

/// `RemoveUnusedPackages::handle`: removes packages that nothing needs anymore, and old kernels.
/// The package managers keep the running kernel. Returns the id of the activity.
pub async fn remove_unused_packages(
    srv: &Srv,
    package_manager: Option<&str>,
) -> Result<i64, String> {
    let commands = remove_unused_commands(package_manager).ok_or_else(|| {
        "Coolify cannot remove unused packages on this operating system.".to_string()
    })?;
    if !srv.info.is_functional() {
        return Err("Server is not reachable or not ready.".to_string());
    }
    let update_id = coolify_domain::new_public_id();
    let activity = queue_activity(srv, "patch.update", Some(&update_id)).await?;
    let (jobs, id, uuid, team_id) = (
        srv.jobs.clone(),
        srv.id(),
        srv.row.uuid.clone(),
        srv.info.team_id,
    );
    let manager = package_manager.map(str::to_string);
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let result = run_activity(&s, activity, &commands, "patch.update").await;
        // `FinishServerPackagesUpdate`: `'packages' => 'unused'` for the action `remove_unused`.
        tracing::info!(
            target: "audit",
            event = "ui.server.packages_update_finished",
            team_id,
            server_uuid = %uuid,
            packages = "unused",
            package_manager = manager.as_deref().unwrap_or_default(),
            activity_id = activity,
            result = if result.is_ok() { "success" } else { "failed" },
            "packages update finished"
        );
        dispatch_manual(&jobs, id, &uuid);
        scan_host_packages_after_update(&jobs, id).await;
    });
    Ok(activity)
}

/// The end of `FinishServerPackagesUpdate`: `ScanHostPackagesJob::dispatchManual($server)` when
/// `ImageRescanJob::canRunOn($server)`, so the OS package vulnerabilities of the server are checked
/// again after the update.
async fn scan_host_packages_after_update(jobs: &Jobs, server_id: i64) {
    use crate::image_scan::{ScanServer, host, scan_jobs};
    match ScanServer::load(jobs, server_id).await {
        Ok(server) if scan_jobs::can_run_on(&server) => host::dispatch_manual(jobs, &server),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(server_id, error = %e, "the host package scan after the update could not be queued");
        }
    }
}

/// `CheckUpdates::KERNELS_COMMAND` (v4.7.5): prints the running kernel (`RUNNING|<uname -r>`),
/// each installed kernel (`KERNEL|<package>|<version>[|<arch>]`), and the kernel that boots next
/// (`BOOT|<uname -r>`, or `BOOT|newest`) when the boot loader tells it.
pub const KERNELS_COMMAND: &str = r#"sh -c 'echo "RUNNING|$(uname -r)"; if command -v dpkg-query >/dev/null 2>&1; then dpkg-query -W -f="\${db:Status-Abbrev}|\${Package}|\${Version}\n" "linux-image-[0-9]*" 2>/dev/null | sed -n "s/^ii *|/KERNEL|/p"; fi; if command -v rpm >/dev/null 2>&1; then rpm -q --qf "KERNEL|%{NAME}|%{VERSION}-%{RELEASE}|%{ARCH}\n" kernel-core kernel kernel-uek-core 2>/dev/null | grep "^KERNEL|"; fi; if command -v grubby >/dev/null 2>&1; then b=$(grubby --default-kernel 2>/dev/null); case "$b" in */vmlinuz-*) echo "BOOT|${b##*/vmlinuz-}";; esac; elif [ -f /etc/default/grub ]; then d=$(cat /etc/default/grub /etc/default/grub.d/*.cfg 2>/dev/null | sed -n "s/^[[:space:]]*GRUB_DEFAULT=//p" | tail -n 1 | tr -d "\042\047"); if [ "$d" = saved ]; then d=$(grub-editenv list 2>/dev/null | sed -n "s/^saved_entry=//p"); fi; case "$d" in ""|0) echo "BOOT|newest";; *) r=$(printf "%s\n" "$d" | sed -n -e "s/.*with Linux \([^ ]*\).*/\1/p" -e "s/.*gnulinux-\([0-9][^>]*\)-\(advanced\|recovery\)-.*/\1/p" | head -n 1); if [ -n "$r" ]; then echo "BOOT|$r"; fi;; esac; fi; exit 0'"#;

/// `withoutEpoch()`.
fn without_epoch(version: &str) -> &str {
    match version.split_once(':') {
        Some((epoch, rest)) if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) => {
            rest
        }
        _ => version,
    }
}

/// `versionAtLeast()`: PHP `version_compare($version, $minimum, '>=')` without the epochs.
fn version_at_least(version: &str, minimum: &str) -> bool {
    super::version_compare(without_epoch(version), without_epoch(minimum))
        != std::cmp::Ordering::Less
}

/// `CheckUpdates::parseKernels()`: `{running, boot, installed: [{package, version, release}]}`,
/// or `None` when the output names no running kernel.
pub fn parse_kernels(output: &str) -> Option<Value> {
    let mut running: Option<String> = None;
    let mut boot: Option<String> = None;
    let mut installed: Vec<(String, String, String)> = Vec::new();
    let filled = |part: Option<&&str>| part.is_some_and(|p| !p.trim().is_empty());
    for line in output.lines() {
        let parts: Vec<&str> = line.trim().split('|').collect();
        match parts.first().copied() {
            Some("RUNNING") if filled(parts.get(1)) => running = Some(parts[1].to_owned()),
            Some("BOOT") if filled(parts.get(1)) => boot = Some(parts[1].to_owned()),
            Some("KERNEL") if filled(parts.get(1)) && filled(parts.get(2)) => {
                // uname -r of this kernel: the package name without the prefix (dpkg), or
                // version.arch (rpm).
                let release = if filled(parts.get(3)) {
                    format!("{}.{}", parts[2], parts[3])
                } else {
                    parts[1]
                        .strip_prefix("linux-image-")
                        .unwrap_or(parts[1])
                        .to_owned()
                };
                installed.push((parts[1].to_owned(), parts[2].to_owned(), release));
            }
            _ => {}
        }
    }
    if boot.as_deref() == Some("newest") {
        let mut newest: Option<&(String, String, String)> = None;
        for kernel in &installed {
            if newest.is_none_or(|n| version_at_least(&kernel.1, &n.1)) {
                newest = Some(kernel);
            }
        }
        boot = newest.map(|k| k.2.clone());
    }
    let running = running?;
    Some(json!({
        "running": running,
        "boot": boot,
        "installed": installed
            .iter()
            .map(|(package, version, release)| json!({"package": package, "version": version, "release": release}))
            .collect::<Vec<_>>(),
    }))
}

/// `CheckUpdates::FULL_UPGRADE_ONLY`.
pub const FULL_UPGRADE_ONLY: &[&str] = &["pacman"];

static KERNEL_FLAVOR_VERSIONED: LazyLock<Regex> =
    LazyLock::new(|| re(r"^linux-.*?-\d+\.\d+\.\d+[^-]*(?:-\d+)?-([a-z].*)$"));
static KERNEL_FLAVOR_META: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^linux-(?:(?:signed-|unsigned-)?image-|headers-|modules-(?:extra-)?|tools-)?([a-z][a-z0-9-]*?)(?:-(?:hwe|lts)-[\d.]+(?:-edge)?)?$",
    )
});
static DEBIAN_KERNEL_PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^linux-.*?-(\d+\.\d+(?:\.\d+)?-\d+)(?:-|$)"));
static RUNNING_DEBIAN_KERNEL: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d+\.\d+(?:\.\d+)?-\d+)"));
static SUSE_RELEASE: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+)-[a-z][a-z0-9_]*$"));
static LAST_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"\.\d+$"));
static LAST_PART: LazyLock<Regex> = LazyLock::new(|| re(r"\.[^.]+$"));

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// `CheckUpdates::kernelFlavor()`: flavor of a Debian or Ubuntu kernel package, so that an update
/// of another flavor (linux-aws for a generic kernel) is not offered as the fix: the suffix after
/// the version (linux-modules-6.8.0-139-generic: generic), or the meta package name without the
/// part and the HWE/LTS suffix (linux-image-generic-hwe-24.04: generic). Ubuntu's virtual meta
/// packages install the generic kernel. `None` when the name has no flavor.
fn kernel_flavor(package: &str) -> Option<String> {
    let flavor = KERNEL_FLAVOR_VERSIONED
        .captures(package)
        .or_else(|| KERNEL_FLAVOR_META.captures(package))?
        .get(1)?
        .as_str();
    Some(
        if flavor == "virtual" {
            "generic"
        } else {
            flavor
        }
        .to_owned(),
    )
}

/// `CheckUpdates::fixingUpdates()` (v4.7.5): updates of a saved check result that fix a vulnerable
/// package: its own update, or for a kernel package that has the version in its name (Debian,
/// Ubuntu: linux-modules-6.8.0-139-generic) the kernel updates of the same flavor, because the
/// fixed kernel comes as a new package of the kernel meta packages. Empty on systems that only do
/// full upgrades.
pub fn fixing_updates(package: &str, patch_result: &Value) -> Vec<Value> {
    let package_manager = str_at(patch_result, "package_manager");
    if package_manager.is_some_and(|pm| FULL_UPGRADE_ONLY.contains(&pm)) {
        return Vec::new();
    }
    let updates = patch_result
        .get("updates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(own) = updates
        .iter()
        .find(|u| str_at(u, "package") == Some(package))
    {
        return vec![own.clone()];
    }
    let Some(flavor) = kernel_flavor(package) else {
        return Vec::new();
    };
    if risk_for(package_manager.unwrap_or_default(), package) != Some("kernel") {
        return Vec::new();
    }
    updates
        .into_iter()
        .filter(|u| {
            str_at(u, "risk") == Some("kernel")
                && kernel_flavor(str_at(u, "package").unwrap_or_default()).as_deref()
                    == Some(flavor.as_str())
        })
        .collect()
}

/// `CheckUpdates::bootKernel()` (v4.7.5): the installed kernel (`{package, version, release}`)
/// that the boot loader starts at the next reboot, or `None` when the last patch check could not
/// tell.
pub fn boot_kernel(patch_result: &Value) -> Option<Value> {
    let kernels = patch_result.get("kernels")?;
    let boot = str_at(kernels, "boot")?;
    kernels
        .get("installed")
        .and_then(Value::as_array)?
        .iter()
        .find(|k| str_at(k, "release") == Some(boot))
        .cloned()
}

/// `CheckUpdates::kernelHasPackage()`: Debian packages of a kernel have its release in the name
/// (linux-modules-6.8.0-139-generic); RPM kernel packages have a fixed name and the version of the
/// kernel.
fn kernel_has_package(kernel: &Value, package: &str, version: &str) -> bool {
    let release = str_at(kernel, "release").unwrap_or_default();
    if package.ends_with(&format!("-{release}")) {
        return true;
    }
    package.starts_with("kernel")
        && without_epoch(version) == without_epoch(str_at(kernel, "version").unwrap_or_default())
}

/// `CheckUpdates::kernelStatus()` (v4.7.5): kernel state of a vulnerable package, from the kernels
/// of the last patch check:
/// - `fixed_after_reboot`: the package belongs to the running kernel, and the kernel that boots
///   next has the fix.
/// - `runs_after_reboot`: the package belongs to the kernel that boots next, which is not the
///   running one.
///
/// Both need the boot kernel ([`boot_kernel`]); without it, there is no claim. Other kernels that
/// the server does not run: see [`is_other_kernel_package`].
pub fn kernel_status(
    package: &str,
    version: &str,
    fixed_in: Option<&str>,
    patch_result: &Value,
) -> Option<&'static str> {
    let kernels = patch_result.get("kernels")?;
    let running = str_at(kernels, "running")?;
    let installed = kernels.get("installed").and_then(Value::as_array)?;
    if installed.is_empty() {
        return None;
    }
    installed
        .iter()
        .find(|k| str_at(k, "release") == Some(running))?;
    let kernel = installed
        .iter()
        .find(|k| kernel_has_package(k, package, version))?;
    let boot = boot_kernel(patch_result)?;
    let boot_release = str_at(&boot, "release").unwrap_or_default();
    if boot_release == running {
        return None;
    }
    let release = str_at(kernel, "release").unwrap_or_default();
    if release != running {
        return (release == boot_release).then_some("runs_after_reboot");
    }
    fixed_in
        .filter(|fixed| version_at_least(str_at(&boot, "version").unwrap_or_default(), fixed))
        .map(|_| "fixed_after_reboot")
}

/// `CheckUpdates::isRpmKernelPackage()`: packages of a bootable RPM kernel (dnf, zypper).
/// Userspace packages such as kernel-tools or kernel-headers have their own version and are not
/// part of a kernel.
fn is_rpm_kernel_package(package: &str) -> bool {
    risk_for("dnf", package) == Some("kernel") || risk_for("zypper", package) == Some("kernel")
}

/// `CheckUpdates::isOtherKernelPackage()` (v4.7.5): true for a package of an installed kernel that
/// the server does not run (`uname -r`), older or newer. Its vulnerabilities are no risk now; a
/// newer one runs after the next reboot and then replaces the running kernel. Debian kernel
/// packages carry the release in the name (linux-modules-6.8.0-139-generic,
/// linux-headers-6.8.0-139), RPM kernel packages the version.
pub fn is_other_kernel_package(
    package: &str,
    version: &str,
    running_release: Option<&str>,
) -> bool {
    let Some(running_release) = running_release.filter(|r| !r.trim().is_empty()) else {
        return false;
    };
    if let Some(package_kernel) = DEBIAN_KERNEL_PACKAGE.captures(package) {
        return RUNNING_DEBIAN_KERNEL
            .captures(running_release)
            .is_some_and(|running| package_kernel[1] != running[1]);
    }
    if !is_rpm_kernel_package(package) {
        return false;
    }
    let version = without_epoch(version);
    // SUSE: uname -r 6.4.0-150600.23.17-default for kernel-default 6.4.0-150600.23.17.1 (the last
    // number is the rebuild).
    if let Some(suse) = SUSE_RELEASE.captures(running_release) {
        return LAST_NUMBER.replace(version, "") != suse[1];
    }
    version != LAST_PART.replace(running_release, "")
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

#[cfg(test)]
mod v475_tests {
    use super::*;

    // `CheckUpdates::parseKernels()` (upstream tests/Unit/CheckUpdatesKernelsTest).
    #[test]
    fn parses_debian_kernels_and_the_newest_boot_kernel() {
        let output = "RUNNING|6.8.0-100-generic\nKERNEL|linux-image-6.8.0-100-generic|6.8.0-100.100\nKERNEL|linux-image-6.8.0-139-generic|6.8.0-139.139\nBOOT|newest\n";
        assert_eq!(
            parse_kernels(output),
            Some(json!({
                "running": "6.8.0-100-generic",
                "boot": "6.8.0-139-generic",
                "installed": [
                    {"package": "linux-image-6.8.0-100-generic", "version": "6.8.0-100.100", "release": "6.8.0-100-generic"},
                    {"package": "linux-image-6.8.0-139-generic", "version": "6.8.0-139.139", "release": "6.8.0-139-generic"},
                ],
            }))
        );
    }

    #[test]
    fn parses_rpm_kernels_with_the_architecture_in_the_release() {
        let output = "RUNNING|5.14.0-427.el9.x86_64\r\nKERNEL|kernel-core|5.14.0-427.el9|x86_64\nBOOT|5.14.0-427.el9.x86_64\n";
        let kernels = parse_kernels(output).unwrap_or_default();
        assert_eq!(kernels["boot"], "5.14.0-427.el9.x86_64");
        assert_eq!(kernels["installed"][0]["release"], "5.14.0-427.el9.x86_64");
        // Without a boot loader answer there is no boot kernel.
        let kernels = parse_kernels("RUNNING|6.1.0\n").unwrap_or_default();
        assert_eq!(kernels["boot"], Value::Null);
        assert_eq!(kernels["installed"], json!([]));
        assert_eq!(parse_kernels("KERNEL|linux-image-6.1|6.1\n"), None);
        assert_eq!(parse_kernels(""), None);
    }

    #[test]
    fn several_packages_are_escaped_one_by_one() {
        let packages = vec![
            "openssl".to_owned(),
            "libc6".to_owned(),
            "openssl".to_owned(),
        ];
        assert_eq!(
            escaped_packages(&packages),
            Ok("'openssl' 'libc6'".to_owned())
        );
        // No leading hyphen, so a name is never read as an option.
        for bad in ["-y", "a b", "x;rm", ""] {
            assert!(escaped_packages(&[bad.to_owned()]).is_err(), "{bad}");
        }
        assert_eq!(
            update_commands(Some("apt"), Some("'openssl' 'libc6'")).map(|c| !c.is_empty()),
            Some(true)
        );
    }

    // Upstream tests/Unit/Actions/Server/ServerPatchCommandsTest.php.
    #[test]
    fn finds_the_packages_of_kernels_that_the_server_does_not_run() {
        let cases = [
            (
                "linux-modules-6.8.0-139-generic",
                "6.8.0-139.149",
                "6.8.0-139-generic",
                false,
            ),
            (
                "linux-modules-6.8.0-130-generic",
                "6.8.0-130.140",
                "6.8.0-139-generic",
                true,
            ),
            (
                "kernel-core",
                "5.14.0-427.13.1.el9_4",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-core",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                true,
            ),
            (
                "kernel-tools",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-headers",
                "5.14.0-362.8.1.el9_3",
                "5.14.0-427.13.1.el9_4.x86_64",
                false,
            ),
            (
                "kernel-default",
                "6.4.0-150600.23.17.1",
                "6.4.0-150600.23.17-default",
                false,
            ),
            (
                "kernel-default",
                "6.4.0-150600.23.14.1",
                "6.4.0-150600.23.17-default",
                true,
            ),
            ("kernel-default", "6.11.8-1.1", "6.11.8-1-default", false),
        ];
        for (package, version, running, other) in cases {
            assert_eq!(
                is_other_kernel_package(package, version, Some(running)),
                other,
                "{package} {version}"
            );
        }
        assert!(!is_other_kernel_package(
            "linux-modules-6.8.0-130-generic",
            "1",
            None
        ));
    }

    #[test]
    fn claims_a_kernel_fix_after_a_reboot_only_when_the_boot_kernel_has_it() {
        let cases = [
            (
                "BOOT|newest",
                Some("fixed_after_reboot"),
                Some("runs_after_reboot"),
            ),
            (
                "BOOT|6.8.0-139-generic",
                Some("fixed_after_reboot"),
                Some("runs_after_reboot"),
            ),
            ("BOOT|6.8.0-130-generic", None, None),
            ("", None, None),
        ];
        for (boot, running_status, new_status) in cases {
            let output = [
                "RUNNING|6.8.0-130-generic",
                "KERNEL|linux-image-6.8.0-130-generic|6.8.0-130.140",
                "KERNEL|linux-image-6.8.0-139-generic|6.8.0-139.149",
                boot,
            ]
            .join("\n");
            let result = json!({"kernels": parse_kernels(&output)});
            assert_eq!(
                kernel_status(
                    "linux-modules-6.8.0-130-generic",
                    "6.8.0-130.140",
                    Some("6.8.0-135.1"),
                    &result
                ),
                running_status,
                "{boot}"
            );
            assert_eq!(
                kernel_status(
                    "linux-modules-6.8.0-139-generic",
                    "6.8.0-139.149",
                    None,
                    &result
                ),
                new_status,
                "{boot}"
            );
        }
    }

    #[test]
    fn claims_a_fix_after_a_reboot_only_when_the_rhel_boot_kernel_has_it() {
        let output = "RUNNING|5.14.0-427.13.1.el9_4.x86_64\nKERNEL|kernel-core|5.14.0-427.13.1.el9_4|x86_64\nKERNEL|kernel-core|5.14.0-503.11.1.el9_5|x86_64\nKERNEL|kernel-core|5.14.0-570.1.1.el9_6|x86_64\nBOOT|5.14.0-503.11.1.el9_5.x86_64";
        let result = json!({"kernels": parse_kernels(output)});
        assert_eq!(
            boot_kernel(&result).and_then(|k| k["version"].as_str().map(str::to_owned)),
            Some("5.14.0-503.11.1.el9_5".to_owned())
        );
        assert_eq!(
            kernel_status(
                "kernel-core",
                "5.14.0-427.13.1.el9_4",
                Some("5.14.0-503.1.1.el9_5"),
                &result
            ),
            Some("fixed_after_reboot")
        );
        assert_eq!(
            kernel_status(
                "kernel-core",
                "5.14.0-427.13.1.el9_4",
                Some("5.14.0-570.1.1.el9_6"),
                &result
            ),
            None
        );
        assert_eq!(
            kernel_status("kernel-core", "5.14.0-570.1.1.el9_6", None, &result),
            None
        );
        assert_eq!(boot_kernel(&json!({})), None);
    }

    // `CheckUpdates::fixingUpdates()`: the own update, or kernel updates of the same flavor.
    #[test]
    fn fixing_updates_are_the_own_update_or_kernels_of_the_same_flavor() {
        let result = json!({
            "package_manager": "apt",
            "updates": [
                {"package": "openssl", "new_version": "3.0.13-2"},
                {"package": "linux-image-generic", "risk": "kernel", "new_version": "6.8.0.139"},
                {"package": "linux-headers-generic-hwe-24.04", "risk": "kernel"},
                {"package": "linux-image-aws", "risk": "kernel"},
            ],
        });
        assert_eq!(
            fixing_updates("openssl", &result),
            vec![json!({"package": "openssl", "new_version": "3.0.13-2"})]
        );
        let kernels = fixing_updates("linux-modules-6.8.0-130-generic", &result);
        let names: Vec<&str> = kernels
            .iter()
            .filter_map(|u| u["package"].as_str())
            .collect();
        assert_eq!(
            names,
            ["linux-image-generic", "linux-headers-generic-hwe-24.04"]
        );
        assert!(fixing_updates("curl", &result).is_empty());
        let pacman = json!({"package_manager": "pacman", "updates": [{"package": "openssl"}]});
        assert!(fixing_updates("openssl", &pacman).is_empty());
        assert_eq!(
            kernel_flavor("linux-image-virtual"),
            Some("generic".to_owned())
        );
        assert_eq!(
            kernel_flavor("linux-modules-6.8.0-139-generic"),
            Some("generic".to_owned())
        );
        assert_eq!(kernel_flavor("openssl"), None);
    }

    // `RemoveUnusedPackages::commands()`.
    #[test]
    fn unused_packages_are_removed_per_package_manager() {
        assert_eq!(
            remove_unused_commands(Some("apt")),
            Some(vec![
                "env DEBIAN_FRONTEND=noninteractive apt-get -y autoremove".to_owned()
            ])
        );
        assert_eq!(
            remove_unused_commands(Some("dnf")),
            Some(vec![
                "dnf autoremove -y".to_owned(),
                "sh -c 'dnf remove -y --oldinstallonly || true'".to_owned()
            ])
        );
        assert_eq!(
            remove_unused_commands(Some("zypper")),
            Some(vec!["zypper --non-interactive purge-kernels".to_owned()])
        );
        for unsupported in [Some("pacman"), Some("apk"), None] {
            assert_eq!(remove_unused_commands(unsupported), None);
        }
    }
}
