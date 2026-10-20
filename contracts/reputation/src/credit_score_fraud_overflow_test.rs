//! Boundary test for the guarded penalty product in `record_fraud`
//! (issue #726).
//!
//! `record_fraud` is a `Result` path, but this crate has no arithmetic
//! overflow variant and the issue keeps the error enum out of scope. The
//! guard therefore rests on a stated precondition: `old_score` and
//! `PENALTY_RATE_BPS` are both `u64`, and `(2^64 - 1)^2 < 2^128`, so their
//! product always fits in `u128` and the checked multiplication cannot take
//! its failure arm. This test drives the largest score the multiplication can
//! ever see and pins that the host call returns the exact value instead of
//! trapping or wrapping.

extern crate std;

use super::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig};

#[test]
fn fraud_at_u64_max_returns_the_exact_score_without_overflow() {
    // Snapshots off: this suite pins behavior with explicit assertions.
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    client.initialize(&admin);

    // Force the stored score to `u64::MAX` — the largest operand the penalty
    // product can be handed. `last_update` is the current ledger so no decay
    // rewrites the operand before the penalty is computed.
    env.as_contract(&contract_id, || {
        credit_score::store(
            &env,
            &buyer,
            &CreditRecord {
                score: u64::MAX,
                completions: 0,
                fraud_count: 0,
                last_update: env.ledger().sequence(),
            },
        );
    });

    // 5_000 bps of `u64::MAX`, rounded down, then subtracted.
    let expected_penalty = (u64::MAX as u128 * PENALTY_RATE_BPS as u128 / 10_000) as u64;
    let expected = u64::MAX - expected_penalty;

    // Assert the returned `Result` arm: success with the exact score, not a
    // host abort from the release profile's overflow trap.
    assert_eq!(client.try_record_fraud(&buyer), Ok(Ok(expected)));

    let record = client.get_credit_record(&buyer).unwrap();
    assert_eq!(record.score, expected);
    assert_eq!(record.fraud_count, 1);
}
