#![cfg(test)]

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, IntoVal, Symbol, Val,
};

const TOKEN_SUPPLY: i128 = 10_000_000;

pub(crate) type Setup = (
    Env,
    UptoAuthorizationClient<'static>,
    Address,
    Address,
    Address,
    Address,
);

fn setup() -> Setup {
    setup_in(Env::default())
}

pub(crate) fn setup_in(env: Env) -> Setup {
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
        m.set(Symbol::new(&env, "max_slippage_bps"), 0u32.into_val(&env));
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

// ── Slippage tolerance ─────────────────────────────────────────────────────

#[test]
fn test_zero_bps_is_strict_cap() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize_with_slippage(&p, &buyer, &recipient, &1000, &1000, &0);
    assert_eq!(
        client.try_settle(&p, &1001),
        Err(Ok(Error::AmountExceedsCap))
    );
    client.settle(&p, &1000);
    assert_eq!(TokenClient::new(&env, &token).balance(&recipient), 1000);
}

#[test]
fn test_settle_within_slippage_succeeds() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    // 2.5% on 10_000 → up to 10_250.
    client.authorize_with_slippage(&p, &buyer, &recipient, &10_000, &1000, &250);
    client.settle(&p, &10_100);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 10_100);
    assert_eq!(tc.balance(&buyer), TOKEN_SUPPLY - 10_100);
    // The unused headroom does not linger as an allowance.
    assert_eq!(tc.allowance(&buyer, &client.address), 0);
}

#[test]
fn test_settle_at_slippage_boundary() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let recipient = Address::generate(&env);

    // Exactly at cap + tolerance succeeds …
    let p1 = pid(&env, 1);
    client.authorize_with_slippage(&p1, &buyer, &recipient, &10_000, &1000, &250);
    client.settle(&p1, &10_250);
    assert_eq!(TokenClient::new(&env, &token).balance(&recipient), 10_250);

    // … one unit above fails.
    let p2 = pid(&env, 2);
    client.authorize_with_slippage(&p2, &buyer, &recipient, &10_000, &1000, &250);
    assert_eq!(
        client.try_settle(&p2, &10_251),
        Err(Ok(Error::AmountExceedsCap))
    );
}

#[test]
fn test_slippage_tolerance_rounds_down() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    // 1 bps of 9_999 is 0.9999 → floors to 0, so the cap stays strict.
    client.authorize_with_slippage(&p, &buyer, &recipient, &9_999, &1000, &1);
    assert_eq!(
        client.try_settle(&p, &10_000),
        Err(Ok(Error::AmountExceedsCap))
    );
    client.settle(&p, &9_999);
}

#[test]
fn test_max_slippage_bps_allows_double_cap() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize_with_slippage(&p, &buyer, &recipient, &1000, &1000, &MAX_SLIPPAGE_BPS);
    assert_eq!(
        client.try_settle(&p, &2001),
        Err(Ok(Error::AmountExceedsCap))
    );
    client.settle(&p, &2000);
    assert_eq!(TokenClient::new(&env, &token).balance(&recipient), 2000);
}

#[test]
fn test_slippage_above_max_rejected() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let recipient = Address::generate(&env);

    for bps in [MAX_SLIPPAGE_BPS + 1, u32::MAX] {
        assert_eq!(
            client.try_authorize_with_slippage(
                &pid(&env, 1),
                &buyer,
                &recipient,
                &1000,
                &1000,
                &bps
            ),
            Err(Ok(Error::InvalidSlippage))
        );
    }
    assert_eq!(client.get_authorization(&pid(&env, 1)), None);
    assert_eq!(
        TokenClient::new(&env, &token).allowance(&buyer, &client.address),
        0
    );
}

#[test]
fn test_slippage_overflow_rejected() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let recipient = Address::generate(&env);

    // i128::MAX with any non-zero tolerance cannot be represented.
    assert_eq!(
        client.try_authorize_with_slippage(
            &pid(&env, 1),
            &buyer,
            &recipient,
            &i128::MAX,
            &1000,
            &1
        ),
        Err(Ok(Error::AmountOverflow))
    );
    // With 0 bps the maximum is the cap itself — no overflow.
    client.authorize_with_slippage(&pid(&env, 2), &buyer, &recipient, &i128::MAX, &1000, &0);
}

