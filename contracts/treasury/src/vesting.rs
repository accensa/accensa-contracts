//! Vesting schedule model and math (issue #467).
//!
//! A schedule is a fixed token allocation that unlocks **linearly** from
//! `start` to `start + duration`, with nothing available until the **cliff**
//! at `start + cliff` has passed. The canonical team/investor allocation is a
//! one-year cliff followed by four years of linear release
//! ([`ONE_YEAR_SECS`] / [`FOUR_YEARS_SECS`]), so a quarter of the allocation
//! is unlocked the moment the cliff expires and the rest drips in over the
//! remaining three years.
//!
//! ```text
//! total              ┤                              ╭───────────
//!                    │                          ╭───╯
//!                    │                     ╭────╯
//!                    │             ╭───────╯
//!   0 ───────────────┴─────────────┴──────────────────────────────
//!                    start      start+cliff              start+duration
//! ```
//!
//! All arithmetic is checked: a schedule whose product of `total` and elapsed
//! time would overflow `i128` is rejected with [`Error::MathOverflow`] rather
//! than silently wrapping (the workspace compiles with `overflow-checks = true`
//! in dev, but release builds do not panic on overflow — so the check is
//! explicit here).

use crate::Error;
use soroban_sdk::{contracttype, Address};

/// Seconds in a 365-day year. The vesting contract measures time in Unix
/// seconds (`env.ledger().timestamp()`), not ledgers, so schedules keep their
/// meaning across a ledger-rate change.
pub const ONE_YEAR_SECS: u64 = 365 * 24 * 60 * 60;

/// The canonical four-year linear release window.
pub const FOUR_YEARS_SECS: u64 = 4 * ONE_YEAR_SECS;

/// A single beneficiary's token allocation and its claim bookkeeping.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VestingSchedule {
    /// The only address that may claim from this schedule.
    pub beneficiary: Address,
    /// Total allocation, in the token's smallest unit.
    pub total: i128,
    /// Amount already paid out by [`crate::Treasury::claim_vested`].
    pub claimed: i128,
    /// Unix timestamp the linear release is measured from.
    pub start: u64,
    /// Seconds after `start` before *anything* unlocks. May be `0` for a
    /// schedule with no cliff.
    pub cliff: u64,
    /// Length of the linear release window, in seconds. Must be `> 0` and
    /// `>= cliff`.
    pub duration: u64,
}

impl VestingSchedule {
    /// Whether the schedule's parameters form a valid allocation: a positive
    /// total, a non-zero release window, and a cliff that fits inside it.
    pub fn is_valid(&self) -> bool {
        self.total > 0 && self.duration > 0 && self.cliff <= self.duration
    }

    /// The cumulative amount unlocked at `now`, ignoring what has already been
    /// claimed.
    ///
    /// - `0` before the cliff (`now < start + cliff`);
    /// - `total` at or after the end (`now >= start + duration`);
    /// - otherwise `floor(total * (now - start) / duration)`.
    ///
    /// # Errors
    ///
    /// [`Error::MathOverflow`] if `start + cliff` or `start + duration` does
    /// not fit in a `u64`, or if scaling `total` by the elapsed time overflows
    /// `i128`.
    pub fn vested(&self, now: u64) -> Result<i128, Error> {
        let cliff_end = self
            .start
            .checked_add(self.cliff)
            .ok_or(Error::MathOverflow)?;
        if now < cliff_end {
            return Ok(0);
        }

        let end = self
            .start
            .checked_add(self.duration)
            .ok_or(Error::MathOverflow)?;
        if now >= end {
            return Ok(self.total);
        }

        let elapsed = now - self.start;
        self.total
            .checked_mul(elapsed as i128)
            .ok_or(Error::MathOverflow)
            .map(|scaled| scaled / self.duration as i128)
    }

