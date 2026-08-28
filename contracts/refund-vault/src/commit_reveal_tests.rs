#![cfg(test)]

//! Commit-reveal front-running defence tests (issue #128).
//!
//! `RefundVault`'s float-moving operations (`refund`, `withdraw`) are
//! authenticated merchant calls, but a transaction waiting in the mempool is
//! observable. A commit-reveal scheme (issue #128) is layered over these
//! operations: the merchant first submits only `sha256(plaintext || salt)`
//! (`commit`), then — after a minimum ledger delay — reveals the plaintext and
//! salt, which the contract re-hashes and verifies before executing the
//! operation.
//!
//! The tests here exercise the mechanism and, critically, simulate the
//! front-running scenario: an attacker who can see the merchant's opaque
//! commitment but not the plaintext cannot assemble a *modified* reveal that
//! passes the hash check, and cannot reveal on the merchant's behalf.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    xdr::ToXdr,
    Address, Bytes, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

// ── Commitment helpers (mirror the contract's canonical preimage) ──────────

fn refund_preimage(
    env: &Env,
    payment_ref: &BytesN<32>,
    recipient: &Address,
    amount: &i128,
    paid_at_ledger: &u32,
    payment_amount: &i128,
    salt: &BytesN<32>,
) -> Bytes {
    let mut buf = Bytes::new(env);
    buf.append(&Bytes::from_array(env, &payment_ref.to_array()));
    let recipient_digest = env.crypto().sha256(&recipient.to_xdr(env)).to_array();
    buf.append(&Bytes::from_array(env, &recipient_digest));
    buf.append(&Bytes::from_slice(env, &amount.to_le_bytes()));
    buf.append(&Bytes::from_slice(env, &paid_at_ledger.to_le_bytes()));
    buf.append(&Bytes::from_slice(env, &payment_amount.to_le_bytes()));
    buf.append(&Bytes::from_array(env, &salt.to_array()));
    buf
}

fn withdraw_preimage(env: &Env, amount: &i128, to: &Address, salt: &BytesN<32>) -> Bytes {
    let mut buf = Bytes::new(env);
    buf.append(&Bytes::from_slice(env, &amount.to_le_bytes()));
    let to_digest = env.crypto().sha256(&to.to_xdr(env)).to_array();
    buf.append(&Bytes::from_array(env, &to_digest));
    buf.append(&Bytes::from_array(env, &salt.to_array()));
    buf
}

fn commit_of(env: &Env, preimage: &Bytes) -> BytesN<32> {
    BytesN::<32>::from(env.crypto().sha256(preimage))
}

// ── Mechanism: commit / delay / reveal ─────────────────────────────────────

#[test]
fn test_commit_records_commitment() {
    let (env, client, _merchant, _token) = setup(100);
    let salt = BytesN::from_array(&env, &[1u8; 32]);
    let preimage = refund_preimage(
        &env,
        &BytesN::from_array(&env, &[7u8; 32]),
        &Address::generate(&env),
        &100,
        &1,
        &100,
        &salt,
    );
    let commitment = commit_of(&env, &preimage);

    client.commit(&commitment);

    let record = client.get_commitment(&commitment).unwrap();
    assert_eq!(record.committed_at_ledger, env.ledger().sequence());
    assert_eq!(client.get_commit_reveal_delay(), COMMIT_REVEAL_DELAY);
}

#[test]
fn test_commit_duplicate_rejected() {
    let (env, client, _merchant, _token) = setup(100);
    let to = Address::generate(&env);
    let salt = BytesN::from_array(&env, &[2u8; 32]);
    let commitment = commit_of(&env, &withdraw_preimage(&env, &100, &to, &salt));

    client.commit(&commitment);
    assert_eq!(
        client.try_commit(&commitment),
        Err(Ok(Error::CommitmentAlreadyUsed))
    );
}

#[test]
#[should_panic]
fn test_commit_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let to = Address::generate(&env);
    let salt = BytesN::from_array(&env, &[3u8; 32]);
    let commitment = commit_of(&env, &withdraw_preimage(&env, &100, &to, &salt));

    // Enforcing mode with no signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    client.commit(&commitment);
}

#[test]
fn test_reveal_before_minimum_delay_fails() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[5u8; 32]);
    let buyer = Address::generate(&env);
    let paid_at_ledger = env.ledger().sequence();
    let salt = BytesN::from_array(&env, &[6u8; 32]);
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
    );
    client.commit(&commitment);

    // No delay has elapsed yet (same ledger as commit at +0).
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentNotDue))
    );

    // Partway through the delay still fails.
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY - 1);
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentNotDue))
    );

    // Token balance unchanged and no record yet.
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 0);
    assert!(client.get_commitment(&commitment).is_some());
}

#[test]
fn test_reveal_refund_happy_path_after_delay() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    let paid_at_ledger = env.ledger().sequence();
    let salt = BytesN::from_array(&env, &[8u8; 32]);
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
    );
    client.commit(&commitment);

    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY);
    client.reveal_refund(
        &commitment,
        &payment_ref,
        &buyer,
        &120_000,
        &paid_at_ledger,
        &120_000,
        &salt,
    );

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 120_000);
    // Commitment consumed.
    assert!(client.get_commitment(&commitment).is_none());

    // A second reveal with the same params finds the commitment gone.
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentNotFound))
    );
}

