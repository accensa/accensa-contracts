//! Unit tests for shard health diagnostics (issue #419).

#![cfg(test)]

use crate::diagnostics::{merkle_depth, ShardDiagnostics};
use crate::pruning::RETENTION_LEDGERS;
use crate::{ReceiptShard, ReceiptShardClient};
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
