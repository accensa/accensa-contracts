//! Shard health diagnostics (issue #419).
//!
//! Indexers and light clients need to check a shard's internal consistency
//! without walking raw storage slots. This module keeps a few O(1) counters,
//! packed into one [`ShardStats`] instance-storage entry so `anchor_batch`
//! pays for a single extra read and write, updated on every anchor and every
//! deletion, and exposes them through the read-only [`ReceiptShard::get_shard_diagnostics`]
//! entry point as a [`ShardDiagnostics`] snapshot.
//!
//! # Counters
//!
//! - `live_batches` / `live_leaves`: batch records currently held in persistent
//!   storage and the sum of their leaf counts. Incremented by `anchor_batch`,
//!   decremented by both pruning paths (`prune_batches`,
//!   `prune_expired_receipts`). Re-anchoring an existing `batch_id` replaces
//!   its contribution instead of double counting.
//! - `total_leaves`: lifetime leaf count ever anchored; never decreases.
//! - `high_water`: one past the highest `batch_id` anchored so far (`0`
//!   before the first anchor, reported as `start`).
//! - `max_batch_count`: the largest leaf count of any batch ever anchored,
//!   reported as a Merkle depth (`⌈log₂(count)⌉`).
//!
//! Shards deployed before these counters existed report `0` for them until
//! new batches are anchored; `consistent` stays `true` in that case because
//! every invariant below holds trivially for zero counters.

use crate::{pruning::RETENTION_LEDGERS, MAX_PROOF_LEN};
use crate::{BatchRecord, DataKey, ReceiptShard, ReceiptShardArgs, ReceiptShardClient};
use soroban_sdk::{contractimpl, contracttype, Env};

/// Read-only operational snapshot of a shard.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardDiagnostics {
    /// First batch id of the shard's assigned `[start, end)` range.
    pub start_batch_id: u64,
    /// One past the last batch id of the shard's assigned range.
    pub end_batch_id: u64,
    /// The pruning cursor: the oldest batch id not yet pruned.
    pub oldest_unpruned_batch_id: u64,
    /// One past the highest batch id anchored so far.
    pub high_water_batch_id: u64,
    /// Batch records currently held in persistent storage.
    pub live_batches: u64,
    /// Sum of the leaf counts of the live batches.
    pub live_leaves: u64,
    /// Lifetime number of leaves anchored into this shard.
    pub total_leaves: u64,
    /// Live batches still inside the retention (dispute) window, i.e. not
    /// yet eligible for `prune_expired_receipts`.
    pub active_dispute_count: u64,
    /// Deepest Merkle tree ever anchored here, `⌈log₂(max batch count)⌉`.
    pub max_merkle_depth: u32,
    /// Persistent storage entries the shard is paying rent for.
    pub storage_entries: u64,
    /// `true` when every internal invariant holds (see
    /// [`ReceiptShard::get_shard_diagnostics`]).
    pub consistent: bool,
}

/// Running counters behind [`ShardDiagnostics`], stored under
/// `DataKey::ShardStats`.
#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShardStats {
    pub live_batches: u64,
    pub live_leaves: u64,
    pub total_leaves: u64,
    pub high_water: u64,
    pub max_batch_count: u32,
}

fn load(env: &Env) -> ShardStats {
    env.storage()
        .instance()
        .get(&DataKey::ShardStats)
        .unwrap_or_default()
}

fn store(env: &Env, stats: &ShardStats) {
    env.storage().instance().set(&DataKey::ShardStats, stats);
}

/// Depth of a sorted-pair Merkle tree over `count` leaves: `⌈log₂(count)⌉`,
/// with `0` for a single leaf (or none).
pub fn merkle_depth(count: u32) -> u32 {
    if count <= 1 {
        0
    } else {
        32 - (count - 1).leading_zeros()
    }
}

