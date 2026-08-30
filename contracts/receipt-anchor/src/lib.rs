#![no_std]

pub mod zk_verifier;

use accensa_common::Error;
use sha2::{Digest, Sha256};
use soroban_sdk::{
    contract, contractclient, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, InvokeError, Vec,
};
pub use zk_verifier::{VerifyingKey, ZkProof};

contractmeta!(key = "name", val = "ReceiptAnchor");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));
contractmeta!(key = "commit_dirty", val = env!("GIT_DIRTY"));

#[contracttype]
pub enum DataKey {
    Admin,
    BatchCount,
    PrunedUpTo,
    RootBuffer,
    LastAnchorTime,
    MinAnchorInterval,
    /// The installed `ReceiptShard` wasm hash, set at `initialize` and used by
    /// the factory to deploy every subsequent shard.
    ShardWasmHash,
    ShardCount,
    /// Maps a shard index (`batch_id_zero_based / SHARD_CAPACITY`) to the
    /// deployed shard's contract address.
    Shard(u64),
}

/// Structurally identical to `receipt-shard::BatchRecord`. See that crate for
/// why the two are duplicated instead of shared: it keeps each contract's wasm
/// independently buildable without a wasm-export collision from depending on
/// the other's `#[contract]` crate directly.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_ledger: u32,
}

/// The `ReceiptShard` entry points this router calls into. Declared as a
/// trait (rather than depending on the `receipt-shard` crate) so
/// `#[contractclient]` can generate `ShardClient` without pulling the shard's
/// own `#[contract]` exports into this contract's wasm.
#[contractclient(name = "ShardClient")]
pub trait ShardInterface {
    fn anchor_batch(
        env: Env,
        batch_id: u64,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    );
    fn get_batch(env: Env, batch_id: u64) -> Result<BatchRecord, Error>;
    fn verify_receipt(
        env: Env,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error>;
    fn extend_batch_ttl(env: Env, batch_id: u64) -> Result<(), Error>;
    fn prune_batches(
        env: Env,
        before_ledger: u32,
        max_batches: u32,
        high_water_batch_id: u64,
    ) -> (u64, u64);
}

/// Emitted when a merchant anchors a batch of receipts.
///
/// Topics: `("anchor_event", batch_id)`. The data map mirrors [`BatchRecord`], so
/// indexers can decode it with the same shape returned by `get_batch`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorEvent {
    #[topic]
    pub batch_id: u64,
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneEvent {
    #[topic]
    pub start_batch_id: u64,
    pub end_batch_id: u64,
}

/// Emitted when the factory spawns a new shard to hold a fresh capacity range.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardCreatedEvent {
    #[topic]
    pub shard_index: u64,
    pub shard_address: Address,
    pub start_batch_id: u64,
    pub end_batch_id: u64,
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
/// This ensures batches survive for long-term audit use before requiring a TTL bump or restoration.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;

const MAX_BATCH_SIZE: u32 = 1000;

/// Maximum valid Merkle proof length, derived from MAX_BATCH_SIZE.
/// A batch of N leaves produces a tree of depth ⌈log₂(N)⌉. For
/// MAX_BATCH_SIZE = 1000, that is 10. Any proof longer is malformed.
const MAX_PROOF_LEN: u32 = 10;

// Compile-time assertion: MAX_PROOF_LEN must equal ⌈log₂(MAX_BATCH_SIZE)⌉.
const _: () = assert!(
    MAX_PROOF_LEN >= 1
        && (1u32 << (MAX_PROOF_LEN - 1)) < MAX_BATCH_SIZE
        && (1u32 << MAX_PROOF_LEN) >= MAX_BATCH_SIZE,
    "MAX_PROOF_LEN must equal ⌈log₂(MAX_BATCH_SIZE)⌉; update together"
);

/// Maximum number of batches to delete in a single `prune_batches` call.
/// Keeps per-transaction compute bounded; callers resume by invoking again
/// (the `PrunedUpTo` cursor advances across calls, potentially across shards).
const MAX_PRUNE_BATCHES: u64 = 100;

/// Maximum number of historical roots retained in the ring buffer.
/// Proofs are valid against any root still in the buffer.
const ROOT_BUFFER_SIZE: u32 = 100;

/// Maximum allowed value for `min_anchor_interval` (24 hours in seconds).
/// Prevents the admin from setting an unreasonably high interval.
const MAX_ANCHOR_INTERVAL: u32 = 86_400;

/// How many batch ids each shard holds before the factory spawns the next
/// one. A shard's persistent storage holds at most `SHARD_CAPACITY`
/// `BatchRecord` entries, keeping its footprint bounded regardless of how
/// much total history `ReceiptAnchor` has anchored.
const SHARD_CAPACITY: u64 = 200;

#[contract]
pub struct ReceiptAnchor;

#[contractimpl]
impl ReceiptAnchor {
    pub fn initialize(
        env: Env,
        merchant: Address,
        shard_wasm_hash: BytesN<32>,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &merchant);
        env.storage().instance().set(&DataKey::BatchCount, &0u64);
        env.storage().instance().set(&DataKey::PrunedUpTo, &1u64);
        env.storage()
            .instance()
            .set(&DataKey::RootBuffer, &Vec::<BytesN<32>>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::MinAnchorInterval, &0u32);
        env.storage()
            .instance()
            .set(&DataKey::ShardWasmHash, &shard_wasm_hash);
        env.storage().instance().set(&DataKey::ShardCount, &0u64);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Anchors a batch of receipts using a state root.
    pub fn anchor_batch(
        env: Env,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        Self::anchor_batch_internal(&env, root, count, period_start, period_end)
    }

