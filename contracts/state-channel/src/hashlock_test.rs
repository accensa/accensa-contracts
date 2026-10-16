//! Hashlock pre-image reveal tests (issue #488).

extern crate std;

use super::*;
use crate::hashlock::HashlockPaymentState;
use crate::htlc::HtlcState;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, BytesN, Env, Event,
};

const DEPOSIT: i128 = 1_000;

fn setup() -> (Env, StateChannelClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&sender, &DEPOSIT);

    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    (env, client, token, sender, receiver)
}

fn pk(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

fn hash(env: &Env, preimage: &[u8]) -> BytesN<32> {
    env.crypto()
        .sha256(&Bytes::from_slice(env, preimage))
        .into()
}

fn preimage_bytes(env: &Env, data: &[u8]) -> Bytes {
    Bytes::from_slice(env, data)
}

/// Sign a state update the way `StateChannel::state_payload` builds it.
fn sign_state(
    env: &Env,
    sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    state: &StateUpdate,
) -> BytesN<64> {
    let mut msg = [0u8; 56];
    msg[..32].copy_from_slice(&sender_pubkey.to_array());
    msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
    msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
    BytesN::from_array(env, &sk.sign(&msg).to_bytes())
}

#[test]
fn valid_reveal_credits_receiver_balance() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);

    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"secret"), &300);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 300);
    assert_eq!(
        client.get_hashlock_payment(&channel_id, &payment_id).state,
        HashlockPaymentState::Pending
    );

    // The reveal is permissionless and verifies sha256(preimage) == hashlock.
    let credited =
        client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"secret"));
    assert_eq!(credited, 300);

    let channel = client.get_channel(&channel_id);
    assert_eq!(channel.balance, 300);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 0);
    assert_eq!(
        client.get_hashlock_payment(&channel_id, &payment_id).state,
        HashlockPaymentState::Resolved
    );
    // No tokens moved out of escrow: the receiver's entitlement grew by
    // exactly what the reservation released.
    assert_eq!(client.get_reserved_escrow(&channel_id), 300);
    assert_eq!(TokenClient::new(&env, &_token).balance(&receiver), 0);
    assert_eq!(
        TokenClient::new(&env, &_token).balance(&client.address),
        DEPOSIT
    );
}

#[test]
fn reveal_event_carries_the_hashlock_not_the_secret() {
    let (env, client, _token, sender, receiver) = setup();
    let contract = client.address.clone();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"secret"), &300);

    client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"secret"));

    // `events().all()` covers the last invocation only: the reveal itself.
    let events = env.events().all().filter_by_contract(&contract);
    assert_eq!(events.events().len(), 1);
    // Compare in the same XDR form the mutual-close tests use.
    let expected = crate::hashlock::HashlockPaymentRevealedEvent {
        channel_id,
        payment_id,
        amount: 300,
        preimage_sha256: hash(&env, b"secret"),
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&env, &contract));
    let _ = &_token;
}

#[test]
fn invalid_reveal_is_rejected_and_payment_stays_pending() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"correct"), &100);

    assert_eq!(
        client.try_reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"wrong")),
        Err(Ok(Error::InvalidPreimage))
    );
    // Nothing changed: still pending, still reserved, receiver not credited.
    assert_eq!(
        client.get_hashlock_payment(&channel_id, &payment_id).state,
        HashlockPaymentState::Pending
    );
    assert_eq!(client.get_hashlock_reserved(&channel_id), 100);
    assert_eq!(client.get_channel(&channel_id).balance, 0);

    // The correct preimage still settles it afterwards.
    client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"correct"));
    assert_eq!(client.get_channel(&channel_id).balance, 100);
}

#[test]
fn double_reveal_is_rejected() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"s"), &100);

    client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"s"));
    assert_eq!(
        client.try_reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"s")),
        Err(Ok(Error::HtlcNotPending))
    );
    // Credited exactly once.
    assert_eq!(client.get_channel(&channel_id).balance, 100);
    let _ = receiver;
}

