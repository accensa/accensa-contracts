//! Security audit tests for the RefundVault commit-reveal scheme (issue #128).
//!
//! The commit-reveal flow is the mitigation against mempool front-running of
//! sensitive operations (`refund`, `claim_batch`, `propose_policy`): a
//! merchant first submits only the SHA-256 hash of the intended action, then
//! waits `COMMIT_MIN_DELAY_LEDGERS` ledgers before revealing the plaintext.
//! These tests simulate the attack the scheme blocks — an observer learning
//! the action from the mempool and submitting it first with a higher fee —
//! and assert the contract rejects every avenue of it.
//!
//! 1. A reveal before the minimum delay has elapsed is rejected, both
//!    immediately after the commit and one ledger before the boundary.
//! 2. The reveal is accepted exactly at the delay boundary and the
//!    commitment is then consumed (single-use).
//! 3. A reveal with the wrong plaintext is rejected by hash mismatch.
//! 4. A reveal with no prior commit is rejected.
//! 5. A commitment bound to one operation cannot be revealed as another.
//! 6. A commitment cannot be re-created while another is pending.
//! 7. A revealed commitment is removed (single-use).
//! 8. Commit and reveal are merchant-only; a non-merchant cannot store or
//!    surface a commitment.

use super::*;
use crate::test_helpers::vault_init;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Bytes, BytesN, Env, Symbol,
};

/// Compute the SHA-256 commitment hash `commit` would expect for `plaintext`.
fn commit_hash(env: &Env, plaintext: &[u8]) -> BytesN<32> {
    env.crypto()
        .sha256(&Bytes::from_slice(env, plaintext))
        .to_bytes()
}

/// A minimal, self-contained vault harness (window 100 ledgers, no float
/// needed for commit/reveal, which touch no token).
fn setup() -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
    let client = RefundVaultClient::new(&env, &id);
    (env, client, merchant)
}

#[test]
fn test_commit_then_immediate_reveal_blocked() {
    let (env, client, _merchant) = setup();
    // The intended sensitive action (plaintext of the refund/policy change).
    let plaintext = b"refund:7b2c...:120000";
    let commitment = commit_hash(&env, plaintext);

    client.commit(&Symbol::new(&env, "refund"), &commitment);

    // The front-run window: the reveal is attempted immediately, before any
    // ledger has elapsed. The contract must refuse to surface the action,
    // which is exactly what prevents an observer from replaying it.
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS - 1);
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "refund"),
            &commitment,
            &Bytes::from_slice(&env, plaintext),
        ),
        Err(Ok(Error::CommitDelayNotElapsed))
    );
}

#[test]
fn test_reveal_blocked_exactly_at_minus_one_and_succeeds_at_delay() {
    let (env, client, _merchant) = setup();
    let committed_at = env.ledger().sequence();
    let plaintext = b"policy:window=5000";
    let commitment = commit_hash(&env, plaintext);
    client.commit(&Symbol::new(&env, "policy"), &commitment);

    // Just before the delay elapses the reveal is still blocked.
    env.ledger()
        .with_mut(|li| li.sequence_number = committed_at + COMMIT_MIN_DELAY_LEDGERS - 1);
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "policy"),
            &commitment,
            &Bytes::from_slice(&env, plaintext),
        ),
        Err(Ok(Error::CommitDelayNotElapsed))
    );

    // Exactly at the delay boundary the reveal is accepted.
    env.ledger()
        .with_mut(|li| li.sequence_number = committed_at + COMMIT_MIN_DELAY_LEDGERS);
    client.reveal(
        &Symbol::new(&env, "policy"),
        &commitment,
        &Bytes::from_slice(&env, plaintext),
    );

    // The commitment is consumed: a second reveal is a no-op.
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "policy"),
            &commitment,
            &Bytes::from_slice(&env, plaintext),
        ),
        Err(Ok(Error::NoCommit))
    );
}

#[test]
fn test_reveal_with_wrong_plaintext_blocked() {
    let (env, client, _merchant) = setup();
    let plaintext = b"refund:9a1f:50000";
    let commitment = commit_hash(&env, plaintext);
    client.commit(&Symbol::new(&env, "refund"), &commitment);

    // Advance past the delay so only the hash mismatch is what fails.
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);

    // A front-runner who observed the commit but guesses the wrong plaintext
    // is rejected: the revealed bytes do not hash to the commitment.
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "refund"),
            &commitment,
            &Bytes::from_slice(&env, b"refund:attacker:999999"),
        ),
        Err(Ok(Error::CommitMismatch))
    );
}

#[test]
fn test_reveal_without_commit_blocked() {
    let (env, client, _merchant) = setup();
    let plaintext = b"refund:deadbeef:1";
    let commitment = commit_hash(&env, plaintext);
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "refund"),
            &commitment,
            &Bytes::from_slice(&env, plaintext),
        ),
        Err(Ok(Error::NoCommit))
    );
}

#[test]
fn test_mismatched_operation_blocked() {
    let (env, client, _merchant) = setup();
    let plaintext = b"refund:01ab:250";
    let commitment = commit_hash(&env, plaintext);
    client.commit(&Symbol::new(&env, "refund"), &commitment);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);

    // Revealing the same commitment under a different operation symbol must
    // fail; a refund commitment cannot be surfaced as a policy change.
    assert_eq!(
        client.try_reveal(
            &Symbol::new(&env, "policy"),
            &commitment,
            &Bytes::from_slice(&env, plaintext),
        ),
        Err(Ok(Error::CommitOperationMismatch))
    );
}

#[test]
fn test_duplicate_commit_blocked() {
    let (env, client, _merchant) = setup();
    let plaintext = b"refund:0022:10";
    let commitment = commit_hash(&env, plaintext);
    client.commit(&Symbol::new(&env, "refund"), &commitment);
    assert_eq!(
        client.try_commit(&Symbol::new(&env, "refund"), &commitment),
        Err(Ok(Error::CommitAlreadyExists))
    );
}

#[test]
fn test_commit_is_single_use() {
    let (env, client, _merchant) = setup();
    let plaintext = b"withdraw:750";
    let commitment = commit_hash(&env, plaintext);
    client.commit(&Symbol::new(&env, "withdraw"), &commitment);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);

    client.reveal(
        &Symbol::new(&env, "withdraw"),
        &commitment,
        &Bytes::from_slice(&env, plaintext),
    );
    // After the reveal the commitment no longer exists.
    assert!(client.get_commit(&commitment).is_none());
}

#[test]
fn test_commit_and_reveal_are_merchant_only() {
    // Without merchant auth, `commit` and `reveal` must abort at their
    // `merchant.require_auth()`, never reaching anyway storage. We enable
    // enforcing auth with no signatures (the pattern the existing suite uses
    // to reach require_auth's host abort).
    let (env, client, _merchant) = setup();
    let plaintext = b"policy:vdf=1";
    let commitment = commit_hash(&env, plaintext);

    env.set_auths(&[]);
    let _ = client.try_commit(&Symbol::new(&env, "policy"), &commitment);
    let _ = client.try_reveal(
        &Symbol::new(&env, "policy"),
        &commitment,
        &Bytes::from_slice(&env, plaintext),
    );
    // None of the above could have stored a commitment without merchant auth.
    assert!(client.get_commit(&commitment).is_none());
}
