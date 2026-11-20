extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};
use state_channel::{
    delegation::DelegationCertificate, ChannelPhase, StateChannel, StateChannelClient, StateUpdate,
};

struct TestFixture {
    env: Env,
    client: StateChannelClient<'static>,
    master_sk: SigningKey,
    ephemeral_sk: SigningKey,
    cert: DelegationCertificate,
    cert_sig: BytesN<64>,
    channel_direct: u64,
    channel_delegated: u64,
}

fn pubkey(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

fn sign_cert(env: &Env, master_sk: &SigningKey, cert: &DelegationCertificate) -> BytesN<64> {
    let payload = cert.payload(env);
    let mut arr = [0u8; 1000];
    let len = payload.len() as usize;
    payload.copy_into_slice(&mut arr[..len]);
    let sig = master_sk.sign(&arr[..len]);
    BytesN::from_array(env, &sig.to_bytes())
}

fn sign_state_direct(
    env: &Env,
    master_sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    nonce: u64,
    balance: i128,
) -> BytesN<64> {
    let mut msg = [0u8; 56];
    msg[..32].copy_from_slice(&sender_pubkey.to_array());
    msg[32..40].copy_from_slice(&nonce.to_be_bytes());
    msg[40..56].copy_from_slice(&balance.to_be_bytes());
    let sig = master_sk.sign(&msg);
    BytesN::from_array(env, &sig.to_bytes())
}

fn sign_state_delegated(
    env: &Env,
    ephemeral_sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    nonce: u64,
    balance: i128,
) -> BytesN<64> {
    let mut msg = [0u8; 56];
    msg[..32].copy_from_slice(&sender_pubkey.to_array());
    msg[32..40].copy_from_slice(&nonce.to_be_bytes());
    msg[40..56].copy_from_slice(&balance.to_be_bytes());
    let sig = ephemeral_sk.sign(&msg);
    BytesN::from_array(env, &sig.to_bytes())
}

fn setup() -> TestFixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let master_sk = SigningKey::from_bytes(&[1u8; SECRET_KEY_LENGTH]);
    let ephemeral_sk = SigningKey::from_bytes(&[2u8; SECRET_KEY_LENGTH]);

    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000_000i128);

    let sender_pub = pubkey(&env, &master_sk);
    let ephemeral_pub = pubkey(&env, &ephemeral_sk);

    let channel_direct = client.open_channel(&sender, &receiver, &sender_pub, &10_000i128, &100u32);
    let channel_delegated =
        client.open_channel(&sender, &receiver, &sender_pub, &10_000i128, &100u32);

    let cert = DelegationCertificate {
        master_pubkey: sender_pub.clone(),
        ephemeral_pubkey: ephemeral_pub,
        expires_at_ledger: 500,
        channel_id: channel_delegated,
    };
    let cert_sig = sign_cert(&env, &master_sk, &cert);

    TestFixture {
        env,
        client,
        master_sk,
        ephemeral_sk,
        cert,
        cert_sig,
        channel_direct,
        channel_delegated,
    }
}

#[test]
fn parity_valid_state_update_succeeds_identically() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    let state = StateUpdate {
        nonce: 1,
        balance: 2_500,
    };

    let sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );
    let sig_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );

    assert_eq!(
        fx.client
            .try_update_state(&fx.channel_direct, &state, &sig_direct),
        Ok(Ok(()))
    );
    assert_eq!(
        fx.client.try_update_state_delegated(
            &fx.channel_delegated,
            &state,
            &sig_delegated,
            &fx.cert,
            &fx.cert_sig
        ),
        Ok(Ok(()))
    );

    let ch_direct = fx.client.get_channel(&fx.channel_direct);
    let ch_delegated = fx.client.get_channel(&fx.channel_delegated);
    assert_eq!(ch_direct.balance, 2_500);
    assert_eq!(ch_delegated.balance, 2_500);
    assert_eq!(ch_direct.nonce, 1);
    assert_eq!(ch_delegated.nonce, 1);
}

#[test]
fn parity_exceeds_payment_when_balance_exceeds_amount() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    let state = StateUpdate {
        nonce: 1,
        balance: 15_000, // exceeds channel amount (10_000)
    };

    let sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );
    let sig_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );

    let err_direct = fx
        .client
        .try_update_state(&fx.channel_direct, &state, &sig_direct);
    let err_delegated = fx.client.try_update_state_delegated(
        &fx.channel_delegated,
        &state,
        &sig_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    assert_eq!(err_direct, Err(Ok(Error::ExceedsPayment)));
    assert_eq!(err_delegated, Err(Ok(Error::ExceedsPayment)));
}

