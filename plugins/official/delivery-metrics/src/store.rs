//! Where deployments live: shards per UTC month, each split into parts of at most 256 KiB, under
//! keys `d/<YYYY-MM>/<nnn>`, plus an index under [`INDEX`].
//!
//! coolify-rs limits a value to 1 MiB and a team's data to 10 MiB. Parts stay at a quarter of the first,
//! and all parts together under 6 MiB ([`Limits::DEFAULT`]), so neither limit is ever reached: when the cap would be
//! passed, the oldest deployments go first. Retention deletes deployments older than the team's setting
//! every time one is recorded. A write that fails anyway prunes the oldest part and retries, and if it
//! still fails the deployment waits in the inbox for the next event, and is never the event's failure.
//!
//! Each deployment also passes through an inbox key of its own, `q/...`, so concurrent events can't
//! overwrite each other's deployments ([`record`]).

use serde::{Deserialize, Serialize};

use crate::record::Deployment;
use crate::time::{self, DAY};

/// How large parts and all parts together may grow.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The largest part, in bytes.
    pub part_max: usize,
    /// All parts together stay under this many bytes.
    pub data_cap: u64,
}

impl Limits {
    /// Parts of a quarter of the 1 MiB value limit, and 6 MiB of deployments, leaving room under the
    /// 10 MiB team quota for the index, for keys (which count too) and for the part being written.
    pub const DEFAULT: Limits = Limits {
        part_max: 256 * 1024,
        data_cap: 6 * 1024 * 1024,
    };
}

const _: () = assert!(Limits::DEFAULT.part_max <= 1024 * 1024 / 2);
const _: () =
    assert!(Limits::DEFAULT.data_cap + 2 * Limits::DEFAULT.part_max as u64 <= 8 * 1024 * 1024);
pub const INDEX: &str = "index";
const PREFIX: &str = "d/";
const INBOX: &str = "q/";
/// Retention bounds, in days.
pub const RETENTION_MIN: u32 = 7;
pub const RETENTION_MAX: u32 = 730;
pub const RETENTION_DEFAULT: u32 = 180;

/// The plugin's key-value storage: `coolify_rs_plugin::storage` in the component, a map in tests.
pub trait Kv {
    fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn set(&mut self, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&mut self, key: &str) -> Result<(), String>;
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub key: String,
    pub bytes: u64,
    pub count: u32,
    /// The oldest and newest deployment in the part.
    pub first: i64,
    pub last: i64,
}

impl Index {
    pub fn bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.bytes).sum()
    }
    pub fn count(&self) -> u64 {
        self.parts.iter().map(|p| u64::from(p.count)).sum()
    }
    pub fn oldest(&self) -> Option<i64> {
        self.parts.iter().map(|p| p.first).min()
    }
    fn sort(&mut self) {
        self.parts.sort_by(|a, b| a.key.cmp(&b.key));
    }
}

/// The retention setting: a number of days, clamped to the bounds; the default when unset or invalid.
pub fn retention_days(setting: Option<&str>) -> u32 {
    setting
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map_or(RETENTION_DEFAULT, |d| d.clamp(RETENTION_MIN, RETENTION_MAX))
}

fn part_key(month: &str, n: u32) -> String {
    format!("{PREFIX}{month}/{n:03}")
}

/// `d/2026-11/004` → ("2026-11", 4).
fn parse_key(key: &str) -> Option<(&str, u32)> {
    let rest = key.strip_prefix(PREFIX)?;
    let (month, n) = rest.split_once('/')?;
    (month.len() == 7 && n.len() == 3).then_some(())?;
    Some((month, n.parse().ok()?))
}

fn read_part(kv: &mut dyn Kv, key: &str) -> Result<Vec<Deployment>, String> {
    Ok(kv
        .get(key)?
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default())
}

fn summarize(key: String, deps: &[Deployment], bytes: usize) -> Part {
    Part {
        key,
        bytes: bytes as u64,
        count: deps.len() as u32,
        first: deps.iter().map(|d| d.at).min().unwrap_or(0),
        last: deps.iter().map(|d| d.at).max().unwrap_or(0),
    }
}

