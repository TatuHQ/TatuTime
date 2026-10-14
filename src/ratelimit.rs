//! In-process rate limiting for authentication endpoints (#47).
//!
//! A fixed-window failure counter per key (e.g. `login:<email>` or
//! `bootstrap`). Successful auth clears the key; repeated failures within the
//! window lock it out. In-memory and per-process — a multi-process deployment
//! shares the data dir but not this map, which is acceptable for a small
//! self-hosted tool (a hardening follow-up could back it with the filesystem).
//!
//! The map is bounded (A12): each write prunes windows that have fully elapsed
//! and enforces a hard key cap, evicting the oldest key when the cap is
//! exceeded. Without this, a flood of attempts with random identities on the
//! public login/bootstrap endpoints would grow the map without bound (a
//! memory-exhaustion DoS). Eviction fails *open* for the evicted key — its
//! counter is forgotten, so that identity gets a fresh window — but that is the
//! deliberate trade-off that keeps memory bounded.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Hard cap on tracked keys. 10k entries keep the map at a few megabytes even
/// under a random-identity flood (A12).
const DEFAULT_MAX_KEYS: usize = 10_000;

#[derive(Debug)]
struct Bucket {
    failures: u32,
    window_start: Option<Instant>,
    /// Monotonic insertion order; the bucket with the smallest `order` is the
    /// oldest and is evicted first when the map exceeds its cap.
    order: u64,
}

pub struct RateLimiter {
    inner: Mutex<HashMap<String, Bucket>>,
    max_failures: u32,
    window: Duration,
    max_keys: usize,
    /// Bumped for every tracked key to stamp [`Bucket::order`].
    next_order: AtomicU64,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self::with_max_keys(max_failures, window, DEFAULT_MAX_KEYS)
    }

    /// Like [`RateLimiter::new`] but with an explicit key cap, so callers (and
    /// tests) can bound the map tightly.
    pub fn with_max_keys(max_failures: u32, window: Duration, max_keys: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_failures,
            window,
            max_keys,
            next_order: AtomicU64::new(0),
        }
    }

    /// Acquire the map guard, recovering from a poisoned mutex instead of
    /// panicking: the map only holds counters, so a panic mid-update leaves it
    /// usable (project convention, cf. `revoke.rs`, `audit.rs`).
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Bucket>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// True when the key is currently locked out (too many recent failures).
    pub fn is_locked(&self, key: &str) -> bool {
        let map = self.lock();
        match map.get(key) {
            Some(b) => {
                let within = b.window_start.is_some_and(|t| t.elapsed() < self.window);
                within && b.failures >= self.max_failures
            }
            None => false,
        }
    }

    /// Record a failed attempt for the key, starting a fresh window if the old
    /// one elapsed.
    pub fn record_failure(&self, key: &str) {
        let mut map = self.lock();
        let now = Instant::now();
        // Prune fully-elapsed windows so stale keys cannot accumulate.
        map.retain(|_, b| b.window_start.is_some_and(|t| t.elapsed() < self.window));

        let order = self.next_order.fetch_add(1, Ordering::Relaxed);
        let b = map.entry(key.to_string()).or_insert_with(|| Bucket {
            failures: 0,
            window_start: None,
            order,
        });
        if b.window_start.is_none_or(|t| t.elapsed() >= self.window) {
            b.window_start = Some(now);
            b.failures = 0;
        }
        b.failures += 1;

        // Hard cap: a live-window flood outruns pruning, so evict the oldest
        // key(s). This fails open for the evicted key but bounds memory (A12).
        while map.len() > self.max_keys {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, b)| b.order)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
    }

    /// Clear the key on a successful attempt.
    pub fn reset(&self, key: &str) {
        let mut map = self.lock();
        map.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_max_failures_and_resets() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        let key = "login:a@b.co";
        assert!(!rl.is_locked(key));
        rl.record_failure(key);
        rl.record_failure(key);
        assert!(!rl.is_locked(key), "2 of 3 failures");
        rl.record_failure(key);
        assert!(rl.is_locked(key), "3rd failure locks");
        rl.reset(key);
        assert!(!rl.is_locked(key), "success clears");
    }

    #[test]
    fn expired_windows_are_pruned_so_the_map_stays_small() {
        // A zero-length window means every stored window is stale the moment it
        // is written, so each write prunes the previous key: a flood of distinct
        // identities cannot grow the map (deterministic, no sleeping).
        let rl = RateLimiter::new(3, Duration::ZERO);
        for i in 0..1_000 {
            rl.record_failure(&format!("login:stale{i}@example.test"));
        }
        assert_eq!(
            rl.lock().len(),
            1,
            "stale keys are pruned on write instead of accumulating"
        );
    }

    #[test]
    fn cap_is_enforced_by_evicting_the_oldest_key() {
        // A long window keeps every key live, so only the cap bounds the map.
        // The bound is derived from the cap and the hit count, not from timing.
        let cap = 3;
        let rl = RateLimiter::with_max_keys(3, Duration::from_secs(60), cap);
        let hits = cap * 4;
        for i in 0..hits {
            rl.record_failure(&format!("login:flood{i}@example.test"));
            assert!(
                rl.lock().len() <= cap,
                "map stays within the cap after {i} hits"
            );
        }
        let map = rl.lock();
        assert_eq!(map.len(), cap);
        // The oldest keys were evicted; the most recent survive.
        assert!(
            !map.contains_key("login:flood0@example.test"),
            "oldest evicted"
        );
        assert!(
            map.contains_key(&format!("login:flood{}@example.test", hits - 1)),
            "newest retained"
        );
    }

    #[test]
    fn poisoned_lock_is_recovered_not_panicked() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        // Poison the mutex: panic while the guard is held.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = rl.lock();
            panic!("poison the rate-limiter mutex");
        }));
        assert!(poisoned.is_err(), "the poisoning closure panicked");
        assert!(rl.inner.is_poisoned(), "mutex is poisoned");

        // Every operation must recover the guard rather than panicking.
        rl.record_failure("login:a@b.co");
        rl.record_failure("login:a@b.co");
        rl.record_failure("login:a@b.co");
        assert!(rl.is_locked("login:a@b.co"), "still counts after recovery");
        rl.reset("login:a@b.co");
        assert!(!rl.is_locked("login:a@b.co"), "reset works after recovery");
    }
}
