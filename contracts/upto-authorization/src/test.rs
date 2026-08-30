#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, IntoVal, Symbol, Val,
};

const TOKEN_SUPPLY: i128 = 10_000_000;

fn setup() -> (
    Env,
    UptoAuthorizationClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();

    StellarAssetClient::new(&env, &token).mint(&buyer, &TOKEN_SUPPLY);

    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    client.initialize(&admin, &token);

    (env, client, admin, buyer, seller, token)
}

fn pid(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

// ── Initialization ─────────────────────────────────────────────────────────

#[test]
fn test_double_initialize_fails() {
    let (env, client, admin, _buyer, _seller, _token) = setup();
    let token_admin2 = Address::generate(&env);
    let sac2 = env.register_stellar_asset_contract_v2(token_admin2);
    let another_token = sac2.address();
    assert_eq!(
        client.try_initialize(&admin, &another_token),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_uninitialized_settle_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    assert_eq!(
        client.try_settle(&pid(&env, 1), &100),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_uninitialized_authorize_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    let from = Address::generate(&env);
    let to = Address::generate(&env);
    assert_eq!(
        client.try_authorize(&pid(&env, 1), &from, &to, &100, &1000),
        Err(Ok(Error::NotInitialized))
    );
}

// ── Recipient binding ──────────────────────────────────────────────────────

#[test]
fn test_recipient_binding_cannot_be_changed_at_settle() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&client.address), 0); // non-custodial
    assert_eq!(tc.balance(&buyer), TOKEN_SUPPLY - 500);
    assert_eq!(tc.balance(&recipient), 500);
}

#[test]
fn test_settle_cannot_redirect_to_different_recipient() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let intended = Address::generate(&env);
    let attacker = Address::generate(&env);

    client.authorize(&p, &buyer, &intended, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &300);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&attacker), 0);
    assert_eq!(tc.balance(&intended), 300);
}

// ── Single settlement ──────────────────────────────────────────────────────

#[test]
fn test_single_settlement_second_settle_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    assert_eq!(client.try_settle(&p, &200), Err(Ok(Error::AlreadySettled)));
}

// ── No residual allowance ──────────────────────────────────────────────────

#[test]
fn test_no_residual_allowance_after_settlement() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let actual = 300i128;

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &actual);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&client.address), 0);
    assert_eq!(tc.balance(&buyer), TOKEN_SUPPLY - actual);
}

#[test]
fn test_cap_minus_actual_does_not_linger() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &100);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&client.address), 0);
}

// ── Expiry (two independent clocks) ────────────────────────────────────────

#[test]
fn test_settle_after_expiry_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 150);
    assert_eq!(client.try_settle(&p, &500), Err(Ok(Error::Expired)));
}

#[test]
fn test_settle_at_expiry_boundary_succeeds() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 100);
    client.settle(&p, &500);
}

#[test]
fn test_settle_just_before_expiry_succeeds() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 99);
    client.settle(&p, &500);
}

// ── Amount cap ─────────────────────────────────────────────────────────────

#[test]
fn test_settle_exceeding_cap_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &500, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    assert_eq!(
        client.try_settle(&p, &600),
        Err(Ok(Error::AmountExceedsCap))
    );
}

#[test]
fn test_settle_at_cap_succeeds() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &500, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 500);
}

#[test]
fn test_settle_zero_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &500, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    assert_eq!(client.try_settle(&p, &0), Err(Ok(Error::InvalidAmount)));
}

// ── Authorization not found / invalid amounts ──────────────────────────────

#[test]
fn test_settle_nonexistent_payment_fails() {
    let (env, client, _admin, _buyer, _seller, _token) = setup();
    assert_eq!(
        client.try_settle(&pid(&env, 1), &100),
        Err(Ok(Error::AuthorizationNotFound))
    );
}

#[test]
fn test_authorize_zero_cap_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let recipient = Address::generate(&env);
    assert_eq!(
        client.try_authorize(&pid(&env, 1), &buyer, &recipient, &0, &1000),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_authorize_negative_cap_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let recipient = Address::generate(&env);
    assert_eq!(
        client.try_authorize(&pid(&env, 1), &buyer, &recipient, &-100, &1000),
        Err(Ok(Error::InvalidAmount))
    );
}

// ── Lapsed authorization / reclaim path ────────────────────────────────────

#[test]
fn test_lapsed_authorization_no_funds_moved() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 200);

    assert_eq!(client.try_settle(&p, &500), Err(Ok(Error::Expired)));

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&buyer), TOKEN_SUPPLY);
}

#[test]
fn test_reauthorize_after_expiry_succeeds() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &500, &100);
    env.ledger().with_mut(|li| li.sequence_number = 200);

    // Re-authorize with new expiry
    client.authorize(&p, &buyer, &recipient, &1000, &300);
    env.ledger().with_mut(|li| li.sequence_number = 250);
    client.settle(&p, &800);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 800);
}

// ── Events ─────────────────────────────────────────────────────────────────