    /// Anchors a batch of receipts by verifying a ZK validity proof of the state root.
    /// Returns the assigned `batch_id` upon successful verification.
    pub fn anchor_batch_zk(
        env: Env,
        state_root: BytesN<32>,
        proof: ZkProof,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        let is_valid = zk_verifier::verify_batch_zk_proof(&env, &state_root, &proof, count)?;
        if !is_valid {
            return Err(Error::InvalidProof);
        }
        Self::anchor_batch_internal(&env, state_root, count, period_start, period_end)
    }

    /// Internal batch anchoring helper.
    fn anchor_batch_internal(
        env: &Env,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        if count > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        // Rate-limit check: enforce only when interval > 0 and a previous anchor exists.
        let min_interval: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinAnchorInterval)
            .unwrap_or(0);
        if min_interval > 0 {
            if let Some(last_time) = env
                .storage()
                .instance()
                .get::<_, u64>(&DataKey::LastAnchorTime)
            {
                let now = env.ledger().timestamp();
                if now < last_time + (min_interval as u64) {
                    return Err(Error::AnchorRateLimited);
                }
            }
        }

        let batch_count: u64 = env.storage().instance().get(&DataKey::BatchCount).unwrap();
        if batch_count > 0 {
            if let Ok(last_batch) = Self::get_batch(env.clone(), batch_count) {
                if last_batch.root == root {
                    return Err(Error::DuplicateRoot);
                }
            }
        }
        let batch_id = batch_count + 1;
        let shard_index = (batch_id - 1) / SHARD_CAPACITY;
        let shard_addr = Self::get_or_create_shard(env, shard_index)?;

        let anchored_ledger = env.ledger().sequence();
        ShardClient::new(env, &shard_addr).anchor_batch(
            &batch_id,
            &root,
            &count,
            &period_start,
            &period_end,
        );

        env.storage()
            .instance()
            .set(&DataKey::BatchCount, &batch_id);

        // Store the anchor timestamp for rate-limiting.
        env.storage()
            .instance()
            .set(&DataKey::LastAnchorTime, &env.ledger().timestamp());

        // Push root into the ring buffer, evicting the oldest if full.
        let mut buffer: Vec<BytesN<32>> =
            env.storage().instance().get(&DataKey::RootBuffer).unwrap();
        if buffer.len() >= ROOT_BUFFER_SIZE {
            buffer.remove(0);
        }
        buffer.push_back(root.clone());
        env.storage().instance().set(&DataKey::RootBuffer, &buffer);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        AnchorEvent {
            batch_id,
            root,
            count,
            period_start,
            period_end,
            anchored_ledger,
        }
        .publish(env);

        Ok(batch_id)
    }

    /// Verifies a Groth16 zero-knowledge proof against public inputs and a verifying key.
    pub fn verify_zk_proof(
        env: Env,
        proof: ZkProof,
        vk: VerifyingKey,
        public_inputs: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        zk_verifier::verify_groth16(&env, &proof, &vk, &public_inputs)
    }

