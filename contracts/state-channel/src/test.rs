#![cfg(test)]

use super::*;
use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (Env, Address, Address, Address, SigningKey) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let signing_key = SigningKey::from_bytes(&[1u8; SECRET_KEY_LENGTH]);

    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();

    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000_000i128);

    (env, sender, receiver, token, signing_key)
}

fn make_channel(
    _env: &Env,
    client: &StateChannelClient,
    sender: &Address,
    receiver: &Address,
    sender_pubkey: &BytesN<32>,
    amount: i128,
    challenge_period: u32,
) -> u64 {
    client.open_channel(sender, receiver, sender_pubkey, &amount, &challenge_period)
}

fn pubkey_from_signing_key(env: &Env, sk: &SigningKey) -> BytesN<32> {
    let vk = sk.verifying_key();
    BytesN::from_array(env, &vk.to_bytes())
}

fn sign_state(
    env: &Env,
    sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    state: &StateUpdate,
) -> BytesN<64> {
    let mut payload = Bytes::new(env);
    payload.extend_from_slice(&sender_pubkey.to_array());
    payload.extend_from_slice(&state.nonce.to_be_bytes());
    payload.extend_from_slice(&state.balance.to_be_bytes());
    let mut msg = [0u8; 56];
    msg[..32].copy_from_slice(&sender_pubkey.to_array());
    msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
    msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
    let sig = sk.sign(&msg);
    BytesN::from_array(env, &sig.to_bytes())
}

fn advance_ledger(env: &Env, to: u32) {
    env.ledger().with_mut(|li| li.sequence_number = to);
}

// ── Initialization ───────────────────────────────────────────────────────────

#[test]
fn test_initialize() {
    let (env, _sender, _receiver, token, _sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);
    assert_eq!(client.get_token(), token);
    assert_eq!(client.get_channel_count(), 0);
}

#[test]
fn test_initialize_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);
    assert_eq!(
        client.try_initialize(&token),
        Err(Ok(Error::AlreadyInitialized))
    );
}

// ── Open Channel ─────────────────────────────────────────────────────────────

#[test]
fn test_open_channel() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);
    assert_eq!(channel_id, 1);
    assert_eq!(client.get_channel_count(), 1);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.sender, sender);
    assert_eq!(ch.receiver, receiver);
    assert_eq!(ch.amount, 1000);
    assert_eq!(ch.nonce, 0);
    assert_eq!(ch.balance, 0);
    assert_eq!(ch.phase, ChannelPhase::Open);
    assert_eq!(ch.sender_pubkey, pk);
}

#[test]
fn test_open_channel_zero_amount_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);
    let pk = pubkey_from_signing_key(&env, &sk);
    assert_eq!(
        client.try_open_channel(&sender, &receiver, &pk, &0, &720),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_open_channel_negative_amount_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);
    let pk = pubkey_from_signing_key(&env, &sk);
    assert_eq!(
        client.try_open_channel(&sender, &receiver, &pk, &-1, &720),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_open_channel_default_challenge_period() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);
    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 0);
    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.challenge_period, DEFAULT_CHALLENGE_PERIOD);
}

#[test]
fn test_open_channel_capped_challenge_period() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);
    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 99999);
    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.challenge_period, MAX_CHALLENGE_PERIOD);
}

// ── Update State ─────────────────────────────────────────────────────────────

#[test]
fn test_update_state() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.update_state(&channel_id, &state, &sig);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.nonce, 1);
    assert_eq!(ch.balance, 100);
}

#[test]
fn test_update_state_stale_nonce_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.update_state(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 1,
        balance: 200,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    assert_eq!(
        client.try_update_state(&channel_id, &state2, &sig2),
        Err(Ok(Error::StaleState))
    );
}

#[test]
fn test_update_state_exceeds_amount_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 1001,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    assert_eq!(
        client.try_update_state(&channel_id, &state, &sig),
        Err(Ok(Error::ExceedsPayment))
    );
}

#[test]
fn test_update_state_on_closed_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let close_state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let close_sig = sign_state(&env, &sk, &pk, &close_state);
    client.close_channel(&channel_id, &close_state, &close_sig);

    let state = StateUpdate {
        nonce: 2,
        balance: 600,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    assert_eq!(
        client.try_update_state(&channel_id, &state, &sig),
        Err(Ok(Error::ChannelNotOpen))
    );
}