#[test]
fn test_slippage_allowance_covers_tolerance() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize_with_slippage(&p, &buyer, &recipient, &10_000, &1000, &500);
    assert_eq!(
        TokenClient::new(&env, &token).allowance(&buyer, &client.address),
        10_500
    );
    let record = client.get_authorization(&p).unwrap();
    assert_eq!(record.cap, 10_000);
    assert_eq!(record.max_slippage_bps, 500);
}

#[test]
fn test_authorize_defaults_to_zero_slippage() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize(&p, &buyer, &recipient, &1000, &1000);
    assert_eq!(client.get_authorization(&p).unwrap().max_slippage_bps, 0);
    assert_eq!(
        TokenClient::new(&env, &token).allowance(&buyer, &client.address),
        1000
    );
}

#[test]
fn test_authorize_with_slippage_event() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    client.authorize_with_slippage(&p, &buyer, &recipient, &1000, &1000, &75);

    let expected_data = {
        let mut m = soroban_sdk::Map::<Symbol, Val>::new(&env);
        m.set(Symbol::new(&env, "cap"), 1000i128.into_val(&env));
        m.set(Symbol::new(&env, "expiry"), 1000u32.into_val(&env));
        m.set(Symbol::new(&env, "from"), buyer.into_val(&env));
        m.set(Symbol::new(&env, "max_slippage_bps"), 75u32.into_val(&env));
        m.set(Symbol::new(&env, "to"), recipient.into_val(&env));
        m.into_val(&env)
    };
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
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
fn test_max_settleable_extremes() {
    assert_eq!(max_settleable(0, MAX_SLIPPAGE_BPS), Some(0));
    assert_eq!(max_settleable(1, MAX_SLIPPAGE_BPS), Some(2));
    assert_eq!(max_settleable(i128::MAX, 0), Some(i128::MAX));
    assert_eq!(max_settleable(i128::MAX, 1), None);
    // Largest cap that still doubles without overflow.
    let half = i128::MAX / 2;
    assert_eq!(max_settleable(half, MAX_SLIPPAGE_BPS), Some(half * 2));
    assert_eq!(max_settleable(half + 1, MAX_SLIPPAGE_BPS), None);
    // Caps far above i128::MAX / 10_000, where a naive cap * bps overflows.
    let big = i128::MAX / 3;
    assert_eq!(max_settleable(big, 5_000), Some(big + big / 2));
    assert_eq!(max_settleable(-1, 0), None);
    assert_eq!(max_settleable(1000, MAX_SLIPPAGE_BPS + 1), None);
}

// ── Domain-separated signatures (issue #416) ────────────────────────────────

fn signing_key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn pubkey_of(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

fn sign_digest(env: &Env, sk: &SigningKey, digest: &BytesN<32>) -> BytesN<64> {
    let sig = sk.sign(&digest.to_array());
    BytesN::from_array(env, &sig.to_bytes())
}

/// The domain separator must differ across networks: a signature made on
/// one network can never produce the same digest on another.
#[test]
fn test_domain_separator_changes_with_network() {
    let (env, client, _admin, _buyer, _seller, _token) = setup();
    let d1 = client.get_domain_separator();

    env.ledger().set_network_id([9u8; 32]);
    let d2 = client.get_domain_separator();

    assert_ne!(d1, d2, "domain separator must bind the network id");
}

/// The digest covers the full authorization tuple: changing any field
/// changes the digest a signer would have to produce.
#[test]
fn test_authorization_digest_binds_every_field() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);

    let base = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);

    // Different cap.
    let other_cap = client.get_authorization_digest(&p, &buyer, &recipient, &999, &1000);
    // Different expiry.
    let other_expiry = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &999);
    // Different payment id.
    let other_pid =
        client.get_authorization_digest(&pid(&env, 2), &buyer, &recipient, &1000, &1000);
    // Different recipient.
    let other_recipient =
        client.get_authorization_digest(&p, &buyer, &Address::generate(&env), &1000, &1000);

    assert_ne!(base, other_cap);
    assert_ne!(base, other_expiry);
    assert_ne!(base, other_pid);
    assert_ne!(base, other_recipient);
}