#[test]
fn test_authorize_event_emitted() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);

    // Check the authorize event emitted by this call
    let events = env.events().all().filter_by_contract(&client.address);
    // The #[contractevent] struct serializes fields alphabetically
    let expected_data = {
        let mut m = soroban_sdk::Map::<Symbol, Val>::new(&env);
        m.set(Symbol::new(&env, "cap"), 1000i128.into_val(&env));
        m.set(Symbol::new(&env, "expiry"), 1000u32.into_val(&env));
        m.set(Symbol::new(&env, "from"), buyer.into_val(&env));
        m.set(Symbol::new(&env, "to"), recipient.into_val(&env));
        m.into_val(&env)
    };
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "authorize_event"), p.clone()).into_val(&env),
                expected_data
            )
        ]
    );
}

#[test]
fn test_settle_event_emitted() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    // Check the settle event
    let events = env.events().all().filter_by_contract(&client.address);
    let expected_settle_data = {
        let mut m = soroban_sdk::Map::<Symbol, Val>::new(&env);
        m.set(Symbol::new(&env, "actual"), 500i128.into_val(&env));
        m.set(Symbol::new(&env, "from"), buyer.into_val(&env));
        m.into_val(&env)
    };
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "settle_event"), p.clone()).into_val(&env),
                expected_settle_data
            )
        ]
    );
}

#[test]
fn test_prune_event_emitted() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.prune_authorization(&p);

    // Check the prune event
    let events = env.events().all().filter_by_contract(&client.address);
    let expected_prune_data = {
        let m = soroban_sdk::Map::<Symbol, Val>::new(&env);
        m.into_val(&env)
    };
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "prune_event"), 1u32).into_val(&env),
                expected_prune_data
            )
        ]
    );
}

// ── Prune ──────────────────────────────────────────────────────────────────

#[test]
fn test_prune_expired_authorization_succeeds() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &100);
    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.prune_authorization(&p);
    assert!(client.get_authorization(&p).is_none());
}

#[test]
fn test_prune_active_authorization_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    assert_eq!(client.try_prune_authorization(&p), Err(Ok(Error::Expired)));
}

#[test]
fn test_prune_nonexistent_fails() {
    let (env, client, _admin, _buyer, _seller, _token) = setup();
    assert_eq!(
        client.try_prune_authorization(&pid(&env, 1)),
        Err(Ok(Error::AuthorizationNotFound))
    );
}

// ── TTL extension ──────────────────────────────────────────────────────────

#[test]
fn test_extend_authorization_ttl_fails_if_missing() {
    let (env, client, _admin, _buyer, _seller, _token) = setup();
    assert_eq!(
        client.try_extend_authorization_ttl(&pid(&env, 1)),
        Err(Ok(Error::AuthorizationNotFound))
    );
}

#[test]
fn test_extend_authorization_ttl_succeeds() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    client.extend_authorization_ttl(&p);
    assert!(client.get_authorization(&p).is_some());
}

// ── Authorization lookup ───────────────────────────────────────────────────

#[test]
fn test_get_authorization_returns_record() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &500);
    let record = client.get_authorization(&p).unwrap();
    assert_eq!(record.from, buyer);
    assert_eq!(record.to, recipient);
    assert_eq!(record.cap, 1000);
    assert_eq!(record.expiry, 500);
    assert!(!record.consumed);
}

#[test]
fn test_get_authorization_after_settle_shows_consumed() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    assert!(client.get_authorization(&p).unwrap().consumed);
}

#[test]
fn test_get_authorization_nonexistent_returns_none() {
    let (env, client, _admin, _buyer, _seller, _token) = setup();
    assert!(client.get_authorization(&pid(&env, 1)).is_none());
}

// ── Non-custodial ──────────────────────────────────────────────────────────

#[test]
fn test_contract_holds_no_funds() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &700);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&client.address), 0);
}

// ── Multiple payment_ids ───────────────────────────────────────────────────

#[test]
fn test_independent_payment_ids() {
    let (env, client, _admin, _buyer, _seller, token) = setup();
    let p1 = pid(&env, 1);
    let p2 = pid(&env, 2);
    // Use different buyers because SEP-41 allowances are per (from, spender)
    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);
    let recipient = Address::generate(&env);
    StellarAssetClient::new(&env, &token).mint(&buyer1, &TOKEN_SUPPLY);
    StellarAssetClient::new(&env, &token).mint(&buyer2, &TOKEN_SUPPLY);

    client.authorize(&p1, &buyer1, &recipient, &1000, &1000);
    client.authorize(&p2, &buyer2, &recipient, &2000, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);

    client.settle(&p1, &300);
    client.settle(&p2, &800);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 1100);
}

// ── Exact settlement (cap == actual) ───────────────────────────────────────

#[test]
fn test_settle_exact_cap() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &500, &1000);
    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 500);
    assert_eq!(tc.balance(&buyer), TOKEN_SUPPLY - 500);
}