    /// The amount unlocked now but not yet claimed.
    ///
    /// Saturates at zero: a stored `claimed` above the currently vested amount
    /// (only reachable if a schedule were mutated out of band) reports nothing
    /// claimable rather than a negative number.
    ///
    /// # Errors
    ///
    /// Propagates [`Error::MathOverflow`] from [`Self::vested`].
    pub fn claimable(&self, now: u64) -> Result<i128, Error> {
        Ok(self.vested(now)?.saturating_sub(self.claimed))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn schedule(
        total: i128,
        claimed: i128,
        start: u64,
        cliff: u64,
        duration: u64,
    ) -> VestingSchedule {
        let env = Env::default();
        VestingSchedule {
            beneficiary: Address::generate(&env),
            total,
            claimed,
            start,
            cliff,
            duration,
        }
    }

    #[test]
    fn is_valid_checks_all_three_rules() {
        // Valid baseline
        assert!(schedule(100, 0, 10, 5, 20).is_valid());

        // Rule 1: total must be > 0
        assert!(!schedule(0, 0, 10, 5, 20).is_valid());
        assert!(!schedule(-1, 0, 10, 5, 20).is_valid());

        // Rule 2: duration must be > 0
        assert!(!schedule(100, 0, 10, 0, 0).is_valid());

        // Rule 3: cliff must be <= duration
        assert!(schedule(100, 0, 10, 20, 20).is_valid());
        assert!(!schedule(100, 0, 10, 21, 20).is_valid());
    }

    #[test]
    fn vested_timeline_and_rounding() {
        let s = schedule(1_000, 0, 1_000, 200, 1_000);

        // Before cliff: 0 unlocked
        assert_eq!(s.vested(999).unwrap(), 0);
        assert_eq!(s.vested(1_000).unwrap(), 0);
        assert_eq!(s.vested(1_199).unwrap(), 0);

        // Exactly at cliff (now = start + cliff = 1200): 1000 * 200 / 1000 = 200
        assert_eq!(s.vested(1_200).unwrap(), 200);

        // Mid-window
        assert_eq!(s.vested(1_500).unwrap(), 500);

        // Exactly at end (now = start + duration = 2000) and past end: total unlocked
        assert_eq!(s.vested(2_000).unwrap(), 1_000);
        assert_eq!(s.vested(2_500).unwrap(), 1_000);

        // Mid-window rounding down: floor(total * elapsed / duration)
        let round_schedule = schedule(10, 0, 0, 0, 3);
        assert_eq!(round_schedule.vested(1).unwrap(), 3);
        assert_eq!(round_schedule.vested(2).unwrap(), 6);
        assert_eq!(round_schedule.vested(3).unwrap(), 10);
    }

    #[test]
    fn claimable_with_partial_claims_and_saturation() {
        let s = schedule(1_000, 150, 1_000, 200, 1_000);

        // At now = 1500, vested is 500. Claimable = 500 - 150 = 350
        assert_eq!(s.claimable(1_500).unwrap(), 350);

        // Saturates at zero when claimed > vested
        let overclaimed = schedule(1_000, 600, 1_000, 200, 1_000);
        assert_eq!(overclaimed.claimable(1_500).unwrap(), 0);

        // Before cliff, vested is 0, so any claimed > 0 saturates to 0
        let pre_cliff_claimed = schedule(1_000, 50, 1_000, 200, 1_000);
        assert_eq!(pre_cliff_claimed.claimable(1_100).unwrap(), 0);
    }

    #[test]
    fn vested_overflow_checks() {
        // start + cliff overflows u64
        let overflow_cliff = schedule(1_000, 0, u64::MAX - 5, 10, 20);
        assert_eq!(overflow_cliff.vested(u64::MAX), Err(Error::MathOverflow));

        // start + duration overflows u64 (cliff does not)
        let overflow_duration = schedule(1_000, 0, u64::MAX - 5, 2, 10);
        // now is past cliff (u64::MAX - 3) so it checks start + duration
        assert_eq!(
            overflow_duration.vested(u64::MAX - 2),
            Err(Error::MathOverflow)
        );
    }
}
