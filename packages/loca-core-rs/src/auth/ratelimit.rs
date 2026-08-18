//! The sliding-window counters behind the pairing and admin-password budgets.
//!
//! Two limiters sit on the same counter with deliberately different policies.
//! The per-IP one records on the **allow** path, so it counts attempts. The
//! password one records only on a **failure**, so a legitimate user is never
//! throttled by their own successful mints.

use std::collections::HashMap;

use super::{
    CLAIMS_PER_IP_MAX, CLAIMS_PER_IP_WINDOW_MS, MAX_TRACKED_IPS, PASSWORD_FAIL_MAX_GLOBAL,
    PASSWORD_FAIL_MAX_PER_KEY, PASSWORD_FAIL_WINDOW_MS,
};

/// The key the global password budget accumulates under.
///
/// The leading space is load-bearing: every real key is `c:<clientId>` or
/// `ip:<peerIp>`, so a space-prefixed key cannot be produced by a caller and the
/// global bucket cannot be reset by resetting a client's.
const GLOBAL_PASSWORD_KEY: &str = " global";

/// Event timestamps per key, pruned on access.
///
/// An event counts as live while `now - t < window_ms`, so one exactly
/// `window_ms` old has expired. Every method takes `&mut self` because reading
/// the count prunes.
pub(crate) struct SlidingWindowCounter {
    window_ms: i64,
    events: HashMap<String, Vec<i64>>,
}

impl SlidingWindowCounter {
    pub(crate) fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            events: HashMap::new(),
        }
    }

    /// Live event count for `key`, dropping expired entries in the same pass.
    pub(crate) fn live_count(&mut self, key: &str, now: i64) -> usize {
        let Some(events) = self.events.get_mut(key) else {
            return 0;
        };
        events.retain(|t| now - t < self.window_ms);
        let live = events.len();
        if live == 0 {
            self.events.remove(key);
        }
        live
    }

    /// Append an event for `key`, pruning expired entries in the same pass.
    pub(crate) fn record(&mut self, key: &str, now: i64) {
        let window = self.window_ms;
        let events = self.events.entry(key.to_owned()).or_default();
        events.retain(|t| now - t < window);
        events.push(now);
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.events.remove(key);
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    /// Drop every key whose window has fully expired, bounding the map across
    /// many distinct keys.
    pub(crate) fn sweep_expired(&mut self, now: i64) {
        let window = self.window_ms;
        self.events
            .retain(|_, events| events.iter().any(|t| now - t < window));
    }

    /// Bring the map back to at most `max` keys, whatever the window says.
    ///
    /// [`sweep_expired`](Self::sweep_expired) alone is not a cap, and the gap is
    /// the case a cap is for: a source rotating addresses faster than the window
    /// keeps every entry LIVE, so the sweep reclaims nothing and the map grows
    /// past the ceiling unbounded. The ceiling only did anything against slow
    /// rotation, which is the case that did not need it.
    ///
    /// Eviction is by least-recent activity, and that order is the security
    /// argument rather than an implementation detail. Evicting a key resets that
    /// key's budget, so evicting the WRONG one hands an attacker a way to clear
    /// their own throttle by flooding from other addresses. Oldest-first means
    /// the entries that go are the ones that have gone quiet, and an address
    /// actively being throttled is the last thing dropped: to evict itself it
    /// would have to out-rotate its own traffic, at which point every one of its
    /// addresses is under the per-IP budget anyway and this limiter is not the
    /// control doing the work.
    pub(crate) fn enforce_cap(&mut self, max: usize, now: i64) {
        self.sweep_expired(now);
        if self.events.len() <= max {
            return;
        }
        // Newest event per key: the key's last activity.
        let mut by_recency: Vec<(i64, String)> = self
            .events
            .iter()
            .map(|(key, events)| {
                (
                    events.iter().copied().max().unwrap_or(i64::MIN),
                    key.clone(),
                )
            })
            .collect();
        by_recency.sort_unstable();
        let excess = self.events.len() - max;
        for (_, key) in by_recency.into_iter().take(excess) {
            self.events.remove(&key);
        }
    }
}

/// Per-IP budget for pairing handshake starts.
pub(crate) struct IpRateLimiter {
    hits: SlidingWindowCounter,
}

impl IpRateLimiter {
    pub(crate) fn new() -> Self {
        Self {
            hits: SlidingWindowCounter::new(CLAIMS_PER_IP_WINDOW_MS),
        }
    }