/// Update the counters for a batch written at `batch_id`. `previous` is the
/// record it overwrote, if any.
pub(crate) fn record_anchor(env: &Env, batch_id: u64, previous: Option<&BatchRecord>, count: u32) {
    let mut stats = load(env);
    match previous {
        Some(old) => stats.live_leaves = stats.live_leaves.saturating_sub(old.count as u64),
        None => stats.live_batches += 1,
    }
    stats.live_leaves += count as u64;
    stats.total_leaves += count as u64;
    stats.high_water = stats.high_water.max(batch_id + 1);
    stats.max_batch_count = stats.max_batch_count.max(count);
    store(env, &stats);
}

/// Update the counters after `count` batches totalling `leaves` leaf counts
/// were deleted from persistent storage, in a single storage write.
///
/// Both pruning paths delete many records in one call, so they accumulate the
/// totals and settle the counters once here. Rewriting the entry per deletion
/// would add a storage read and write to every removal on the hot prune path —
/// CPU that scales with the batch size and buys nothing, since no reader can
/// observe the counters part-way through the call.
pub(crate) fn record_removals(env: &Env, count: u64, leaves: u64) {
    if count == 0 {
        return;
    }
    let mut stats = load(env);
    stats.live_batches = stats.live_batches.saturating_sub(count);
    stats.live_leaves = stats.live_leaves.saturating_sub(leaves);
    store(env, &stats);
}

/// Number of batches in `[cursor, high_water)` still inside the retention
/// window.
///
/// The router anchors batch ids in increasing order, so `anchored_ledger` is
/// non-decreasing across the live range and the expired batches form a
/// prefix of it. A binary search finds the first still-retained batch in
/// `O(log n)` storage reads instead of a full scan. A missing record is
/// treated as expired (it can only sit in the pruned prefix).
fn count_retained(env: &Env, cursor: u64, high_water: u64) -> u64 {
    let now = env.ledger().sequence();
    let (mut lo, mut hi) = (cursor, high_water);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let retained = env
            .storage()
            .persistent()
            .get::<_, BatchRecord>(&DataKey::Batch(mid))
            .is_some_and(|r| now.saturating_sub(r.anchored_ledger) < RETENTION_LEDGERS);
        if retained {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    high_water - lo
}

#[contractimpl]
impl ReceiptShard {
    /// Read-only health snapshot for indexers and light clients (issue #419).
    ///
    /// `consistent` is `true` when all of these hold:
    /// - `start <= oldest_unpruned <= end` and `start <= high_water <= end`;
    /// - the live batches fit between the cursor and the high-water mark
    ///   (`live_batches <= high_water - oldest_unpruned`);
    /// - `live_leaves <= total_leaves`;
    /// - `max_merkle_depth` does not exceed the maximum verifiable proof
    ///   length, so every anchored batch can still be proven.
    pub fn get_shard_diagnostics(env: Env) -> ShardDiagnostics {
        let storage = env.storage().instance();
        let start: u64 = storage.get(&DataKey::StartBatchId).unwrap();
        let end: u64 = storage.get(&DataKey::EndBatchId).unwrap();
        let cursor: u64 = storage.get(&DataKey::PrunedUpTo).unwrap_or(start);
        let stats = load(&env);
        let high_water = stats.high_water.max(start);
        let ShardStats {
            live_batches,
            live_leaves,
            total_leaves,
            ..
        } = stats;
        let max_merkle_depth = merkle_depth(stats.max_batch_count);

        let active_dispute_count = if cursor < high_water {
            count_retained(&env, cursor, high_water).min(live_batches)
        } else {
            0
        };

        let consistent = start <= cursor
            && cursor <= end
            && start <= high_water
            && high_water <= end
            && live_batches <= high_water.saturating_sub(cursor)
            && live_leaves <= total_leaves
            && max_merkle_depth <= MAX_PROOF_LEN;

        ShardDiagnostics {
            start_batch_id: start,
            end_batch_id: end,
            oldest_unpruned_batch_id: cursor,
            high_water_batch_id: high_water,
            live_batches,
            live_leaves,
            total_leaves,
            active_dispute_count,
            max_merkle_depth,
            storage_entries: live_batches,
            consistent,
        }
    }
}