/// The index, reconciled with the keys actually stored: a part written without its index entry (the call
/// ended in between) is added, an entry without its part is dropped, and a missing or unreadable index is
/// rebuilt from the parts.
pub fn load_index(kv: &mut dyn Kv) -> Result<Index, String> {
    let mut index: Index = kv
        .get(INDEX)?
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    let keys = kv.keys(PREFIX)?;
    index.parts.retain(|p| keys.contains(&p.key));
    for key in keys {
        if index.parts.iter().any(|p| p.key == key) {
            continue;
        }
        if parse_key(&key).is_none() {
            kv.delete(&key)?;
            continue;
        }
        let raw = kv.get(&key)?.unwrap_or_default();
        match serde_json::from_slice::<Vec<Deployment>>(&raw) {
            Ok(deps) if !deps.is_empty() => index.parts.push(summarize(key, &deps, raw.len())),
            _ => kv.delete(&key)?,
        }
    }
    index.sort();
    Ok(index)
}

#[derive(Debug, PartialEq)]
pub enum Recorded {
    Stored,
    /// Already recorded: the same deployment with the same outcome.
    Duplicate,
    /// Older than the retention period.
    Expired,
    /// Storage refused it even after pruning; the reason is logged, the event still succeeds.
    Dropped(String),
}

/// How long a deployment stays in the inbox once it is in a part: far longer than any call runs (events
/// 5 s, requests 10 s), so every call that read the part without it has finished.
pub const INBOX_HOLD: i64 = 60;

/// The inbox key of a deployment: unique per deployment and outcome, or per call when the event had no
/// deployment uuid.
pub fn inbox_key(dep: &Deployment, nonce: u64) -> String {
    let id = if dep.deployment.is_empty() {
        format!("{nonce:016x}")
    } else {
        dep.deployment.clone()
    };
    let outcome = if dep.succeeded { "ok" } else { "failed" };
    format!("{INBOX}{:011}-{id}-{outcome}", dep.at.max(0))
}

/// The same deployment: by uuid and outcome, or, without a uuid, by time and outcome.
fn same(a: &Deployment, b: &Deployment) -> bool {
    a.app == b.app
        && a.succeeded == b.succeeded
        && a.deployment == b.deployment
        && (!a.deployment.is_empty() || a.at == b.at)
}

/// A part to write: its number, its deployments, the inbox entries it takes in (key and time), and
/// whether the deployment being recorded is one of them.
struct PartWrite {
    n: u32,
    deps: Vec<Deployment>,
    taken: Vec<(String, i64)>,
    has_dep: bool,
}

/// Writes a part; when storage refuses, deletes the oldest other part to make room and tries again, up to
/// three times.
fn write_part(kv: &mut dyn Kv, index: &mut Index, key: &str, body: &[u8]) -> Result<(), String> {
    let mut attempt = 0;
    while let Err(e) = kv.set(key, body) {
        attempt += 1;
        match index.parts.iter().position(|p| p.key != key) {
            Some(i) if attempt <= 3 => {
                let gone = index.parts.remove(i);
                kv.delete(&gone.key)?;
            }
            _ => return Err(e),
        }
    }
    Ok(())
}