    pub fn get_batch(env: Env, batch_id: u64) -> Result<BatchRecord, Error> {
        let shard_addr = Self::shard_for_batch(&env, batch_id)?;
        Self::unwrap_shard_result(ShardClient::new(&env, &shard_addr).try_get_batch(&batch_id))
    }

    pub fn verify_receipt(
        env: Env,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        let shard_addr = Self::shard_for_batch(&env, batch_id)?;
        Self::unwrap_shard_result(
            ShardClient::new(&env, &shard_addr).try_verify_receipt(&batch_id, &leaf, &proof),
        )
    }

    /// Verify a receipt against any root in the historical ring buffer.
    /// Returns `true` if the root is in the buffer AND the Merkle proof is valid.
    pub fn verify_receipt_by_root(
        env: Env,
        root: BytesN<32>,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        if proof.len() > MAX_PROOF_LEN {
            return Err(Error::ProofTooLong);
        }
        let buffer: Vec<BytesN<32>> = env
            .storage()
            .instance()
            .get(&DataKey::RootBuffer)
            .ok_or(Error::NotInitialized)?;

        let mut found = false;
        for stored_root in buffer.iter() {
            if stored_root == root {
                found = true;
                break;
            }
        }
        if !found {
            return Err(Error::RootNotFound);
        }

        let computed_hash = Self::fold_proof(leaf.to_array(), proof);

        Ok(computed_hash == root.to_array())
    }

    /// Folds a sorted-pair Merkle proof with one allocation-free guest loop.
    fn fold_proof(mut computed_hash: [u8; 32], proof: Vec<BytesN<32>>) -> [u8; 32] {
        for sibling_bytes in proof.into_iter() {
            let sibling = sibling_bytes.to_array();
            let mut combined = [0u8; 64];
            if computed_hash <= sibling {
                combined[..32].copy_from_slice(&computed_hash);
                combined[32..].copy_from_slice(&sibling);
            } else {
                combined[..32].copy_from_slice(&sibling);
                combined[32..].copy_from_slice(&computed_hash);
            }
            let mut hasher = Sha256::new();
            hasher.update(combined);
            computed_hash = hasher.finalize().into();
        }
        computed_hash
    }

