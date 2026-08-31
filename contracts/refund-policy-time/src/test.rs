#![cfg(test)]

use super::*;
use accensa_common::{PolicyContext, RefundPolicyClient, TimePolicyParams};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    xdr::ToXdr,
    Address, Bytes, BytesN, Env,
};

fn env_with_ledger(sequence: u32, timestamp: u64) -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = sequence;
        li.timestamp = timestamp;
    });
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    StellarAssetClient::new(&env, &sac.address());
    env
}

fn register(env: &Env) -> RefundPolicyClient<'_> {
    let id = env.register(TimePolicy, ());
    RefundPolicyClient::new(env, &id)
}

fn params(env: &Env, window: u32, deadline: u64) -> Bytes {
    TimePolicyParams { window, deadline }.to_xdr(env)
}

fn ctx(env: &Env, paid_at_ledger: u32, payment_ref: &BytesN<32>) -> PolicyContext {
    PolicyContext {
        payment_ref: payment_ref.clone(),
        amount: 100,
        paid_at_ledger,
        current_ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
        vdf_proof: None,
    }
}

#[test]
fn rejects_claim_outside_window() {
    let env = env_with_ledger(101, 0);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let result = client.try_evaluate(&params(&env, 100, 0), &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Err(Ok(Error::WindowExpired)));
}

#[test]
fn admits_claim_inside_window() {
    let env = env_with_ledger(100, 0);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[2u8; 32]);
    let result = client.try_evaluate(&params(&env, 100, 0), &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Ok(Ok(())));
}

#[test]
fn window_is_relative_to_paid_at_ledger() {
    let env = env_with_ledger(250, 0);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let result = client.try_evaluate(&params(&env, 100, 0), &ctx(&env, 150, &payment_ref));
    assert_eq!(result, Ok(Ok(())));
}

#[test]
fn rejects_claim_after_deadline() {
    let env = env_with_ledger(0, 10_000);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[4u8; 32]);
    let result = client.try_evaluate(&params(&env, 0, 9_999), &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Err(Ok(Error::RefundExpired)));
}

#[test]
fn admits_claim_exactly_on_deadline() {
    let env = env_with_ledger(0, 9_999);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[5u8; 32]);
    let result = client.try_evaluate(&params(&env, 0, 9_999), &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Ok(Ok(())));
}

#[test]
fn disabled_gates_admit_every_claim() {
    let env = env_with_ledger(10_000, 100_000);
    let client = register(&env);
    let payment_ref = BytesN::from_array(&env, &[6u8; 32]);
    let result = client.try_evaluate(&params(&env, 0, 0), &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Ok(Ok(())));
}

#[test]
fn rejects_params_that_do_not_decode() {
    let env = env_with_ledger(0, 0);
    let client = register(&env);
    let wrong_type = 7u32.to_xdr(&env);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let result = client.try_evaluate(&wrong_type, &ctx(&env, 0, &payment_ref));
    assert_eq!(result, Err(Ok(Error::InvalidPolicyParams)));
}