/// Records one deployment, then applies retention and the size cap. Reads and writes the inbox, the
/// newest part of the deployment's month, the oldest parts and the index.
///
/// Events of a team can run at the same time, and a part is read, changed and written back, so one call
/// could overwrite another's deployment. Each deployment is therefore first written to an inbox key of its
/// own (`q/...`), which no other call writes, and every call merges the whole inbox into the parts,
/// reading the inbox before the parts. A deployment leaves the inbox only once it is in a part and
/// [`INBOX_HOLD`] has passed, so a call that wrote a part without it has long finished, and every later
/// call reads it from the part or the inbox. `nonce` makes the inbox key of a deployment without a uuid
/// unique.
pub fn record(
    kv: &mut dyn Kv,
    dep: &Deployment,
    now: i64,
    retention: u32,
    limits: Limits,
    nonce: u64,
) -> Result<Recorded, String> {
    let cutoff = now - i64::from(retention) * DAY;
    if dep.at < cutoff {
        return Ok(Recorded::Expired);
    }
    // Failing to queue it only loses the protection against concurrent calls: it is merged below anyway.
    let body = serde_json::to_vec(dep).map_err(|e| e.to_string())?;
    let _ = kv.set(&inbox_key(dep, nonce), &body);

    let mut pending: Vec<(Option<String>, Deployment)> = Vec::new();
    for key in kv.keys(INBOX)? {
        match kv
            .get(&key)?
            .and_then(|v| serde_json::from_slice::<Deployment>(&v).ok())
        {
            Some(d) if d.at >= cutoff => pending.push((Some(key), d)),
            _ => kv.delete(&key)?,
        }
    }
    if !pending.iter().any(|(_, d)| same(d, dep)) {
        pending.push((None, dep.clone()));
    }
    pending.sort_by_key(|(_, d)| d.at);
    let mut months: Vec<String> = pending.iter().map(|(_, d)| time::month(d.at)).collect();
    months.dedup();

    let mut index = load_index(kv)?;
    let mut outcome = Recorded::Duplicate;
    // Inbox entries that are in a part now, with their time.
    let mut placed: Vec<(String, i64)> = Vec::new();
    for month in months {
        let items: Vec<&(Option<String>, Deployment)> = pending
            .iter()
            .filter(|(_, d)| time::month(d.at) == month)
            .collect();
        let earliest = items.first().map_or(now, |(_, d)| d.at);
        let mut parts: Vec<(u32, i64)> = index
            .parts
            .iter()
            .filter_map(|p| {
                parse_key(&p.key)
                    .filter(|(m, _)| *m == month)
                    .map(|(_, n)| (n, p.last))
            })
            .collect();
        parts.sort_unstable();
        let latest = parts.pop().map(|(n, _)| n);
        // Older parts of the month can hold these deployments only if they reach their time.
        let mut existing = Vec::new();
        for (n, last) in parts {
            if last >= earliest {
                existing.extend(read_part(kv, &part_key(&month, n))?);
            }
        }
        let mut n = latest.unwrap_or(0);
        let mut deps = match latest {
            Some(n) => read_part(kv, &part_key(&month, n))?,
            None => Vec::new(),
        };
        let mut len = serde_json::to_vec(&deps).map_err(|e| e.to_string())?.len();
        let mut writes: Vec<PartWrite> = Vec::new();
        let mut taken = Vec::new();
        let mut has_dep = false;
        for (key, d) in items {
            if existing.iter().chain(deps.iter()).any(|e| same(e, d)) {
                placed.extend(key.clone().map(|k| (k, d.at)));
                continue;
            }
            let size = serde_json::to_vec(d).map_err(|e| e.to_string())?.len();
            if !deps.is_empty() && len + size + 1 > limits.part_max {
                writes.push(PartWrite {
                    n,
                    deps: std::mem::take(&mut deps),
                    taken: std::mem::take(&mut taken),
                    has_dep,
                });
                n += 1;
                len = 2;
                has_dep = false;
            }
            len += size + usize::from(!deps.is_empty());
            deps.push(d.clone());
            taken.extend(key.clone().map(|k| (k, d.at)));
            has_dep |= same(d, dep);
        }
        if !taken.is_empty() || has_dep {
            writes.push(PartWrite {
                n,
                deps,
                taken,
                has_dep,
            });
        }
        for PartWrite {
            n,
            deps,
            taken,
            has_dep,
        } in writes
        {
            let key = part_key(&month, n);
            let body = serde_json::to_vec(&deps).map_err(|e| e.to_string())?;
            match write_part(kv, &mut index, &key, &body) {
                Ok(()) => {
                    index.parts.retain(|p| p.key != key);
                    index.parts.push(summarize(key, &deps, body.len()));
                    index.sort();
                    placed.extend(taken);
                    if has_dep {
                        outcome = Recorded::Stored;
                    }
                }
                Err(e) if has_dep => outcome = Recorded::Dropped(e),
                Err(_) => {}
            }
        }
    }
    for (key, at) in placed {
        if now - at >= INBOX_HOLD {
            kv.delete(&key)?;
        }
    }
    prune(kv, &mut index, cutoff, limits)?;
    // Failing to save the index loses no deployment: `load_index` adds parts it doesn't know, and the
    // entry of a part is summarized again whenever the part is read or written.
    let _ = save_index(kv, &index);
    Ok(outcome)
}