    /// Returns the current ring buffer of historical roots (read-only).
    pub fn get_root_buffer(env: Env) -> Vec<BytesN<32>> {
        env.storage()
            .instance()
            .get(&DataKey::RootBuffer)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the maximum number of historical roots retained in the ring buffer.
    pub fn get_root_buffer_size(_env: Env) -> u32 {
        ROOT_BUFFER_SIZE
    }

    pub fn get_batch_count(env: Env) -> Result<u64, Error> {
        env.storage()
            .instance()
            .get(&DataKey::BatchCount)
            .ok_or(Error::NotInitialized)
    }

    /// Sets the minimum interval (in seconds) between consecutive anchors.
    /// Must be ≤ `MAX_ANCHOR_INTERVAL` (86,400 / 24 h). Setting to 0 disables
    /// rate-limiting entirely.
    pub fn set_min_anchor_interval(env: Env, interval: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if interval > MAX_ANCHOR_INTERVAL {
            return Err(Error::BatchTooLarge);
        }

        env.storage()
            .instance()
            .set(&DataKey::MinAnchorInterval, &interval);
        Ok(())
    }

    /// Returns the current minimum anchor interval in seconds (read-only).
    pub fn get_min_anchor_interval(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MinAnchorInterval)
            .unwrap_or(0)
    }
    /// Returns the admin (merchant) address, or `NotInitialized` if the
    /// contract has not been initialized.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the pruned-up-to batch ID. Batches with IDs less than or equal
    /// to this value have been pruned and are no longer verifiable on-chain.
    pub fn get_pruned_up_to(env: Env) -> Result<u64, Error> {
        env.storage()
            .instance()
            .get(&DataKey::PrunedUpTo)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the maximum number of receipts allowed in a single `anchor_batch`.
    ///
    /// Clients should call this rather than hard-coding the limit so they stay
    /// in sync if the constant is ever tuned.
    pub fn get_max_batch_size(_env: Env) -> u32 {
        MAX_BATCH_SIZE
    }

    pub fn get_max_proof_len(_env: Env) -> u32 {
        MAX_PROOF_LEN
    }

    pub fn get_shard_capacity(_env: Env) -> u64 {
        SHARD_CAPACITY
    }

    pub fn get_shard_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ShardCount)
            .unwrap_or(0)
    }

    pub fn get_shard_address(env: Env, shard_index: u64) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Shard(shard_index))
            .ok_or(Error::BatchNotFound)
    }

    pub fn extend_batch_ttl(env: Env, batch_id: u64) -> Result<(), Error> {
        let shard_addr = Self::shard_for_batch(&env, batch_id)?;
        Self::unwrap_shard_result(
            ShardClient::new(&env, &shard_addr).try_extend_batch_ttl(&batch_id),
        )
    }

    pub fn prune_batches(env: Env, before_ledger: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let start_batch_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::PrunedUpTo)
            .unwrap_or(1);
        let batch_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::BatchCount)
            .unwrap_or(0);

        let mut cursor = start_batch_id;
        let mut remaining = MAX_PRUNE_BATCHES;

        while remaining > 0 && cursor <= batch_count {
            let shard_index = (cursor - 1) / SHARD_CAPACITY;
            let Some(shard_addr) = env
                .storage()
                .instance()
                .get::<_, Address>(&DataKey::Shard(shard_index))
            else {
                break;
            };
            // Never let a shard treat a not-yet-anchored batch id as prunable.
            let shard_end_exclusive = shard_index * SHARD_CAPACITY + SHARD_CAPACITY + 1;
            let high_water = shard_end_exclusive.min(batch_count + 1);

            let (new_cursor, pruned) = ShardClient::new(&env, &shard_addr).prune_batches(
                &before_ledger,
                &(remaining as u32),
                &high_water,
            );

            cursor = new_cursor;
            remaining -= pruned;

            if pruned == 0 {
                break;
            }
        }

        if cursor > start_batch_id {
            env.storage().instance().set(&DataKey::PrunedUpTo, &cursor);
            PruneEvent {
                start_batch_id,
                end_batch_id: cursor,
            }
            .publish(&env);
        }

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Returns the shard address that owns `batch_id`, deploying it via the
    /// factory if this is the first batch to land in its capacity range.
    fn get_or_create_shard(env: &Env, shard_index: u64) -> Result<Address, Error> {
        let key = DataKey::Shard(shard_index);
        if let Some(addr) = env.storage().instance().get::<_, Address>(&key) {
            return Ok(addr);
        }

        let wasm_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::ShardWasmHash)
            .ok_or(Error::NotInitialized)?;

        let start_batch_id = shard_index * SHARD_CAPACITY + 1;
        let end_batch_id = start_batch_id + SHARD_CAPACITY;

        let salt = Self::shard_salt(env, shard_index);
        let shard_addr = env.deployer().with_current_contract(salt).deploy_v2(
            wasm_hash,
            (env.current_contract_address(), start_batch_id, end_batch_id),
        );

        env.storage().instance().set(&key, &shard_addr);
        let shard_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::ShardCount, &(shard_count + 1));

        ShardCreatedEvent {
            shard_index,
            shard_address: shard_addr.clone(),
            start_batch_id,
            end_batch_id,
        }
        .publish(env);

        Ok(shard_addr)
    }

    /// Deterministic per-shard deploy salt: the shard index big-endian in the
    /// low 8 bytes, zero-padded. Deterministic so the same shard index always
    /// resolves to the same address, and distinct across indices so shards
    /// never collide.
    fn shard_salt(env: &Env, shard_index: u64) -> BytesN<32> {
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&shard_index.to_be_bytes());
        BytesN::from_array(env, &bytes)
    }

    fn shard_for_batch(env: &Env, batch_id: u64) -> Result<Address, Error> {
        if batch_id == 0 {
            return Err(Error::BatchNotFound);
        }
        let shard_index = (batch_id - 1) / SHARD_CAPACITY;
        env.storage()
            .instance()
            .get(&DataKey::Shard(shard_index))
            .ok_or(Error::BatchNotFound)
    }

    fn unwrap_shard_result<T, C>(
        res: Result<Result<T, C>, Result<Error, InvokeError>>,
    ) -> Result<T, Error> {
        match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err(Error::ShardCallFailed),
            Err(Ok(e)) => Err(e),
            Err(Err(_)) => Err(Error::ShardCallFailed),
        }
    }
}

#[cfg(test)]
mod fuzz_test;
#[cfg(test)]
mod test;