#[test]
fn payment_cannot_exceed_free_escrow() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &100, &720);

    assert_eq!(
        client.try_add_hashlock_payment(&channel_id, &hash(&env, b"x"), &101),
        Err(Ok(Error::HtlcInsufficientEscrow))
    );

    // Two payments exactly covering the escrow fit; a third does not.
    client.add_hashlock_payment(&channel_id, &hash(&env, b"one"), &40);
    client.add_hashlock_payment(&channel_id, &hash(&env, b"two"), &60);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 100);
    assert_eq!(
        client.try_add_hashlock_payment(&channel_id, &hash(&env, b"three"), &1),
        Err(Ok(Error::HtlcInsufficientEscrow))
    );
}

#[test]
fn payments_share_the_ceiling_with_htlcs() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);

    client.add_htlc(&channel_id, &hash(&env, b"htlc"), &400, &500, &None);

    // The HTLC reserved 400; a hashlock payment can only take 600 more.
    assert_eq!(
        client.try_add_hashlock_payment(&channel_id, &hash(&env, b"inv"), &601),
        Err(Ok(Error::HtlcInsufficientEscrow))
    );
    client.add_hashlock_payment(&channel_id, &hash(&env, b"inv"), &600);
    assert_eq!(client.get_htlc_reserved(&channel_id), 400);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 600);
    assert_eq!(client.get_reserved_escrow(&channel_id), DEPOSIT);

    // Revealing the payment releases only its own reservation.
    client.reveal_preimage(&channel_id, &1, &preimage_bytes(&env, b"inv"));
    assert_eq!(client.get_channel(&channel_id).balance, 600);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 0);
    assert_eq!(client.get_reserved_escrow(&channel_id), DEPOSIT);
    let _ = receiver;
}

#[test]
#[should_panic]
fn sender_auth_is_required_to_lock_a_payment() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &1_000, &720);
    env.set_auths(&[]);
    client.add_hashlock_payment(&channel_id, &hash(&env, b"x"), &100);
}

#[test]
fn unknown_payment_is_not_found() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);
    assert_eq!(
        client.try_get_hashlock_payment(&channel_id, &999),
        Err(Ok(Error::HtlcNotFound))
    );
    assert_eq!(
        client.try_reveal_preimage(&channel_id, &999, &preimage_bytes(&env, b"x")),
        Err(Ok(Error::HtlcNotFound))
    );
    let _ = receiver;
}

#[test]
fn close_with_preimage_rejects_a_wrong_preimage_atomically() {
    let (env, client, token, sender, receiver) = setup();

    let sk = SigningKey::from_bytes(&[9u8; 32]);
    let sender_pubkey = BytesN::from_array(&env, &sk.verifying_key().to_bytes());
    let channel_id = client.open_channel(&sender, &receiver, &sender_pubkey, &DEPOSIT, &720);

    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"invoice"), &300);
    let state = StateUpdate {
        nonce: 1,
        balance: 400,
    };
    let sig = sign_state(&env, &sk, &sender_pubkey, &state);

    // Valid signature, wrong secret: the reveal fails and nothing about the
    // close is recorded — the invoice is not lost, the channel stays open.
    assert_eq!(
        client.try_close_channel_with_preimage(
            &channel_id,
            &state,
            &sig,
            &payment_id,
            &preimage_bytes(&env, b"not-the-secret"),
        ),
        Err(Ok(Error::InvalidPreimage))
    );
    assert_eq!(client.get_channel(&channel_id).phase, ChannelPhase::Open);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 300);
    assert_eq!(TokenClient::new(&env, &token).balance(&receiver), 0);
}

