//! Integration test verifying state channel challenge period constants and getters (issue #1011).

#![cfg(test)]

use ed25519_dalek::{SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, BytesN, Env};
use state_channel::{StateChannel, StateChannelClient};

const DEFAULT_CHALLENGE_PERIOD: u32 = 720;
const MAX_CHALLENGE_PERIOD: u32 = 17_280;
const DEFAULT_MAX_CHANNEL_LIFETIME: u32 = 1_209_600;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: StateChannelClient<'static>,
    sender: Address,
    receiver: Address,
    sender_pubkey: BytesN<32>,
}

fn setup() -> Fixture {
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

    let sk = SigningKey::from_bytes(&[3u8; SECRET_KEY_LENGTH]);
    let vk = sk.verifying_key();
    let sender_pubkey = BytesN::from_array(&env, &vk.to_bytes());

    Fixture {
        env,
        client,
        sender,
        receiver,
        sender_pubkey,
    }
}

#[test]
fn test_challenge_period_getters_and_effective_values() {
    let fx = setup();

    // Verify public getters match documented constants
    assert_eq!(
        fx.client.get_default_challenge_period(),
        DEFAULT_CHALLENGE_PERIOD
    );
    assert_eq!(fx.client.get_max_challenge_period(), MAX_CHALLENGE_PERIOD);
    assert_eq!(
        fx.client.get_max_channel_lifetime(),
        DEFAULT_MAX_CHANNEL_LIFETIME
    );

    // Channel opened with challenge_period = 0 resolves to DEFAULT_CHALLENGE_PERIOD (720)
    let ch1 = fx
        .client
        .open_channel(&fx.sender, &fx.receiver, &fx.sender_pubkey, &10_000, &0);
    let record1 = fx.client.get_channel(&ch1);
    assert_eq!(record1.challenge_period, DEFAULT_CHALLENGE_PERIOD);

    // Channel opened with excessive challenge_period (e.g. 50,000) is capped at MAX_CHALLENGE_PERIOD (17,280)
    let ch2 = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &10_000,
        &50_000,
    );
    let record2 = fx.client.get_channel(&ch2);
    assert_eq!(record2.challenge_period, MAX_CHALLENGE_PERIOD);
}
