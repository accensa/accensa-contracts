#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

const FLOAT: i128 = 1_000_000;

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

#[test]
fn test_double_initialize_fails() {
    let (_env, client, merchant, token) = setup(100);
    assert_eq!(
        client.try_initialize(&merchant, &token, &100),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_deposit_moves_tokens_into_vault() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &600_000);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 600_000);
    assert_eq!(token_client.balance(&merchant), FLOAT - 600_000);
}

#[test]
fn test_deposit_from_non_merchant_fails() {
    let (env, client, _merchant, _token) = setup(100);
    let stranger = Address::generate(&env);
    assert_eq!(
        client.try_deposit(&stranger, &100),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_refund_happy_path() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &120_000, &0);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 120_000);
    assert_eq!(token_client.balance(&client.address), 380_000);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount, 120_000);
    assert_eq!(record.recipient, buyer);
}

#[test]
fn test_double_refund_same_payment_ref_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0);

    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &0),
        Err(Ok(Error::AlreadyRefunded))
    );
}

#[test]
fn test_refund_outside_window_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 500);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    // Paid at ledger 100 with a 100-ledger window: expired at 200, now 500.
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &100),
        Err(Ok(Error::WindowExpired))
    );
}

#[test]
fn test_refund_at_window_boundary_succeeds() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 200);

    let payment_ref = BytesN::from_array(&env, &[2u8; 32]);
    let buyer = Address::generate(&env);
    // current (200) == paid_at (100) + window (100): still inside the window.
    client.refund(&payment_ref, &buyer, &100, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_zero_window_disables_expiry() {
    let (env, client, merchant, _token) = setup(0);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_refund_exceeding_float_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &100);

    let payment_ref = BytesN::from_array(&env, &[4u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &10_000, &0),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_withdraw_returns_float_to_merchant() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);
    client.withdraw(&200_000, &merchant);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 300_000);
    assert_eq!(token_client.balance(&merchant), FLOAT - 300_000);
}

#[test]
fn test_withdraw_exceeding_float_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &100);
    assert_eq!(
        client.try_withdraw(&10_000, &merchant),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_set_refund_window_takes_effect() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 500);

    let payment_ref = BytesN::from_array(&env, &[5u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &100),
        Err(Ok(Error::WindowExpired))
    );

    client.set_refund_window(&1000);
    client.refund(&payment_ref, &buyer, &100, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_uninitialized_calls_fail() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    let addr = Address::generate(&env);
    let payment_ref = BytesN::from_array(&env, &[6u8; 32]);

    assert_eq!(
        client.try_deposit(&addr, &100),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &addr, &100, &0),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_withdraw(&100, &addr),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_set_refund_window(&10),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
#[should_panic]
fn test_refund_requires_merchant_auth() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    // Enforcing mode with no signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    let payment_ref = BytesN::from_array(&env, &[8u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0);
}
