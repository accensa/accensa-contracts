//! Integration test pinning challenge-period clamping at open_channel (issue #1011).

#![cfg(test)]

use ed25519_dalek::{SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, Val, Vec,
};
use state_channel::{ChannelOpened, StateChannel, StateChannelClient};

const DEFAULT_CHALLENGE_PERIOD: u32 = 720;
const MAX_CHALLENGE_PERIOD: u32 = 17_280;

#[test]
fn test_challenge_period_clamping_at_open_channel() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

    let sk = SigningKey::from_bytes(&[4u8; SECRET_KEY_LENGTH]);
    let vk = sk.verifying_key();
    let sender_pubkey = BytesN::from_array(&env, &vk.to_bytes());

    // 1. challenge_period = 0 clamps to DEFAULT_CHALLENGE_PERIOD (720)
    let ch_zero = client.open_channel(&sender, &receiver, &sender_pubkey, &10_000, &0);
    let record_zero = client.get_channel(&ch_zero);
    assert_eq!(record_zero.challenge_period, DEFAULT_CHALLENGE_PERIOD);

    // 2. challenge_period = u32::MAX clamps to MAX_CHALLENGE_PERIOD (17280)
    let ch_max = client.open_channel(&sender, &receiver, &sender_pubkey, &10_000, &u32::MAX);
    let record_max = client.get_channel(&ch_max);
    assert_eq!(record_max.challenge_period, MAX_CHALLENGE_PERIOD);

    // 3. Verify ChannelOpened events report the correct clamped challenge periods.
    let events = env.events().all();
    // Find events matching ChannelOpened
    let mut found_zero_event = false;
    let mut found_max_event = false;

    for (_address, _topics, data) in events.iter() {
        // Try to decode ChannelOpened from event data
        if let Ok(opened) = ChannelOpened::try_from_val(&env, &data) {
            if opened.channel_id == ch_zero {
                assert_eq!(opened.challenge_period, DEFAULT_CHALLENGE_PERIOD);
                found_zero_event = true;
            } else if opened.channel_id == ch_max {
                assert_eq!(opened.challenge_period, MAX_CHALLENGE_PERIOD);
                found_max_event = true;
            }
        }
    }

    assert!(found_zero_event, "ChannelOpened event for ch_zero not found");
    assert!(found_max_event, "ChannelOpened event for ch_max not found");
}
