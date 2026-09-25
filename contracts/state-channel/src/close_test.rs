//! Cooperative mutual close tests (issue #412).

extern crate std;

use super::*;
use crate::close::{close_payload, ChannelClosedCooperative, MutualCloseState};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Event,
};

const DEPOSIT: i128 = 1_000;
const CHALLENGE: u32 = 100;

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    contract: Address,
    token: Address,
    sender: Address,
    receiver: Address,
    sender_sk: SigningKey,
    receiver_sk: SigningKey,
}

fn setup() -> Setup {
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

    Setup {
        env,
        client,
        contract,
        token,
        sender,
        receiver,
        sender_sk: SigningKey::from_bytes(&[5u8; 32]),
        receiver_sk: SigningKey::from_bytes(&[6u8; 32]),
    }
}

fn pubkey(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

impl Setup {
    /// Open a channel and register the receiver's key.
    fn open(&self) -> u64 {
        let id = self.client.open_channel(
            &self.sender,
            &self.receiver,
            &pubkey(&self.env, &self.sender_sk),
            &DEPOSIT,
            &CHALLENGE,
        );
        self.client
            .register_receiver_key(&id, &pubkey(&self.env, &self.receiver_sk));
        id
    }

    fn final_state(&self, channel_id: u64, receiver: i128, sender: i128) -> MutualCloseState {
        MutualCloseState {
            channel_id,
            receiver_balance: receiver,
            sender_balance: sender,
        }
    }

    fn sign(&self, sk: &SigningKey, state: &MutualCloseState) -> BytesN<64> {
        let payload = close_payload(&self.env, &self.contract, state);
        let mut msg = std::vec![0u8; payload.len() as usize];
        payload.copy_into_slice(&mut msg);
        BytesN::from_array(&self.env, &sk.sign(&msg).to_bytes())
    }

    /// Sender-signed unilateral state, as used by `close_channel`/`dispute`.
    fn sign_update(&self, state: &StateUpdate) -> BytesN<64> {
        let pk = pubkey(&self.env, &self.sender_sk).to_array();
        let mut msg = [0u8; 56];
        msg[..32].copy_from_slice(&pk);
        msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
        msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
        BytesN::from_array(&self.env, &self.sender_sk.sign(&msg).to_bytes())
    }

    fn balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(who)
    }
}

#[test]
fn mutual_close_pays_both_parties_immediately() {
    let s = setup();
    let id = s.open();
    let state = s.final_state(id, 700, 300);

    s.client.mutual_close(
        &state,
        &s.sign(&s.sender_sk, &state),
        &s.sign(&s.receiver_sk, &state),
    );

    assert_eq!(
        s.env.events().all().filter_by_contract(&s.contract),
        std::vec![ChannelClosedCooperative {
            channel_id: id,
            receiver_balance: 700,
            sender_balance: 300,
        }
        .to_xdr(&s.env, &s.contract)]
    );
    assert_eq!(s.balance(&s.receiver), 700);
    assert_eq!(s.balance(&s.sender), 300);
    assert_eq!(s.balance(&s.contract), 0);
}

#[test]
fn mutual_close_reclaims_channel_storage() {
    let s = setup();
    let id = s.open();
    let state = s.final_state(id, 0, DEPOSIT);
    s.client.mutual_close(
        &state,
        &s.sign(&s.sender_sk, &state),
        &s.sign(&s.receiver_sk, &state),
    );

    assert_eq!(
        s.client.try_get_channel(&id),
        Err(Ok(Error::ChannelNotFound))
    );
    assert_eq!(s.client.get_receiver_key(&id), None);
    s.env.as_contract(&s.contract, || {
        assert!(!s.env.storage().instance().has(&DataKey::Channel(id)));
        assert!(!s.env.storage().instance().has(&DataKey::ReceiverPubkey(id)));
    });

    // The same envelope cannot be replayed once the channel is gone.
    assert_eq!(
        s.client.try_mutual_close(
            &state,
            &s.sign(&s.sender_sk, &state),
            &s.sign(&s.receiver_sk, &state),
        ),
        Err(Ok(Error::ChannelNotFound))
    );
}

#[test]
fn mutual_close_bypasses_an_active_dispute_window() {
    let s = setup();
    let id = s.open();

    let update = StateUpdate {
        nonce: 1,
        balance: 200,
    };
    s.client
        .close_channel(&id, &update, &s.sign_update(&update));
    let newer = StateUpdate {
        nonce: 2,
        balance: 400,
    };
    s.client.dispute(&id, &newer, &s.sign_update(&newer));
    assert_eq!(s.client.get_channel(&id).phase, ChannelPhase::Disputed);
    assert_eq!(
        s.client.try_finalize_dispute(&id),
        Err(Ok(Error::ChallengeActive))
    );

    let state = s.final_state(id, 400, 600);
    s.client.mutual_close(
        &state,
        &s.sign(&s.sender_sk, &state),
        &s.sign(&s.receiver_sk, &state),
    );
    assert_eq!(s.balance(&s.receiver), 400);
    assert_eq!(s.balance(&s.sender), 600);
}

