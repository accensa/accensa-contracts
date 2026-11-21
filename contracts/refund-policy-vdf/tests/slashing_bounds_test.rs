//! Boundary coverage for `calculate_slashing_penalty` after issue #986
//! turned its silent clamps into typed rejections.

use refund_policy_vdf::calculate_slashing_penalty;
use soroban_sdk::Env;

#[test]
fn penalty_above_100_is_rejected() {
    let env = Env::default();
    // Before #986 these returned the full `amount` — a config of 150 percent
    // became a 100 percent slash rather than a rejected config.
    assert_eq!(
        calculate_slashing_penalty(&env, 1_000, 101),
        Err(accensa_common::Error::InvalidAmount)
    );
    assert_eq!(
        calculate_slashing_penalty(&env, 1_000, u32::MAX),
        Err(accensa_common::Error::InvalidAmount)
    );
}

#[test]
fn negative_amount_is_rejected_for_every_accepted_percentage() {
    let env = Env::default();
    // The percentage cannot go negative (it is `u32`), but `amount` can, and
    // a negative penalty on a punitive path is a payout.
    for penalty in [0u32, 1, 50, 99, 100] {
        assert_eq!(
            calculate_slashing_penalty(&env, -1, penalty),
            Err(accensa_common::Error::InvalidAmount),
            "amount = -1 at penalty = {penalty}"
        );
    }
}

#[test]
fn accepted_bounds_hit_the_expected_endpoints() {
    let env = Env::default();
    assert_eq!(calculate_slashing_penalty(&env, 1_000, 100), Ok(1_000));
    assert_eq!(calculate_slashing_penalty(&env, 1_000, 0), Ok(0));
    assert_eq!(calculate_slashing_penalty(&env, 0, 0), Ok(0));
    assert_eq!(calculate_slashing_penalty(&env, 0, 100), Ok(0));
    assert_eq!(calculate_slashing_penalty(&env, 0, 33), Ok(0));
}