// ── Close Channel ────────────────────────────────────────────────────────────

#[test]
fn test_close_channel() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 3,
        balance: 750,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Closed);
    assert_eq!(ch.balance, 750);
    assert_eq!(ch.nonce, 3);
    // closed_at is set from env.ledger().sequence() at close time.
}

#[test]
fn test_close_already_closed_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 600,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    assert_eq!(
        client.try_close_channel(&channel_id, &state2, &sig2),
        Err(Ok(Error::ChannelNotOpen))
    );
}

// ── Dispute ──────────────────────────────────────────────────────────────────

#[test]
fn test_dispute() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let challenge_period = 10u32;
    let channel_id = make_channel(
        &env,
        &client,
        &sender,
        &receiver,
        &pk,
        1000,
        challenge_period,
    );

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.close_channel(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 500,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.dispute(&channel_id, &state2, &sig2);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Disputed);
    assert_eq!(ch.nonce, 2);
    assert_eq!(ch.balance, 500);
    // The dispute records the exact ledger at which it was initiated.
    assert_eq!(ch.disputed_at, env.ledger().sequence());
}

#[test]
fn test_dispute_stale_nonce_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state5 = StateUpdate {
        nonce: 5,
        balance: 500,
    };
    let sig5 = sign_state(&env, &sk, &pk, &state5);
    client.close_channel(&channel_id, &state5, &sig5);

    let state3 = StateUpdate {
        nonce: 3,
        balance: 300,
    };
    let sig3 = sign_state(&env, &sk, &pk, &state3);
    assert_eq!(
        client.try_dispute(&channel_id, &state3, &sig3),
        Err(Ok(Error::StaleState))
    );
}

#[test]
fn test_dispute_on_open_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    assert_eq!(
        client.try_dispute(&channel_id, &state, &sig),
        Err(Ok(Error::ChannelNotOpen))
    );
}

// ── Claim ────────────────────────────────────────────────────────────────────

#[test]
fn test_claim_after_dispute_window() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let challenge_period = 10u32;
    let channel_id = make_channel(
        &env,
        &client,
        &sender,
        &receiver,
        &pk,
        1000,
        challenge_period,
    );

    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    let ch = client.get_channel(&channel_id);
    advance_ledger(&env, ch.closed_at + challenge_period + 1);

    client.claim(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
}

#[test]
fn test_claim_during_dispute_window_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    assert_eq!(
        client.try_claim(&channel_id),
        Err(Ok(Error::ChallengeActive))
    );
}

#[test]
fn test_claim_on_open_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let _channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    assert_eq!(
        client.try_claim(&_channel_id),
        Err(Ok(Error::ChallengeActive))
    );
}

#[test]
fn test_claim_zero_balance() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state = StateUpdate {
        nonce: 1,
        balance: 0,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    let ch = client.get_channel(&channel_id);
    advance_ledger(&env, ch.closed_at + 11);

    client.claim(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
}

// ── Reclaim ──────────────────────────────────────────────────────────────────

#[test]
fn test_reclaim_expired_channel() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);
    let ch = client.get_channel(&channel_id);

    let max_lifetime = client.get_max_channel_lifetime();
    advance_ledger(&env, ch.opened_at + max_lifetime + 1);

    client.reclaim(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
}

#[test]
fn test_reclaim_active_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    assert_eq!(
        client.try_reclaim(&channel_id),
        Err(Ok(Error::ChannelNotOpen))
    );
}