#[test]
#[should_panic]
fn corrupted_receiver_signature_is_rejected() {
    let s = setup();
    let id = s.open();
    let state = s.final_state(id, 500, 500);
    let mut bad = s.sign(&s.receiver_sk, &state).to_array();
    bad[0] ^= 0x01;
    s.client.mutual_close(
        &state,
        &s.sign(&s.sender_sk, &state),
        &BytesN::from_array(&s.env, &bad),
    );
}

#[test]
#[should_panic]
fn corrupted_sender_signature_is_rejected() {
    let s = setup();
    let id = s.open();
    let state = s.final_state(id, 500, 500);
    let mut bad = s.sign(&s.sender_sk, &state).to_array();
    bad[63] ^= 0x01;
    s.client.mutual_close(
        &state,
        &BytesN::from_array(&s.env, &bad),
        &s.sign(&s.receiver_sk, &state),
    );
}

#[test]
fn signatures_over_a_different_split_do_not_verify() {
    let s = setup();
    let id = s.open();
    let agreed = s.final_state(id, 100, 900);
    let tampered = s.final_state(id, 900, 100);
    let result = s.client.try_mutual_close(
        &tampered,
        &s.sign(&s.sender_sk, &agreed),
        &s.sign(&s.receiver_sk, &agreed),
    );
    assert!(result.is_err());
    // Nothing moved: the channel is still open and fully escrowed.
    assert_eq!(s.client.get_channel(&id).phase, ChannelPhase::Open);
    assert_eq!(s.balance(&s.contract), DEPOSIT);
}

#[test]
fn signing_with_the_same_key_twice_is_rejected() {
    let s = setup();
    let id = s.open();
    let state = s.final_state(id, 1_000, 0);
    // The sender cannot stand in for the receiver.
    let sender_sig = s.sign(&s.sender_sk, &state);
    assert!(s
        .client
        .try_mutual_close(&state, &sender_sig, &sender_sig)
        .is_err());
    assert_eq!(s.balance(&s.contract), DEPOSIT);
}

#[test]
fn split_must_add_up_to_the_escrow() {
    let s = setup();
    let id = s.open();
    for (receiver, sender) in [(600, 300), (600, 500), (-1, 1_001)] {
        let state = s.final_state(id, receiver, sender);
        assert_eq!(
            s.client.try_mutual_close(
                &state,
                &s.sign(&s.sender_sk, &state),
                &s.sign(&s.receiver_sk, &state),
            ),
            Err(Ok(Error::ExceedsPayment))
        );
    }
}

#[test]
fn mutual_close_requires_a_registered_receiver_key() {
    let s = setup();
    let id = s.client.open_channel(
        &s.sender,
        &s.receiver,
        &pubkey(&s.env, &s.sender_sk),
        &DEPOSIT,
        &CHALLENGE,
    );
    let state = s.final_state(id, 500, 500);
    assert_eq!(
        s.client.try_mutual_close(
            &state,
            &s.sign(&s.sender_sk, &state),
            &s.sign(&s.receiver_sk, &state),
        ),
        Err(Ok(Error::InvalidSignature))
    );
}

#[test]
fn finalized_channel_cannot_be_mutually_closed() {
    let s = setup();
    let id = s.open();
    let update = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    s.client
        .close_channel(&id, &update, &s.sign_update(&update));
    s.env
        .ledger()
        .with_mut(|l| l.sequence_number += CHALLENGE + 1);
    s.client.claim(&id);

    let state = s.final_state(id, 0, DEPOSIT);
    assert_eq!(
        s.client.try_mutual_close(
            &state,
            &s.sign(&s.sender_sk, &state),
            &s.sign(&s.receiver_sk, &state),
        ),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
}

#[test]
#[should_panic]
fn only_the_receiver_can_register_its_key() {
    let s = setup();
    let id = s.client.open_channel(
        &s.sender,
        &s.receiver,
        &pubkey(&s.env, &s.sender_sk),
        &DEPOSIT,
        &CHALLENGE,
    );
    s.env.set_auths(&[]);
    s.client
        .register_receiver_key(&id, &pubkey(&s.env, &s.receiver_sk));
}
