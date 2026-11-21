use accensa_common::math::{add_amounts, mul_amounts, mul_ratio};
use accensa_common::Error;
use soroban_sdk::{contracttype, Env};

#[contracttype]
pub struct SlashingConfig {
    pub penalty_percentage: u32,
}

/// Returns the slashing penalty `(amount * penalty) / 100` for `penalty` in
/// `0..=100`.
///
/// Rejects a negative `amount` and any `penalty > 100` with
/// [`Error::InvalidAmount`]: a negative amount would turn a punitive call into
/// a payout, and a misconfigured percentage must be visible to the caller
/// rather than silently clamped to `100`. `amount == 0` returns `Ok(0)`.
pub fn calculate_slashing_penalty(_env: &Env, amount: i128, penalty: u32) -> Result<i128, Error> {
    if amount < 0 {
        return Err(Error::InvalidAmount);
    }
    if penalty > 100 {
        return Err(Error::InvalidAmount);
    }
    if amount == 0 {
        return Ok(0);
    }

    // `amount >= 0` and `penalty <= 100` bound every product by `amount`:
    // decomposing `amount = whole * 100 + rest` gives `whole * penalty <=
    // whole * 100 <= amount` and `0 <= rest <= 99`, so `mul_ratio(rest,
    // penalty, 100)` is at most 99 and the sum reconstructs at most `amount`.
    // The checked helpers therefore never trap, and the `.expect` arms state
    // the invariant rather than clamping.
    let whole = amount / 100;
    let rest = amount % 100;
    let whole_penalty =
        mul_amounts(whole, penalty as i128).expect("penalty <= 100, so whole * penalty <= amount");
    let rest_penalty = mul_ratio(rest, penalty as i128, 100)
        .expect("rest is within 0..=99 and penalty <= 100, so the product fits");
    Ok(add_amounts(whole_penalty, rest_penalty).expect("whole_penalty + rest_penalty <= amount"))
}

/// Boundary tests for the guarded slashing-penalty computation (issue #716).
#[cfg(test)]
#[path = "slashing_overflow_tests.rs"]
mod slashing_overflow_tests;
