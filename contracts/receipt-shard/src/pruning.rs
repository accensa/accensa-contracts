//! Storage eviction policy for expired receipt-shard entries (issue #395).
//!
//! While `ReceiptShard::prune_batches` prunes by *cursor position* (a
//! contiguous prefix keyed by batch id, router-gated), this module adds a
//! policy-driven eviction path: batches whose anchor ledger is older than
//! the retention window may be deleted to reclaim persistent-storage rent,
//! and whoever triggers the cleanup accrues a per-batch bounty they can
//! claim from the contract's own token balance.
//!
//! # Policy
//!
//! - [`RETENTION_LEDGERS`] (~180 days at ~5 s/ledger) is the protocol
//!   retention period. A batch is prunable only when
//!   `current_ledger - batch.anchored_ledger >= RETENTION_LEDGERS`;
//!   anything younger is active under the dispute/retention window and is
//!   never touched, so `prune_expired_receipts` can never delete an
//!   unexpired receipt.
//! - `max_count` bounds the deletions per call (and therefore the bounty
//!   payout), keeping the loop's cost predictable.
//! - Every call emits [`ReceiptsPrunedEvent`] with the reclaimed storage
//!   count, even when it deletes nothing (count `0`).
//!
//! # Bounty economics
//!
//! Each pruned batch accrues [`PRUNE_BOUNTY_PER_BATCH`] stroops to the
//! cleanup caller in instance storage (`DataKey::PruneBounty`), keyed by
//! the caller's address. The caller identifies itself by passing its own
//! address and authorizing the call with `require_auth` — the same
//! authorize-by-argument pattern every other contract in this workspace
//! uses — so bounty attribution is sound and a third party cannot divert
//! someone else's accrual to itself. The accrued balance is settled
//! through `claim_prune_bounty`, which zeroes the accrual *before* the
//! transfer so a claim cannot pay out twice.

use crate::{
    BatchRecord, DataKey, ReceiptShard, ReceiptShardArgs, ReceiptShardClient, TTL_EXTEND,
    TTL_THRESHOLD,
};
use soroban_sdk::{contractevent, contractimpl, token::Client as TokenClient, Address, Env};

/// Protocol retention period: a receipt is prunable only once it is at
/// least this many ledgers old. ~180 days at ~5 s/ledger
/// (180 * 17,280 = 3,110,400).
pub const RETENTION_LEDGERS: u32 = 3_110_400;

/// Bounty accrued to the cleanup caller per pruned batch, in stroops,
/// claimable from the contract's own token balance.
pub const PRUNE_BOUNTY_PER_BATCH: i128 = 100;

/// Emitted when expired receipts are evicted from this shard (issue #395).
///
/// Topics: `("receipts_pruned", pruner)`. The data map carries the number
/// of reclaimed storage entries so indexers can track storage pressure.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptsPrunedEvent {
    #[topic]
    pub pruner: Address,
    pub pruned_count: u32,
    pub new_cursor: u64,
}

