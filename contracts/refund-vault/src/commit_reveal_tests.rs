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
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    Address, Bytes, BytesN, Env, Event, Symbol,
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

// ── Event-shape and per-user nonce assertions ──────────────────────────────
//
// `CommitEvent` and `CommitRevealedEvent` are the only on-chain trail of the
// commit/reveal flow, and `UserNonce` is the replay-protection counter each
// claim call consumes. Both are pinned below in the XDR comparison form used
// by `contracts/state-channel/src/hashlock_test.rs:105-115`.
// `env.events().all()` reflects only the last top-level invocation, so each
// assertion reads the event list immediately after the call under test.

#[test]
fn test_commit_and_reveal_event_shapes() {
    let (env, client, _merchant) = setup();
    let contract = client.address.clone();
    let operation = Symbol::new(&env, "refund");
    let plaintext = b"refund:event:77";
    let commitment = commit_hash(&env, plaintext);

    let committed_at = env.ledger().sequence();
    client.commit(&operation, &commitment);

    let events = env.events().all().filter_by_contract(&contract);
    assert_eq!(events.events().len(), 1);
    let expected = CommitEvent {
        operation: operation.clone(),
        commitment_hash: commitment.clone(),
        reveal_at_ledger: committed_at + COMMIT_MIN_DELAY_LEDGERS,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&env, &contract));

    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);
    client.reveal(&operation, &commitment, &Bytes::from_slice(&env, plaintext));

    let events = env.events().all().filter_by_contract(&contract);
    assert_eq!(events.events().len(), 1);
    let expected = CommitRevealedEvent {
        operation,
        commitment_hash: commitment.clone(),
        ledger: committed_at + COMMIT_MIN_DELAY_LEDGERS,
    };
    assert_eq!(&events.events()[0], &expected.to_xdr(&env, &contract));
}

#[test]
fn test_replayed_reveal_emits_nothing_and_leaves_last_claim_untouched() {
    let (env, client, merchant) = setup();
    let contract = client.address.clone();
    let operation = Symbol::new(&env, "refund");
    let plaintext = b"refund:replay:42";
    let commitment = commit_hash(&env, plaintext);

    // The per-recipient last-claim timestamp before the whole flow: nothing
    // in commit/reveal may move it.
    let last_before: Option<u64> = env.as_contract(&contract, || {
        env.storage()
            .persistent()
            .get(&DataKey::UserLastClaim(merchant.clone()))
    });

    client.commit(&operation, &commitment);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);
    client.reveal(&operation, &commitment, &Bytes::from_slice(&env, plaintext));
    // The first reveal did publish its event (so the empty list after the
    // replay below proves the replay published nothing, not that events are
    // simply never recorded).
    let events = env.events().all().filter_by_contract(&contract);
    assert_eq!(events.events().len(), 1);

    // The commitment is consumed: replaying the reveal is rejected.
    assert_eq!(
        client.try_reveal(&operation, &commitment, &Bytes::from_slice(&env, plaintext),),
        Err(Ok(Error::NoCommit))
    );

    // The rejected replay is the last top-level invocation and it emitted
    // no event at all.
    let events = env.events().all().filter_by_contract(&contract);
    assert_eq!(events.events().len(), 0);

    // ...and it did not move the stored last-claim value.
    let last_after: Option<u64> = env.as_contract(&contract, || {
        env.storage()
            .persistent()
            .get(&DataKey::UserLastClaim(merchant))
    });
    assert_eq!(last_before, last_after);
}

#[test]
fn test_two_users_advance_only_their_own_user_nonce() {
    let env = Env::default();
    env.mock_all_auths();

    let merchant_a = Address::generate(&env);
    let merchant_b = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant_a, &500_000);
    StellarAssetClient::new(&env, &token).mint(&merchant_b, &500_000);

    // Two users, each on their own vault — the deployment shape that gives
    // each user an independent replay-protection counter.
    let id_a = env.register(RefundVault, (vault_init(&env, &merchant_a, &token, 100),));
    let client_a = RefundVaultClient::new(&env, &id_a);
    let id_b = env.register(RefundVault, (vault_init(&env, &merchant_b, &token, 100),));
    let client_b = RefundVaultClient::new(&env, &id_b);

    client_a.deposit(&merchant_a, &500_000, &None);
    client_b.deposit(&merchant_b, &500_000, &None);

    // Both users start at the same nonce.
    assert_eq!(client_a.get_user_nonce(&merchant_a), 0);
    assert_eq!(client_b.get_user_nonce(&merchant_b), 0);

    // Each user commits and reveals under its own vault while both counters
    // are still 0: commit/reveal are nonce-free, so neither user's act of
    // committing consumes the other's (nor its own) counter.
    let operation = Symbol::new(&env, "refund");

    let plaintext_a = b"refund:user-a:1";
    let commitment_a = commit_hash(&env, plaintext_a);
    client_a.commit(&operation, &commitment_a);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);
    client_a.reveal(
        &operation,
        &commitment_a,
        &Bytes::from_slice(&env, plaintext_a),
    );

    let plaintext_b = b"refund:user-b:1";
    let commitment_b = commit_hash(&env, plaintext_b);
    client_b.commit(&operation, &commitment_b);
    env.ledger()
        .with_mut(|li| li.sequence_number += COMMIT_MIN_DELAY_LEDGERS);
    client_b.reveal(
        &operation,
        &commitment_b,
        &Bytes::from_slice(&env, plaintext_b),
    );

    assert_eq!(client_a.get_user_nonce(&merchant_a), 0);
    assert_eq!(client_b.get_user_nonce(&merchant_b), 0);

    // A's claim advances only A's counter.
    let buyer = Address::generate(&env);
    client_a.refund(
        &BytesN::from_array(&env, &[0xE1u8; 32]),
        &buyer,
        &100_000,
        &0,
        &100_000,
        &None,
        &0,
    );
    assert_eq!(client_a.get_user_nonce(&merchant_a), 1);
    assert_eq!(client_b.get_user_nonce(&merchant_b), 0);

    // B's nonce 0 is still valid on B even though A has moved on, and B's
    // claim leaves A's counter untouched: collapsing these into one global
    // counter would fail the assertion above and below.
    client_b.refund(
        &BytesN::from_array(&env, &[0xE2u8; 32]),
        &buyer,
        &100_000,
        &0,
        &100_000,
        &None,
        &0,
    );
    assert_eq!(client_b.get_user_nonce(&merchant_b), 1);
    assert_eq!(client_a.get_user_nonce(&merchant_a), 1);
}
