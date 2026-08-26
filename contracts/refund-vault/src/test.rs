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

#[test]
fn test_deposit_invalid_amount_fails() {
    let (_env, client, merchant, _token) = setup(100);
    assert_eq!(
        client.try_deposit(&merchant, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_deposit(&merchant, &-100),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_refund_invalid_amount_fails() {
    let (env, client, _merchant, _token) = setup(100);
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &0, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &-100, &0),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_withdraw_invalid_amount_fails() {
    let (_env, client, merchant, _token) = setup(100);
    assert_eq!(
        client.try_withdraw(&0, &merchant),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_withdraw(&-100, &merchant),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_pause_unpause() {
    let (_env, client, _merchant, _token) = setup(100);
    client.pause();
    client.unpause();
}

#[test]
fn test_deposit_when_paused_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.pause();
    assert_eq!(client.try_deposit(&merchant, &100), Err(Ok(Error::Paused)));
}

#[test]
fn test_refund_when_paused_fails() {
    let (env, client, _merchant, _token) = setup(100);
    client.pause();
    let payment_ref = BytesN::from_array(&env, &[10u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &0),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_withdraw_when_paused_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.pause();
    assert_eq!(client.try_withdraw(&100, &merchant), Err(Ok(Error::Paused)));
}

#[test]
#[should_panic]
fn test_pause_requires_merchant_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.pause();
}

#[test]
#[should_panic]
fn test_unpause_requires_merchant_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.unpause();
}

#[test]
fn test_extend_refund_ttl_fails_if_missing() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);
    let payment_ref = BytesN::from_array(&env, &[99u8; 32]);
    assert_eq!(
        client.try_extend_refund_ttl(&payment_ref),
        Err(Ok(Error::RefundNotFound))
    );
}

#[test]
fn test_extend_refund_ttl_succeeds() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &120_000, &0);

    // This shouldn't fail since the refund exists.
    client.extend_refund_ttl(&payment_ref);
}

#[test]
fn test_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Symbol};
    let (env, client, merchant, _token) = setup(100);

    client.deposit(&merchant, &500_000);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "deposit_event"), merchant.clone()).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 500_000i128)].into_val(&env)
            )
        ]
    );

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);

    client.refund(&payment_ref, &buyer, &120_000, &0);

    let refund_events = env.events().all().filter_by_contract(&client.address);
    let refund_record = client.get_refund(&payment_ref);
    assert_eq!(
        refund_events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "refund_event"), payment_ref.clone()).into_val(&env),
                refund_record.into_val(&env)
            )
        ]
    );

    client.withdraw(&100_000, &merchant);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "withdraw_event"), merchant.clone()).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 100_000i128)].into_val(&env)
            )
        ]
    );
}

#[test]
#[should_panic(expected = "HostError")]
fn test_refund_without_trustline() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[11u8; 32]);
    let stranger = Address::from_string(&soroban_sdk::String::from_str(
        &env,
        "GBJCHUKZMTFJWQYW2HX4XAZ2ZV7UYWV6X4XAZ2ZV7UYWV6X4XAZ2ZV7U",
    ));

    // stranger has no trustline.
    client.refund(&payment_ref, &stranger, &120_000, &0);
}

// ── Two-step admin transfer tests ──────────────────────────────────────────

#[test]
fn test_transfer_admin_happy_path() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // Admin hasn't changed yet — original admin can still act.
    client.pause();
    client.unpause();
}

#[test]
fn test_accept_admin_transfers_role() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    // New admin can call admin-only functions (set_refund_window needs no token balance).
    client.set_refund_window(&200);
}

#[test]
fn test_accept_admin_without_pending_fails() {
    let (_env, client, _merchant, _token) = setup(100);

    // No transfer initiated — accept should fail.
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
}

#[test]
fn test_cancel_admin_transfer_succeeds() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.cancel_admin_transfer();

    // After cancel, accept should fail.
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
}

#[test]
fn test_cancel_without_pending_fails() {
    let (_env, client, _merchant, _token) = setup(100);

    assert_eq!(
        client.try_cancel_admin_transfer(),
        Err(Ok(Error::NoPendingTransfer))
    );
}

#[test]
fn test_cancel_then_reinitiate_works() {
    let (env, client, _merchant, _token) = setup(100);
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    // Initiate to A, cancel, then initiate to B and accept.
    client.transfer_admin(&admin_a);
    client.cancel_admin_transfer();
    client.transfer_admin(&admin_b);
    client.accept_admin();

    // B is now admin — set_refund_window should work.
    client.set_refund_window(&200);
}

#[test]
fn test_overwrite_pending_admin() {
    let (env, client, _merchant, _token) = setup(100);
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    // Initiate to A, then re-initiate to B without cancelling.
    client.transfer_admin(&admin_a);
    client.transfer_admin(&admin_b);

    // Accept — B should become admin.
    client.accept_admin();
    client.set_refund_window(&200);
}

#[test]
fn test_old_admin_cannot_act_after_transfer() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    // New admin can call admin-only functions.
    client.set_refund_window(&200);
}

#[test]
fn test_transfer_admin_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    let addr = Address::generate(&env);

    assert_eq!(
        client.try_transfer_admin(&addr),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_cancel_admin_transfer_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(
        client.try_cancel_admin_transfer(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
#[should_panic]
fn test_transfer_admin_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    env.set_auths(&[]);
    client.transfer_admin(&new_admin);
}

#[test]
#[should_panic]
fn test_accept_admin_requires_pending_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // Clear all auths — pending_admin.require_auth() should panic.
    env.set_auths(&[]);
    client.accept_admin();
}

#[test]
#[should_panic]
fn test_cancel_admin_transfer_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    env.set_auths(&[]);
    client.cancel_admin_transfer();
}

#[test]
fn test_admin_transfer_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    let empty_data: Map<Val, Val> = Map::new(&env);
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "admin_transfer_initiated_event"),
                    merchant.clone(),
                    new_admin.clone()
                )
                    .into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    client.accept_admin();

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "admin_transfer_accepted_event"),
                    merchant.clone(),
                    new_admin.clone()
                )
                    .into_val(&env),
                empty_data.into_val(&env)
            )
        ]
    );
}

// ---------------------------------------------------------------------------
// Shared Test Vectors (Issue #184)
// ---------------------------------------------------------------------------

#[path = "refund_vectors.rs"]
mod refund_vectors;

#[test]
fn test_shared_refund_vectors_match_typescript_sdk() {
    let (env, client, merchant, _token) = setup(1000);
    client.deposit(&merchant, &1_000_000);

    let recipient = Address::generate(&env);

    for v in refund_vectors::VECTORS {
        let payment_ref = BytesN::from_array(&env, &v.payment_ref);
        let res = client.try_refund(&payment_ref, &recipient, &v.amount, &v.paid_at_ledger);

        assert_eq!(
            res.is_ok(),
            v.expected_success,
            "vector {:?}: contract returned is_ok={}, expected={}",
            v.name,
            res.is_ok(),
            v.expected_success
        );
    }
}

#[test]
fn test_shared_refund_vectors_cover_both_outcomes() {
    assert!(refund_vectors::VECTORS.iter().any(|v| v.expected_success));
    assert!(refund_vectors::VECTORS.iter().any(|v| !v.expected_success));
}

#[test]
fn test_shared_refund_vectors_include_live_testnet_refund() {
    let live = &refund_vectors::VECTORS[0];
    assert!(live.expected_success);
    assert!(live.tx_hash.is_some());
}