#[contractimpl]
impl ReceiptShard {
    /// Policy-driven eviction entry point (issue #395).
    ///
    /// Deletes up to `max_count` batch records whose anchor ledger is at
    /// least [`RETENTION_LEDGERS`] old, advancing the shard's `PrunedUpTo`
    /// cursor over exactly the batches it deleted. Anyone may call this —
    /// the retention check is the authorization for *what* gets deleted —
    /// and `pruner` accrues [`PRUNE_BOUNTY_PER_BATCH`] stroops per pruned
    /// batch, claimable via `claim_prune_bounty`.
    ///
    /// `pruner` must authorize the call (`require_auth`); this attributes
    /// the bounty to the address that actually submitted the transaction.
    ///
    /// Returns the number of records deleted (the reclaimed storage
    /// slots). A call that finds nothing expired is a successful no-op
    /// returning `0`.
    pub fn prune_expired_receipts(env: Env, pruner: Address, max_count: u32) -> u32 {
        pruner.require_auth();

        let current_ledger = env.ledger().sequence();
        let end: u64 = env.storage().instance().get(&DataKey::EndBatchId).unwrap();
        let mut cursor: u64 = env.storage().instance().get(&DataKey::PrunedUpTo).unwrap();

        let mut pruned: u32 = 0;

        // Scan forward from the cursor over the shard's assigned range.
        // The cursor is a contiguous prefix by construction (both pruning
        // paths advance it only over batches they consumed), so already
        // pruned batches are skipped in O(1).
        while cursor < end && pruned < max_count {
            match env
                .storage()
                .persistent()
                .get::<_, BatchRecord>(&DataKey::Batch(cursor))
            {
                // Expired: older than the retention window, delete it.
                Some(record)
                    if current_ledger.saturating_sub(record.anchored_ledger)
                        >= RETENTION_LEDGERS =>
                {
                    env.storage().persistent().remove(&DataKey::Batch(cursor));
                    crate::diagnostics::record_removal(&env, &record);
                    pruned += 1;
                    cursor += 1;
                }
                // Present but still within retention: the policy keeps
                // this receipt. The prefix is age-ordered, so nothing
                // further along can be older — stop scanning.
                Some(_) => break,
                // Already deleted (e.g. by the router's cursor pruning) or
                // never anchored: nothing further along can exist — the
                // cursor is a contiguous prefix by construction, so stop
                // scanning instead of consuming footprint entries for the
                // rest of the shard's range.
                None => break,
            }
        }

        if pruned > 0 {
            env.storage().instance().set(&DataKey::PrunedUpTo, &cursor);
            env.storage()
                .instance()
                .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

            let accrued: i128 = env
                .storage()
                .instance()
                .get(&DataKey::PruneBounty(pruner.clone()))
                .unwrap_or(0);
            env.storage().instance().set(
                &DataKey::PruneBounty(pruner.clone()),
                &(accrued + PRUNE_BOUNTY_PER_BATCH * pruned as i128),
            );
        }

        ReceiptsPrunedEvent {
            pruner,
            pruned_count: pruned,
            new_cursor: cursor,
        }
        .publish(&env);

        pruned
    }

    /// Settle a caller's accrued pruning bounty.
    ///
    /// Transfers `claimer`'s full accrued balance from the contract's own
    /// balance of `token` (capped at what the contract actually holds) and
    /// zeroes the accrual *before* the transfer, so a re-entered or
    /// repeated claim cannot pay out twice. Returns the amount paid.
    pub fn claim_prune_bounty(env: Env, token: Address, claimer: Address) -> i128 {
        claimer.require_auth();

        let accrued: i128 = env
            .storage()
            .instance()
            .get(&DataKey::PruneBounty(claimer.clone()))
            .unwrap_or(0);

        env.storage()
            .instance()
            .set(&DataKey::PruneBounty(claimer.clone()), &0i128);

        if accrued > 0 {
            let contract_addr = env.current_contract_address();
            let balance = TokenClient::new(&env, &token).balance(&contract_addr);
            let payout = accrued.min(balance);
            if payout > 0 {
                TokenClient::new(&env, &token).transfer(&contract_addr, &claimer, &payout);
            }
            return payout;
        }
        0
    }

    /// The bounty `caller` has accrued but not yet claimed, in stroops.
    pub fn get_prune_bounty(env: Env, caller: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::PruneBounty(caller))
            .unwrap_or(0)
    }

    /// The retention period currently enforced (in ledgers).
    pub fn get_retention_ledgers(_env: Env) -> u32 {
        RETENTION_LEDGERS
    }

    /// The per-batch cleanup bounty (in stroops).
    pub fn get_prune_bounty_per_batch(_env: Env) -> i128 {
        PRUNE_BOUNTY_PER_BATCH
    }
}
