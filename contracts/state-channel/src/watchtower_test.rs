//! Watchtower reward bounty tests (issue #459).

extern crate std;

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Event,
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

#[test]
fn bounty_set_and_reset_events_assert_exact_shape() {
    let s = setup();
    let contract = s.client.address.clone();
    let channel_id = s.open(1_000, 100);

    // Initial bounty set
    s.client.set_watchtower_bounty(&channel_id, &1_000);
    let events = s.env.events().all().filter_by_contract(&contract);
    let expected_set = crate::watchtower::WatchtowerBountySetEvent {
        channel_id,
        reward_bps: 1_000,
    };
    assert_eq!(
        events.events().last().unwrap(),
        &expected_set.to_xdr(&s.env, &contract)
    );

    // Verify storage under Bounty key matches
    let stored_initial: Option<crate::watchtower::WatchtowerBounty> =
        s.env.as_contract(&contract, || {
            s.env.storage().instance().get(&DataKey::Bounty(channel_id))
        });
    assert_eq!(
        stored_initial,
        Some(crate::watchtower::WatchtowerBounty {
            reward_bps: 1_000,
            watchtower: None,
        })
    );

    // Overwrite bounty with a new value
    s.client.set_watchtower_bounty(&channel_id, &1_500);
    let events_reset = s.env.events().all().filter_by_contract(&contract);
    let expected_reset = crate::watchtower::WatchtowerBountySetEvent {
        channel_id,
        reward_bps: 1_500,
    };
    assert_eq!(
        events_reset.events().last().unwrap(),
        &expected_reset.to_xdr(&s.env, &contract)
    );

    let stored_reset: Option<crate::watchtower::WatchtowerBounty> =
        s.env.as_contract(&contract, || {
            s.env.storage().instance().get(&DataKey::Bounty(channel_id))
        });
    assert_eq!(
        stored_reset,
        Some(crate::watchtower::WatchtowerBounty {
            reward_bps: 1_500,
            watchtower: None,
        })
    );
}

#[test]
fn watchtower_reward_event_matches_balance_and_pins_bounty_key_lifecycle() {
    let s = setup();
    let contract = s.client.address.clone();
    let channel_id = s.open(1_000, 100);
    let watchtower = Address::generate(&s.env);

    // Set 10% bounty
    s.client.set_watchtower_bounty(&channel_id, &1_000);

    // Before counter-evidence, watchtower is None in storage
    let stored_before: Option<crate::watchtower::WatchtowerBounty> =
        s.env.as_contract(&contract, || {
            s.env.storage().instance().get(&DataKey::Bounty(channel_id))
        });
    assert_eq!(
        stored_before,
        Some(crate::watchtower::WatchtowerBounty {
            reward_bps: 1_000,
            watchtower: None,
        })
    );

    // Channel transitions: close, dispute, then watchtower counter-evidence
    let s1 = s.state(1, 100);
    s.client.close_channel(&channel_id, &s1, &s.sign(1, 100));
    let s2 = s.state(2, 300);
    s.client.dispute(&channel_id, &s2, &s.sign(2, 300));

    let s3 = s.state(3, 500);
    s.client
        .watchtower_counter_evidence(&channel_id, &s3, &s.sign(3, 500), &watchtower);

    // After counter-evidence accepted, watchtower field is recorded in storage
    let stored_after_counter: Option<crate::watchtower::WatchtowerBounty> =
        s.env.as_contract(&contract, || {
            s.env.storage().instance().get(&DataKey::Bounty(channel_id))
        });
    assert_eq!(
        stored_after_counter,
        Some(crate::watchtower::WatchtowerBounty {
            reward_bps: 1_000,
            watchtower: Some(watchtower.clone()),
        })
    );

    let watchtower_bal_before = s.balance(&watchtower);
    assert_eq!(watchtower_bal_before, 0);

    // Finalize dispute and payout
    s.env.ledger().set_sequence_number(101);
    s.client.finalize_dispute(&channel_id);

    // Assert WatchtowerRewardEvent was emitted with exact payload
    let events = s.env.events().all().filter_by_contract(&contract);
    let expected_reward = crate::watchtower::WatchtowerRewardEvent {
        channel_id,
        watchtower: watchtower.clone(),
        reward: 50,
    };
    let reward_xdr = expected_reward.to_xdr(&s.env, &contract);
    assert!(
        events.events().iter().any(|e| e == &reward_xdr),
        "WatchtowerRewardEvent was not emitted with expected payload"
    );

    // Tie event reward amount to token transfer observed on token client
    let watchtower_bal_after = s.balance(&watchtower);
    assert_eq!(watchtower_bal_after, 50);
    assert_eq!(
        watchtower_bal_after - watchtower_bal_before,
        expected_reward.reward
    );

    // Post-payment state under Bounty key: watchtower is cleared to None (one-shot), reward_bps preserved
    let stored_post_payment: Option<crate::watchtower::WatchtowerBounty> =
        s.env.as_contract(&contract, || {
            s.env.storage().instance().get(&DataKey::Bounty(channel_id))
        });
    assert_eq!(
        stored_post_payment,
        Some(crate::watchtower::WatchtowerBounty {
            reward_bps: 1_000,
            watchtower: None,
        })
    );
}