    /// True when this attempt is allowed, recording it. False when the IP is at
    /// or over its cap, recording nothing: a refused attempt must not extend the
    /// lockout, or a client hammering the endpoint would never recover.
    pub(crate) fn record_and_check(&mut self, ip: &str, now: i64) -> bool {
        // Bound the map across many distinct source addresses. Only reachable
        // off loopback, and cheap enough to keep as a belt-and-braces bound.
        //
        // `enforce_cap` rather than `sweep_expired`, because the sweep alone
        // reclaims nothing from the case that produces the growth: an address
        // rotation faster than the ten-minute window leaves every entry live.
        if self.hits.len() > MAX_TRACKED_IPS {
            self.hits.enforce_cap(MAX_TRACKED_IPS, now);
        }
        if self.hits.live_count(ip, now) >= CLAIMS_PER_IP_MAX {
            return false;
        }
        self.hits.record(ip, now);
        true
    }
}

/// Failure budget for admin-password guesses, keyed by the caller's client id,
/// the socket peer address, and a global bucket.
pub(crate) struct PasswordRateLimiter {
    fails: SlidingWindowCounter,
}

impl PasswordRateLimiter {
    pub(crate) fn new() -> Self {
        Self {
            fails: SlidingWindowCounter::new(PASSWORD_FAIL_WINDOW_MS),
        }
    }

    /// True when the global cap is reached, or any of `keys` is at its own cap.
    pub(crate) fn locked(&mut self, keys: &[String], now: i64) -> bool {
        if self.fails.live_count(GLOBAL_PASSWORD_KEY, now) >= PASSWORD_FAIL_MAX_GLOBAL {
            return true;
        }
        keys.iter()
            .any(|k| self.fails.live_count(k, now) >= PASSWORD_FAIL_MAX_PER_KEY)
    }

    /// Count one wrong guess against every key **and** the global bucket. Both
    /// the client id and the peer address are charged, so rotating either one
    /// does not buy a fresh budget.
    pub(crate) fn record_failure(&mut self, keys: &[String], now: i64) {
        for key in keys {
            self.fails.record(key, now);
        }
        self.fails.record(GLOBAL_PASSWORD_KEY, now);
    }

