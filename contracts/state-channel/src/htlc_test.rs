//! Virtual multi-hop HTLC tests (issue #458).

extern crate std;

use super::*;
use crate::htlc::{HtlcRef, HtlcState};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, BytesN, Env,
};

fn setup() -> (
    Env,
    StateChannelClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let charlie = Address::generate(&env);

    let token = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&alice, &1_000_000);
    StellarAssetClient::new(&env, &token).mint(&bob, &1_000_000);

    (env, client, token, alice, bob, charlie)
}

fn pk(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

fn hash(env: &Env, preimage: &[u8]) -> BytesN<32> {
    env.crypto()
        .sha256(&Bytes::from_slice(env, preimage))
        .into()
}

fn bytes(env: &Env, data: &[u8]) -> Bytes {
    Bytes::from_slice(env, data)
}

#[test]
fn multi_hop_route_resolves_with_preimage() {
    let (env, client, token, alice, bob, charlie) = setup();
    // Alice -> Bob and Bob -> Charlie, two linked channels.
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    let c2 = client.open_channel(&bob, &charlie, &pk(&env), &1_000, &720);

    let preimage = b"shared-secret";
    let h = hash(&env, preimage);

    let h1 = client.add_htlc(&c1, &h, &100, &1_000, &None);
    let h2 = client.add_htlc(
        &c2,
        &h,
        &100,
        &900,
        &Some(HtlcRef {
            channel_id: c1,
            htlc_id: h1,
        }),
    );

    // Timeouts are strictly decreasing along the route (downstream expires first).
    assert_eq!(client.get_htlc(&c1, &h1).timeout_ledger, 1_000);
    assert_eq!(client.get_htlc(&c2, &h2).timeout_ledger, 900);
    assert_eq!(client.get_htlc_reserved(&c1), 100);
    assert_eq!(client.get_htlc_reserved(&c2), 100);

    // Walk the preimage back from the last hop to the first.
    client.resolve_htlc(&c2, &h2, &bytes(&env, preimage));
    client.resolve_htlc(&c1, &h1, &bytes(&env, preimage));

    assert_eq!(client.get_htlc(&c1, &h1).state, HtlcState::Resolved);
    assert_eq!(client.get_htlc(&c2, &h2).state, HtlcState::Resolved);
    assert_eq!(client.get_htlc_reserved(&c1), 0);
    assert_eq!(client.get_htlc_reserved(&c2), 0);

    // Resolution credits each receiver's channel balance; tokens stay escrowed
    // until the channel settles.
    assert_eq!(client.get_channel(&c1).balance, 100);
    assert_eq!(client.get_channel(&c2).balance, 100);
    // Bob funded the second channel's escrow, so his liquid balance is short
    // by that 1,000; no resolution moves tokens out of escrow.
    assert_eq!(TokenClient::new(&env, &token).balance(&bob), 999_000);
    assert_eq!(TokenClient::new(&env, &token).balance(&charlie), 0);
}

#[test]
fn downstream_timeout_must_be_strictly_smaller() {
    let (env, client, _token, alice, bob, charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    let c2 = client.open_channel(&bob, &charlie, &pk(&env), &1_000, &720);
    let h = hash(&env, b"s");

    let h1 = client.add_htlc(&c1, &h, &100, &1_000, &None);
    let parent = Some(HtlcRef {
        channel_id: c1,
        htlc_id: h1,
    });

    // Equal and greater timeouts are both rejected.
    assert_eq!(
        client.try_add_htlc(&c2, &h, &100, &1_000, &parent),
        Err(Ok(Error::HtlcTimeoutOutOfOrder))
    );
    assert_eq!(
        client.try_add_htlc(&c2, &h, &100, &1_001, &parent),
        Err(Ok(Error::HtlcTimeoutOutOfOrder))
    );
    // Strictly smaller is accepted.
    client.add_htlc(&c2, &h, &100, &999, &parent);
}

#[test]
fn timed_out_hop_refunds_to_sender() {
    let (env, client, _token, alice, bob, _charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    let h = hash(&env, b"stale");

    let htlc_id = client.add_htlc(&c1, &h, &250, &100, &None);
    assert_eq!(client.get_htlc_reserved(&c1), 250);

    // Not refundable until the timeout has passed.
    assert_eq!(
        client.try_refund_htlc(&c1, &htlc_id),
        Err(Ok(Error::HtlcNotExpired))
    );

    env.ledger().set_sequence_number(101);
    client.refund_htlc(&c1, &htlc_id);

    assert_eq!(client.get_htlc_reserved(&c1), 0);
    assert_eq!(client.get_htlc(&c1, &htlc_id).state, HtlcState::Refunded);
    // A refunded hop cannot be resolved or refunded again.
    assert_eq!(
        client.try_refund_htlc(&c1, &htlc_id),
        Err(Ok(Error::HtlcNotPending))
    );
    assert_eq!(
        client.try_resolve_htlc(&c1, &htlc_id, &bytes(&env, b"stale")),
        Err(Ok(Error::HtlcNotPending))
    );
}

#[test]
fn wrong_preimage_is_rejected() {
    let (env, client, _token, alice, bob, _charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    let h = hash(&env, b"correct");
    let htlc_id = client.add_htlc(&c1, &h, &100, &500, &None);

    assert_eq!(
        client.try_resolve_htlc(&c1, &htlc_id, &bytes(&env, b"wrong")),
        Err(Ok(Error::InvalidPreimage))
    );
    // Still pending and fully reserved.
    assert_eq!(client.get_htlc(&c1, &htlc_id).state, HtlcState::Pending);
    assert_eq!(client.get_htlc_reserved(&c1), 100);
}

#[test]
fn hop_cannot_exceed_free_escrow() {
    let (env, client, _token, alice, bob, _charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &100, &720);
    let h = hash(&env, b"x");

    assert_eq!(
        client.try_add_htlc(&c1, &h, &101, &500, &None),
        Err(Ok(Error::HtlcInsufficientEscrow))
    );
    client.add_htlc(&c1, &h, &100, &500, &None);
    // The escrow is fully reserved now, so another hop cannot fit.
    assert_eq!(
        client.try_add_htlc(&c1, &h, &1, &400, &None),
        Err(Ok(Error::HtlcInsufficientEscrow))
    );
}

#[test]
fn timeout_must_be_in_the_future() {
    let (env, client, _token, alice, bob, _charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    let h = hash(&env, b"x");
    // Genesis ledger is 0, so a timeout of 0 is already in the past.
    assert_eq!(
        client.try_add_htlc(&c1, &h, &100, &0, &None),
        Err(Ok(Error::HtlcTimeoutElapsed))
    );
}

#[test]
fn unknown_hop_is_not_found() {
    let (env, client, _token, alice, bob, _charlie) = setup();
    let c1 = client.open_channel(&alice, &bob, &pk(&env), &1_000, &720);
    assert_eq!(client.try_get_htlc(&c1, &999), Err(Ok(Error::HtlcNotFound)));
}
