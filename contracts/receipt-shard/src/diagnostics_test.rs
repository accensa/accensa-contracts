//! Unit tests for shard health diagnostics (issue #419).

#![cfg(test)]

use crate::diagnostics::{merkle_depth, ShardDiagnostics};
use crate::pruning::RETENTION_LEDGERS;
use crate::{DataKey, ReceiptShard, ReceiptShardClient};
use accensa_common::Error;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

const BASE_LEDGER: u32 = 100_000;

fn setup() -> (Env, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);
    (env, client)
}

fn anchor(env: &Env, client: &ReceiptShardClient, batch_id: u64, count: u32) {
    let root = BytesN::from_array(env, &[batch_id as u8; 32]);
    client.anchor_batch(&batch_id, &root, &count, &0, &100);
}

fn advance(env: &Env, n: u32) {
    env.ledger().with_mut(|li| li.sequence_number += n);
}

#[test]
fn fresh_shard_reports_empty_consistent_state() {
    let (_env, client) = setup();
    assert_eq!(
        client.get_shard_diagnostics(),
        ShardDiagnostics {
            start_batch_id: 1,
            end_batch_id: 1001,
            oldest_unpruned_batch_id: 1,
            high_water_batch_id: 1,
            live_batches: 0,
            live_leaves: 0,
            total_leaves: 0,
            active_dispute_count: 0,
            max_merkle_depth: 0,
            storage_entries: 0,
            consistent: true,
        }
    );
}

#[test]
fn anchoring_updates_counts_depth_and_high_water() {
    let (env, client) = setup();
    anchor(&env, &client, 1, 4);
    anchor(&env, &client, 2, 1000);
    anchor(&env, &client, 3, 7);

    let d = client.get_shard_diagnostics();
    assert_eq!(d.live_batches, 3);
    assert_eq!(d.storage_entries, 3);
    assert_eq!(d.live_leaves, 1011);
    assert_eq!(d.total_leaves, 1011);
    assert_eq!(d.high_water_batch_id, 4);
    assert_eq!(d.max_merkle_depth, 10);
    assert_eq!(d.active_dispute_count, 3);
    assert!(d.consistent);
}

#[test]
fn reanchoring_same_batch_does_not_double_count_live_state() {
    let (env, client) = setup();
    anchor(&env, &client, 1, 10);
    anchor(&env, &client, 1, 3);

    let d = client.get_shard_diagnostics();
    assert_eq!(d.live_batches, 1);
    assert_eq!(d.live_leaves, 3);
    assert_eq!(d.total_leaves, 13, "lifetime total counts every anchor");
    assert!(d.consistent);
}

#[test]
fn router_pruning_moves_cursor_and_releases_storage() {
    let (env, client) = setup();
    for id in 1..=5 {
        anchor(&env, &client, id, 2);
    }
    advance(&env, 10);
    client.prune_batches(&(BASE_LEDGER + 1), &3, &6);

    let d = client.get_shard_diagnostics();
    assert_eq!(d.oldest_unpruned_batch_id, 4);
    assert_eq!(d.live_batches, 2);
    assert_eq!(d.live_leaves, 4);
    assert_eq!(d.total_leaves, 10);
    assert_eq!(d.active_dispute_count, 2);
    assert!(d.consistent);
}

#[test]
fn expired_batches_leave_the_active_dispute_count() {
    let (env, client) = setup();
    anchor(&env, &client, 1, 1);
    anchor(&env, &client, 2, 1);
    advance(&env, 1_000);
    anchor(&env, &client, 3, 1);
    anchor(&env, &client, 4, 1);

    // Batches 1-2 cross the retention boundary, 3-4 are still retained.
    advance(&env, RETENTION_LEDGERS - 500);
    let d = client.get_shard_diagnostics();
    assert_eq!(d.live_batches, 4);
    assert_eq!(d.active_dispute_count, 2);

    // Evicting the expired prefix leaves only the retained batches live.
    let pruner = Address::generate(&env);
    assert_eq!(client.prune_expired_receipts(&pruner, &10), 2);
    let d = client.get_shard_diagnostics();
    assert_eq!(d.oldest_unpruned_batch_id, 3);
    assert_eq!(d.live_batches, 2);
    assert_eq!(d.active_dispute_count, 2);
    assert!(d.consistent);

    // Once everything ages out, nothing is under dispute.
    advance(&env, 1_000);
    assert_eq!(client.get_shard_diagnostics().active_dispute_count, 0);
}

