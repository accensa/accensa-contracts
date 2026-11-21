//! Params that point the oracle at the policy itself must be rejected before
//! anything is dispatched (issue #977).
//!
//! `resolve` calls whatever address sits in `TimeOraclePolicyParams::oracle`.
//! When that address is the policy itself the call cannot reach a delivery
//! report — `TimePolicy` exports only `evaluate` — so it failed at dispatch
//! and `resolve` mapped the failure to a fallback. The configuration error was
//! therefore reported as the clock verdict instead: `Ok(())` for an in-window
//! claim, `WindowExpired` for a late one, and with `window` and `deadline`
//! both zero an unconditional admission for a claim that has no usable report.
//!
//! The guard turns that into `InvalidPolicyParams` ahead of the dispatch, which
//! is also the observation that no call went out: the pre-guard verdicts above
//! are exactly what a fallback produces, and the same params with `window: 0`
//! now report the configuration instead of the clock.

use accensa_common::{Error, PolicyContext, RefundPolicyClient};
use refund_policy_time::oracle::TimeOraclePolicyParams;
use soroban_sdk::{testutils::Ledger, xdr::ToXdr, Address, Bytes, BytesN, Env};

const PAYMENT_REF: [u8; 32] = [11u8; 32];
const WINDOW: u32 = 100;

fn setup(sequence: u32) -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let policy = env.register(refund_policy_time::TimePolicy, ());
    env.ledger().set_sequence_number(sequence);
    env.ledger().set_timestamp(0);
    (env, policy)
}

fn params(env: &Env, oracle: &Address, window: u32) -> Bytes {
    TimeOraclePolicyParams {
        window,
        deadline: 0,
        oracle: oracle.clone(),
        max_report_age: 3_600,
    }
    .to_xdr(env)
}

fn verdict(
    env: &Env,
    policy: &Address,
    oracle: &Address,
    window: u32,
    paid: u32,
) -> Result<(), Error> {
    let ctx = PolicyContext {
        payment_ref: BytesN::from_array(env, &PAYMENT_REF),
        amount: 100,
        paid_at_ledger: paid,
        current_ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
        vdf_proof: None,
    };
    match RefundPolicyClient::new(env, policy).try_evaluate(&params(env, oracle, window), &ctx) {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        other => panic!("expected a policy verdict, got {other:?}"),
    }
}

#[test]
fn a_policy_pointed_at_itself_is_rejected() {
    let (env, policy) = setup(1_000);
    // In-window and otherwise admissible: the configuration alone decides.
    assert_eq!(
        verdict(&env, &policy, &policy, WINDOW, 1_000),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn the_rejection_holds_when_the_clock_gates_are_disabled() {
    // `window = 0` would make the fallback path admit every claim, so this
    // pins the guard ahead of the gates rather than as a third gate after them.
    let (env, policy) = setup(1_000);
    assert_eq!(
        verdict(&env, &policy, &policy, 0, 1_000),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn a_late_claim_against_itself_is_still_a_params_error() {
    // The order of checks is the guard first, gates second: an outside-the-
    // window claim gets the configuration error, not `WindowExpired`.
    let (env, policy) = setup(1_000);
    assert_eq!(
        verdict(&env, &policy, &policy, WINDOW, 1_000 - WINDOW - 1),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn an_unrelated_contract_address_still_reaches_the_clock_gates() {
    // The control, proving the guard is specific: a second policy instance in
    // the same ledger has no `get_delivery_status`, so the oracle call fails
    // and the claim is judged by the window exactly as before the guard.
    let (env, policy) = setup(1_000);
    let stand_in = env.register(refund_policy_time::TimePolicy, ());
    assert_ne!(stand_in, policy);
    assert_eq!(verdict(&env, &policy, &stand_in, WINDOW, 1_000), Ok(()));
    assert_eq!(
        verdict(&env, &policy, &stand_in, WINDOW, 1_000 - WINDOW - 1),
        Err(Error::WindowExpired)
    );
}
