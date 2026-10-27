//! Rate limiting with token buckets.
//!
//! A bucket holds up to `burst` tokens and gets `per_second` new tokens every second. Each request
//! takes one token. A request that finds the bucket empty is refused and told how long to wait.
//! So a person clicking quickly never notices (the burst covers it), while a bot sending hundreds
//! of requests a second runs dry and is held to `per_second`.
//!
//! One [`Limiter`] keeps one bucket per key (a user id, an IP address). It knows nothing about
//! HTTP: the caller passes the current time, so the tests can move the clock by hand.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::Instant;

/// The most distinct keys one limiter remembers. Buckets that have refilled completely are
/// forgotten (a fresh bucket is the same thing), so only keys active in the last few seconds count.
const MAX_KEYS: usize = 100_000;

/// How big a bucket is and how fast it refills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// Tokens a full bucket holds: how many requests may arrive at once.
    pub burst: u32,
    /// Tokens added per second: the steady speed allowed over time.
    pub per_second: f64,
}

impl Limit {
    pub fn new(burst: u32, per_second: f64) -> Result<Limit, String> {
        if !(1..=1_000_000).contains(&burst) {
            return Err(format!("burst must be from 1 to 1000000, got {burst}"));
        }
        if !per_second.is_finite() || per_second <= 0.0 || per_second > 1_000_000.0 {
            return Err(format!(
                "per-second rate must be above 0 and at most 1000000, got {per_second}"
            ));
        }
        Ok(Limit { burst, per_second })
    }

    /// Reads `"30/15"`: a burst of 30, refilling 15 tokens a second.
    pub fn parse(spec: &str) -> Result<Limit, String> {
        let (burst, rate) = spec
            .split_once('/')
            .ok_or_else(|| format!("expected BURST/PER_SECOND such as 30/15, got {spec:?}"))?;
        let burst = burst
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("the burst in {spec:?} is not a whole number"))?;
        let rate = rate
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("the per-second rate in {spec:?} is not a number"))?;
        Limit::new(burst, rate)
    }
}

/// The answer to "may this request go through?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// Refused. Try again after this many seconds (always at least 1).
    Denied {
        retry_after_secs: u64,
    },
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// A set of token buckets, one per key.
pub struct Limiter<K> {
    limit: Limit,
    max_keys: usize,
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    pub fn new(limit: Limit) -> Self {
        Self::with_max_keys(limit, MAX_KEYS)
    }