/// Deletes deployments before `cutoff`, then the oldest parts while all parts pass the cap.
pub fn prune(
    kv: &mut dyn Kv,
    index: &mut Index,
    cutoff: i64,
    limits: Limits,
) -> Result<(), String> {
    let mut kept = Vec::with_capacity(index.parts.len());
    for part in std::mem::take(&mut index.parts) {
        if part.last < cutoff {
            kv.delete(&part.key)?;
        } else if part.first < cutoff {
            let deps: Vec<Deployment> = read_part(kv, &part.key)?
                .into_iter()
                .filter(|d| d.at >= cutoff)
                .collect();
            if deps.is_empty() {
                kv.delete(&part.key)?;
            } else {
                let body = serde_json::to_vec(&deps).map_err(|e| e.to_string())?;
                kv.set(&part.key, &body)?;
                kept.push(summarize(part.key, &deps, body.len()));
            }
        } else {
            kept.push(part);
        }
    }
    index.parts = kept;
    while index.bytes() > limits.data_cap && index.parts.len() > 1 {
        let gone = index.parts.remove(0);
        kv.delete(&gone.key)?;
    }
    Ok(())
}

fn save_index(kv: &mut dyn Kv, index: &Index) -> Result<(), String> {
    let body = serde_json::to_vec(index).map_err(|e| e.to_string())?;
    kv.set(INDEX, &body)
}

