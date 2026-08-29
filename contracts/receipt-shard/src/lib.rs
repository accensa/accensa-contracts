#![no_std]

//! `ReceiptShard` holds a bounded, contiguous range of `[start_batch_id,
//! end_batch_id)` Merkle batch anchors for a `ReceiptAnchor` router.
//!
//! A shard trusts exactly one caller — the `router` address recorded at
//! construction time — for every state-changing entry point. The router is
//! the only party that can compute a valid `batch_id` for this shard, so a
//! shard never re-derives or second-guesses routing decisions; it only
//! enforces that writes land inside its own assigned range.

use accensa_common::Error;
use soroban_sdk::{contract, contractimpl, contractmeta, contracttype, Address, BytesN, Env, Vec};

contractmeta!(key = "name", val = "ReceiptShard");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

#[contracttype]
pub enum DataKey {
    Router,
    StartBatchId,
    EndBatchId,
    Batch(u64),
    PrunedUpTo,
}

/// Structurally identical to `ReceiptAnchor::BatchRecord`. Soroban cross-contract
/// calls decode by shape (field names, types, and order), not by Rust type
/// identity, so the two independently defined structs stay wire-compatible as
/// long as this shape doesn't drift from the router's copy.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_ledger: u32,
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger. Mirrors
/// `ReceiptAnchor`'s TTL policy so shard-held batches archive on the same
/// schedule as the router's own instance storage.
const TTL_EXTEND: u32 = 518_400;
const TTL_THRESHOLD: u32 = 100;

/// Maximum valid Merkle proof length. Matches the router's `MAX_PROOF_LEN`.
/// A batch of N leaves produces a tree of depth ⌈log₂(N)⌉. For
/// MAX_BATCH_SIZE = 1000 (router constant), that is 10.
const MAX_PROOF_LEN: u32 = 10;

use sha2::{Digest, Sha256};

#[contract]
pub struct ReceiptShard;

#[contractimpl]
impl ReceiptShard {
    /// Runs atomically with `deploy_v2`, so a shard is never observable in an
    /// uninitialized state between deployment and setup.
    pub fn __constructor(env: Env, router: Address, start_batch_id: u64, end_batch_id: u64) {
        env.storage().instance().set(&DataKey::Router, &router);
        env.storage()
            .instance()
            .set(&DataKey::StartBatchId, &start_batch_id);
        env.storage()
            .instance()
            .set(&DataKey::EndBatchId, &end_batch_id);
        env.storage()
            .instance()
            .set(&DataKey::PrunedUpTo, &start_batch_id);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
    }

    /// Writes a batch anchored by the router. `batch_id` must fall in this
    /// shard's assigned `[start, end)` range — the router is solely
    /// responsible for computing that correctly, so a violation here means a
    /// router bug, not a recoverable user error.
    pub fn anchor_batch(
        env: Env,
        batch_id: u64,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) {
        let router: Address = env.storage().instance().get(&DataKey::Router).unwrap();
        router.require_auth();

        let start: u64 = env
            .storage()
            .instance()
            .get(&DataKey::StartBatchId)
            .unwrap();
        let end: u64 = env.storage().instance().get(&DataKey::EndBatchId).unwrap();
        assert!(
            batch_id >= start && batch_id < end,
            "batch_id out of shard range"
        );

        let record = BatchRecord {
            root,
            count,
            period_start,
            period_end,
            anchored_ledger: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Batch(batch_id), &record);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), TTL_THRESHOLD, TTL_EXTEND);
    }

    pub fn get_batch(env: Env, batch_id: u64) -> Result<BatchRecord, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Batch(batch_id))
            .ok_or(Error::BatchNotFound)
    }

    pub fn verify_receipt(
        env: Env,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        if proof.len() > MAX_PROOF_LEN {
            return Err(Error::ProofTooLong);
        }
        let batch = Self::get_batch(env.clone(), batch_id)?;
        let mut computed_hash = leaf.to_array();

        // Use a static buffer on the guest stack to process the Merkle proof
        // avoiding dynamic host object overhead or heap allocations.
        let mut proof_buffer = [[0u8; 32]; 128];
        let len = proof.len() as usize;
        assert!(len <= 128, "proof length exceeds static buffer limit");

        for (i, sibling_bytes) in proof.into_iter().enumerate() {
            proof_buffer[i] = sibling_bytes.to_array();
        }

        for sibling in proof_buffer.iter().take(len) {
            let mut combined = [0u8; 64];
            if computed_hash <= *sibling {
                combined[..32].copy_from_slice(&computed_hash);
                combined[32..].copy_from_slice(sibling);
            } else {
                combined[..32].copy_from_slice(sibling);
                combined[32..].copy_from_slice(&computed_hash);
            }
            let mut hasher = Sha256::new();
            hasher.update(combined);
            computed_hash = hasher.finalize().into();
        }

        Ok(computed_hash == batch.root.to_array())
    }

    pub fn extend_batch_ttl(env: Env, batch_id: u64) -> Result<(), Error> {
        if !env.storage().persistent().has(&DataKey::Batch(batch_id)) {
            return Err(Error::BatchNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Deletes batches from this shard's local `PrunedUpTo` cursor forward,
    /// stopping at the first batch not yet old enough, `high_water_batch_id`
    /// (never prune ahead of what the router has actually written), or
    /// `max_batches` deletions — whichever comes first. Returns the advanced
    /// cursor and how many batches were pruned, so the router can decide
    /// whether to keep going within this shard or move to the next one.
    ///
    /// Router-gated: pruning order is a router-level invariant (a contiguous
    /// global prefix), so only the router may advance this shard's cursor.
    pub fn prune_batches(
        env: Env,
        before_ledger: u32,
        max_batches: u32,
        high_water_batch_id: u64,
    ) -> (u64, u64) {
        let router: Address = env.storage().instance().get(&DataKey::Router).unwrap();
        router.require_auth();

        let end_batch_id: u64 = env.storage().instance().get(&DataKey::EndBatchId).unwrap();
        let ceiling = high_water_batch_id.min(end_batch_id);

        let mut cursor: u64 = env.storage().instance().get(&DataKey::PrunedUpTo).unwrap();
        let mut pruned: u64 = 0;

        while cursor < ceiling && pruned < max_batches as u64 {
            match env
                .storage()
                .persistent()
                .get::<_, BatchRecord>(&DataKey::Batch(cursor))
            {
                Some(record) if record.anchored_ledger < before_ledger => {
                    env.storage().persistent().remove(&DataKey::Batch(cursor));
                    cursor += 1;
                    pruned += 1;
                }
                Some(_) => break,
                None => {
                    cursor += 1;
                    pruned += 1;
                }
            }
        }

        if pruned > 0 {
            env.storage().instance().set(&DataKey::PrunedUpTo, &cursor);
            env.storage()
                .instance()
                .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        }

        (cursor, pruned)
    }

    pub fn get_router(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Router).unwrap()
    }

    /// Returns `(start_batch_id, end_batch_id)`, the shard's assigned
    /// half-open range.
    pub fn get_range(env: Env) -> (u64, u64) {
        (
            env.storage()
                .instance()
                .get(&DataKey::StartBatchId)
                .unwrap(),
            env.storage().instance().get(&DataKey::EndBatchId).unwrap(),
        )
    }
}

mod test;