#[test]
fn test_reclaim_closed_channel_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.close_channel(&channel_id, &state, &sig);

    assert_eq!(
        client.try_reclaim(&channel_id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
}

// ── Full lifecycle ───────────────────────────────────────────────────────────

#[test]
fn test_full_lifecycle_cooperative() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    for nonce in 1..=5u64 {
        let state = StateUpdate {
            nonce,
            balance: nonce as i128 * 100,
        };
        let sig = sign_state(&env, &sk, &pk, &state);
        client.update_state(&channel_id, &state, &sig);
    }

    let final_state = StateUpdate {
        nonce: 6,
        balance: 600,
    };
    let sig = sign_state(&env, &sk, &pk, &final_state);
    client.close_channel(&channel_id, &final_state, &sig);

    let ch = client.get_channel(&channel_id);
    advance_ledger(&env, ch.closed_at + DEFAULT_CHALLENGE_PERIOD + 1);

    client.claim(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
    assert_eq!(ch.balance, 600);
}

#[test]
fn test_full_lifecycle_dispute() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.update_state(&channel_id, &state1, &sig1);
    client.close_channel(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 500,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.dispute(&channel_id, &state2, &sig2);

    // The dispute settles after its window elapses.
    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Disputed);
    advance_ledger(&env, ch.disputed_at + 11);

    client.finalize_dispute(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
    assert_eq!(ch.balance, 500);
}

#[test]
fn test_dispute_then_counter_evidence_then_finalize() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.close_channel(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 300,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.dispute(&channel_id, &state2, &sig2);

    // Sender replies with counter-evidence: an even newer signed state.
    let state3 = StateUpdate {
        nonce: 3,
        balance: 600,
    };
    let sig3 = sign_state(&env, &sk, &pk, &state3);
    client.submit_counter_evidence(&channel_id, &state3, &sig3);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Disputed);
    assert_eq!(ch.balance, 600);

    // Counter-evidence re-arms the window from its own submission ledger.
    advance_ledger(&env, ch.disputed_at + 11);
    client.finalize_dispute(&channel_id);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.phase, ChannelPhase::Finalized);
    assert_eq!(ch.balance, 600);
}

#[test]
fn test_late_counter_proof_extends_the_dispute_window() {
    let (env, sender, receiver, token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 100);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    client.close_channel(&channel_id, &state1, &sign_state(&env, &sk, &pk, &state1));

    let state2 = StateUpdate {
        nonce: 2,
        balance: 200,
    };
    client.dispute(&channel_id, &state2, &sign_state(&env, &sk, &pk, &state2));

    let disputed_at = client.get_channel(&channel_id).disputed_at;
    let period = client.get_channel(&channel_id).challenge_period;
    let original_deadline = disputed_at + period;

    // Land inside the final stretch of the window (40 ledgers to go).
    advance_ledger(&env, original_deadline - 40);
    let state3 = StateUpdate {
        nonce: 3,
        balance: 300,
    };
    client.submit_counter_evidence(&channel_id, &state3, &sign_state(&env, &sk, &pk, &state3));

    // The window moved forward past its original deadline.
    assert!(client.get_channel(&channel_id).disputed_at > disputed_at);

    // At the original deadline the dispute is still active because it extended.
    advance_ledger(&env, original_deadline);
    assert_eq!(
        client.try_finalize_dispute(&channel_id),
        Err(Ok(Error::ChallengeActive))
    );

    // Past the extended deadline it finalizes normally.
    advance_ledger(&env, original_deadline + 60);
    client.finalize_dispute(&channel_id);
    assert_eq!(
        client.get_channel(&channel_id).phase,
        ChannelPhase::Finalized
    );
}

#[test]
fn test_dispute_extension_cap_is_enforced() {
    let (env, sender, receiver, token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 100);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    client.close_channel(&channel_id, &state1, &sign_state(&env, &sk, &pk, &state1));
    let state2 = StateUpdate {
        nonce: 2,
        balance: 200,
    };
    client.dispute(&channel_id, &state2, &sign_state(&env, &sk, &pk, &state2));

    // Three late counter-proofs each extend the window by one stretch.
    for i in 1..=3u32 {
        let ch = client.get_channel(&channel_id);
        let deadline = ch.disputed_at + ch.challenge_period;
        advance_ledger(&env, deadline - 40);
        let state = StateUpdate {
            nonce: 2 + i as u64,
            balance: 100 * (i as i128 + 2),
        };
        let sig = sign_state(&env, &sk, &pk, &state);
        client.submit_counter_evidence(&channel_id, &state, &sig);
    }
    assert_eq!(client.get_channel(&channel_id).dispute_extensions, 3);

    // A fourth late counter-proof is rejected: the extension budget is spent.
    let ch = client.get_channel(&channel_id);
    advance_ledger(&env, ch.disputed_at + ch.challenge_period - 40);
    let state = StateUpdate {
        nonce: 6,
        balance: 900,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    assert_eq!(
        client.try_submit_counter_evidence(&channel_id, &state, &sig),
        Err(Ok(Error::DisputeExtensionLimitReached))
    );
}

#[test]
fn test_finalize_dispute_before_window_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.close_channel(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 300,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.dispute(&channel_id, &state2, &sig2);

    // Still inside the dispute window.
    assert_eq!(
        client.try_finalize_dispute(&channel_id),
        Err(Ok(Error::ChallengeActive))
    );
}

#[test]
fn test_finalize_dispute_on_non_disputed_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    // Channel is still open — nothing to finalize.
    assert_eq!(
        client.try_finalize_dispute(&channel_id),
        Err(Ok(Error::ChannelNotOpen))
    );
}

