//! Per-IP token buckets for registrations and for messages.
//!
//! Time is passed in rather than read from the clock so the buckets can be
//! tested exactly instead of with sleeps.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::config::RelayConfig;

/// Once the table is this large, a prune runs before a new IP is inserted.
/// Chosen well above any plausible number of concurrent source addresses so a
/// busy relay never pays for it, and low enough that a spray of one-packet
/// connections from random addresses cannot grow the table unboundedly.
const PRUNE_THRESHOLD: usize = 4096;

/// An IP whose buckets are full and that has been silent this long carries no
/// information, so it is forgotten.
const IDLE_BEFORE_PRUNE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
pub struct Quota {
    /// Tokens the bucket holds when full; the size of an allowed burst.
    pub burst: f64,
    /// Tokens added per second. Zero means the quota is not enforced.
    pub per_second: f64,
}

impl Quota {
    pub fn unlimited() -> Self {
        Self {
            burst: 0.0,
            per_second: 0.0,
        }
    }

    pub fn is_enforced(&self) -> bool {
        self.per_second > 0.0 && self.burst > 0.0
    }
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(quota: &Quota, now: Instant) -> Self {
        Self {
            tokens: quota.burst,
            last: now,
        }
    }

    fn refill(&mut self, quota: &Quota, now: Instant) {
        // `saturating_duration_since` because `now` is supplied by the caller
        // and a clock that goes backwards must not mint tokens.
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * quota.per_second).min(quota.burst);
            self.last = now;
        }
    }

    fn try_take(&mut self, quota: &Quota, now: Instant) -> bool {
        self.refill(quota, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Token level this bucket would have at `now`, without mutating it.
    fn tokens_at(&self, quota: &Quota, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        (self.tokens + elapsed * quota.per_second).min(quota.burst)
    }

    /// Whether the bucket has recovered to full as of `now`.
    ///
    /// This has to project the refill forward rather than read the stored
    /// level: tokens are only written back by `try_take`, so an address that
    /// sent one message and then went silent keeps `burst - 1` on record
    /// forever. Reading that stored value made the prune predicate
    /// unsatisfiable for every address that had ever sent anything, which is
    /// all of them -- so the table grew without bound and the rate limiter
    /// became the memory-exhaustion vector it exists to prevent.
    fn is_full(&self, quota: &Quota, now: Instant) -> bool {
        self.tokens_at(quota, now) >= quota.burst
    }
}

#[derive(Debug)]
struct IpState {
    registrations: TokenBucket,
    messages: TokenBucket,
}

#[derive(Debug)]
pub struct RateLimiter {
    registrations: Quota,
    messages: Quota,
    state: Mutex<HashMap<IpAddr, IpState>>,
}

impl RateLimiter {
    pub fn new(registrations: Quota, messages: Quota) -> Self {
        Self {
            registrations,
            messages,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_config(config: &RelayConfig) -> Self {
        Self::new(
            Self::quota(
                config.registrations_per_minute / 60.0,
                config.registration_burst,
            ),
            Self::quota(config.messages_per_second, config.message_burst),
        )
    }

    /// A non-positive rate or burst is how an operator turns a limiter off.
    fn quota(per_second: f64, burst: u32) -> Quota {
        if per_second > 0.0 && burst > 0 {
            Quota {
                burst: f64::from(burst),
                per_second,
            }
        } else {
            Quota::unlimited()
        }
    }

    pub fn allow_registration(&self, ip: IpAddr, now: Instant) -> bool {
        self.take(ip, now, true)
    }

    pub fn allow_message(&self, ip: IpAddr, now: Instant) -> bool {
        self.take(ip, now, false)
    }

    fn take(&self, ip: IpAddr, now: Instant, registration: bool) -> bool {
        let quota = if registration {
            &self.registrations
        } else {
            &self.messages
        };
        if !quota.is_enforced() {
            return true;
        }

        let mut state = self.state.lock();
        if !state.contains_key(&ip) && state.len() >= PRUNE_THRESHOLD {
            let (regs, msgs) = (&self.registrations, &self.messages);
            state.retain(|_, s| {
                let idle = now.saturating_duration_since(s.registrations.last.max(s.messages.last));
                let rested = s.registrations.is_full(regs, now) && s.messages.is_full(msgs, now);
                !(rested && idle >= IDLE_BEFORE_PRUNE)
            });
        }

        let entry = state.entry(ip).or_insert_with(|| IpState {
            registrations: TokenBucket::new(&self.registrations, now),
            messages: TokenBucket::new(&self.messages, now),
        });
        if registration {
            entry.registrations.try_take(&self.registrations, now)
        } else {
            entry.messages.try_take(&self.messages, now)
        }
    }

    #[cfg(test)]
    fn tracked_addresses(&self) -> usize {
        self.state.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last_octet: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last_octet))
    }

    fn limiter() -> RateLimiter {
        RateLimiter::new(
            Quota {
                burst: 2.0,
                per_second: 1.0,
            },
            Quota {
                burst: 3.0,
                per_second: 1.0,
            },
        )
    }

    #[test]
    fn allows_a_burst_then_refuses_until_tokens_refill() {
        let limiter = limiter();
        let t0 = Instant::now();
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(!limiter.allow_registration(ip(1), t0));

        // Half a token is not a token.
        assert!(!limiter.allow_registration(ip(1), t0 + Duration::from_millis(500)));
        assert!(limiter.allow_registration(ip(1), t0 + Duration::from_millis(1100)));
    }

    #[test]
    fn never_refills_past_the_burst_size() {
        let limiter = limiter();
        let t0 = Instant::now();
        assert!(limiter.allow_registration(ip(1), t0));
        // An hour of idling must not bank an hour of registrations.
        let later = t0 + Duration::from_secs(3600);
        assert!(limiter.allow_registration(ip(1), later));
        assert!(limiter.allow_registration(ip(1), later));
        assert!(!limiter.allow_registration(ip(1), later));
    }

    #[test]
    fn keeps_one_addresss_budget_separate_from_anothers() {
        let limiter = limiter();
        let t0 = Instant::now();
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(!limiter.allow_registration(ip(1), t0));
        assert!(limiter.allow_registration(ip(2), t0));
    }

    #[test]
    fn counts_registrations_and_messages_against_separate_budgets() {
        let limiter = limiter();
        let t0 = Instant::now();
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(!limiter.allow_registration(ip(1), t0));
        // Registrations being exhausted says nothing about ordinary traffic.
        assert!(limiter.allow_message(ip(1), t0));
        assert!(limiter.allow_message(ip(1), t0));
        assert!(limiter.allow_message(ip(1), t0));
        assert!(!limiter.allow_message(ip(1), t0));
    }

    #[test]
    fn a_zero_rate_or_zero_burst_setting_turns_the_limiter_off() {
        let off = RateLimiter::from_config(&RelayConfig {
            registrations_per_minute: 0.0,
            message_burst: 0,
            ..RelayConfig::default()
        });
        let t0 = Instant::now();
        for _ in 0..1_000 {
            assert!(off.allow_registration(ip(1), t0));
            assert!(off.allow_message(ip(1), t0));
        }
    }

    #[test]
    fn a_zero_rate_quota_is_not_enforced_at_all() {
        let limiter = RateLimiter::new(Quota::unlimited(), Quota::unlimited());
        let t0 = Instant::now();
        for _ in 0..10_000 {
            assert!(limiter.allow_registration(ip(1), t0));
            assert!(limiter.allow_message(ip(1), t0));
        }
        // An unenforced quota should not even allocate per-IP state.
        assert_eq!(limiter.tracked_addresses(), 0);
    }

    #[test]
    fn forgets_idle_addresses_once_the_table_grows() {
        let limiter = RateLimiter::new(
            Quota {
                burst: 1.0,
                per_second: 1_000.0,
            },
            Quota {
                burst: 1.0,
                per_second: 1_000.0,
            },
        );
        let t0 = Instant::now();
        for n in 0..PRUNE_THRESHOLD {
            let addr = IpAddr::V4(Ipv4Addr::from((n as u32) + 1));
            assert!(limiter.allow_message(addr, t0));
        }
        assert_eq!(limiter.tracked_addresses(), PRUNE_THRESHOLD);

        // Everything above has refilled by now, so inserting one more address
        // should sweep the idle ones away instead of growing the table.
        let much_later = t0 + IDLE_BEFORE_PRUNE + Duration::from_secs(1);
        assert!(limiter.allow_message(ip(200), much_later));
        assert_eq!(limiter.tracked_addresses(), 1);
    }

    #[test]
    fn a_clock_that_jumps_backwards_does_not_mint_tokens() {
        let limiter = limiter();
        let t0 = Instant::now() + Duration::from_secs(10);
        assert!(limiter.allow_registration(ip(1), t0));
        assert!(limiter.allow_registration(ip(1), t0));
        let earlier = t0 - Duration::from_secs(5);
        assert!(!limiter.allow_registration(ip(1), earlier));
    }
}
