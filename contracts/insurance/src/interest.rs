//! Compound-style interest accrual for the insurance pool (issue #445).
//!
//! Fixed-point math at [`PRECISION`] (1e18), higher precision than the
//! treasury accumulator (1e9) because per-second interest rates are tiny.
//!
//! # Model
//!
//! - **Utilization.** `borrowed / supplied`, clamped to
//!   `[0, MAX_UTILIZATION_BPS]` (10 000 bps = 100%).
//! - **Algorithmic borrow rate.** `BASE_RATE_BPS + SLOPE_BPS * utilization`,
//!   converted to a per-second rate in [`PRECISION`] scale.
//! - **Supply rate.** The borrow rate scaled by utilization and reduced by
//!   the protocol reserve factor: `borrow_rate * util * (1 - reserve)`.
//! - **Index accrual.** `index * (1 + rate * dt)` in fixed point. Indexes
//!   start at [`PRECISION`] (1.0) so principal multiplies cleanly.

use crate::Error;

/// Fixed-point scale: 1.0 is `PRECISION`.
pub const PRECISION: i128 = 1_000_000_000_000_000_000; // 1e18
/// Seconds in a year (365 days).
pub const SECONDS_PER_YEAR: i128 = 31_536_000;
/// Protocol reserve factor in basis points (10%).
pub const RESERVE_FACTOR_BPS: i128 = 1_000;
/// Base borrow rate in annual basis points (2%).
pub const BASE_RATE_BPS: i128 = 200;
/// Slope in annual basis points at 100% utilization (100%).
pub const SLOPE_BPS: i128 = 10_000;
/// Maximum utilization in basis points.
pub const MAX_UTILIZATION_BPS: i128 = 10_000;

/// Utilization in basis points (0–10000), clamped.
///
/// Empty supply with any borrow reports full utilization; empty supply with
/// no borrow reports zero.
pub fn utilization_ratio(borrowed: i128, supplied: i128) -> i128 {
    if borrowed <= 0 {
        return 0;
    }
    if supplied <= 0 {
        return MAX_UTILIZATION_BPS;
    }
    let util = borrowed
        .checked_mul(MAX_UTILIZATION_BPS)
        .map(|v| v / supplied)
        .unwrap_or(MAX_UTILIZATION_BPS);
    util.clamp(0, MAX_UTILIZATION_BPS)
}

/// Algorithmic borrow rate: `base + slope * utilization`, returned as a
/// per-second rate in [`PRECISION`] scale.
pub fn borrow_rate(utilization_bps: i128) -> i128 {
    let util = utilization_bps.clamp(0, MAX_UTILIZATION_BPS);
    let annual_bps = match SLOPE_BPS
        .checked_mul(util)
        .map(|v| v / MAX_UTILIZATION_BPS)
        .and_then(|slope| BASE_RATE_BPS.checked_add(slope))
    {
        Some(v) => v,
        None => return 0,
    };
    let denom = 10_000i128.checked_mul(SECONDS_PER_YEAR).unwrap_or(1);
    annual_bps
        .checked_mul(PRECISION)
        .map(|v| v / denom)
        .unwrap_or(0)
}

/// Supply rate: `borrow_rate * utilization * (1 - reserve_factor)`, in
/// [`PRECISION`] scale.
pub fn supply_rate(borrow_rate: i128, utilization_bps: i128) -> i128 {
    let util = utilization_bps.clamp(0, MAX_UTILIZATION_BPS);
    let keep_bps = MAX_UTILIZATION_BPS - RESERVE_FACTOR_BPS;
    let denom = 10_000i128 * 10_000i128;
    borrow_rate
        .checked_mul(util)
        .and_then(|v| v.checked_mul(keep_bps))
        .map(|v| v / denom)
        .unwrap_or(0)
}

/// `new_borrow_index = borrow_index * (1 + borrow_rate * dt)` in fixed point.
pub fn accrue_interest(borrow_index: i128, borrow_rate: i128, dt: i128) -> Result<i128, Error> {
    if dt <= 0 {
        return Ok(borrow_index);
    }
    let growth = borrow_rate
        .checked_mul(dt)
        .ok_or(Error::ArithmeticOverflow)?;
    let factor = PRECISION
        .checked_add(growth)
        .ok_or(Error::ArithmeticOverflow)?;
    borrow_index
        .checked_mul(factor)
        .ok_or(Error::ArithmeticOverflow)
        .map(|v| v / PRECISION)
}

/// `new_supply_index = supply_index * (1 + supply_rate * dt)` in fixed point.
pub fn accrue_supply(supply_index: i128, supply_rate: i128, dt: i128) -> Result<i128, Error> {
    if dt <= 0 {
        return Ok(supply_index);
    }
    let growth = supply_rate
        .checked_mul(dt)
        .ok_or(Error::ArithmeticOverflow)?;
    let factor = PRECISION
        .checked_add(growth)
        .ok_or(Error::ArithmeticOverflow)?;
    supply_index
        .checked_mul(factor)
        .ok_or(Error::ArithmeticOverflow)
        .map(|v| v / PRECISION)
}
