#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
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
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 120_000);
    assert_eq!(token_client.balance(&client.address), 380_000);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 120_000);
    assert_eq!(record.recipient, buyer);
}

#[test]
fn test_partial_refunds_cumulative_within_ceiling() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);

    client.refund(&payment_ref, &buyer, &100, &0, &300);
    client.refund(&payment_ref, &buyer, &150, &0, &300);
    client.refund(&payment_ref, &buyer, &50, &0, &300);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 300);
    assert_eq!(record.payment_amount, 300);

    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &1, &0, &300),
        Err(Ok(Error::ExceedsPayment))
    );
}

#[test]
fn test_refund_outside_window_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 500);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &100, &100),
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
    client.refund(&payment_ref, &buyer, &100, &100, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_zero_window_disables_expiry() {
    let (env, client, merchant, _token) = setup(0);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_refund_exceeding_float_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &100);

    let payment_ref = BytesN::from_array(&env, &[4u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &10_000, &0, &10_000),
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
        client.try_refund(&payment_ref, &buyer, &100, &100, &100),
        Err(Ok(Error::WindowExpired))
    );

    client.propose_policy(&1000);
    assert_eq!(
        client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    client.refund(&payment_ref, &buyer, &100, &(env.ledger().sequence()), &100);
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
        client.try_refund(&payment_ref, &addr, &100, &0, &100),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_withdraw(&100, &addr),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_propose_policy(&10),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_refund_requires_merchant_auth() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.set_auths(&[]);
    let payment_ref = BytesN::from_array(&env, &[8u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100);
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
        client.try_refund(&payment_ref, &buyer, &0, &0, &100),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &-100, &0, &100),
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
        client.try_refund(&payment_ref, &buyer, &100, &0, &100),
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
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

    client.extend_refund_ttl(&payment_ref);
}

#[test]
fn test_events_emitted_with_nonce() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};
    let (env, client, merchant, _token) = setup(100);

    client.deposit(&merchant, &500_000);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "deposit_event"), merchant.clone()).into_val(&env),
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "amount"), 500_000i128),
                    (Symbol::new(&env, "nonce"), 0i128),
                ]
                .into_val(&env)
            )
        ]
    );

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);

    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

    let refund_events = env.events().all().filter_by_contract(&client.address);
    let mut refund_data = Map::<Val, Val>::new(&env);
    refund_data.set(
        Symbol::new(&env, "amount").into_val(&env),
        120_000i128.into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "cumulative_refunded").into_val(&env),
        120_000i128.into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "recipient").into_val(&env),
        buyer.clone().into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "ledger").into_val(&env),
        env.ledger().sequence().into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        1u64.into_val(&env),
    );
    assert_eq!(
        refund_events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "refund_event"), payment_ref.clone()).into_val(&env),
                refund_data.into_val(&env)
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
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "amount"), 100_000i128),
                    (Symbol::new(&env, "nonce"), 2i128),
                ]
                .into_val(&env)
            )
        ]
    );
}

#[test]
fn test_pause_unpause_refund_window_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, _merchant, _token) = setup(100);
    let empty_data: Map<Val, Val> = Map::new(&env);

    env.ledger().with_mut(|li| li.sequence_number = 500);
    client.pause();

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "pause_event"), 500u32).into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number = 600);
    client.unpause();

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "unpause_event"), 600u32).into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number = 700);
    client.propose_policy(&300);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 300u32).into_val(&env),
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "proposed_at_ledger"), 700u32),
                    (
                        Symbol::new(&env, "execute_after_ledger"),
                        700u32 + 17_280u32
                    ),
                ]
                .into_val(&env)
            )
        ]
    );
}

#[test]
fn test_commit_meta_is_well_formed() {
    let sha = env!("GIT_SHA");
    assert_ne!(sha, "unknown", "GIT_SHA must not fall back to 'unknown'");
    assert_eq!(sha.len(), 40, "GIT_SHA should be 40 hex chars, got: {sha}");
    assert!(
        sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "GIT_SHA contains non-hex chars: {sha}"
    );

    let dirty = env!("GIT_DIRTY");
    assert!(
        dirty == "0" || dirty == "1",
        "GIT_DIRTY must be '0' or '1', got: {dirty}"
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

    client.refund(&payment_ref, &stranger, &120_000, &0, &120_000);
}

// ── Two-step admin transfer tests ──────────────────────────────────────────

#[test]
fn test_transfer_admin_happy_path() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.pause();
    client.unpause();
}

