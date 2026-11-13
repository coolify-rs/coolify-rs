//! What upstream keeps in the Laravel cache for image scans: the progress and the last error of
//! a resource, the marks of waiting deploy gates and prepare jobs, and the server lock
//! (`Cache::lock`). coolify-rs runs its jobs in one process (ADR 0009), so the values live in
//! memory: they are gone after a restart, like a flushed cache.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

type Entries = Mutex<HashMap<String, (Instant, Value)>>;

static ENTRIES: LazyLock<Entries> = LazyLock::new(Entries::default);
static LOCKS: LazyLock<Mutex<HashMap<String, (Instant, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn entries() -> std::sync::MutexGuard<'static, HashMap<String, (Instant, Value)>> {
    ENTRIES.lock().unwrap_or_else(|p| p.into_inner())
}

fn locks() -> std::sync::MutexGuard<'static, HashMap<String, (Instant, String)>> {
    LOCKS.lock().unwrap_or_else(|p| p.into_inner())
}

/// `Cache::put($key, $value, $seconds)`.
pub fn put(key: &str, value: Value, ttl: Duration) {
    entries().insert(key.to_owned(), (Instant::now() + ttl, value));
}

/// `Cache::get($key)`.
pub fn get(key: &str) -> Option<Value> {
    let mut map = entries();
    match map.get(key) {
        Some((until, value)) if *until > Instant::now() => Some(value.clone()),
        Some(_) => {
            map.remove(key);
            None
        }
        None => None,
    }
}

/// `Cache::has($key)`.
pub fn has(key: &str) -> bool {
    get(key).is_some()
}

/// `Cache::forget($key)`.
pub fn forget(key: &str) {
    entries().remove(key);
}

/// `Cache::add($key, $value, $seconds)`: stores the value unless the key is set. Returns whether
/// it stored it.
pub fn add(key: &str, value: Value, ttl: Duration) -> bool {
    let mut map = entries();
    let now = Instant::now();
    if map.get(key).is_some_and(|(until, _)| *until > now) {
        return false;
    }
    map.insert(key.to_owned(), (now + ttl, value));
    true
}

/// `Cache::increment($key)` of a counter that ends after `ttl` (set when the counter starts).
pub fn increment(key: &str, ttl: Duration) -> i64 {
    let mut map = entries();
    let now = Instant::now();
    let (until, current) = match map.get(key) {
        Some((until, value)) if *until > now => (*until, value.as_i64().unwrap_or(0)),
        _ => (now + ttl, 0),
    };
    map.insert(key.to_owned(), (until, Value::from(current + 1)));
    current + 1
}

/// `Cache::lock($name, $seconds, $owner)`: a lock that expires on its own and that only its
/// owner releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLock {
    name: String,
    ttl: Duration,
    owner: String,
}

impl CacheLock {
    pub fn new(name: impl Into<String>, ttl: Duration) -> Self {
        Self {
            name: name.into(),
            ttl,
            owner: coolify_domain::new_public_id(),
        }
    }

    /// `$lock->get()`: takes the lock when it is free. Never waits.
    pub fn get(&self) -> bool {
        let mut map = locks();
        let now = Instant::now();
        if map.get(&self.name).is_some_and(|(until, _)| *until > now) {
            return false;
        }
        map.insert(self.name.clone(), (now + self.ttl, self.owner.clone()));
        true
    }

    /// `$lock->block($seconds)`: waits up to `seconds` for the lock. False for
    /// `LockTimeoutException`.
    pub async fn block(&self, seconds: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if self.get() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// `$lock->release()`: frees the lock when this owner still holds it.
    pub fn release(&self) {
        let mut map = locks();
        if map
            .get(&self.name)
            .is_some_and(|(_, owner)| *owner == self.owner)
        {
            map.remove(&self.name);
        }
    }

    /// Whether anyone holds the lock now.
    pub fn is_held(name: &str) -> bool {
        locks()
            .get(name)
            .is_some_and(|(until, _)| *until > Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn key(name: &str) -> String {
        format!("test:{name}:{}", coolify_domain::new_public_id())
    }

    #[test]
    fn values_expire() {
        let k = key("value");
        assert_eq!(get(&k), None);
        put(&k, json!({"stage": "sbom"}), Duration::from_secs(60));
        assert_eq!(get(&k), Some(json!({"stage": "sbom"})));
        assert!(has(&k));
        forget(&k);
        assert!(!has(&k));
        put(&k, json!(1), Duration::ZERO);
        assert_eq!(get(&k), None);
    }

    #[test]
    fn add_keeps_the_first_value() {
        let k = key("add");
        assert!(add(&k, json!("a"), Duration::from_secs(60)));
        assert!(!add(&k, json!("b"), Duration::from_secs(60)));
        assert_eq!(get(&k), Some(json!("a")));
        let counter = key("counter");
        assert_eq!(increment(&counter, Duration::from_secs(60)), 1);
        assert_eq!(increment(&counter, Duration::from_secs(60)), 2);
    }

    #[test]
    fn a_lock_is_held_by_one_owner() {
        let name = key("lock");
        let first = CacheLock::new(name.clone(), Duration::from_secs(60));
        let second = CacheLock::new(name.clone(), Duration::from_secs(60));
        assert!(first.get());
        assert!(!second.get());
        assert!(CacheLock::is_held(&name));
        // Only the owner releases.
        second.release();
        assert!(!second.get());
        first.release();
        assert!(!CacheLock::is_held(&name));
        assert!(second.get());
        second.release();
    }

    #[test]
    fn an_expired_lock_is_free() {
        let name = key("expired");
        let first = CacheLock::new(name.clone(), Duration::ZERO);
        assert!(first.get());
        let second = CacheLock::new(name, Duration::from_secs(60));
        assert!(second.get());
        // The first owner lost the lock and does not release the second one's.
        first.release();
        assert!(!first.get());
        second.release();
    }

    #[tokio::test]
    async fn block_waits_for_the_lock() {
        let name = key("block");
        let first = CacheLock::new(name.clone(), Duration::from_secs(60));
        let second = CacheLock::new(name, Duration::from_secs(60));
        assert!(first.get());
        assert!(!second.block(0).await);
        let release = first.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            release.release();
        });
        assert!(second.block(5).await);
        second.release();
    }
}
