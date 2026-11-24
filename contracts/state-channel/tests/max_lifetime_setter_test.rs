//! Integration tests verifying the admin setter for state-channel MaxChannelLifetime (issue #1012).
//!
//! Covers:
//! - Unauthorized access and authentication enforcement
//! - Bound validation against `DEFAULT_CHALLENGE_PERIOD`
//! - Event emission (`MaxLifetimeUpdatedEvent`)
//! - Subsequent channel creation and expiration behavior
//! - Admin rotation and governance

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Events as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, Event,
};
use state_channel::{
    MaxLifetimeUpdatedEvent, StateChannel, StateChannelClient, StateUpdate,
};

const DEFAULT_CHALLENGE_PERIOD: u32 = 720;
const DEFAULT_MAX_CHANNEL_LIFETIME: u32 = 1_209_600;

struct Fixture {
    env: Env,
    contract_id: Address,
    client: StateChannelClient<'static>,
    admin: Address,
    sender: Address,
    receiver: Address,
    sk: SigningKey,
    sender_pubkey: BytesN<32>,
}

fn setup() -> Fixture {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let token_admin = Address::generate(&env);
    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000_000i128);

    let sk = SigningKey::from_bytes(&[17u8; SECRET_KEY_LENGTH]);
    let vk = sk.verifying_key();
    let sender_pubkey = BytesN::from_array(&env, &vk.to_bytes());

    Fixture {
        env,
        contract_id,
        client,
        admin,
        sender,
        receiver,
        sk,
        sender_pubkey,
    }
}

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
    let sig = sk.sign(&msg);
    BytesN::from_array(env, &sig.to_bytes())
}

#[test]
fn test_default_max_channel_lifetime() {
    let fx = setup();
    assert_eq!(
        fx.client.get_max_channel_lifetime(),
        DEFAULT_MAX_CHANNEL_LIFETIME
    );
}

#[test]
fn test_set_max_channel_lifetime_uninitialized_admin_fails() {
    let fx = setup();
    // Before admin is configured, calling setter fails with NotInitialized
    let res = fx.client.try_set_max_channel_lifetime(&100_000);
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_set_max_channel_lifetime_requires_admin_auth() {
    let fx = setup();
    fx.client.set_admin(&fx.admin);

    // Revoke mock authorizations: require_auth will fail for callers without explicit auth
    fx.env.set_auths(&[]);
    fx.client.set_max_channel_lifetime(&100_000);
}

#[test]
fn test_set_max_channel_lifetime_boundary_validation() {
    let fx = setup();
    fx.client.set_admin(&fx.admin);

    let default_lifetime = fx.client.get_max_channel_lifetime();

    // 0 is strictly < DEFAULT_CHALLENGE_PERIOD (720) -> InvalidAmount
    assert_eq!(
        fx.client.try_set_max_channel_lifetime(&0),
        Err(Ok(Error::InvalidAmount))
    );
    // Values below 720 -> InvalidAmount
    assert_eq!(
        fx.client.try_set_max_channel_lifetime(&1),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        fx.client.try_set_max_channel_lifetime(&719),
        Err(Ok(Error::InvalidAmount))
    );

    // Stored lifetime must remain unchanged after rejected attempts
    assert_eq!(fx.client.get_max_channel_lifetime(), default_lifetime);

    // Exact boundary value: 720 == DEFAULT_CHALLENGE_PERIOD -> accepted
    assert_eq!(
        fx.client.try_set_max_channel_lifetime(&DEFAULT_CHALLENGE_PERIOD),
        Ok(Ok(()))
    );
    assert_eq!(
        fx.client.get_max_channel_lifetime(),
        DEFAULT_CHALLENGE_PERIOD
    );

    // Valid larger lifetimes
    assert_eq!(
        fx.client.try_set_max_channel_lifetime(&2_000_000),
        Ok(Ok(()))
    );
    assert_eq!(fx.client.get_max_channel_lifetime(), 2_000_000);
}

#[test]
fn test_set_max_channel_lifetime_emits_event() {
    let fx = setup();
    fx.client.set_admin(&fx.admin);

    let prev = fx.client.get_max_channel_lifetime();
    let new_lifetime = 500_000u32;

    fx.client.set_max_channel_lifetime(&new_lifetime);

    let events = fx.env.events().all().filter_by_contract(&fx.contract_id);
    let expected = MaxLifetimeUpdatedEvent {
        admin: fx.admin.clone(),
        previous_lifetime: prev,
        new_lifetime,
    };
    assert_eq!(
        events.events().last().unwrap(),
        &expected.to_xdr(&fx.env, &fx.contract_id)
    );
}

#[test]
fn test_subsequent_channel_creation_and_expiration() {
    let fx = setup();
    fx.client.set_admin(&fx.admin);

    // Set max lifetime to 1_000 ledgers (valid, >= 720)
    let tuned_lifetime = 1_000u32;
    fx.client.set_max_channel_lifetime(&tuned_lifetime);
    assert_eq!(fx.client.get_max_channel_lifetime(), tuned_lifetime);

    let amount = 50_000i128;
    let challenge_period = 720u32;

    // Channel 1: opened at sequence 100
    let channel_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &amount,
        &challenge_period,
    );

    // Within lifetime (100 + 500 <= 100 + 1000): reclaim must fail
    fx.env.ledger().set_sequence_number(600);
    assert_eq!(
        fx.client.try_reclaim(&channel_id),
        Err(Ok(Error::ChannelNotOpen))
    );

    // Beyond tuned lifetime (100 + 1001 > 100 + 1000):
    fx.env.ledger().set_sequence_number(100 + tuned_lifetime + 1);

    // Attempting to close an expired channel must fail with ChannelExpired
    let state = StateUpdate {
        nonce: 1,
        balance: 10_000,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);
    assert_eq!(
        fx.client.try_close_channel(&channel_id, &state, &sig),
        Err(Ok(Error::ChannelExpired))
    );

    // Reclaim should now succeed on expired channel, returning escrow to sender
    assert_eq!(fx.client.try_reclaim(&channel_id), Ok(Ok(())));
}

#[test]
fn test_admin_rotation_and_governance() {
    let fx = setup();
    let admin2 = Address::generate(&fx.env);

    // Initial admin configuration
    assert_eq!(fx.client.get_admin(), None);
    fx.client.set_admin(&fx.admin);
    assert_eq!(fx.client.get_admin(), Some(fx.admin.clone()));

    // Transfer admin to admin2
    fx.client.set_admin(&admin2);
    assert_eq!(fx.client.get_admin(), Some(admin2.clone()));

    // New admin successfully sets max channel lifetime
    fx.client.set_max_channel_lifetime(&800);
    assert_eq!(fx.client.get_max_channel_lifetime(), 800);
}