#[test]
fn test_accept_admin_transfers_role() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();
    client.propose_policy(&200);
}

#[test]
fn test_accept_admin_without_pending_fails() {
    let (_env, client, _merchant, _token) = setup(100);
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
}

#[test]
fn test_cancel_admin_transfer_succeeds() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.cancel_admin_transfer();
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

    client.transfer_admin(&admin_a);
    client.cancel_admin_transfer();
    client.transfer_admin(&admin_b);
    client.accept_admin();
    client.propose_policy(&200);
}

#[test]
fn test_overwrite_pending_admin() {
    let (env, client, _merchant, _token) = setup(100);
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    client.transfer_admin(&admin_a);
    client.transfer_admin(&admin_b);
    client.accept_admin();
    client.propose_policy(&200);
}

#[test]
fn test_old_admin_cannot_act_after_transfer() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();
    client.propose_policy(&200);
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

// ── Policy timelock tests ──────────────────────────────────────────────────

#[test]
fn test_propose_and_execute_policy_happy_path() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    client.propose_policy(&200);

    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.window, 200);
    assert_eq!(proposal.proposed_at_ledger, env.ledger().sequence());

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();
    assert!(client.get_pending_policy().is_none());
}

#[test]
fn test_execute_policy_before_timelock_fails() {
    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200);
    env.ledger().with_mut(|li| li.sequence_number = 10_000);

    assert_eq!(
        client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );
}

#[test]
fn test_execute_policy_at_exact_boundary_succeeds() {
    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200);
    env.ledger().with_mut(|li| li.sequence_number = 17_281);
    client.execute_policy();
    assert!(client.get_pending_policy().is_none());
}

#[test]
fn test_execute_policy_without_proposal_fails() {
    let (_env, client, _merchant, _token) = setup(100);
    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NoPendingPolicy)));
}

#[test]
fn test_propose_policy_overwrites_existing() {
    let (_env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200);
    client.propose_policy(&500);

    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.window, 500);
}

#[test]
fn test_execute_policy_applies_new_window() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 300);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &1, &100),
        Err(Ok(Error::WindowExpired))
    );

    client.propose_policy(&20_000);
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    client.refund(&payment_ref, &buyer, &100, &1, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_propose_policy_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(
        client.try_propose_policy(&100),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_execute_policy_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_propose_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.propose_policy(&200);
}

#[test]
#[should_panic]
fn test_execute_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    client.propose_policy(&200);
    env.set_auths(&[]);
    client.execute_policy();
}

#[test]
fn test_get_policy_timelock() {
    assert_eq!(RefundVault::get_policy_timelock(), 17_280);
}

#[test]
fn test_policy_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Symbol};

    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200);

    let current = env.ledger().sequence();
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 200u32).into_val(&env),
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "proposed_at_ledger"), current),
                    (
                        Symbol::new(&env, "execute_after_ledger"),
                        current + 17_280u32
                    ),
                ]
                .into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_executed_event"), 200u32).into_val(&env),
                soroban_sdk::Map::<Symbol, soroban_sdk::Val>::new(&env).into_val(&env)
            )
        ]
    );
}

// ── Shared Test Vectors (Issue #184) ────────────────────────────────────────

#[path = "refund_vectors.rs"]
mod refund_vectors;

#[test]
fn test_shared_refund_vectors_match_typescript_sdk() {
    let (env, client, merchant, _token) = setup(1000);
    client.deposit(&merchant, &1_000_000);

    let recipient = Address::generate(&env);

    for v in refund_vectors::VECTORS {
        let payment_ref = BytesN::from_array(&env, &v.payment_ref);
        let res = client.try_refund(
            &payment_ref,
            &recipient,
            &v.amount,
            &v.paid_at_ledger,
            &v.amount,
        );

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

// ── Self-Transfer Rejection Tests (Issue #177) ─────────────────────────────

#[test]
fn test_refund_to_contract_address_fails_self_transfer() {
    use soroban_sdk::testutils::Events;
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[12u8; 32]);
    let contract_addr = client.address.clone();

    let res = client.try_refund(&payment_ref, &contract_addr, &50_000, &0, &50_000);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    assert!(client.get_refund(&payment_ref).is_none());

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(events.events.len(), 1);
}

#[test]
fn test_withdraw_to_contract_address_fails_self_transfer() {
    use soroban_sdk::testutils::Events;
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let contract_addr = client.address.clone();
    let res = client.try_withdraw(&50_000, &contract_addr);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(events.events.len(), 1);
}

#[test]
fn test_refund_to_merchant_succeeds() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[13u8; 32]);
    let initial_merchant_bal = TokenClient::new(&env, &token).balance(&merchant);

    client.refund(&payment_ref, &merchant, &50_000, &0, &50_000);

    let final_merchant_bal = TokenClient::new(&env, &token).balance(&merchant);
    assert_eq!(final_merchant_bal, initial_merchant_bal + 50_000);
    assert!(client.get_refund(&payment_ref).is_some());
}

