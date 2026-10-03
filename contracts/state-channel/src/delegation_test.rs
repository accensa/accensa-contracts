//! Tests for ephemeral key delegation (issue #461).

extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Bytes, BytesN, Env,
};

use crate::{
    delegation::DelegationCertificate, ChannelPhase, StateChannel, StateChannelClient, StateUpdate,
};

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

fn sign_state(
    env: &Env,
    ephemeral_sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    nonce: u64,
    balance: i128,
) -> BytesN<64> {
    let mut buf = std::vec::Vec::new();
    buf.extend_from_slice(&sender_pubkey.to_array());
    buf.extend_from_slice(&nonce.to_be_bytes());
    buf.extend_from_slice(&balance.to_be_bytes());
    let sig = ephemeral_sk.sign(&buf);
    BytesN::from_array(env, &sig.to_bytes())
}

#[test]
fn test_delegation_certificate_happy_path() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);

    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 1,
    };

    let cert_sig = sign_cert(&env, &master_sk, &cert);

    // Verify certificate directly
    assert_eq!(cert.verify(&env, &cert_sig, &cert.master_pubkey, 1), Ok(()));

    // Sign dummy state with ephemeral key
    let state_payload = Bytes::from_slice(&env, b"dummy-channel-state");
    let state_sig = {
        let sig = ephemeral_sk.sign(b"dummy-channel-state");
        BytesN::from_array(&env, &sig.to_bytes())
    };

    assert_eq!(
        cert.verify_delegated_state_signature(
            &env,
            &cert_sig,
            &cert.master_pubkey,
            1,
            &state_payload,
            &state_sig,
        ),
        Ok(())
    );
}

#[test]
fn test_delegation_expired_ledger_sequence_rejected() {
    let env = Env::default();
    // Current ledger sequence is 250, but cert expires at 200
    env.ledger().set_sequence_number(250);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);

    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 1,
    };

    let cert_sig = sign_cert(&env, &master_sk, &cert);

    assert_eq!(
        cert.verify(&env, &cert_sig, &cert.master_pubkey, 1),
        Err(Error::WindowExpired)
    );
}

#[test]
fn test_delegation_wrong_channel_rejected() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);

    // Bound to channel 1
    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 1,
    };

    let cert_sig = sign_cert(&env, &master_sk, &cert);

    // Verifying against channel 2 must be rejected
    assert_eq!(
        cert.verify(&env, &cert_sig, &cert.master_pubkey, 2),
        Err(Error::Unauthorized)
    );
}

#[test]
fn test_delegation_wildcard_channel_accepted_for_any_channel() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);

    // channel_id 0 means wildcard (valid for any channel owned by master)
    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 0,
    };

    let cert_sig = sign_cert(&env, &master_sk, &cert);

    assert_eq!(
        cert.verify(&env, &cert_sig, &cert.master_pubkey, 42),
        Ok(())
    );
    assert_eq!(
        cert.verify(&env, &cert_sig, &cert.master_pubkey, 999),
        Ok(())
    );
}

#[test]
#[should_panic]
fn test_delegation_corrupted_master_signature_panics() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let attacker_sk = SigningKey::from_bytes(&[99u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);

    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 1,
    };

    // Attacker signs instead of master
    let bad_sig = sign_cert(&env, &attacker_sk, &cert);

    let _ = cert.verify(&env, &bad_sig, &cert.master_pubkey, 1);
}

#[test]
#[should_panic]
fn test_delegated_state_corrupted_ephemeral_signature_panics() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);

    let master_sk = SigningKey::from_bytes(&[10u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[20u8; 32]);
    let attacker_sk = SigningKey::from_bytes(&[99u8; 32]);

    let cert = DelegationCertificate {
        master_pubkey: pubkey(&env, &master_sk),
        ephemeral_pubkey: pubkey(&env, &ephemeral_sk),
        expires_at_ledger: 200,
        channel_id: 1,
    };

    let cert_sig = sign_cert(&env, &master_sk, &cert);

    let state_payload = Bytes::from_slice(&env, b"dummy-channel-state");
    // Attacker signs state instead of ephemeral key
    let bad_state_sig = {
        let sig = attacker_sk.sign(b"dummy-channel-state");
        BytesN::from_array(&env, &sig.to_bytes())
    };

    let _ = cert.verify_delegated_state_signature(
        &env,
        &cert_sig,
        &cert.master_pubkey,
        1,
        &state_payload,
        &bad_state_sig,
    );
}

#[test]
fn test_state_channel_delegated_update_and_close_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();

    client.initialize(&token_addr);

    let master_sk = SigningKey::from_bytes(&[50u8; 32]);
    let ephemeral_sk = SigningKey::from_bytes(&[60u8; 32]);
    let master_pk = pubkey(&env, &master_sk);
    let ephemeral_pk = pubkey(&env, &ephemeral_sk);

    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token_addr);
    token_client.mint(&sender, &10_000);

    let channel_id = client.open_channel(&sender, &receiver, &master_pk, &5_000, &100);

    // Create delegation certificate valid until ledger 300
    let cert = DelegationCertificate {
        master_pubkey: master_pk.clone(),
        ephemeral_pubkey: ephemeral_pk,
        expires_at_ledger: 300,
        channel_id,
    };
    let cert_sig = sign_cert(&env, &master_sk, &cert);

    // Update state using ephemeral key signature
    let state_1 = StateUpdate {
        nonce: 1,
        balance: 1_000,
    };
    let state_1_sig = sign_state(&env, &ephemeral_sk, &master_pk, 1, 1_000);

    client.update_state_delegated(&channel_id, &state_1, &state_1_sig, &cert, &cert_sig);

    let ch = client.get_channel(&channel_id);
    assert_eq!(ch.balance, 1_000);
    assert_eq!(ch.nonce, 1);

    // Cooperative close using ephemeral key signature
    let state_2 = StateUpdate {
        nonce: 2,
        balance: 2_500,
    };
    let state_2_sig = sign_state(&env, &ephemeral_sk, &master_pk, 2, 2_500);

    client.close_channel_delegated(&channel_id, &state_2, &state_2_sig, &cert, &cert_sig);

    let ch_closed = client.get_channel(&channel_id);
    assert_eq!(ch_closed.phase, ChannelPhase::Closed);
    assert_eq!(ch_closed.balance, 2_500);

    // Wait until challenge period ends and claim
    env.ledger().set_sequence_number(ch_closed.closed_at + 101);
    client.claim(&channel_id);

    let ch_finalized = client.get_channel(&channel_id);
    assert_eq!(ch_finalized.phase, ChannelPhase::Finalized);

    let receiver_token = soroban_sdk::token::Client::new(&env, &token_addr);
    assert_eq!(receiver_token.balance(&receiver), 2_500);
    assert_eq!(receiver_token.balance(&sender), 5_000);
}