#[test]
fn oversized_batch_is_flagged_inconsistent() {
    let (env, client) = setup();
    // 2^10 + 1 leaves needs an 11-level proof, beyond MAX_PROOF_LEN.
    anchor(&env, &client, 1, 1025);
    let d = client.get_shard_diagnostics();
    assert_eq!(d.max_merkle_depth, 11);
    assert!(!d.consistent);
}

#[test]
fn merkle_depth_is_ceil_log2() {
    assert_eq!(merkle_depth(0), 0);
    assert_eq!(merkle_depth(1), 0);
    assert_eq!(merkle_depth(2), 1);
    assert_eq!(merkle_depth(3), 2);
    assert_eq!(merkle_depth(4), 2);
    assert_eq!(merkle_depth(5), 3);
    assert_eq!(merkle_depth(1000), 10);
    assert_eq!(merkle_depth(1024), 10);
    assert_eq!(merkle_depth(1025), 11);
}

// Missing range keys must degrade to the typed failure instead of trapping
// this read-only monitoring call (issue #615).

#[test]
fn ttl_evicted_range_keys_report_not_initialized_instead_of_trapping() {
    let (env, client) = setup();

    // In the testbed a stored instance entry is auto-restored on access rather
    // than dropped when its TTL lapses, so the post-TTL state of an idle
    // shard — the range keys gone from instance storage — is reproduced
    // directly: remove exactly the entries that the eviction loses.
    let contract_id = client.address.clone();
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::StartBatchId);
        env.storage().instance().remove(&DataKey::EndBatchId);
        assert!(!env.storage().instance().has(&DataKey::StartBatchId));
        assert!(!env.storage().instance().has(&DataKey::EndBatchId));
    });

    // A healthy shard still answers; only the degraded one must give an `Err`,
    // and it must be the typed variant, not a host trap.
    assert_eq!(
        client.try_get_shard_diagnostics(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn healthy_shard_reports_consistent_range_across_population_and_drain() {
    let (env, client) = setup();

    // Populated state: anchor a few batches and pin the documented range
    // invariants on the snapshot.
    for id in 1..=4 {
        anchor(&env, &client, id, 2);
    }
    let d = client.get_shard_diagnostics();
    assert_eq!(d.start_batch_id, 1);
    assert_eq!(d.end_batch_id, 1001);
    assert_eq!(d.oldest_unpruned_batch_id, 1);
    assert_eq!(d.high_water_batch_id, 5);
    assert_eq!(d.live_batches, 4);
    assert_eq!(d.live_leaves, 8);
    assert!(d.start_batch_id <= d.oldest_unpruned_batch_id);
    assert!(d.oldest_unpruned_batch_id <= d.end_batch_id);
    assert!(d.live_batches <= d.total_leaves);
    assert!(d.consistent);

    // Drained state: every anchored batch pruned away; the snapshot must stay
    // healthy with an empty live set inside the same `[start, end)` range.
    advance(&env, 10);
    client.prune_batches(&(BASE_LEDGER + 1), &10, &5);
    let d = client.get_shard_diagnostics();
    assert_eq!(d.start_batch_id, 1);
    assert_eq!(d.end_batch_id, 1001);
    assert_eq!(d.oldest_unpruned_batch_id, 5);
    assert_eq!(d.high_water_batch_id, 5);
    assert_eq!(d.live_batches, 0);
    assert_eq!(d.live_leaves, 0);
    assert_eq!(d.storage_entries, 0);
    assert!(d.start_batch_id <= d.oldest_unpruned_batch_id);
    assert!(d.oldest_unpruned_batch_id <= d.end_batch_id);
    assert!(d.live_batches <= d.total_leaves);
    assert!(d.consistent);
}