#[test]
fn parity_exceeds_payment_when_balance_negative() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    let state = StateUpdate {
        nonce: 1,
        balance: -1,
    };

    let sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );
    let sig_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );

    let err_direct = fx
        .client
        .try_update_state(&fx.channel_direct, &state, &sig_direct);
    let err_delegated = fx.client.try_update_state_delegated(
        &fx.channel_delegated,
        &state,
        &sig_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    assert_eq!(err_direct, Err(Ok(Error::ExceedsPayment)));
    assert_eq!(err_delegated, Err(Ok(Error::ExceedsPayment)));
}

#[test]
fn parity_stale_state_when_balance_regresses() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    // Initial update to balance 5000
    let state1 = StateUpdate {
        nonce: 1,
        balance: 5_000,
    };
    let sig1_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state1.nonce,
        state1.balance,
    );
    let sig1_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state1.nonce,
        state1.balance,
    );
    fx.client
        .update_state(&fx.channel_direct, &state1, &sig1_direct);
    fx.client.update_state_delegated(
        &fx.channel_delegated,
        &state1,
        &sig1_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    // Stale update: higher nonce but lower balance (3000 < 5000)
    let state2 = StateUpdate {
        nonce: 2,
        balance: 3_000,
    };
    let sig2_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state2.nonce,
        state2.balance,
    );
    let sig2_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state2.nonce,
        state2.balance,
    );

    let err_direct = fx
        .client
        .try_update_state(&fx.channel_direct, &state2, &sig2_direct);
    let err_delegated = fx.client.try_update_state_delegated(
        &fx.channel_delegated,
        &state2,
        &sig2_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    assert_eq!(err_direct, Err(Ok(Error::StaleState)));
    assert_eq!(err_delegated, Err(Ok(Error::StaleState)));
}

#[test]
fn parity_nonce_replay_rejected() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    let state = StateUpdate {
        nonce: 1,
        balance: 1_000,
    };
    let sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );
    let sig_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        state.nonce,
        state.balance,
    );

    fx.client
        .update_state(&fx.channel_direct, &state, &sig_direct);
    fx.client.update_state_delegated(
        &fx.channel_delegated,
        &state,
        &sig_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    // Replay same nonce
    let err_direct = fx
        .client
        .try_update_state(&fx.channel_direct, &state, &sig_direct);
    let err_delegated = fx.client.try_update_state_delegated(
        &fx.channel_delegated,
        &state,
        &sig_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    assert_eq!(err_direct, Err(Ok(Error::StaleState)));
    assert_eq!(err_delegated, Err(Ok(Error::StaleState)));
}

#[test]
fn parity_channel_not_open_rejected() {
    let fx = setup();
    let sender_pub = pubkey(&fx.env, &fx.master_sk);

    // Close both channels
    let final_state = StateUpdate {
        nonce: 1,
        balance: 1_000,
    };
    let final_sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        final_state.nonce,
        final_state.balance,
    );
    fx.client
        .close_channel(&fx.channel_direct, &final_state, &final_sig_direct);
    let final_sig_delegated = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        final_state.nonce,
        final_state.balance,
    );
    fx.client
        .close_channel(&fx.channel_delegated, &final_state, &final_sig_delegated);

    assert_eq!(
        fx.client.get_channel(&fx.channel_direct).phase,
        ChannelPhase::Closed
    );
    assert_eq!(
        fx.client.get_channel(&fx.channel_delegated).phase,
        ChannelPhase::Closed
    );

    // Attempting state updates after closure fails on both
    let new_state = StateUpdate {
        nonce: 2,
        balance: 2_000,
    };
    let new_sig_direct = sign_state_direct(
        &fx.env,
        &fx.master_sk,
        &sender_pub,
        new_state.nonce,
        new_state.balance,
    );
    let new_sig_delegated = sign_state_delegated(
        &fx.env,
        &fx.ephemeral_sk,
        &sender_pub,
        new_state.nonce,
        new_state.balance,
    );

    let err_direct = fx
        .client
        .try_update_state(&fx.channel_direct, &new_state, &new_sig_direct);
    let err_delegated = fx.client.try_update_state_delegated(
        &fx.channel_delegated,
        &new_state,
        &new_sig_delegated,
        &fx.cert,
        &fx.cert_sig,
    );

    assert_eq!(err_direct, Err(Ok(Error::ChannelNotOpen)));
    assert_eq!(err_delegated, Err(Ok(Error::ChannelNotOpen)));
}