#[test]
fn test_submit_counter_evidence_after_dispute_window_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.close_channel(&channel_id, &state1, &sig1);

    let state2 = StateUpdate {
        nonce: 2,
        balance: 300,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.dispute(&channel_id, &state2, &sig2);

    let ch = client.get_channel(&channel_id);
    advance_ledger(&env, ch.disputed_at + 11);

    let state3 = StateUpdate {
        nonce: 3,
        balance: 600,
    };
    let sig3 = sign_state(&env, &sk, &pk, &state3);
    assert_eq!(
        client.try_submit_counter_evidence(&channel_id, &state3, &sig3),
        Err(Ok(Error::ChallengeExpired))
    );
}

#[test]
fn test_submit_counter_evidence_on_non_disputed_fails() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 10);

    let state1 = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig1 = sign_state(&env, &sk, &pk, &state1);
    client.close_channel(&channel_id, &state1, &sig1);

    // Channel is closed but not disputed — counter-evidence is invalid here.
    let state2 = StateUpdate {
        nonce: 2,
        balance: 300,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    assert_eq!(
        client.try_submit_counter_evidence(&channel_id, &state2, &sig2),
        Err(Ok(Error::ChannelNotOpen))
    );
}

// ── Multiple channels ────────────────────────────────────────────────────────

#[test]
fn test_multiple_channels() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let id1 = make_channel(&env, &client, &sender, &receiver, &pk, 500, 720);

    let sk2 = SigningKey::from_bytes(&[2u8; SECRET_KEY_LENGTH]);
    let pk2 = pubkey_from_signing_key(&env, &sk2);
    let id2 = make_channel(&env, &client, &sender, &receiver, &pk2, 300, 720);

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(client.get_channel_count(), 2);

    let ch1 = client.get_channel(&id1);
    let ch2 = client.get_channel(&id2);
    assert_eq!(ch1.amount, 500);
    assert_eq!(ch2.amount, 300);
}

// ── Config getters ───────────────────────────────────────────────────────────

