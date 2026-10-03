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
