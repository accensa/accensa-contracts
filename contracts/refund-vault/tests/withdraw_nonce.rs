//! #970 — A successful `withdraw` bumps the shared nonce exactly once.
//!
//! `withdraw` transfers the token and *then* calls `increment_nonce`, a helper
//! that stores `current + 1` and returns the value `current` had. So the two
//! halves of one convention are observable at once: `WithdrawEvent.nonce` is
//! the counter as it was *before* the withdrawal, and `get_nonce()` is the
//! counter after it. Clients sign follow-up calls against both halves, which is
//! why both are asserted here rather than one derived from the other.
//!
//! Coverage before this file was partial: `src/test.rs` grew the counter by one
//! after a withdrawal but started from the setup value and never looked at the
//! event field, and its "no increment on failure" test exercised a failed
//! `refund`, not a failed `withdraw`. Nothing pinned that `SelfTransfer` and
//! `InsufficientFloat` return *before* the bump. Move the bump above either
//! rejection and the counter assertions below fail, because a rejected
//! withdrawal would then consume the nonce every caller had already read.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, IntoVal, Map, Symbol, Val,
};

const FLOAT: i128 = 1_000_000;

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    token: Address,
}

fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &FLOAT, &None);

    Vault {
        env,
        contract,
        client,
        merchant,
        token,
    }
}

/// The shape `src/test.rs` builds for `WithdrawEvent`: topic
/// `("withdraw_event", to)`, data map `{amount, nonce}`. Copied, not invented,
/// because the topic symbols and field names are the public interface indexers
/// read.
fn withdraw_event_data(env: &Env, amount: i128, nonce: u64) -> Map<Val, Val> {
    let mut m = Map::new(env);
    m.set(
        Symbol::new(env, "amount").into_val(env),
        amount.into_val(env),
    );
    m.set(Symbol::new(env, "nonce").into_val(env), nonce.into_val(env));
    m
}

/// Assert the last call published exactly the `withdraw` event for `amount`
/// carrying `nonce` — and nothing else.
fn assert_withdraw_event(v: &Vault, to: &Address, amount: i128, nonce: u64) {
    assert_eq!(
        v.env.events().all().filter_by_contract(&v.contract),
        vec![
            &v.env,
            (
                v.contract.clone(),
                (Symbol::new(&v.env, "withdraw_event"), to.clone()).into_val(&v.env),
                withdraw_event_data(&v.env, amount, nonce).into_val(&v.env)
            )
        ]
    );
}

/// Assert the last call published no event from the vault at all.
fn assert_no_vault_event(v: &Vault) {
    assert_eq!(
        v.env
            .events()
            .all()
            .filter_by_contract(&v.contract)
            .events()
            .len(),
        0,
        "a rejected withdrawal must not publish `WithdrawEvent`",
    );
}

/// Withdraw `amount` to `to`, asserting the two halves of the nonce convention
/// hold for exactly this call: the event carries the counter as it read *before*
/// the withdrawal and the counter rises by one.
///
/// The event check runs before the counter is read again: `env.events().all()`
/// only reflects the *last* top-level invocation, so a `get_nonce` in between
/// would have emptied it and proven nothing.
fn withdraw_one(v: &Vault, amount: i128, to: &Address) {
    let before = v.client.get_nonce();
    v.client.withdraw(&amount, to);
    assert_withdraw_event(v, to, amount, before);

    let after = v.client.get_nonce();
    assert_eq!(after, before + 1, "one withdrawal, one bump");
}

#[test]
fn the_baseline_is_nonzero_so_the_bumps_are_measurable_against_it() {
    let v = setup();
    // `deposit` shares the counter with `withdraw`, so the funded vault starts
    // above zero. If this ever reads 0 the two-bump assertion below would pass
    // against a counter that only ever counted deposits.
    assert_eq!(v.client.get_nonce(), 1);
}

