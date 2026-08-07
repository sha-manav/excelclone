//! A token bucket per key, for the two endpoints a public deployment exposes
//! to people who have not been vouched for.
//!
//! Deliberately in-process and in-memory. That means it resets on restart and
//! does not coordinate across replicas, so it is a brake on casual abuse
//! rather than a security boundary — stated here rather than discovered
//! later. A single small instance is what this is sized for; a deployment
//! that outgrows one process needs a shared store, not a bigger `HashMap`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// How many keys the map may hold. It is keyed by client IP on a public
/// endpoint, so an unbounded one is a memory leak with a trivially cheap
/// trigger.
const MAX_KEYS: usize = 10_000;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Debug)]
pub struct RateLimit {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimit {
    pub fn per_hour(n: u32) -> Self {
        Self::new(f64::from(n), f64::from(n) / 3600.0)
    }

    pub fn per_minute(n: u32) -> Self {
        Self::new(f64::from(n), f64::from(n) / 60.0)
    }

    fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Spend one token for `key`, refilling first. False means refused.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// `allow`, with the clock supplied — so a test can assert the refill rate
    /// instead of sleeping through it.
    pub fn allow_at(&self, key: &str, now: Instant) -> bool {
        // A poisoned lock means another thread panicked mid-update. The worst
        // a stale bucket can do here is allow or refuse one request, which is
        // a far better outcome than propagating the panic into every later
        // request for the same key.
        let mut buckets = match self.buckets.lock() {
            Ok(b) => b,
            Err(poisoned) => poisoned.into_inner(),
        };

        let capacity = self.capacity;
        let rate = self.refill_per_sec;

        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            last: now,
        });
        bucket.tokens = refilled(bucket, now, rate, capacity);
        bucket.last = now;

        let allowed = if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        };

        if buckets.len() > MAX_KEYS {
            evict(&mut buckets, now, rate, capacity);
        }

        allowed
    }
}

/// What a bucket *would* hold at `now`. Refill is lazy — a bucket's stored
/// count is only updated when its own key is touched — so anything reasoning
/// about a bucket it did not just touch has to do this itself. Reading
/// `b.tokens` directly is how the first version of the eviction pass came to
/// believe every idle bucket was still empty, and pruned nothing.
fn refilled(b: &Bucket, now: Instant, rate: f64, capacity: f64) -> f64 {
    // `saturating_duration_since`, because `now` may predate `last` when a
    // test supplies its own instants; a negative elapsed would refill the
    // bucket backwards.
    let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
    (b.tokens + elapsed * rate).min(capacity)
}

fn evict(buckets: &mut HashMap<String, Bucket>, now: Instant, rate: f64, capacity: f64) {
    // A bucket back at capacity is indistinguishable from one that was never
    // created, so dropping it forgets nothing. Buckets still paying off a
    // burst are kept, which is the whole point: eviction must not be a cheap
    // way to reset your own limit.
    buckets.retain(|_, b| refilled(b, now, rate, capacity) < capacity);
    if buckets.len() <= MAX_KEYS {
        return;
    }

    // Still over, so every remaining bucket is mid-burst and something has to
    // go regardless. The oldest `last` is the one closest to being forgiven
    // anyway, so it is the cheapest thing to forget. Reaching this branch
    // means someone pushed ten thousand distinct keys through since the last
    // sweep, which costs them far more than the handful of requests it buys.
    let mut ages: Vec<(Instant, String)> =
        buckets.iter().map(|(k, b)| (b.last, k.clone())).collect();
    ages.sort_unstable_by_key(|(last, _)| *last);
    for (_, key) in ages.into_iter().take(buckets.len() - MAX_KEYS) {
        buckets.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_burst_is_allowed_up_to_capacity_and_then_refused() {
        let limit = RateLimit::per_hour(3);
        let now = Instant::now();
        assert!(limit.allow_at("a", now));
        assert!(limit.allow_at("a", now));
        assert!(limit.allow_at("a", now));
        assert!(!limit.allow_at("a", now), "the fourth in a burst of 3");
    }

    #[test]
    fn keys_do_not_share_a_bucket() {
        let limit = RateLimit::per_hour(1);
        let now = Instant::now();
        assert!(limit.allow_at("a", now));
        assert!(!limit.allow_at("a", now));
        assert!(limit.allow_at("b", now), "b is not paying for a's burst");
    }

    #[test]
    fn tokens_come_back_at_the_stated_rate() {
        let limit = RateLimit::per_minute(60); // one per second
        let start = Instant::now();
        for _ in 0..60 {
            assert!(limit.allow_at("a", start));
        }
        assert!(!limit.allow_at("a", start));
        assert!(
            !limit.allow_at("a", start + Duration::from_millis(999)),
            "just under a second is not yet a token"
        );
        assert!(limit.allow_at("a", start + Duration::from_millis(1_000)));
        assert!(
            !limit.allow_at("a", start + Duration::from_millis(1_000)),
            "and only the one"
        );
    }

    #[test]
    fn refill_is_capped_at_capacity() {
        let limit = RateLimit::per_minute(2);
        let start = Instant::now();
        assert!(limit.allow_at("a", start));
        assert!(limit.allow_at("a", start));
        // An hour of idling must not bank an hour's worth of requests.
        let later = start + Duration::from_secs(3600);
        assert!(limit.allow_at("a", later));
        assert!(limit.allow_at("a", later));
        assert!(!limit.allow_at("a", later), "capacity is 2, not 120");
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_grant_tokens() {
        let limit = RateLimit::per_minute(1);
        let start = Instant::now() + Duration::from_secs(10);
        assert!(limit.allow_at("a", start));
        assert!(
            !limit.allow_at("a", start - Duration::from_secs(10)),
            "a backwards clock must not refill the bucket"
        );
    }

    #[test]
    fn idle_buckets_are_pruned_and_a_key_mid_burst_is_not() {
        let limit = RateLimit::per_hour(2);
        let start = Instant::now();
        // `victim` spends its whole allowance and stays over for an hour.
        assert!(limit.allow_at("victim", start));
        assert!(limit.allow_at("victim", start));
        assert!(!limit.allow_at("victim", start));

        // Fillers touched once at `start`, then the map is pushed over its
        // bound a second later. By then the fillers are still mid-burst too,
        // so nothing is eligible and the hard bound does the work.
        for i in 0..=MAX_KEYS {
            limit.allow_at(&format!("filler{i}"), start);
        }
        assert!(
            !limit.allow_at("victim", start + Duration::from_secs(1)),
            "eviction must not be a way to reset your own limit"
        );

        // An hour on, the fillers have refilled and are indistinguishable from
        // keys that never existed, so the sweep collects them.
        let later = start + Duration::from_secs(3600);
        limit.allow_at("trigger", later);
        let len = limit.buckets.lock().unwrap().len();
        assert!(
            len < 10,
            "idle buckets should have been pruned, {len} remain"
        );
    }

    #[test]
    fn the_map_is_hard_bounded_even_when_nothing_is_eligible() {
        let limit = RateLimit::per_hour(1);
        let now = Instant::now();
        // Every one of these is left with zero tokens, so none is prunable on
        // the "back at capacity" rule; the bound has to hold anyway.
        for i in 0..(MAX_KEYS * 2) {
            limit.allow_at(&format!("k{i}"), now);
        }
        let len = limit.buckets.lock().unwrap().len();
        assert!(len <= MAX_KEYS + 1, "map grew to {len}");
    }
}
