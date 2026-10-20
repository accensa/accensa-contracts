//! Boundary tests for the guarded slashing-penalty computation (issue #716).
//!
//! `calculate_slashing_penalty` returns a plain `i128`, so it cannot surface a
//! typed `MathError`. The guard is a stated precondition instead: `penalty >
//! 100` returns early, and for `penalty <= 100` the decomposition
//! `amount = whole * 100 + rest` bounds both products by `amount`. These tests
//! drive the type's maximum, where the old bare `amount * penalty` trapped.

use soroban_sdk::Env;

use crate::calculate_slashing_penalty;

#[test]
fn penalty_at_i128_max_does_not_trap() {
    let env = Env::default();
    // A bare `amount * penalty` traps at `i128::MAX`; the checked pipeline
    // must return the exact penalty instead.
    assert_eq!(calculate_slashing_penalty(&env, i128::MAX, 100), i128::MAX);
    assert_eq!(
        calculate_slashing_penalty(&env, i128::MAX, 1),
        i128::MAX / 100
    );
}

#[test]
fn penalty_above_100_keeps_the_full_amount() {
    let env = Env::default();
    assert_eq!(calculate_slashing_penalty(&env, 1_000, 101), 1_000);
}

#[test]
fn rounding_matches_the_original_truncating_division() {
    let env = Env::default();
    assert_eq!(calculate_slashing_penalty(&env, 999, 10), 99);
    assert_eq!(calculate_slashing_penalty(&env, -999, 10), -99);
    assert_eq!(calculate_slashing_penalty(&env, i128::MIN, 100), i128::MIN);
}
