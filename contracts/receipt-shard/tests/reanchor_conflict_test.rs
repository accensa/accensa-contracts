//! #995 — a second anchor at an occupied batch id must never overwrite the
//! root the first one committed.
//!
//! Receipts are proven against the stored Merkle root, so silently replacing
//! a root would invalidate every leaf already anchored under it while making
//! the replacement look canonical. These tests pin the two sides of the rule:
//!
//! - re-submitting the exact same anchoring request is an idempotent no-op —
//!   the record (including the ledger it was first anchored at) and its
//!   diagnostics counters are untouched, so a replay can never bump
//!   `live_batches` / `total_leaves`;
//! - re-submitting a *different* root at the same id answers
//!   [`Error::DuplicateRoot`], keeps the first record, and leaves the earlier
//!   leaves verifying;
//! - the legacy `anchor_batch` entry point, which delegates to the checked
//!   variant, traps rather than overwriting.

#![cfg(test)]

use accensa_common::Error;
use receipt_shard::{ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, BytesN, Env,
};

const BATCH_ID: u64 = 1;

fn setup(env: &Env) -> ReceiptShardClient<'static> {
    env.mock_all_auths();
    let id = env.register(
        ReceiptShard,
        (Address::generate(env), 1u64, 4u64, Address::generate(env)),
    );
    ReceiptShardClient::new(env, &id)
}

fn root(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

#[test]
fn identical_reanchor_is_an_idempotent_noop() {
    let env = Env::default();
    let client = setup(&env);
    let root_a = root(&env, 0xaa);

    client.anchor_batch(&BATCH_ID, &root_a, &5, &100, &200);
    let record_before = client.get_batch(&BATCH_ID);
    let stats_before = client.get_shard_diagnostics();

    // The replay lands in a later ledger: the request is still identical, so
    // the call succeeds without rewriting the record or its counters.
    env.ledger().with_mut(|li| li.sequence_number += 1);
    client.anchor_batch_checked(&BATCH_ID, &root_a, &5, &100, &200);

    let record_after = client.get_batch(&BATCH_ID);
    assert_eq!(
        record_after, record_before,
        "a replay must leave the stored record untouched"
    );
    assert_eq!(
        record_after.anchored_ledger, record_before.anchored_ledger,
        "a replay must not re-date the batch to the replay ledger"
    );

    let stats_after = client.get_shard_diagnostics();
    assert_eq!(
        stats_after.live_batches, stats_before.live_batches,
        "a replay must not double-count live batches"
    );
    assert_eq!(
        stats_after.live_leaves, stats_before.live_leaves,
        "a replay must not double-count live leaves"
    );
    assert_eq!(
        stats_after.total_leaves, stats_before.total_leaves,
        "a replay must not accrue another anchor in total_leaves"
    );
}

#[test]
fn conflicting_reanchor_returns_duplicate_root_and_keeps_first_record() {
    let env = Env::default();
    let client = setup(&env);
    let root_a = root(&env, 0xaa);
    let root_b = root(&env, 0xbb);

    // A one-leaf batch: its root *is* the leaf, and its proof is empty.
    client.anchor_batch(&BATCH_ID, &root_a, &1, &100, &200);

    assert_eq!(
        client.try_anchor_batch_checked(&BATCH_ID, &root_b, &1, &100, &200),
        Err(Ok(Error::DuplicateRoot))
    );

    let stored = client.get_batch(&BATCH_ID);
    assert_eq!(
        stored.root, root_a,
        "get_batch must keep reporting the first root"
    );
    assert_eq!(stored.count, 1);

    // The original leaf still verifies; the rejected replacement does not.
    assert!(client.verify_receipt(&BATCH_ID, &root_a, &vec![&env]));
    assert!(!client.verify_receipt(&BATCH_ID, &root_b, &vec![&env]));
}

#[test]
fn legacy_anchor_batch_reports_the_typed_conflict() {
    let env = Env::default();
    let client = setup(&env);
    let root_a = root(&env, 0xaa);

    client.anchor_batch(&BATCH_ID, &root_a, &1, &100, &200);

    assert_eq!(
        client.try_anchor_batch(&BATCH_ID, &root(&env, 0xbb), &1, &100, &200),
        Err(Ok(Error::DuplicateRoot))
    );
    assert_eq!(client.get_batch(&BATCH_ID).root, root_a);
}

#[test]
#[should_panic]
fn legacy_anchor_batch_traps_instead_of_overwriting() {
    let env = Env::default();
    let client = setup(&env);
    let root_a = root(&env, 0xaa);

    client.anchor_batch(&BATCH_ID, &root_a, &1, &100, &200);

    // The non-try client unwraps the returned error and aborts the host call.
    client.anchor_batch(&BATCH_ID, &root(&env, 0xbb), &1, &100, &200);
}
