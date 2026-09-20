//! Reconnect pacing for the relay client.
//!
//! The Electron original retried on a fixed 1.5 s timer, so a relay that went
//! down took a synchronised 40-requests-per-minute battering from every desk
//! that had ever connected to it. This backs off exponentially and jitters each
//! delay so that a fleet coming back after an outage spreads itself out instead
//! of reconnecting in lockstep.

use std::time::Duration;

/// First reconnect delay. Short enough that a dropped socket on a healthy link
/// is invisible to the user.
pub const BASE_DELAY: Duration = Duration::from_millis(500);

/// Longest reconnect delay. A desk that has been offline for a while should
/// still come back within half a minute of the relay returning.
pub const MAX_DELAY: Duration = Duration::from_secs(30);

/// Fraction of each delay that is fixed; the rest is jitter.
///
/// 0.75 is not arbitrary. The delay for attempt *n* lies in
/// `[0.75·2ⁿ·base, 2ⁿ·base]`, and `0.75 · 2 = 1.5 > 1`, so every draw for
/// attempt *n+1* is strictly larger than every draw for attempt *n*. The
/// schedule is therefore genuinely monotonic however the dice fall, which
/// "full jitter" (`[0, 2ⁿ·base]`) is not.
const FIXED_FRACTION: f64 = 0.75;

/// Exponential-with-jitter delay sequence, capped.
#[derive(Debug, Clone)]
pub struct Backoff {
    attempt: u32,
    base: Duration,
    cap: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(BASE_DELAY, MAX_DELAY)
    }
}

impl Backoff {
    /// Builds a schedule doubling from `base` up to `cap`.
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            attempt: 0,
            base,
            cap,
        }
    }

    /// Forgets the failure history, so the next delay is the first one again.
    /// Called once a connection has actually been established.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// How many failures have been counted since the last [`reset`](Self::reset).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The next delay, using a fresh random jitter.
    pub fn next_delay(&mut self) -> Duration {
        self.next_delay_with_jitter(rand::random::<f64>())
    }

    /// The next delay for a caller-supplied jitter draw in `[0, 1]`.
    ///
    /// Split out from [`next_delay`](Self::next_delay) so the schedule can be
    /// tested at both extremes of the jitter range instead of statistically.
    pub fn next_delay_with_jitter(&mut self, unit: f64) -> Duration {
        // A hostile or buggy caller must not turn into a negative delay.
        let unit = unit.clamp(0.0, 1.0);

        // Saturating shift: after ~30 attempts the doubling would overflow, and
        // the cap makes the exact value irrelevant anyway.
        let ceiling = self
            .base
            .checked_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX))
            .unwrap_or(self.cap)
            .min(self.cap);

        self.attempt = self.attempt.saturating_add(1);

        let scale = FIXED_FRACTION + (1.0 - FIXED_FRACTION) * unit;
        ceiling.mul_f64(scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every jitter draw for attempt n+1 must exceed every draw for attempt n,
    /// so a flapping relay always gets more breathing room, never less.
    #[test]
    fn the_schedule_is_monotonic_whatever_the_jitter_draws() {
        let mut luckiest = Backoff::default();
        let mut unluckiest = Backoff::default();

        let mut previous_max = Duration::ZERO;
        for _ in 0..6 {
            let low = luckiest.next_delay_with_jitter(0.0);
            let high = unluckiest.next_delay_with_jitter(1.0);
            assert!(low <= high, "{low:?} should not exceed {high:?}");
            assert!(
                low > previous_max,
                "attempt floor {low:?} must beat the previous ceiling {previous_max:?}"
            );
            previous_max = high;
        }
    }

    #[test]
    fn jitter_actually_moves_the_delay() {
        let mut low = Backoff::default();
        let mut high = Backoff::default();
        assert_ne!(
            low.next_delay_with_jitter(0.0),
            high.next_delay_with_jitter(1.0)
        );
    }

    #[test]
    fn delays_never_exceed_the_cap() {
        let mut backoff = Backoff::default();
        for _ in 0..200 {
            assert!(backoff.next_delay_with_jitter(1.0) <= MAX_DELAY);
        }
    }

    /// The doubling would overflow a `Duration` long before attempt 200; the
    /// saturating shift must clamp rather than wrap back to a tight retry loop.
    #[test]
    fn a_very_long_outage_does_not_wrap_back_to_a_tight_loop() {
        let mut backoff = Backoff::default();
        for _ in 0..1000 {
            backoff.next_delay_with_jitter(0.5);
        }
        let delay = backoff.next_delay_with_jitter(0.0);
        assert!(
            delay >= MAX_DELAY.mul_f64(FIXED_FRACTION),
            "clamped delay {delay:?} collapsed"
        );
    }

    #[test]
    fn a_successful_connection_resets_the_schedule() {
        let mut backoff = Backoff::default();
        let first = backoff.next_delay_with_jitter(0.0);
        backoff.next_delay_with_jitter(0.0);
        backoff.next_delay_with_jitter(0.0);
        assert_eq!(backoff.attempt(), 3);

        backoff.reset();
        assert_eq!(backoff.attempt(), 0);
        assert_eq!(backoff.next_delay_with_jitter(0.0), first);
    }

    #[test]
    fn a_jitter_draw_outside_the_unit_range_is_clamped() {
        let mut backoff = Backoff::default();
        let low = backoff.next_delay_with_jitter(-5.0);
        backoff.reset();
        let floor = backoff.next_delay_with_jitter(0.0);
        assert_eq!(low, floor);

        backoff.reset();
        let high = backoff.next_delay_with_jitter(9.0);
        backoff.reset();
        assert_eq!(high, backoff.next_delay_with_jitter(1.0));
    }
}
