use accensa_common::math::{add_amounts, mul_amounts, mul_ratio};
use soroban_sdk::{contracttype, Env};

#[contracttype]
pub struct SlashingConfig {
    pub penalty_percentage: u32,
}

pub fn calculate_slashing_penalty(_env: &Env, amount: i128, penalty: u32) -> i128 {
    if penalty > 100 {
        return amount; // or throw error
    }

    // `calculate_slashing_penalty` returns a plain `i128`, so it cannot
    // surface a typed `MathError`: the failure arm has to be a precondition
    // that keeps every operation provably in range. `penalty <= 100` (checked
    // above) supplies it. Decomposing `amount = whole * 100 + rest` keeps both
    // products bounded: `whole * penalty <= whole * 100 <= amount`, and
    // `|rest| <= 99` so the remainder product is at most `9_900`. Their sum
    // reconstructs at most `|amount|`. The checked helpers therefore never
    // trap on a host abort, and the `.expect` arms state the invariant rather
    // than clamping (issue #716).
    let whole = amount / 100;
    let rest = amount % 100;
    let whole_penalty =
        mul_amounts(whole, penalty as i128).expect("penalty <= 100, so whole * penalty <= amount");
    let rest_penalty = mul_ratio(rest, penalty as i128, 100)
        .expect("rest is within -99..=99 and penalty <= 100, so the product fits");
    add_amounts(whole_penalty, rest_penalty)
        .expect("whole_penalty + rest_penalty is at most |amount|")
}

/// Boundary tests for the guarded slashing-penalty computation (issue #716).
#[cfg(test)]
#[path = "slashing_overflow_tests.rs"]
mod slashing_overflow_tests;
