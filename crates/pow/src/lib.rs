//! Adaptive proof-of-work (docs/AUTH.md section 9).
//!
//! A client that keeps hitting the rate limits is asked to solve a small puzzle before each
//! request. Solving costs the client some computing time and costs the server almost nothing to
//! check, so a bot sending thousands of requests pays for every one, while a person who tripped
//! the limit once barely notices. The more often an address is refused, the harder the puzzle.
//!
//! The puzzle: given a challenge text, find a number `nonce` so that the SHA-256 hash of
//! `"{challenge}~{nonce}"` starts with at least `bits` zero bits. Each extra bit doubles the
//! expected work (16 bits is about 65 000 hashes, 22 bits about 4 million).
//!
//! Challenges are stateless: the server signs them (HMAC-SHA-256) instead of storing them. A
//! signed challenge is tied to one client address, expires after a short time, and can be used
//! once (the server remembers the used ones until they expire).
//!
//! This crate knows nothing about HTTP; the caller passes the time, so tests can move the clock.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Most used challenges (and most struck keys) remembered at once.
const MAX_ENTRIES: usize = 100_000;
/// A solution longer than this is not even looked at.
const MAX_SOLUTION_LEN: usize = 300;
/// The hardest puzzle allowed. Above this a browser would take minutes.
const HARDEST: u32 = 30;

/// Why a solution was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowError {
    /// Not in the expected shape.
    Malformed,
    /// The signature is wrong: not our challenge, or for another address.
    BadSignature,
    /// Older than the challenge's lifetime.
    Expired,
    /// The challenge was issued for an easier puzzle than the one required now.
    TooEasy,
    /// The hash does not start with enough zero bits.
    NotSolved,
    /// This challenge was already used.
    Replayed,
    /// Too many unexpired challenges are being remembered; try again later.
    Busy,
}

/// Counts how often each key (an address) was refused recently. Strikes are forgotten once a key
/// has been left alone for the whole `window`; every new strike restarts that wait.
pub struct Strikes<K> {
    window: Duration,
    max_keys: usize,
    seen: Mutex<HashMap<K, (u32, Instant)>>,
}