/// Every stored deployment at or after `since`, oldest first, and the index.
pub fn load_since(kv: &mut dyn Kv, since: i64) -> Result<(Vec<Deployment>, Index), String> {
    let mut index = load_index(kv)?;
    let mut out = Vec::new();
    for part in index.parts.iter_mut().filter(|p| p.last >= since) {
        let raw = kv.get(&part.key)?.unwrap_or_default();
        let deps: Vec<Deployment> = serde_json::from_slice(&raw).unwrap_or_default();
        // A write may be between its part and the index: describe the part as it was read.
        *part = summarize(std::mem::take(&mut part.key), &deps, raw.len());
        out.extend(deps.into_iter().filter(|d| d.at >= since));
    }
    out.sort_by_key(|d| d.at);
    Ok((out, index))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The host's storage rules: 256-byte keys, 1 MiB values, a 10 MiB quota counting keys and values.
    #[derive(Default)]
    pub struct Memory {
        pub data: BTreeMap<String, Vec<u8>>,
        pub quota: usize,
        /// Refuse this many writes of parts.
        pub fail_sets: u32,
    }

    impl Memory {
        pub fn new() -> Self {
            Self {
                quota: 10 * 1024 * 1024,
                ..Self::default()
            }
        }
        pub fn used(&self) -> usize {
            self.data.iter().map(|(k, v)| k.len() + v.len()).sum()
        }
    }

    impl Kv for Memory {
        fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.data.get(key).cloned())
        }
        fn set(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
            if self.fail_sets > 0 && key.starts_with(PREFIX) {
                self.fail_sets -= 1;
                return Err("storage quota exceeded".into());
            }
            assert!(key.len() <= 256, "key too long");
            assert!(value.len() <= 1024 * 1024, "value too large");
            let old = self.data.get(key).map_or(0, |v| key.len() + v.len());
            if self.used() - old + key.len() + value.len() > self.quota {
                return Err("storage quota exceeded".into());
            }
            self.data.insert(key.into(), value.to_vec());
            Ok(())
        }
        fn delete(&mut self, key: &str) -> Result<(), String> {
            self.data.remove(key);
            Ok(())
        }
        fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
            Ok(self
                .data
                .keys()
                .filter(|k| k.starts_with(prefix))
                .take(1000)
                .cloned()
                .collect())
        }
    }

    pub const NOW: i64 = 1_793_966_400; // 2026-11-06T12:00:00Z
    const L: Limits = Limits::DEFAULT;

    pub fn dep(at: i64, app: &str, ok: bool) -> Deployment {
        Deployment {
            at,
            app: app.into(),
            app_name: format!("{app}-name"),
            project: "Shop".into(),
            environment: "production".into(),
            succeeded: ok,
            pull_request: 0,
            deployment: format!("dep-{at}-{app}-{ok}"),
            commit: String::new(),
            commit_at: None,
        }
    }

    #[test]
    fn records_by_month_and_reads_back_in_order() {
        let mut kv = Memory::new();
        let oct = NOW - 20 * DAY; // 2026-10-17
        assert_eq!(
            record(&mut kv, &dep(NOW, "a", true), NOW, 180, L, 0),
            Ok(Recorded::Stored)
        );
        assert_eq!(
            record(&mut kv, &dep(oct, "a", false), NOW, 180, L, 0),
            Ok(Recorded::Stored)
        );
        assert_eq!(
            record(&mut kv, &dep(NOW, "a", true), NOW, 180, L, 0),
            Ok(Recorded::Duplicate)
        );
        // The October deployment left the inbox at once; today's waits there for a minute.
        let keys: Vec<&String> = kv.data.keys().collect();
        let today = format!("q/0{NOW}-dep-{NOW}-a-true-ok");
        assert_eq!(keys, ["d/2026-10/000", "d/2026-11/000", "index", &today]);
        assert!(today.len() <= 256);
        record(&mut kv, &dep(NOW + 60, "b", true), NOW + 60, 180, L, 0).unwrap();
        assert!(!kv.data.contains_key(&today));
        let (deps, index) = load_since(&mut kv, 0).unwrap();
        assert_eq!(
            deps.iter().map(|d| d.at).collect::<Vec<_>>(),
            [oct, NOW, NOW + 60]
        );
        assert_eq!(index.count(), 3);
        let (recent, _) = load_since(&mut kv, NOW - DAY).unwrap();
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn retention_prunes_on_write() {
        let mut kv = Memory::new();
        let old = NOW - 200 * DAY;
        assert_eq!(
            record(&mut kv, &dep(old, "a", true), NOW, 180, L, 0),
            Ok(Recorded::Expired)
        );
        // Recorded while they were recent, then the clock moves on.
        record(&mut kv, &dep(old, "a", true), old, 180, L, 0).unwrap();
        record(
            &mut kv,
            &dep(old + DAY * 15, "a", true),
            old + DAY * 15,
            180,
            L,
            0,
        )
        .unwrap();
        assert_eq!(load_since(&mut kv, 0).unwrap().0.len(), 2);
        record(&mut kv, &dep(NOW, "b", true), NOW, 180, L, 0).unwrap();
        let (deps, index) = load_since(&mut kv, 0).unwrap();
        assert_eq!(
            deps.iter().map(|d| d.app.as_str()).collect::<Vec<_>>(),
            ["b"]
        );
        assert_eq!(index.parts.len(), 1);
        let parts: Vec<&String> = kv.data.keys().filter(|k| k.starts_with(PREFIX)).collect();
        assert_eq!(
            parts,
            ["d/2026-11/000"],
            "the old months' parts are deleted"
        );

        // A part that straddles the cutoff keeps only its recent deployments.
        let mut kv = Memory::new();
        let start = NOW - 10 * DAY; // 2026-10-27
        for i in 0..5 {
            record(
                &mut kv,
                &dep(start + i * DAY, "a", true),
                start + i * DAY,
                180,
                L,
                0,
            )
            .unwrap();
        }
        let now = start + 7 * DAY + 1;
        record(&mut kv, &dep(start + 5 * DAY, "a", true), now, 7, L, 0).unwrap();
        let (deps, _) = load_since(&mut kv, 0).unwrap();
        assert_eq!(deps.len(), 5, "the first deployment fell out of 7 days");
        assert_eq!(deps[0].at, start + DAY);
    }

    #[test]
    fn parts_split_and_the_cap_keeps_the_team_under_its_quota() {
        // Small limits, so the test is quick: 4 KiB parts, 32 KiB in all.
        let small = Limits {
            part_max: 4096,
            data_cap: 32 * 1024,
        };
        let mut kv = Memory::new();
        let mut at = NOW - 25 * DAY;
        for n in 0..2_000 {
            let mut d = dep(at, &format!("app-{}", n % 50), n % 7 != 0);
            d.commit = "a".repeat(40);
            assert_eq!(record(&mut kv, &d, at, 180, small, 0), Ok(Recorded::Stored));
            at += 600;
        }
        let index = load_index(&mut kv).unwrap();
        assert!(index.parts.len() > 2);
        assert!(
            index
                .parts
                .iter()
                .all(|p| p.bytes as usize <= small.part_max)
        );
        assert!(index.bytes() <= small.data_cap);
        let stored: usize = kv
            .data
            .iter()
            .filter(|(k, _)| k.starts_with(PREFIX))
            .map(|(_, v)| v.len())
            .sum();
        assert_eq!(stored as u64, index.bytes(), "the index matches the parts");
        // The newest deployment is always kept; the oldest went first.
        let (deps, _) = load_since(&mut kv, 0).unwrap();
        assert_eq!(deps.last().unwrap().at, at - 600);
        assert!(deps.first().unwrap().at > NOW - 25 * DAY);
        assert!(deps.len() < 2_000);

        // The real limits: a busy month splits into parts and stays under the cap and the quota.
        let mut kv = Memory::new();
        let mut at = NOW - 20 * DAY;
        let mut d = dep(at, "app", true);
        d.app_name = "x".repeat(120);
        d.project = "p".repeat(120);
        d.environment = "e".repeat(120);
        d.commit = "a".repeat(40);
        let one = serde_json::to_vec(&d).unwrap().len();
        let needed = (Limits::DEFAULT.data_cap as usize + 2 * Limits::DEFAULT.part_max) / one;
        // Write whole parts at once rather than one deployment at a time, then record on top of them.
        let per_part = Limits::DEFAULT.part_max / (one + 1) - 1;
        let mut n = 0;
        while n < needed {
            let batch: Vec<Deployment> = (0..per_part)
                .map(|i| Deployment {
                    at: at + i as i64,
                    deployment: format!("d{n}-{i}"),
                    ..d.clone()
                })
                .collect();
            kv.set(
                &part_key("2026-10", (n / per_part) as u32),
                &serde_json::to_vec(&batch).unwrap(),
            )
            .unwrap();
            n += per_part;
            at += per_part as i64;
        }
        for i in 0..50 {
            let next = Deployment {
                at: at + i,
                deployment: format!("x{i}"),
                ..d.clone()
            };
            assert_eq!(
                record(&mut kv, &next, at + i, 180, L, 0),
                Ok(Recorded::Stored)
            );
        }
        let index = load_index(&mut kv).unwrap();
        assert!(
            index.bytes() <= Limits::DEFAULT.data_cap,
            "{}",
            index.bytes()
        );
        assert!(
            index
                .parts
                .iter()
                .all(|p| p.bytes as usize <= Limits::DEFAULT.part_max)
        );
        assert!(kv.used() < 8 * 1024 * 1024, "{}", kv.used());
        assert_eq!(
            load_since(&mut kv, 0).unwrap().0.last().unwrap().deployment,
            "x49"
        );
    }

    #[test]
    fn a_full_store_drops_deployments_not_events() {
        let mut kv = Memory::new();
        for i in 0..3 {
            record(
                &mut kv,
                &dep(NOW - (40 - i) * DAY, "a", true),
                NOW,
                180,
                L,
                0,
            )
            .unwrap();
        }
        // The first write fails: the oldest part makes room and the write is retried.
        kv.fail_sets = 1;
        assert_eq!(
            record(&mut kv, &dep(NOW, "a", false), NOW, 180, L, 0),
            Ok(Recorded::Stored)
        );
        let (deps, _) = load_since(&mut kv, 0).unwrap();
        assert_eq!(deps.len(), 1, "the September part made room");
        // Nothing helps: the deployment isn't in a part, the index stays consistent, and the deployment
        // waits in the inbox for the next event.
        kv.fail_sets = 4;
        let r = record(&mut kv, &dep(NOW + 1, "b", true), NOW + 1, 180, L, 0).unwrap();
        assert!(matches!(r, Recorded::Dropped(_)), "{r:?}");
        let index = load_index(&mut kv).unwrap();
        assert_eq!(kv.keys(PREFIX).unwrap().len(), index.parts.len());
        assert_eq!(
            index.count(),
            load_since(&mut kv, 0).unwrap().0.len() as u64
        );
        kv.fail_sets = 0;
        record(&mut kv, &dep(NOW + 2, "c", true), NOW + 2, 180, L, 0).unwrap();
        let (deps, _) = load_since(&mut kv, 0).unwrap();
        let apps: Vec<&str> = deps.iter().map(|d| d.app.as_str()).collect();
        assert_eq!(apps, ["a", "b", "c"]);
    }

    #[test]
    fn concurrent_events_lose_no_deployment() {
        let mut kv = Memory::new();
        let a = dep(NOW, "a", true);
        record(&mut kv, &a, NOW, 180, L, 0).unwrap();
        // A call that read the part and the inbox before `a` arrived writes the part back without it.
        let b = dep(NOW + 1, "b", false);
        kv.set(&inbox_key(&b, 0), &serde_json::to_vec(&b).unwrap())
            .unwrap();
        kv.set("d/2026-11/000", &serde_json::to_vec(&[&b]).unwrap())
            .unwrap();
        assert_eq!(
            load_since(&mut kv, 0).unwrap().0.len(),
            1,
            "a was overwritten"
        );
        // The next event merges the inbox back in.
        record(&mut kv, &dep(NOW + 5, "c", true), NOW + 5, 180, L, 0).unwrap();
        let (deps, _) = load_since(&mut kv, 0).unwrap();
        let apps: Vec<&str> = deps.iter().map(|d| d.app.as_str()).collect();
        assert_eq!(apps, ["a", "b", "c"]);
        assert_eq!(kv.keys(INBOX).unwrap().len(), 3, "held for a minute");
        // A minute later the inbox lets go of what is in the part.
        let d = dep(NOW + 120, "d", true);
        record(&mut kv, &d, NOW + 120, 180, L, 0).unwrap();
        assert_eq!(kv.keys(INBOX).unwrap(), [inbox_key(&d, 0)]);
        assert_eq!(load_since(&mut kv, 0).unwrap().0.len(), 4);

        // Deployments without a uuid get a key of their own per call.
        let mut x = dep(NOW, "x", true);
        x.deployment.clear();
        assert_ne!(inbox_key(&x, 1), inbox_key(&x, 2));
    }

    #[test]
    fn the_index_is_rebuilt_from_the_parts() {
        let mut kv = Memory::new();
        record(&mut kv, &dep(NOW - 20 * DAY, "a", true), NOW, 180, L, 0).unwrap();
        record(&mut kv, &dep(NOW, "a", false), NOW, 180, L, 0).unwrap();
        kv.data.insert(INDEX.into(), b"not json".to_vec());
        kv.data.insert("d/junk".into(), b"[]".to_vec());
        kv.data.insert("d/2026-09/000".into(), b"oops".to_vec());
        let index = load_index(&mut kv).unwrap();
        let keys: Vec<&str> = index.parts.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["d/2026-10/000", "d/2026-11/000"]);
        assert!(!kv.data.contains_key("d/junk"));
        assert!(!kv.data.contains_key("d/2026-09/000"));
        assert_eq!(index.count(), 2);
    }

    #[test]
    fn retention_setting_is_clamped() {
        assert_eq!(retention_days(None), 180);
        assert_eq!(retention_days(Some("90")), 90);
        assert_eq!(retention_days(Some(" 3 ")), 7);
        assert_eq!(retention_days(Some("100000")), 730);
        assert_eq!(retention_days(Some("-1")), 180);
        assert_eq!(retention_days(Some("soon")), 180);
    }
}
