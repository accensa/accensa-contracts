//! Boundary tests for the guarded slashing-penalty computation (issue #716).
//!
//! After #986 the helper returns `Result<i128, Error>`: `amount < 0` and
//! `penalty > 100` are rejected with `Error::InvalidAmount` rather than
//! silently clamped. For an accepted `penalty <= 100` the decomposition
//! `amount = whole * 100 + rest` still bounds both products by `amount`, so
//! the checked pipeline returns the exact penalty where a bare `amount *
//! penalty` would trap at the type's maximum.

use soroban_sdk::Env;

use crate::calculate_slashing_penalty;

#[test]
fn penalty_at_i128_max_does_not_trap() {
    let env = Env::default();
    // A bare `amount * penalty` traps at `i128::MAX`; the checked pipeline
    // must return the exact penalty instead.
    assert_eq!(
        calculate_slashing_penalty(&env, i128::MAX, 100),
        Ok(i128::MAX)
    );
    assert_eq!(
        calculate_slashing_penalty(&env, i128::MAX, 1),
        Ok(i128::MAX / 100)
    );
}

#[test]
fn penalty_above_100_is_rejected() {
    let env = Env::default();
    assert_eq!(
        calculate_slashing_penalty(&env, 1_000, 101),
        Err(accensa_common::Error::InvalidAmount)
    );
}

#[test]
fn negative_amount_is_rejected() {
    let env = Env::default();
    assert_eq!(
        calculate_slashing_penalty(&env, -999, 10),
        Err(accensa_common::Error::InvalidAmount)
    );
    assert_eq!(
        calculate_slashing_penalty(&env, i128::MIN, 100),
        Err(accensa_common::Error::InvalidAmount)
    );
}

#[test]
fn rounding_matches_the_original_truncating_division() {
    let env = Env::default();
    assert_eq!(calculate_slashing_penalty(&env, 999, 10), Ok(99));
    assert_eq!(calculate_slashing_penalty(&env, 999, 33), Ok(329));
}