#[test]
fn two_withdrawals_raise_the_counter_by_exactly_two() {
    let v = setup();
    let baseline = v.client.get_nonce();
    assert!(baseline > 0, "the withdrawals must not start from 0");

    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    withdraw_one(&v, 100_000, &buyer_a);
    withdraw_one(&v, 250_000, &buyer_b);

    assert_eq!(v.client.get_nonce(), baseline + 2);
    assert_eq!(
        TokenClient::new(&v.env, &v.token).balance(&buyer_a),
        100_000
    );
    assert_eq!(
        TokenClient::new(&v.env, &v.token).balance(&buyer_b),
        250_000
    );
}

/// `SelfTransfer` returns before the bump, and the corrected call then reports
/// the same pre-read nonce the rejected one would have consumed.
#[test]
fn self_transfer_leaves_the_counter_for_its_retry() {
    let v = setup();
    let before = v.client.get_nonce();

    assert_eq!(
        v.client.try_withdraw(&100_000, &v.contract),
        Err(Ok(Error::SelfTransfer)),
    );
    assert_no_vault_event(&v);
    assert_eq!(
        v.client.get_nonce(),
        before,
        "`SelfTransfer` must return above `increment_nonce`",
    );

    let buyer = Address::generate(&v.env);
    v.client.withdraw(&100_000, &buyer);
    assert_withdraw_event(&v, &buyer, 100_000, before);
    assert_eq!(v.client.get_nonce(), before + 1);
}

/// An over-float request returns before the bump too. The retry uses a smaller
/// amount against the same float — the float is not topped up here, because a
/// `deposit` would itself consume a nonce and blur which call bumped.
#[test]
fn insufficient_float_leaves_the_counter_for_its_retry() {
    let v = setup();
    let before = v.client.get_nonce();

    assert_eq!(
        v.client
            .try_withdraw(&(FLOAT * 2), &Address::generate(&v.env)),
        Err(Ok(Error::InsufficientFloat)),
    );
    assert_no_vault_event(&v);
    assert_eq!(
        v.client.get_nonce(),
        before,
        "`InsufficientFloat` must return above `increment_nonce`",
    );

    let buyer = Address::generate(&v.env);
    v.client.withdraw(&(FLOAT / 2), &buyer);
    assert_withdraw_event(&v, &buyer, FLOAT / 2, before);
    assert_eq!(v.client.get_nonce(), before + 1);
}

/// The float is topped up between cases instead of rebuilding the vault, so
/// the counter keeps one continuous history — and the top-up is visible in it,
/// which is why each case re-reads its own baseline instead of assuming one.
#[test]
fn topping_up_the_float_keeps_one_continuous_nonce_history() {
    let v = setup();
    let start = v.client.get_nonce();

    let buyer = Address::generate(&v.env);
    withdraw_one(&v, 900_000, &buyer);

    // Only 100_000 of float is left, so 500_000 is over it: rejected, no bump.
    let before_retry = v.client.get_nonce();
    assert_eq!(
        v.client
            .try_withdraw(&(FLOAT / 2), &Address::generate(&v.env)),
        Err(Ok(Error::InsufficientFloat)),
    );
    assert_no_vault_event(&v);
    assert_eq!(v.client.get_nonce(), before_retry);

    // The setup deposit took the merchant's whole float, so fund the top-up.
    StellarAssetClient::new(&v.env, &v.token).mint(&v.merchant, &(FLOAT / 2));
    v.client.deposit(&v.merchant, &(FLOAT / 2), &None);
    let after_top_up = v.client.get_nonce();
    assert_eq!(after_top_up, before_retry + 1, "the top-up used one nonce");

    v.client.withdraw(&(FLOAT / 2), &buyer);
    assert_withdraw_event(&v, &buyer, FLOAT / 2, after_top_up);
    assert_eq!(v.client.get_nonce(), after_top_up + 1);

    // `start` already counts the setup deposit, so three bumps more: the
    // withdrawal, the top-up deposit, the second withdrawal. The rejected
    // over-float call in between consumed none.
    assert_eq!(v.client.get_nonce(), start + 3);
}
