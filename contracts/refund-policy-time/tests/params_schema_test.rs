//! A params blob of the wrong shape must be rejected as
//! `Error::InvalidPolicyParams`, never accepted and never raised as a host
//! error (issue #978).
//!
//! `TimePolicy::evaluate` picks the params schema by shape — it decodes to a
//! `Map<Symbol, Val>`, branches on the presence of an `oracle` key, and
//! otherwise decodes `TimePolicyParams` — because decoding straight into the
//! wrong struct traps the host instead of returning `Err`. Only one negative
//! case existed, a bare `u32` that fails at the first decode step and so never
//! reaches a shape check. The rows below are the cases that *do* reach it: a
//! well-formed map belonging to another policy, maps missing a field, an empty
//! map, and an `oracle` key holding something that is not an address.
//!
//! Mixing the two policy addresses is a realistic operator error — the VDF
//! policy crate produces exactly the first blob — and both crates are deployed
//! against the same vault entry point, so the shape of `params` is the only
//! thing telling them apart.
//!
//! Every row asserts through `try_evaluate`, where `Err(Ok(..))` is a policy
//! verdict and `Err(Err(..))` is a host failure; the helper panics on the
//! latter, so a trap cannot pass as a rejection.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams, VdfPolicyParams};
use refund_policy_time::oracle::TimeOraclePolicyParams;
use soroban_sdk::{
    symbol_short, testutils::Ledger, xdr::ToXdr, Address, Bytes, BytesN, Env, Map, Symbol, Val,
};

const PAYMENT_REF: [u8; 32] = [13u8; 32];
const WINDOW: u32 = 100;

fn setup() -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let policy = env.register(refund_policy_time::TimePolicy, ());
    env.ledger().set_sequence_number(1_000);
    env.ledger().set_timestamp(0);
    (env, policy)
}

fn val<T: soroban_sdk::IntoVal<Env, Val>>(env: &Env, value: T) -> Val {
    value.into_val(env)
}

fn field(env: &Env, key: Symbol, value: Val) -> Map<Symbol, Val> {
    let mut map = Map::<Symbol, Val>::new(env);
    map.set(key, value);
    map
}

/// Evaluate `params` for an in-window claim and report the verdict.
fn verdict(env: &Env, policy: &Address, params: &Bytes) -> Result<(), Error> {
    let ctx = PolicyContext {
        payment_ref: BytesN::from_array(env, &PAYMENT_REF),
        amount: 100,
        // `current_ledger` is 1_000, so this claim is inside `WINDOW`: a shape
        // that were merely tolerated would come back `Ok(())` rather than an
        // error, which is what makes the assertion below discriminating.
        paid_at_ledger: 1_000,
        current_ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
        vdf_proof: None,
    };
    match RefundPolicyClient::new(env, policy).try_evaluate(params, &ctx) {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        other => panic!("expected a policy verdict, got {other:?}"),
    }
}

#[test]
fn a_vdf_params_blob_is_rejected() {
    // The blob the sibling VDF policy produces: a well-formed map, wrong shape.
    let (env, policy) = setup();
    let params = VdfPolicyParams { delay: 10 }.to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn a_map_missing_window_is_rejected() {
    let (env, policy) = setup();
    let params = field(&env, symbol_short!("deadline"), val(&env, 0u64)).to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn an_empty_map_is_rejected() {
    let (env, policy) = setup();
    let params = Map::<Symbol, Val>::new(&env).to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn an_oracle_key_holding_a_non_address_is_rejected() {
    // Reaching the oracle branch is not the same as being valid params: the
    // `oracle` field has to decode as an `Address`.
    let (env, policy) = setup();
    let params = field(&env, symbol_short!("oracle"), val(&env, 7u32)).to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn an_oracle_shape_missing_window_is_rejected() {
    let (env, policy) = setup();
    // `max_report_age` is longer than a small symbol allows, so it is built
    // the way the derive expansion of `TimeOraclePolicyParams` builds it.
    let mut params_map = field(&env, symbol_short!("oracle"), val(&env, policy.clone()));
    params_map.set(Symbol::new(&env, "max_report_age"), val(&env, 3_600u64));
    let params = params_map.to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn a_time_shape_with_a_non_numeric_window_is_rejected() {
    // A `window` that is present but is a symbol rather than a `u32`.
    let (env, policy) = setup();
    let mut params_map = field(
        &env,
        symbol_short!("window"),
        val(&env, symbol_short!("lots")),
    );
    params_map.set(symbol_short!("deadline"), val(&env, 0u64));
    let params = params_map.to_xdr(&env);
    assert_eq!(
        verdict(&env, &policy, &params),
        Err(Error::InvalidPolicyParams)
    );
}

#[test]
fn the_two_supported_shapes_still_decode() {
    // The control that keeps the rows above honest: a valid `TimePolicyParams`
    // and a valid `TimeOraclePolicyParams` are both accepted for the same
    // in-window claim, so only foreign shapes are rejected.
    let (env, policy) = setup();
    let plain = TimePolicyParams {
        window: WINDOW,
        deadline: 0,
    }
    .to_xdr(&env);
    assert_eq!(verdict(&env, &policy, &plain), Ok(()));

    let oracle_params = TimeOraclePolicyParams {
        window: WINDOW,
        deadline: 0,
        // A second policy instance: not this contract, so no self-reference,
        // and it has no `get_delivery_status`, so the claim falls back to the
        // window it is inside of.
        oracle: env.register(refund_policy_time::TimePolicy, ()),
        max_report_age: 3_600,
    }
    .to_xdr(&env);
    assert_eq!(verdict(&env, &policy, &oracle_params), Ok(()));
}
