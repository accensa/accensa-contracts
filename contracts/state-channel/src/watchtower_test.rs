//! Watchtower reward bounty tests (issue #459).

extern crate std;

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    token: Address,
    sender: Address,
    receiver: Address,
    sk: SigningKey,
    pk: BytesN<32>,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let sk = SigningKey::from_bytes(&[1u8; 32]);
    let pk = BytesN::from_array(&env, &sk.verifying_key().to_bytes());

    let token = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);
    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

    Setup {
        env,
        client,
        token,
        sender,
        receiver,
        sk,
        pk,
    }
}

impl Setup {
    fn sign(&self, nonce: u64, balance: i128) -> BytesN<64> {
        let mut msg = [0u8; 56];
        msg[..32].copy_from_slice(&self.pk.to_array());
        msg[32..40].copy_from_slice(&nonce.to_be_bytes());
        msg[40..56].copy_from_slice(&balance.to_be_bytes());
        BytesN::from_array(&self.env, &self.sk.sign(&msg).to_bytes())
    }

    fn state(&self, nonce: u64, balance: i128) -> StateUpdate {
        StateUpdate { nonce, balance }
    }

    fn open(&self, amount: i128, challenge: u32) -> u64 {
        self.client
            .open_channel(&self.sender, &self.receiver, &self.pk, &amount, &challenge)
    }

    fn balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(who)
    }
}

/// A watchtower that files the winning counter-evidence earns its bounty out of
/// the receiver's recovery, leaving the escrow balanced.
#[test]
fn watchtower_is_paid_from_recovered_balance() {
    let s = setup();
    let channel_id = s.open(1_000, 100);
    let watchtower = Address::generate(&s.env);

    // 10% bounty, posted by the receiver.
    s.client.set_watchtower_bounty(&channel_id, &1_000);

    // Sender closes with a stale (low) balance.
    let s1 = s.state(1, 100);
    s.client.close_channel(&channel_id, &s1, &s.sign(1, 100));
    // Receiver disputes with a newer balance.
    let s2 = s.state(2, 300);
    s.client.dispute(&channel_id, &s2, &s.sign(2, 300));

    // Watchtower submits the newest state on the receiver's behalf.
    let s3 = s.state(3, 500);
    s.client
        .watchtower_counter_evidence(&channel_id, &s3, &s.sign(3, 500), &watchtower);

    // Finalize once the dispute window elapses.
    s.env.ledger().set_sequence_number(101);
    s.client.finalize_dispute(&channel_id);

    // reward = 10% of 500 = 50; receiver keeps 450; sender refunds 500.
    assert_eq!(s.balance(&watchtower), 50);
    assert_eq!(s.balance(&s.receiver), 450);
    assert_eq!(s.balance(&s.sender), 1_000_000 - 1_000 + 500);
    assert_eq!(
        s.client.get_channel(&channel_id).phase,
        ChannelPhase::Finalized
    );
}

/// Without a recorded watchtower the receiver keeps the whole payout, exactly
/// as before the bounty feature existed.
#[test]
fn bounty_without_watchtower_pays_receiver_in_full() {
    let s = setup();
    let channel_id = s.open(1_000, 100);
    s.client.set_watchtower_bounty(&channel_id, &1_000);

    let s1 = s.state(1, 100);
    s.client.close_channel(&channel_id, &s1, &s.sign(1, 100));
    let s2 = s.state(2, 400);
    s.client.dispute(&channel_id, &s2, &s.sign(2, 400));

    s.env.ledger().set_sequence_number(101);
    s.client.finalize_dispute(&channel_id);

    assert_eq!(s.balance(&s.receiver), 400);
    assert_eq!(s.balance(&s.sender), 1_000_000 - 1_000 + 600);
}

#[test]
fn bounty_is_capped() {
    let s = setup();
    let channel_id = s.open(1_000, 100);
    assert_eq!(s.client.get_watchtower_bounty(&channel_id), 0);
    s.client.set_watchtower_bounty(&channel_id, &2_000);
    assert_eq!(s.client.get_watchtower_bounty(&channel_id), 2_000);

    let over = s.client.try_set_watchtower_bounty(&channel_id, &2_001);
    assert_eq!(over, Err(Ok(Error::InvalidRatio)));
}
