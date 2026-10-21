//! Boundary tests for the guarded growth product and sum in
//! `record_completion` (issue #725).
//!
//! `record_completion` is a `Result` path, but this crate has no arithmetic
//! overflow variant and the issue keeps the error enum out of scope, so the
//! guards rest on stated preconditions: `headroom` is capped at `MAX_SCORE`
//! before it is multiplied, and `GROWTH_RATE_BPS` (2_500) sits under its
//! 10_000 denominator, so `growth <= headroom` and the sum never passes
//! `MAX_SCORE`. The `MAX_SCORE - old_score` subtraction immediately above the
//! guards (outside this issue's scope) aborts any call whose stored score is
//! above `MAX_SCORE`, so `old_score == MAX_SCORE` is the largest operand those
//! two lines can ever be handed. These tests drive both operands to their
//! reachable maximum through the public entry point and pin the returned
//! `Result` arm instead of a panic or a host abort.

extern crate std;

use super::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig};

struct Setup {
    env: Env,
    client: ReputationClient<'static>,
    contract_id: Address,
    buyer: Address,
}

fn setup() -> Setup {
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

    Setup {
        env,
        client,
        contract_id,
        buyer,
    }
}

/// Force the stored score for the fixture's buyer to `score`, with
/// `last_update` on the current ledger so no decay rewrites the operand
/// before the growth curve reads it.
fn force_score(s: &Setup, score: u64) {
    s.env.as_contract(&s.contract_id, || {
        credit_score::store(
            &s.env,
            &s.buyer,
            &CreditRecord {
                score,
                completions: 0,
                fraud_count: 0,
                last_update: s.env.ledger().sequence(),
            },
        );
    });
}

#[test]
fn completion_with_score_at_max_score_lands_the_sum_on_the_cap() {
    let s = setup();
    force_score(&s, MAX_SCORE);

    // Tightest reachable boundary for the guarded addition: `old_score` is
    // the largest operand the two guard lines can see, headroom is 0, growth
    // is 0, and the sum sits exactly on `MAX_SCORE`. Assert the returned
    // `Result` arm — success with the exact value — rather than a panic from
    // the release profile's overflow trap.
    assert_eq!(
        s.client.try_record_completion(&s.buyer, &1),
        Ok(Ok(MAX_SCORE))
    );

    let record = s.client.get_credit_record(&s.buyer).unwrap();
    assert_eq!(record.score, MAX_SCORE);
    assert_eq!(record.completions, 1);
}

#[test]
fn completion_with_maximal_headroom_computes_the_largest_growth() {
    let s = setup();
    force_score(&s, 0);

    // Largest product the guarded multiplication ever computes:
    // `MAX_SCORE * GROWTH_RATE_BPS` = 1_000 * 2_500 = 2_500_000 in `u128`,
    // / 10_000 = 250 — exactly 25% of the full headroom, added to a score of
    // 0. Again the typed `Ok` arm, not a host abort.
    assert_eq!(s.client.try_record_completion(&s.buyer, &1), Ok(Ok(250)));

    let record = s.client.get_credit_record(&s.buyer).unwrap();
    assert_eq!(record.score, 250);
}
