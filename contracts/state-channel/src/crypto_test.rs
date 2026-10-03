//! Batched Ed25519 verification tests (issue #471).
//!
//! Covers the three behaviours the batch helper is responsible for: a fully
//! valid batch verifies, a length mismatch is rejected as a caller error
//! without touching the host, and a mixed batch containing one forged
//! signature aborts (the host traps) rather than silently accepting the valid
//! half.

extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{Bytes, BytesN, Env};

use crate::crypto::verify_signatures;

/// The canonical message the batch is verified against.
const MESSAGE: &[u8] = b"accensa-batch-v1";

fn pubkey(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

fn sig(env: &Env, sk: &SigningKey, message: &[u8]) -> BytesN<64> {
    BytesN::from_array(env, &sk.sign(message).to_bytes())
}

#[test]
fn a_fully_valid_batch_verifies() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);
    let c = SigningKey::from_bytes(&[3u8; 32]);

    let signers = [pubkey(&env, &a), pubkey(&env, &b), pubkey(&env, &c)];
    let signatures = [
        sig(&env, &a, MESSAGE),
        sig(&env, &b, MESSAGE),
        sig(&env, &c, MESSAGE),
    ];

    assert_eq!(
        verify_signatures(&env, &payload, &signers, &signatures),
        Ok(())
    );
}

#[test]
fn length_mismatch_is_rejected_without_touching_the_host() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    // Two signers, one signature: rejected before any verification runs. Were
    // the check missing, `zip` would silently drop the spare signer and the
    // batch would "verify" while skipping a participant.
    let signers = [pubkey(&env, &a), pubkey(&env, &b)];
    let signatures = [sig(&env, &a, MESSAGE)];

    assert_eq!(
        verify_signatures(&env, &payload, &signers, &signatures),
        Err(Error::InvalidSignature)
    );
}

#[test]
#[should_panic]
fn one_forged_signature_aborts_the_whole_batch() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    let signers = [pubkey(&env, &a), pubkey(&env, &b)];
    let signatures = [
        // Valid: signed the right message.
        sig(&env, &a, MESSAGE),
        // Forged: `b`'s key over a different message, so it must not verify.
        sig(&env, &b, b"accensa-batch-v1-forged"),
    ];

    let _ = verify_signatures(&env, &payload, &signers, &signatures);
}
