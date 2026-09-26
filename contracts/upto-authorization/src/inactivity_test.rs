//! Tests for inactivity auto-cancellation (issue #435).

#![cfg(test)]

use crate::inactivity::DEFAULT_INACTIVITY_LEDGERS;
use crate::test::setup_in;
use crate::{Error, UptoAuthorizationClient};
use soroban_sdk::{testutils::Ledger as _, token::TokenClient, BytesN, Env};

fn pid(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

fn set_ledger(env: &Env, sequence: u32) {
    env.ledger().with_mut(|l| l.sequence_number = sequence);
}

/// A live authorization: buyer caps a payment at `cap` until `expiry`, created
/// at `created_ledger`, with no settlement.
fn authorize(
    env: &Env,
    client: &UptoAuthorizationClient<'_>,
    buyer: &soroban_sdk::Address,
    seller: &soroban_sdk::Address,
    payment_id: &BytesN<32>,
    cap: i128,
    created_ledger: u32,
    expiry: u32,
) {
    set_ledger(env, created_ledger);
    client.authorize(payment_id, buyer, seller, &cap, &expiry);
}

#[test]
fn the_default_timeout_is_about_thirty_days() {
    let (env, client, _admin, _buyer, _seller, _token) = setup_in(Env::default());
    assert_eq!(client.get_inactivity_timeout(), DEFAULT_INACTIVITY_LEDGERS);
    let _ = env;
}

#[test]
fn cancelling_after_the_window_releases_the_buyers_allowance() {
    let (env, client, _admin, buyer, seller, token) = setup_in(Env::default());
    client.set_inactivity_timeout(&50);

    let payment_id = pid(&env, 1);
    authorize(&env, &client, &buyer, &seller, &payment_id, 500, 10, 1_000);

    let spender = client.address.clone();
    assert_eq!(
        TokenClient::new(&env, &token).allowance(&buyer, &spender),
        500,
        "the authorization locks the buyer's allowance"
    );

    // 10 + 50 = 60; one ledger later the authorization is dormant.
    set_ledger(&env, 61);
    assert_eq!(client.cancel_inactive_escrow(&payment_id), 500);

    assert_eq!(
        TokenClient::new(&env, &token).allowance(&buyer, &spender),
        0,
        "the locked allowance is released back to the buyer"
    );
    assert_eq!(
        client.get_authorization(&payment_id),
        None,
        "the storage entry is deleted"
    );
}

#[test]
fn cancelling_at_the_edge_of_the_window_is_rejected() {
    let (env, client, _admin, buyer, seller, _token) = setup_in(Env::default());
    client.set_inactivity_timeout(&50);
    let payment_id = pid(&env, 2);
    authorize(&env, &client, &buyer, &seller, &payment_id, 500, 10, 1_000);

    // Exactly at the deadline: still not cancellable.
    set_ledger(&env, 60);
    assert_eq!(
        client.try_cancel_inactive_escrow(&payment_id),
        Err(Ok(Error::NotInactive))
    );
    // One ledger later it is.
    set_ledger(&env, 61);
    assert_eq!(client.cancel_inactive_escrow(&payment_id), 500);
}

#[test]
fn a_zero_timeout_makes_an_authorization_cancellable_immediately_after_creation() {
    let (env, client, _admin, buyer, seller, _token) = setup_in(Env::default());
    client.set_inactivity_timeout(&0);
    let payment_id = pid(&env, 3);
    authorize(&env, &client, &buyer, &seller, &payment_id, 250, 5, 1_000);

    // created at 5; with a 0 timeout it is cancellable from ledger 6.
    set_ledger(&env, 5);
    assert_eq!(
        client.try_cancel_inactive_escrow(&payment_id),
        Err(Ok(Error::NotInactive))
    );
    set_ledger(&env, 6);
    assert_eq!(client.cancel_inactive_escrow(&payment_id), 250);
}

#[test]
fn a_settled_authorization_cannot_be_cancelled() {
    let (env, client, _admin, buyer, seller, _token) = setup_in(Env::default());
    client.set_inactivity_timeout(&0);
    let payment_id = pid(&env, 4);
    authorize(&env, &client, &buyer, &seller, &payment_id, 500, 1, 1_000);
    client.settle(&payment_id, &300);

    set_ledger(&env, 100);
    assert_eq!(
        client.try_cancel_inactive_escrow(&payment_id),
        Err(Ok(Error::AlreadySettled))
    );
}

#[test]
fn an_unknown_authorization_is_rejected() {
    let (env, client, _admin, _buyer, _seller, _token) = setup_in(Env::default());
    assert_eq!(
        client.try_cancel_inactive_escrow(&pid(&env, 9)),
        Err(Ok(Error::AuthorizationNotFound))
    );
}

#[test]
fn only_the_admin_can_change_the_timeout() {
    let (env, client, _admin, _buyer, _seller, _token) = setup_in(Env::default());
    env.set_auths(&[]);
    assert!(client.try_set_inactivity_timeout(&10).is_err());
    assert_eq!(client.get_inactivity_timeout(), DEFAULT_INACTIVITY_LEDGERS);
}

#[test]
fn the_buyer_must_authorize_the_cancellation() {
    let (env, client, _admin, buyer, seller, _token) = setup_in(Env::default());
    client.set_inactivity_timeout(&0);
    let payment_id = pid(&env, 7);
    authorize(&env, &client, &buyer, &seller, &payment_id, 500, 1, 1_000);

    // No buyer auth: the allowance cannot be released.
    set_ledger(&env, 100);
    env.set_auths(&[]);
    assert!(client.try_cancel_inactive_escrow(&payment_id).is_err());
    assert!(client.get_authorization(&payment_id).is_some());
}