#[test]
fn test_register_signer_and_get() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let sk = signing_key(7);
    let pk = pubkey_of(&env, &sk);

    assert_eq!(client.get_signer(&buyer), None);
    client.register_signer(&buyer, &pk);
    assert_eq!(client.get_signer(&buyer), Some(pk));
}

#[test]
fn test_authorize_signed_unregistered_signer_fails() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let sig = BytesN::from_array(&env, &[0u8; 64]);

    assert_eq!(
        client.try_authorize_signed(&p, &buyer, &recipient, &1000, &1000, &sig),
        Err(Ok(Error::SignerNotRegistered))
    );
}

/// End-to-end happy path: register the key, sign the domain-separated
/// digest, authorize — and the regular settle flow still works afterwards.
#[test]
fn test_authorize_signed_valid_signature_succeeds() {
    let (env, client, _admin, buyer, _seller, token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let sk = signing_key(7);
    let pk = pubkey_of(&env, &sk);

    client.register_signer(&buyer, &pk);
    let digest = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);
    let sig = sign_digest(&env, &sk, &digest);

    client.authorize_signed(&p, &buyer, &recipient, &1000, &1000, &sig);
    let record = client.get_authorization(&p).unwrap();
    assert_eq!(record.cap, 1000);
    assert_eq!(record.from, buyer);
    assert_eq!(record.to, recipient);

    env.ledger().with_mut(|li| li.sequence_number = 50);
    client.settle(&p, &500);
    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&recipient), 500);
}

/// Signatures are network-bound: the *same* signature that verified before
/// a network switch must be rejected afterwards, because the digest moved
/// with the domain separator.
#[test]
fn test_authorize_signed_rejected_on_different_network() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let sk = signing_key(7);
    let pk = pubkey_of(&env, &sk);

    client.register_signer(&buyer, &pk);
    let digest_before = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);
    let sig = sign_digest(&env, &sk, &digest_before);

    // Simulate the same signed payload arriving on another network.
    env.ledger().set_network_id([9u8; 32]);

    let digest_after = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);
    assert_ne!(digest_before, digest_after, "digest must move with network");

    // ed25519_verify traps on mismatch — the whole call errors out.
    assert!(client
        .try_authorize_signed(&p, &buyer, &recipient, &1000, &1000, &sig)
        .is_err());
    assert!(
        client.get_authorization(&p).is_none(),
        "no authorization may be recorded after a failed verification"
    );
}

/// A signature produced by a key other than the registered one is rejected.
#[test]
fn test_authorize_signed_rejected_for_wrong_key() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let registered = signing_key(7);
    let attacker = signing_key(8);

    client.register_signer(&buyer, &pubkey_of(&env, &registered));
    let digest = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);
    let sig = sign_digest(&env, &attacker, &digest);

    assert!(client
        .try_authorize_signed(&p, &buyer, &recipient, &1000, &1000, &sig)
        .is_err());
    assert!(client.get_authorization(&p).is_none());
}

/// The signature binds the exact signed tuple: a valid signature over
/// `cap = 1000` does not authorize `cap = 999`.
#[test]
fn test_authorize_signed_rejected_when_cap_differs() {
    let (env, client, _admin, buyer, _seller, _token) = setup();
    let p = pid(&env, 1);
    let recipient = Address::generate(&env);
    let sk = signing_key(7);

    client.register_signer(&buyer, &pubkey_of(&env, &sk));
    let digest = client.get_authorization_digest(&p, &buyer, &recipient, &1000, &1000);
    let sig = sign_digest(&env, &sk, &digest);

    assert!(client
        .try_authorize_signed(&p, &buyer, &recipient, &999, &1000, &sig)
        .is_err());
    assert!(client.get_authorization(&p).is_none());
}