impl<K: Hash + Eq + Clone> Strikes<K> {
    pub fn new(window: Duration) -> Self {
        Strikes {
            window,
            max_keys: MAX_ENTRIES,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Notes one refusal of `key` at time `now`.
    pub fn record(&self, key: &K, now: Instant) {
        let mut seen = self.seen.lock().expect("strikes");
        if !seen.contains_key(key) && seen.len() >= self.max_keys {
            seen.retain(|_, (_, last)| now.saturating_duration_since(*last) <= self.window);
            if seen.len() >= self.max_keys {
                // Full of keys that are all being struck right now: not worth growing for.
                return;
            }
        }
        let entry = seen.entry(key.clone()).or_insert((0, now));
        if now.saturating_duration_since(entry.1) > self.window {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    /// How many refusals count against `key` right now (0 if it behaved for the whole window).
    pub fn count(&self, key: &K, now: Instant) -> u32 {
        let seen = self.seen.lock().expect("strikes");
        match seen.get(key) {
            Some((count, last)) if now.saturating_duration_since(*last) <= self.window => *count,
            _ => 0,
        }
    }
}

/// Makes and checks challenges.
pub struct Pow {
    key: [u8; 32],
    base_bits: u32,
    max_bits: u32,
    ttl_secs: u64,
    /// Used challenges (by signature) and when they expire.
    used: Mutex<HashMap<String, u64>>,
}

impl Pow {
    /// `base_bits` is the puzzle after the first refusal; each further refusal adds 2 bits, up to
    /// `max_bits`. A challenge stays valid for `ttl_secs`. The signing key is random and lives only
    /// in memory, so challenges stop working when the server restarts (clients just ask again).
    pub fn new(base_bits: u32, max_bits: u32, ttl_secs: u64) -> Result<Pow, String> {
        if !(1..=HARDEST).contains(&base_bits) || !(base_bits..=HARDEST).contains(&max_bits) {
            return Err(format!(
                "bits must satisfy 1 <= base <= max <= {HARDEST}, got base {base_bits}, max {max_bits}"
            ));
        }
        if !(1..=3600).contains(&ttl_secs) {
            return Err(format!(
                "lifetime must be 1 to 3600 seconds, got {ttl_secs}"
            ));
        }
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        Ok(Pow {
            key,
            base_bits,
            max_bits,
            ttl_secs,
            used: Mutex::new(HashMap::new()),
        })
    }

    /// How hard the puzzle is for a key with this many strikes. 0 means no puzzle is needed.
    pub fn bits_for(&self, strikes: u32) -> u32 {
        if strikes == 0 {
            return 0;
        }
        let extra = strikes.saturating_sub(1).saturating_mul(2);
        self.base_bits.saturating_add(extra).min(self.max_bits)
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    pub fn base_bits(&self) -> u32 {
        self.base_bits
    }

    fn sign(&self, body: &str, binding: &str) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC takes any key length");
        mac.update(body.as_bytes());
        mac.update(b"|");
        mac.update(binding.as_bytes());
        mac
    }

    /// A new challenge for the client at `binding` (its address), `bits` hard, valid from `now`.
    pub fn issue(&self, binding: &str, bits: u32, now_unix: u64) -> String {
        let mut random = [0u8; 8];
        OsRng.fill_bytes(&mut random);
        let body = format!(
            "v1.{}.{}.{}",
            now_unix + self.ttl_secs,
            bits.min(HARDEST),
            hex::encode(random)
        );
        let tag = hex::encode(self.sign(&body, binding).finalize().into_bytes());
        format!("{body}.{tag}")
    }

    /// Checks `solution` (`"{challenge}~{nonce}"`) from the client at `binding`. It must be one of
    /// our unexpired, unused challenges, at least `min_bits` hard, and actually solved. A good
    /// solution is used up: sending it again fails with [`PowError::Replayed`].
    pub fn verify(
        &self,
        binding: &str,
        solution: &str,
        min_bits: u32,
        now_unix: u64,
    ) -> Result<(), PowError> {
        if solution.len() > MAX_SOLUTION_LEN {
            return Err(PowError::Malformed);
        }
        let (challenge, nonce) = solution.split_once('~').ok_or(PowError::Malformed)?;
        if nonce.is_empty() || nonce.len() > 20 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(PowError::Malformed);
        }
        let parts: Vec<&str> = challenge.split('.').collect();
        let [version, expiry, bits, random, tag] = parts[..] else {
            return Err(PowError::Malformed);
        };
        if version != "v1" {
            return Err(PowError::Malformed);
        }
        let expiry: u64 = expiry.parse().map_err(|_| PowError::Malformed)?;
        let bits: u32 = bits.parse().map_err(|_| PowError::Malformed)?;
        let tag_bytes = hex::decode(tag).map_err(|_| PowError::Malformed)?;
        let body = format!("v1.{expiry}.{bits}.{random}");
        // `verify_slice` compares in constant time.
        self.sign(&body, binding)
            .verify_slice(&tag_bytes)
            .map_err(|_| PowError::BadSignature)?;
        if expiry <= now_unix {
            return Err(PowError::Expired);
        }
        if bits < min_bits || bits > HARDEST {
            return Err(PowError::TooEasy);
        }
        if leading_zero_bits(&Sha256::digest(solution.as_bytes())) < bits {
            return Err(PowError::NotSolved);
        }

        let mut used = self.used.lock().expect("pow");
        if used.contains_key(tag) {
            return Err(PowError::Replayed);
        }
        if used.len() >= MAX_ENTRIES {
            used.retain(|_, expires| *expires > now_unix);
            if used.len() >= MAX_ENTRIES {
                return Err(PowError::Busy);
            }
        }
        used.insert(tag.to_string(), expiry);
        Ok(())
    }
}

/// How many bits at the start of `hash` are zero.
pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in hash {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

/// Finds a nonce that solves `challenge` at `bits` (what the browser does; here for tests and
/// tools). Takes about `2^bits` tries.
pub fn solve(challenge: &str, bits: u32) -> u64 {
    let mut nonce = 0u64;
    loop {
        let candidate = format!("{challenge}~{nonce}");
        if leading_zero_bits(&Sha256::digest(candidate.as_bytes())) >= bits {
            return nonce;
        }
        nonce += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn pow() -> Pow {
        Pow::new(8, 12, 60).unwrap()
    }

    fn solved(pow: &Pow, binding: &str, bits: u32) -> String {
        let challenge = pow.issue(binding, bits, NOW);
        format!("{challenge}~{}", solve(&challenge, bits))
    }

    #[test]
    fn leading_zero_bits_are_counted() {
        assert_eq!(leading_zero_bits(&[0xff]), 0);
        assert_eq!(leading_zero_bits(&[0x7f]), 1);
        assert_eq!(leading_zero_bits(&[0x01]), 7);
        assert_eq!(leading_zero_bits(&[0x00, 0x10]), 11);
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
        assert_eq!(leading_zero_bits(&[]), 0);
    }

    #[test]
    fn the_solver_finds_the_same_answer_as_the_browser() {
        // Golden value, also tested in web/src/pow/pow.test.js: both sides hash the same text.
        assert_eq!(solve("golden", 12), 2561);
    }

    #[test]
    fn the_puzzle_gets_harder_with_every_strike_up_to_a_cap() {
        let pow = pow();
        assert_eq!(pow.bits_for(0), 0);
        assert_eq!(pow.bits_for(1), 8);
        assert_eq!(pow.bits_for(2), 10);
        assert_eq!(pow.bits_for(3), 12);
        assert_eq!(pow.bits_for(4), 12);
        assert_eq!(pow.bits_for(u32::MAX), 12);
    }

    #[test]
    fn a_solved_challenge_is_accepted_once() {
        let pow = pow();
        let solution = solved(&pow, "1.2.3.4", 8);
        assert_eq!(pow.verify("1.2.3.4", &solution, 8, NOW), Ok(()));
        assert_eq!(
            pow.verify("1.2.3.4", &solution, 8, NOW),
            Err(PowError::Replayed)
        );
    }

    #[test]
    fn an_unsolved_or_forged_challenge_is_refused() {
        let pow = pow();
        let challenge = pow.issue("1.2.3.4", 12, NOW);
        // Find a nonce that does NOT solve it.
        let wrong = (0u64..)
            .find(|n| {
                leading_zero_bits(&Sha256::digest(format!("{challenge}~{n}").as_bytes())) < 12
            })
            .unwrap();
        assert_eq!(
            pow.verify("1.2.3.4", &format!("{challenge}~{wrong}"), 8, NOW),
            Err(PowError::NotSolved)
        );
        // A challenge from another server (another key) has a bad signature.
        let other = Pow::new(8, 12, 60).unwrap();
        let theirs = solved(&other, "1.2.3.4", 8);
        assert_eq!(
            pow.verify("1.2.3.4", &theirs, 8, NOW),
            Err(PowError::BadSignature)
        );
        // Lowering the claimed difficulty breaks the signature too.
        let honest = solved(&pow, "1.2.3.4", 12);
        let cheated = honest.replacen(".12.", ".1.", 1);
        assert_eq!(
            pow.verify("1.2.3.4", &cheated, 1, NOW),
            Err(PowError::BadSignature)
        );
    }

    #[test]
    fn a_challenge_belongs_to_one_address() {
        let pow = pow();
        let solution = solved(&pow, "1.2.3.4", 8);
        assert_eq!(
            pow.verify("5.6.7.8", &solution, 8, NOW),
            Err(PowError::BadSignature)
        );
        // The failed attempt did not use it up.
        assert_eq!(pow.verify("1.2.3.4", &solution, 8, NOW), Ok(()));
    }

    #[test]
    fn a_challenge_expires() {
        let pow = pow();
        let solution = solved(&pow, "1.2.3.4", 8);
        assert_eq!(
            pow.verify("1.2.3.4", &solution, 8, NOW + 60),
            Err(PowError::Expired)
        );
        assert_eq!(pow.verify("1.2.3.4", &solution, 8, NOW + 59), Ok(()));
    }

    #[test]
    fn an_easier_challenge_than_required_is_refused() {
        let pow = pow();
        let solution = solved(&pow, "1.2.3.4", 8);
        assert_eq!(
            pow.verify("1.2.3.4", &solution, 10, NOW),
            Err(PowError::TooEasy)
        );
    }

    #[test]
    fn nonsense_is_refused_without_panicking() {
        let pow = pow();
        for bad in [
            "",
            "~",
            "abc",
            "v1.1.2.3.4~5",
            "v1.x.8.00.00~1",
            "v2.100.8.00.00~1",
            "v1.2000000.8.00.zz~1",
            "v1.2000000.8.00.00~",
            "v1.2000000.8.00.00~a b",
        ] {
            assert_eq!(
                pow.verify("1.2.3.4", bad, 8, NOW),
                Err(PowError::Malformed),
                "{bad:?}"
            );
        }
        let long = format!("v1.2.3.4.5~{}", "9".repeat(400));
        assert_eq!(pow.verify("x", &long, 8, NOW), Err(PowError::Malformed));
    }

    #[test]
    fn settings_are_checked() {
        assert!(Pow::new(0, 10, 60).is_err());
        assert!(Pow::new(12, 8, 60).is_err());
        assert!(Pow::new(8, 31, 60).is_err());
        assert!(Pow::new(8, 12, 0).is_err());
        assert!(Pow::new(8, 12, 99_999).is_err());
        assert!(Pow::new(16, 22, 120).is_ok());
    }

    #[test]
    fn strikes_pile_up_and_are_forgotten_after_a_quiet_window() {
        let strikes = Strikes::new(Duration::from_secs(10));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(strikes.count(&"a", t0), 0);
        strikes.record(&"a", t0);
        strikes.record(&"a", at(5));
        assert_eq!(strikes.count(&"a", at(5)), 2);
        assert_eq!(strikes.count(&"b", at(5)), 0);
        // Each strike restarts the wait: still counted 10 seconds after the last one...
        assert_eq!(strikes.count(&"a", at(15)), 2);
        // ...and forgotten after that.
        assert_eq!(strikes.count(&"a", at(16)), 0);
        strikes.record(&"a", at(20));
        assert_eq!(strikes.count(&"a", at(20)), 1);
    }

    #[test]
    fn a_flood_of_struck_keys_does_not_grow_the_table() {
        let mut strikes = Strikes::new(Duration::from_secs(10));
        strikes.max_keys = 2;
        let t0 = Instant::now();
        strikes.record(&1u32, t0);
        strikes.record(&2u32, t0);
        strikes.record(&3u32, t0);
        assert_eq!(strikes.count(&3, t0), 0);
        // Once the old ones are forgotten there is room again.
        let later = t0 + Duration::from_secs(11);
        strikes.record(&3u32, later);
        assert_eq!(strikes.count(&3, later), 1);
    }
}
