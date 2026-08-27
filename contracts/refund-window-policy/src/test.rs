#![cfg(test)]

use crate::{RefundWindowPolicy, RefundWindowPolicyClient};
use accensa_common::Error;
use soroban_sdk::{testutils::Ledger, Bytes, BytesN, Env};

fn setup() -> (Env, RefundWindowPolicyClient<'static>) {
    let env = Env::default();
    let contract_id = env.register(RefundWindowPolicy, ());
    let client = RefundWindowPolicyClient::new(&env, &contract_id);
    (env, client)
}

#[test]
fn test_policy_name() {
    let (env, client) = setup();
    assert_eq!(
        client.get_policy_name(),
        Bytes::from_slice(&env, b"RefundWindowPolicy")
    );
}

#[test]
fn test_refund_allowed_within_window() {
    let (env, client) = setup();
    let payment_ref = payment_ref(&env, 1);
    // paid_at_ledger = 100, current = 100 -> within a 100-ledger window.
    assert!(client
        .try_check_refund(&payment_ref, &100, &100, &100, &0, &100)
        .is_ok());
}

#[test]
fn test_refund_allowed_at_last_inclusive_ledger() {
    let (env, client) = setup();
    let payment_ref = payment_ref(&env, 2);
    env.ledger().with_mut(|li| li.sequence_number = 200);
    // current (200) == paid_at(100) + window(100): 200 <= 200, still allowed.
    assert!(client
        .try_check_refund(&payment_ref, &100, &100, &100, &0, &100)
        .is_ok());
}

#[test]
fn test_refund_expired_past_window() {
    let (env, client) = setup();
    let payment_ref = payment_ref(&env, 3);
    env.ledger().with_mut(|li| li.sequence_number = 201);
    assert_eq!(
        client.try_check_refund(&payment_ref, &100, &100, &100, &0, &100),
        Err(Ok(Error::WindowExpired))
    );
}

#[test]
fn test_zero_window_is_unbounded() {
    let (env, client) = setup();
    let payment_ref = payment_ref(&env, 4);
    env.ledger().with_mut(|li| li.sequence_number = 9_999_999);
    assert!(client
        .try_check_refund(&payment_ref, &100, &0, &100, &0, &0)
        .is_ok());
}

#[test]
fn test_ignores_other_policy_inputs() {
    let (env, client) = setup();
    let payment_ref = payment_ref(&env, 5);
    // The window rule does not depend on amount/payment_amount/cumulative:
    // an amount above the ceiling still passes the *window* rule (the vault
    // enforces the ceiling itself).
    assert!(client
        .try_check_refund(&payment_ref, &i128::MAX, &100, &100, &i128::MAX, &100)
        .is_ok());
}

fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
    let mut arr = [0u8; 32];
    arr[0] = slot;
    BytesN::from_array(env, &arr)
}