    /// Clear the per-key budgets after a correct guess. The global bucket is
    /// deliberately left alone: one legitimate success must not wipe the
    /// evidence of a spray across many clients.
    pub(crate) fn reset(&mut self, keys: &[String]) {
        for key in keys {
            self.fails.remove(key);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn keys(client: &str, ip: &str) -> Vec<String> {
        vec![format!("c:{client}"), format!("ip:{ip}")]
    }

    #[test]
    fn the_cap_holds_against_a_rotation_faster_than_the_window() {
        // The case the sweep cannot touch, and the one a cap exists for. Every
        // key here is recorded at `now`, so every key is LIVE: `sweep_expired`
        // finds nothing to drop and the map keeps growing past its ceiling for
        // as long as the source keeps rotating. The old code called only the
        // sweep, so the ceiling did something only against slow rotation, which
        // is the case that never threatened it.
        let mut c = SlidingWindowCounter::new(1000);
        for i in 0..50 {
            c.record(&format!("ip:{i}"), 0);
        }
        c.enforce_cap(10, 0);
        assert_eq!(
            c.len(),
            10,
            "the map grew past its cap while every entry was still inside its window, so a \
             source rotating addresses can hold unbounded memory",
        );
    }

    #[test]
    fn the_cap_evicts_the_quietest_keys_and_keeps_the_active_one() {
        // The eviction ORDER, which is the security half. Evicting a key resets
        // its budget, so evicting the currently-hot address would let an
        // attacker clear their own throttle by flooding from others. Oldest
        // activity first means the address still sending is the last to go.
        let mut c = SlidingWindowCounter::new(10_000);
        for i in 0..20 {
            c.record(&format!("stale:{i}"), 100);
        }
        c.record("hot", 900);

        c.enforce_cap(5, 1000);

        assert_eq!(c.len(), 5);
        assert_eq!(
            c.live_count("hot", 1000),
            1,
            "the most recently active key was evicted, which resets exactly the budget that was \
             doing work and lets a rotating source clear its own throttle",
        );
    }

    #[test]
    fn an_event_exactly_one_window_old_has_expired() {
        let mut c = SlidingWindowCounter::new(1000);
        c.record("k", 0);
        assert_eq!(c.live_count("k", 999), 1);
        assert_eq!(c.live_count("k", 1000), 0);
    }

    #[test]
    fn an_emptied_key_stops_being_tracked() {
        let mut c = SlidingWindowCounter::new(1000);
        c.record("k", 0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.live_count("k", 5000), 0);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn sweep_expired_drops_only_fully_expired_keys() {
        let mut c = SlidingWindowCounter::new(1000);
        c.record("old", 0);
        c.record("mixed", 0);
        c.record("mixed", 900);
        c.sweep_expired(1500);
        assert_eq!(c.len(), 1);
        assert_eq!(c.live_count("mixed", 1500), 1);
    }

    #[test]
    fn the_ip_limiter_allows_twenty_and_refuses_the_twenty_first() {
        let mut l = IpRateLimiter::new();
        for i in 0..CLAIMS_PER_IP_MAX {
            assert!(l.record_and_check("10.0.0.1", i as i64), "attempt {i}");
        }
        assert!(!l.record_and_check("10.0.0.1", 0));
        // Another address keeps its own budget.
        assert!(l.record_and_check("10.0.0.2", 0));
    }

    #[test]
    fn a_refused_attempt_does_not_extend_the_lockout() {
        let mut l = IpRateLimiter::new();
        for _ in 0..CLAIMS_PER_IP_MAX {
            assert!(l.record_and_check("ip", 0));
        }
        // Hammering while locked records nothing, so the window still clears at
        // exactly one window past the last ALLOWED attempt.
        for t in 1..500 {
            assert!(!l.record_and_check("ip", t));
        }
        assert!(l.record_and_check("ip", CLAIMS_PER_IP_WINDOW_MS));
    }

    #[test]
    fn the_window_rolling_forward_frees_the_ip_budget() {
        let mut l = IpRateLimiter::new();
        for _ in 0..CLAIMS_PER_IP_MAX {
            assert!(l.record_and_check("ip", 0));
        }
        assert!(!l.record_and_check("ip", CLAIMS_PER_IP_WINDOW_MS - 1));
        assert!(l.record_and_check("ip", CLAIMS_PER_IP_WINDOW_MS));
    }

    #[test]
    fn five_failures_lock_a_password_key() {
        let mut l = PasswordRateLimiter::new();
        let k = keys("c1", "10.0.0.1");
        for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
            assert!(!l.locked(&k, 0));
            l.record_failure(&k, 0);
        }
        assert!(l.locked(&k, 0));
        assert!(!l.locked(&k, PASSWORD_FAIL_WINDOW_MS));
    }

    /// Both halves of the key set are charged, so neither rotating the client id
    /// nor rotating the source address buys a fresh budget.
    #[test]
    fn rotating_one_half_of_the_key_pair_does_not_reset_the_budget() {
        let mut l = PasswordRateLimiter::new();
        for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
            l.record_failure(&keys("c1", "10.0.0.1"), 0);
        }
        // Same client, new address: still locked, because `c:c1` is at its cap.
        assert!(l.locked(&keys("c1", "10.0.0.9"), 0));
        // New client, same address: still locked, because `ip:10.0.0.1` is.
        assert!(l.locked(&keys("c9", "10.0.0.1"), 0));
        // Both rotated: a fresh budget, which is the intended granularity.
        assert!(!l.locked(&keys("c9", "10.0.0.9"), 0));
    }

    #[test]
    fn a_correct_guess_clears_the_per_key_budget_but_not_the_global_one() {
        let mut l = PasswordRateLimiter::new();
        let k = keys("c1", "10.0.0.1");
        for _ in 0..PASSWORD_FAIL_MAX_PER_KEY {
            l.record_failure(&k, 0);
        }
        assert!(l.locked(&k, 0));
        l.reset(&k);
        assert!(!l.locked(&k, 0));
        // The global bucket still carries those five, which is what makes a
        // spray across many clients accumulate.
        for i in 0..(PASSWORD_FAIL_MAX_GLOBAL - PASSWORD_FAIL_MAX_PER_KEY) {
            l.record_failure(&keys(&format!("c{i}"), &format!("10.1.0.{i}")), 0);
        }
        assert!(l.locked(&keys("fresh", "192.168.0.1"), 0));
    }

    #[test]
    fn the_global_cap_locks_every_key() {
        let mut l = PasswordRateLimiter::new();
        for i in 0..PASSWORD_FAIL_MAX_GLOBAL {
            let k = keys(&format!("c{i}"), &format!("10.0.{i}.1"));
            assert!(!l.locked(&k, 0));
            l.record_failure(&k, 0);
        }
        assert!(l.locked(&keys("never-seen", "203.0.113.1"), 0));
        assert!(!l.locked(&keys("never-seen", "203.0.113.1"), PASSWORD_FAIL_WINDOW_MS));
    }
}