    fn with_max_keys(limit: Limit, max_keys: usize) -> Self {
        Limiter {
            limit,
            max_keys,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key` at time `now`, if there is one. A refused request takes nothing.
    pub fn check(&self, key: &K, now: Instant) -> Verdict {
        let burst = f64::from(self.limit.burst);
        let rate = self.limit.per_second;
        let mut buckets = self.buckets.lock().expect("rate limiter");

        if !buckets.contains_key(key) && buckets.len() >= self.max_keys {
            // Forget the buckets that have refilled completely: they equal a fresh one.
            buckets.retain(|_, bucket| refilled(bucket, now, burst, rate) < burst);
            if buckets.len() >= self.max_keys {
                // Still full of busy keys (somebody is spraying addresses): refuse new ones.
                return Verdict::Denied {
                    retry_after_secs: 1,
                };
            }
        }

        let bucket = buckets.entry(key.clone()).or_insert(Bucket {
            tokens: burst,
            updated: now,
        });
        bucket.tokens = refilled(bucket, now, burst, rate);
        bucket.updated = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Verdict::Allowed
        } else {
            let wait = ((1.0 - bucket.tokens) / rate).ceil().max(1.0);
            Verdict::Denied {
                retry_after_secs: wait as u64,
            }
        }
    }

    /// How many keys are being remembered (for tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().expect("rate limiter").len()
    }
}

/// The tokens a bucket would hold at `now`, never more than `burst`.
fn refilled(bucket: &Bucket, now: Instant, burst: f64, rate: f64) -> f64 {
    let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
    (bucket.tokens + elapsed * rate).min(burst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn limit(burst: u32, per_second: f64) -> Limit {
        Limit::new(burst, per_second).unwrap()
    }

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn a_burst_is_allowed_and_then_the_bucket_runs_dry() {
        let limiter = Limiter::new(limit(3, 1.0));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(limiter.check(&"a", t0), Verdict::Allowed);
        }
        assert_eq!(
            limiter.check(&"a", t0),
            Verdict::Denied {
                retry_after_secs: 1
            }
        );
    }

    #[test]
    fn tokens_come_back_at_the_steady_rate() {
        let limiter = Limiter::new(limit(2, 2.0));
        let t0 = Instant::now();
        assert_eq!(limiter.check(&"a", t0), Verdict::Allowed);
        assert_eq!(limiter.check(&"a", t0), Verdict::Allowed);
        assert!(matches!(limiter.check(&"a", t0), Verdict::Denied { .. }));
        // Half a second at 2 tokens a second is exactly one token.
        assert_eq!(limiter.check(&"a", at(t0, 500)), Verdict::Allowed);
        assert!(matches!(
            limiter.check(&"a", at(t0, 500)),
            Verdict::Denied { .. }
        ));
    }

    #[test]
    fn the_bucket_never_holds_more_than_the_burst() {
        let limiter = Limiter::new(limit(2, 100.0));
        let t0 = Instant::now();
        // A long quiet time does not save up extra tokens.
        let later = at(t0, 60_000);
        assert_eq!(limiter.check(&"a", later), Verdict::Allowed);
        assert_eq!(limiter.check(&"a", later), Verdict::Allowed);
        assert!(matches!(limiter.check(&"a", later), Verdict::Denied { .. }));
    }

    #[test]
    fn keys_do_not_share_a_bucket() {
        let limiter = Limiter::new(limit(1, 0.1));
        let t0 = Instant::now();
        assert_eq!(limiter.check(&1u64, t0), Verdict::Allowed);
        assert!(matches!(limiter.check(&1u64, t0), Verdict::Denied { .. }));
        assert_eq!(limiter.check(&2u64, t0), Verdict::Allowed);
    }

    #[test]
    fn the_wait_is_rounded_up_to_whole_seconds() {
        // One token every 4 seconds.
        let limiter = Limiter::new(limit(1, 0.25));
        let t0 = Instant::now();
        assert_eq!(limiter.check(&"a", t0), Verdict::Allowed);
        assert_eq!(
            limiter.check(&"a", t0),
            Verdict::Denied {
                retry_after_secs: 4
            }
        );
        // After 1.5 seconds 2.5 seconds are left, which is told as 3.
        assert_eq!(
            limiter.check(&"a", at(t0, 1500)),
            Verdict::Denied {
                retry_after_secs: 3
            }
        );
    }

    #[test]
    fn a_refused_request_costs_nothing() {
        let limiter = Limiter::new(limit(1, 1.0));
        let t0 = Instant::now();
        assert_eq!(limiter.check(&"a", t0), Verdict::Allowed);
        // Hammering while empty does not push the next token further away.
        for ms in [0, 100, 200, 300, 400] {
            assert!(matches!(
                limiter.check(&"a", at(t0, ms)),
                Verdict::Denied { .. }
            ));
        }
        assert_eq!(limiter.check(&"a", at(t0, 1000)), Verdict::Allowed);
    }

    #[test]
    fn idle_keys_are_forgotten_and_a_flood_of_busy_keys_is_refused() {
        let limiter = Limiter::with_max_keys(limit(1, 1.0), 3);
        let t0 = Instant::now();
        for key in 1u32..=3 {
            assert_eq!(limiter.check(&key, t0), Verdict::Allowed);
        }
        assert_eq!(limiter.len(), 3);
        // All three are still empty, so a fourth key is refused rather than remembered.
        assert!(matches!(limiter.check(&4, t0), Verdict::Denied { .. }));
        assert_eq!(limiter.len(), 3);
        // Two seconds later they have all refilled, so they are forgotten to make room.
        assert_eq!(limiter.check(&4, at(t0, 2000)), Verdict::Allowed);
        assert_eq!(limiter.len(), 1);
    }

    #[test]
    fn limits_are_read_from_text() {
        assert_eq!(Limit::parse("30/15").unwrap(), limit(30, 15.0));
        assert_eq!(Limit::parse(" 10 / 0.2 ").unwrap(), limit(10, 0.2));
        for bad in [
            "", "30", "x/1", "30/x", "0/1", "30/0", "30/-1", "30/inf", "30/NaN",
        ] {
            assert!(Limit::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