#[test]
fn close_with_preimage_settles_the_invoice_without_waiting() {
    let (env, client, token, sender, receiver) = setup();

    let sk = SigningKey::from_bytes(&[9u8; 32]);
    let sender_pubkey = BytesN::from_array(&env, &sk.verifying_key().to_bytes());
    let channel_id = client.open_channel(&sender, &receiver, &sender_pubkey, &DEPOSIT, &720);

    let preimage = b"invoice-secret";
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, preimage), &250);

    let state = StateUpdate {
        nonce: 1,
        balance: 400,
    };
    let sig = sign_state(&env, &sk, &sender_pubkey, &state);

    client.close_channel_with_preimage(
        &channel_id,
        &state,
        &sig,
        &payment_id,
        &preimage_bytes(&env, preimage),
    );

    let channel = client.get_channel(&channel_id);
    assert_eq!(channel.phase, ChannelPhase::Closed);
    // Signed balance 400 + revealed 250.
    assert_eq!(channel.balance, 650);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 0);
    assert_eq!(client.get_reserved_escrow(&channel_id), 650);

    // Settlement pays exactly balance (invoice included) to the receiver and
    // the rest to the sender once the challenge window lapses.
    env.ledger()
        .with_mut(|l| l.sequence_number = channel.closed_at + channel.challenge_period + 1);
    client.claim(&channel_id);

    // `claim` pays the receiver exactly the settled balance; the sender's
    // remainder stays escrowed (the contract's asymmetric close flow).
    assert_eq!(TokenClient::new(&env, &token).balance(&receiver), 650);
    assert_eq!(TokenClient::new(&env, &token).balance(&sender), 0);
    assert_eq!(TokenClient::new(&env, &token).balance(&client.address), 350);
    assert_eq!(
        client.get_channel(&channel_id).phase,
        ChannelPhase::Finalized
    );
}

#[test]
fn close_with_preimage_rejects_revealing_twice() {
    let (env, client, _token, sender, receiver) = setup();

    let sk = SigningKey::from_bytes(&[9u8; 32]);
    let sender_pubkey = BytesN::from_array(&env, &sk.verifying_key().to_bytes());
    let channel_id = client.open_channel(&sender, &receiver, &sender_pubkey, &DEPOSIT, &720);

    let preimage = b"secret";
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, preimage), &100);

    // A standalone reveal consumes the payment first...
    client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, preimage));

    let state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig = sign_state(&env, &sk, &sender_pubkey, &state);
    // ...so the close-time reveal cannot credit it a second time.
    assert_eq!(
        client.try_close_channel_with_preimage(
            &channel_id,
            &state,
            &sig,
            &payment_id,
            &preimage_bytes(&env, preimage),
        ),
        Err(Ok(Error::HtlcNotPending))
    );
    assert_eq!(client.get_channel(&channel_id).phase, ChannelPhase::Open);
}

// ── Reserve bookkeeping: allocation cursor and reserved total (issue #622) ──

/// Recompute the reserve invariant from the payment records themselves
/// (`DataKey::HashlockPayment`): `DataKey::HashlockReserved` (lib.rs:129)
/// must equal the sum of the amounts of the still-`Pending` payments among
/// `ids` after every transition — nothing more (escrow stays over-committed)
/// and nothing less (a resolve left the reserve stale). `expected_pending` is
/// spelled out at every call site so the invariant is pinned against an
/// independently-derived number, not just against itself.
fn assert_reserve_invariant(
    client: &StateChannelClient<'static>,
    channel_id: u64,
    ids: &[u64],
    expected_pending: i128,
) {
    let mut pending_sum: i128 = 0;
    for id in ids {
        let payment = client.get_hashlock_payment(&channel_id, id);
        if payment.state == HashlockPaymentState::Pending {
            pending_sum += payment.amount;
        }
    }
    assert_eq!(
        pending_sum, expected_pending,
        "test bug: wrong pending expectation"
    );
    assert_eq!(client.get_hashlock_reserved(&channel_id), pending_sum);
}

#[test]
fn count_key_allocates_distinct_sequential_ids() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);

    // `DataKey::HashlockPaymentCount` (lib.rs:126) is the allocation
    // cursor: ids start at 1 and advance by exactly one per add, so two
    // payments on the same channel can never collide.
    let one = client.add_hashlock_payment(&channel_id, &hash(&env, b"one"), &100);
    let two = client.add_hashlock_payment(&channel_id, &hash(&env, b"two"), &150);
    assert_eq!(one, 1);
    assert_eq!(two, one + 1);

    // Resolving a payment must not rewind the cursor: the next add still
    // gets the next id and never reuses a settled slot.
    client.reveal_preimage(&channel_id, &one, &preimage_bytes(&env, b"one"));
    let three = client.add_hashlock_payment(&channel_id, &hash(&env, b"three"), &50);
    assert_eq!(three, two + 1);

    // 150 still pending (two) + 50 just added (three); one's 100 released.
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 200);
}