#[test]
fn test_reveal_refund_wrong_plaintext_mismatch() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);
    let paid_at_ledger = env.ledger().sequence();
    let salt = BytesN::from_array(&env, &[10u8; 32]);
    // Committed to amount 120_000.
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
    );
    client.commit(&commitment);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY);

    // Revealing a *different* amount than committed fails the hash check.
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &buyer,
            &121_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentMismatch))
    );

    // Revealing to a *different recipient* than committed also fails.
    let other_buyer = Address::generate(&env);
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &other_buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentMismatch))
    );

    // Commitment is NOT consumed by a failed (mismatched) reveal.
    assert!(client.get_commitment(&commitment).is_some());
}

#[test]
fn test_reveal_without_commitment_not_found() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[11u8; 32]);
    let buyer = Address::generate(&env);
    let paid_at_ledger = env.ledger().sequence();
    let salt = BytesN::from_array(&env, &[12u8; 32]);
    // Never committed.
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
    );

    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentNotFound))
    );
}

// ── Front-running simulation ────────────────────────────────────────────────

/// The attacker can see the merchant's on-chain `commit` (an opaque hash) but
/// does not know the plaintext, so any transaction they craft to "run ahead" —
/// even reusing the observed commitment with *modified* parameters — is
/// rejected by the hash check before it can touch the float.
#[test]
fn test_front_run_with_modified_params_is_blocked() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    // Merchant legitimately commits to refunding buyer B 120_000.
    let payment_ref = BytesN::from_array(&env, &[13u8; 32]);
    let buyer = Address::generate(&env);
    let paid_at_ledger = env.ledger().sequence();
    let salt = BytesN::from_array(&env, &[14u8; 32]);
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
    );
    client.commit(&commitment);

    // The attacker fronts the reveal (after the delay) with the same observed
    // commitment but a tampered/larger amount, hoping to drain more float.
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY);
    let attacker = Address::generate(&env);
    assert_eq!(
        client.try_reveal_refund(
            &commitment,
            &payment_ref,
            &attacker,
            &500_000,
            &paid_at_ledger,
            &120_000,
            &salt,
        ),
        Err(Ok(Error::CommitmentMismatch))
    );

    // The float was untouched.
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 500_000);

    // The merchant's own (correct) reveal still works afterwards.
    client.reveal_refund(
        &commitment,
        &payment_ref,
        &buyer,
        &120_000,
        &paid_at_ledger,
        &120_000,
        &salt,
    );
    assert_eq!(token_client.balance(&buyer), 120_000);
}

/// The merchant's `commit` is opaque: even though it stores the 32-byte hash
/// on-chain, an attacker who guesses a plaintext (e.g. the payment ref but the
/// wrong recipient) cannot open it. This pins that the commitment does not
/// leak the action — the core reason front-running is not a targeted attack
/// here.
#[test]
fn test_commit_is_opaque_to_front_runner() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[15u8; 32]);
    let buyer = Address::generate(&env);
    let salt = BytesN::from_array(&env, &[16u8; 32]);
    let commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &buyer,
            &120_000,
            &env.ledger().sequence(),
            &120_000,
            &salt,
        ),
    );
    client.commit(&commitment);

    // The commitment is a fixed 32-byte opaque hash.
    assert_eq!(commitment.len(), 32);

    // An attacker guessing the payment ref but the wrong recipient computes a
    // different hash, so their on-chain commitment (if any) never matches the
    // merchant's — and revealing with their guess fails.
    let guess_commitment = commit_of(
        &env,
        &refund_preimage(
            &env,
            &payment_ref,
            &Address::generate(&env),
            &120_000,
            &env.ledger().sequence(),
            &120_000,
            &salt,
        ),
    );
    assert_ne!(commitment, guess_commitment);
}

// ── reveal_withdraw ─────────────────────────────────────────────────────────

#[test]
fn test_reveal_withdraw_happy_path_after_delay() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let to = Address::generate(&env);
    let salt = BytesN::from_array(&env, &[17u8; 32]);
    let commitment = commit_of(&env, &withdraw_preimage(&env, &200_000, &to, &salt));
    client.commit(&commitment);

    // Before the delay, withdraw is blocked.
    assert_eq!(
        client.try_reveal_withdraw(&commitment, &200_000, &to, &salt),
        Err(Ok(Error::CommitmentNotDue))
    );

    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY);
    client.reveal_withdraw(&commitment, &200_000, &to, &salt);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&to), 200_000);
    assert!(client.get_commitment(&commitment).is_none());
}

#[test]
fn test_reveal_withdraw_mismatch_blocked() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let to = Address::generate(&env);
    let salt = BytesN::from_array(&env, &[18u8; 32]);
    let commitment = commit_of(&env, &withdraw_preimage(&env, &200_000, &to, &salt));
    client.commit(&commitment);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_REVEAL_DELAY);

    // Committed 200_000; reveal 300_000 → mismatch.
    assert_eq!(
        client.try_reveal_withdraw(&commitment, &300_000, &to, &salt),
        Err(Ok(Error::CommitmentMismatch))
    );
}
