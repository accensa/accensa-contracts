#![cfg(test)]

//! Test verifying that verify_vdf handles uninitialized/zero divisor safely
//! and does not panic on NonZero unwrap.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, VdfPolicyParams};
use refund_policy_vdf::VdfPolicy;
use soroban_sdk::{testutils::EnvTestConfig, xdr::ToXdr, Bytes, BytesN, Env};

#[test]
fn test_verify_vdf_with_zero_delay_succeeds() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let contract_id = env.register(VdfPolicy, ());
    let client = RefundPolicyClient::new(&env, &contract_id);
    let params = VdfPolicyParams { delay: 0 }.to_xdr(&env);
    let ctx = PolicyContext {
        amount: 100,
        merchant: soroban_sdk::Address::generate(&env),
        paid_at_ledger: 10,
        payment_amount: 100,
        payment_ref: BytesN::from_array(&env, &[1u8; 32]),
        recipient: soroban_sdk::Address::generate(&env),
        vdf_proof: None,
    };
    assert_eq!(client.try_evaluate(&params, &ctx), Ok(Ok(())));
}