#[test]
fn test_config_getters() {
    let (env, _sender, _receiver, _token, _sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    assert_eq!(
        client.get_default_challenge_period(),
        DEFAULT_CHALLENGE_PERIOD
    );
    assert_eq!(client.get_max_challenge_period(), MAX_CHALLENGE_PERIOD);
    assert_eq!(
        client.get_max_channel_lifetime(),
        DEFAULT_MAX_CHANNEL_LIFETIME
    );
}

// ── Bitmap nonce window (issue #374) ─────────────────────────────────────────

/// Out-of-order arrival: nonce 5 lands before 3, both with a non-regressing
/// balance — previously the strict `nonce <= channel.nonce` check rejected
/// the late sibling; the bitmap window accepts each nonce exactly once.
#[test]
fn test_out_of_order_nonce_arrival_accepted() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state5 = StateUpdate {
        nonce: 5,
        balance: 100,
    };
    let sig5 = sign_state(&env, &sk, &pk, &state5);
    client.update_state(&channel_id, &state5, &sig5);

    // Nonce 3 arrives afterwards: fresh bit, balance grows — accepted.
    let state3 = StateUpdate {
        nonce: 3,
        balance: 200,
    };
    let sig3 = sign_state(&env, &sk, &pk, &state3);
    client.update_state(&channel_id, &state3, &sig3);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.balance, 200);
    // The high-water mark keeps the highest nonce ever consumed.
    assert_eq!(ch.nonce, 5);

    let window = client.get_nonce_window(&channel_id);
    assert_eq!(window.base, 0);
    assert!(window.is_consumed(3));
    assert!(window.is_consumed(5));
    assert!(!window.is_consumed(4));
}
/// Replay: after out-of-order consumption, re-submitting either nonce must
/// fail with `StaleState` — the bit stays set.
#[test]
fn test_replayed_nonce_rejected_after_out_of_order_use() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state5 = StateUpdate {
        nonce: 5,
        balance: 100,
    };
    let sig5 = sign_state(&env, &sk, &pk, &state5);
    client.update_state(&channel_id, &state5, &sig5);

    let state3 = StateUpdate {
        nonce: 3,
        balance: 200,
    };
    let sig3 = sign_state(&env, &sk, &pk, &state3);
    client.update_state(&channel_id, &state3, &sig3);

    // Replay of the out-of-order nonce 3 (balance would even be valid).
    let replay3 = StateUpdate {
        nonce: 3,
        balance: 300,
    };
    let sig_r3 = sign_state(&env, &sk, &pk, &replay3);
    assert_eq!(
        client.try_update_state(&channel_id, &replay3, &sig_r3),
        Err(Ok(Error::StaleState))
    );

    // Replay of nonce 5 as well.
    let replay5 = StateUpdate {
        nonce: 5,
        balance: 400,
    };
    let sig_r5 = sign_state(&env, &sk, &pk, &replay5);
    assert_eq!(
        client.try_update_state(&channel_id, &replay5, &sig_r5),
        Err(Ok(Error::StaleState))
    );
}
/// Window slide: consuming nonce 300 moves the window to base 256, after
/// which old-window nonces (e.g. 2) are permanently out of range while
/// in-window replays (300) stay rejected.
#[test]
fn test_nonce_window_slide_rejects_behind_nonces() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    // First a small in-window nonce so the channel has a baseline state.
    let state2 = StateUpdate {
        nonce: 2,
        balance: 100,
    };
    let sig2 = sign_state(&env, &sk, &pk, &state2);
    client.update_state(&channel_id, &state2, &sig2);

    // Jump past the first window: 300 lands in bucket base 256.
    let state300 = StateUpdate {
        nonce: 300,
        balance: 200,
    };
    let sig300 = sign_state(&env, &sk, &pk, &state300);
    client.update_state(&channel_id, &state300, &sig300);

    let window = client.get_nonce_window(&channel_id);
    assert_eq!(window.base, 256);
    assert!(window.is_consumed(300));
    assert!(!window.is_consumed(301));
    // Nonce 2 is now before the window base — consumed-state, never usable.
    assert!(window.is_consumed(2));

    // Replay of 300: bit still set inside the window.
    let replay = StateUpdate {
        nonce: 300,
        balance: 300,
    };
    let sig_r = sign_state(&env, &sk, &pk, &replay);
    assert_eq!(
        client.try_update_state(&channel_id, &replay, &sig_r),
        Err(Ok(Error::StaleState))
    );

    // An old-window nonce can never re-enter: out of range entirely.
    let old = StateUpdate {
        nonce: 2,
        balance: 400,
    };
    let sig_old = sign_state(&env, &sk, &pk, &old);
    assert_eq!(
        client.try_update_state(&channel_id, &old, &sig_old),
        Err(Ok(Error::StaleState))
    );
    // The next fresh nonce inside the current window still works.
    let state301 = StateUpdate {
        nonce: 301,
        balance: 500,
    };
    let sig301 = sign_state(&env, &sk, &pk, &state301);
    client.update_state(&channel_id, &state301, &sig301);
    assert_eq!(client.get_channel(&channel_id).balance, 500);
}
/// A signed state that lowers the receiver's entitlement is stale even when
/// its nonce is fresh — the balance-regression guard (issue #374).
#[test]
fn test_balance_regression_rejected_with_fresh_nonce() {
    let (env, sender, receiver, _token, sk) = setup();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&_token);

    let pk = pubkey_from_signing_key(&env, &sk);
    let channel_id = make_channel(&env, &client, &sender, &receiver, &pk, 1000, 720);

    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = sign_state(&env, &sk, &pk, &state);
    client.update_state(&channel_id, &state, &sig);

    let regressed = StateUpdate {
        nonce: 2,
        balance: 300,
    };
    let sig_r = sign_state(&env, &sk, &pk, &regressed);
    assert_eq!(
        client.try_update_state(&channel_id, &regressed, &sig_r),
        Err(Ok(Error::StaleState))
    );
}