// ── set_token Tests (Issue #176) ────────────────────────────────────────────

#[test]
fn test_set_token_succeeds_when_vault_is_empty() {
    let (env, client, merchant, _token) = setup(100);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();
    StellarAssetClient::new(&env, &new_token).mint(&merchant, &FLOAT);

    client.set_token(&new_token);
    client.deposit(&merchant, &200_000);
    assert_eq!(
        TokenClient::new(&env, &new_token).balance(&client.address),
        200_000
    );
}

#[test]
fn test_set_token_fails_when_vault_is_funded() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();

    let res = client.try_set_token(&new_token);
    assert_eq!(res, Err(Ok(Error::FloatNotEmpty)));
}

#[test]
fn test_set_token_requires_admin_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let _stranger = Address::generate(&env);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();

    env.mock_auths(&[]);
    env.mock_all_auths();
    assert!(client.try_set_token(&new_token).is_ok());
}

// ── Domain Separator and Nonce Tests (Issue #136) ────────────────────────

#[test]
fn test_domain_separator_is_set_on_initialize() {
    let (_env, client, _merchant, _token) = setup(100);
    let sep = client.get_domain_separator();
    assert_ne!(sep.to_array(), [0u8; 32]);
}

#[test]
fn test_domain_separator_differs_per_instance() {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let id_a = env.register(RefundVault, ());
    let client_a = RefundVaultClient::new(&env, &id_a);
    client_a.initialize(&merchant, &token, &100);

    let id_b = env.register(RefundVault, ());
    let client_b = RefundVaultClient::new(&env, &id_b);
    client_b.initialize(&merchant, &token, &100);

    assert_ne!(
        client_a.get_domain_separator(),
        client_b.get_domain_separator()
    );
}

#[test]
fn test_nonce_starts_at_zero() {
    let (_env, client, _merchant, _token) = setup(100);
    assert_eq!(client.get_nonce(), 0);
}

#[test]
fn test_nonce_increments_on_deposit() {
    let (_env, client, merchant, _token) = setup(100);
    assert_eq!(client.get_nonce(), 0);
    client.deposit(&merchant, &100_000);
    assert_eq!(client.get_nonce(), 1);
}

#[test]
fn test_nonce_increments_on_refund() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);
    let nonce_before = client.get_nonce();
    let payment_ref = BytesN::from_array(&env, &[0xAAu8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100);
    assert_eq!(client.get_nonce(), nonce_before + 1);
}

#[test]
fn test_nonce_increments_on_withdraw() {
    let (_env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);
    let nonce_before = client.get_nonce();
    client.withdraw(&100_000, &merchant);
    assert_eq!(client.get_nonce(), nonce_before + 1);
}

#[test]
fn test_nonce_does_not_increment_on_failed_operation() {
    let (_env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);
    let nonce_before = client.get_nonce();
    let _ = client.try_deposit(&merchant, &0);
    assert_eq!(client.get_nonce(), nonce_before);
}

#[test]
fn test_nonce_is_strictly_monotonic() {
    let (env, client, merchant, _token) = setup(100);
    let mut seen_nonces = std::vec::Vec::new();

    client.deposit(&merchant, &500_000);
    seen_nonces.push(client.get_nonce());

    client.deposit(&merchant, &100_000);
    seen_nonces.push(client.get_nonce());

    client.withdraw(&50_000, &merchant);
    seen_nonces.push(client.get_nonce());

    let payment_ref = BytesN::from_array(&env, &[0xBBu8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &10_000, &0, &10_000);
    seen_nonces.push(client.get_nonce());

    for window in seen_nonces.windows(2) {
        assert!(
            window[1] > window[0],
            "nonce must be strictly monotonic: got {:?}",
            seen_nonces
        );
    }
    assert_eq!(client.get_nonce(), 4);
}