#[test]
fn hashlock_reserved_equals_pending_sum_after_every_transition() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);

    // Every add raises the reserve by exactly the new payment's amount.
    let one = client.add_hashlock_payment(&channel_id, &hash(&env, b"one"), &100);
    assert_reserve_invariant(&client, channel_id, &[one], 100);
    let two = client.add_hashlock_payment(&channel_id, &hash(&env, b"two"), &150);
    assert_reserve_invariant(&client, channel_id, &[one, two], 250);
    let three = client.add_hashlock_payment(&channel_id, &hash(&env, b"three"), &50);
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 300);

    // A reveal releases exactly the revealed payment's own amount.
    client.reveal_preimage(&channel_id, &two, &preimage_bytes(&env, b"two"));
    assert_eq!(client.get_channel(&channel_id).balance, 150);
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 150);

    // A double reveal is rejected, and the reserve is left exactly at the
    // pending sum — it can neither re-release nor keep escrow reserved.
    assert_eq!(
        client.try_reveal_preimage(&channel_id, &two, &preimage_bytes(&env, b"two")),
        Err(Ok(Error::HtlcNotPending))
    );
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 150);

    client.reveal_preimage(&channel_id, &one, &preimage_bytes(&env, b"one"));
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 50);

    client.reveal_preimage(&channel_id, &three, &preimage_bytes(&env, b"three"));
    assert_reserve_invariant(&client, channel_id, &[one, two, three], 0);
    assert_eq!(client.get_channel(&channel_id).balance, 300);
    assert_eq!(client.get_hashlock_reserved(&channel_id), 0);
}

#[test]
fn refund_and_resolve_each_release_exactly_their_own_amount() {
    let (env, client, _token, sender, receiver) = setup();
    let channel_id = client.open_channel(&sender, &receiver, &pk(&env), &DEPOSIT, &720);

    // A hashlock invoice (120) and an HTLC hop (300) share the escrow; the
    // invoice's reserve lives under its own key, separate from the hop's.
    let payment_id = client.add_hashlock_payment(&channel_id, &hash(&env, b"inv"), &120);
    let htlc_id = client.add_htlc(&channel_id, &hash(&env, b"hop"), &300, &100, &None);
    assert_reserve_invariant(&client, channel_id, &[payment_id], 120);
    assert_eq!(client.get_htlc_reserved(&channel_id), 300);
    assert_eq!(client.get_reserved_escrow(&channel_id), 420);

    // The hop cannot be refunded before its timeout has passed.
    assert_eq!(
        client.try_refund_htlc(&channel_id, &htlc_id),
        Err(Ok(Error::HtlcNotExpired))
    );

    env.ledger().set_sequence_number(101);
    client.refund_htlc(&channel_id, &htlc_id);

    // The refund released exactly the hop's own 300 back to free escrow —
    // the hashlock reserve and the pending invoice are untouched — and a
    // refunded hop cannot be refunded again.
    assert_eq!(client.get_htlc_reserved(&channel_id), 0);
    assert_eq!(
        client.get_htlc(&channel_id, &htlc_id).state,
        HtlcState::Refunded
    );
    assert_reserve_invariant(&client, channel_id, &[payment_id], 120);
    assert_eq!(client.get_reserved_escrow(&channel_id), 120);
    assert_eq!(
        client.try_refund_htlc(&channel_id, &htlc_id),
        Err(Ok(Error::HtlcNotPending))
    );

    // The reveal released exactly the invoice's own 120 into the
    // receiver's balance.
    let credited = client.reveal_preimage(&channel_id, &payment_id, &preimage_bytes(&env, b"inv"));
    assert_eq!(credited, 120);
    assert_reserve_invariant(&client, channel_id, &[payment_id], 0);
    assert_eq!(client.get_channel(&channel_id).balance, 120);
    assert_eq!(client.get_reserved_escrow(&channel_id), 120);
}
