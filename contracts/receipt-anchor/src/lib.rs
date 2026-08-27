#![no_std]

use accensa_common::Error;
use soroban_sdk::{
    contract, contractclient, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, InvokeError, Vec,
};

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

/// Maximum number of batches to delete in a single `prune_batches` call.
/// Keeps per-transaction compute bounded; callers resume by invoking again
/// (the `PrunedUpTo` cursor advances across calls, potentially across shards).
const MAX_PRUNE_BATCHES: u64 = 100;

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
            .set(&DataKey::ShardWasmHash, &shard_wasm_hash);
        env.storage().instance().set(&DataKey::ShardCount, &0u64);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn anchor_batch(
        env: Env,
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

        let batch_count: u64 = env.storage().instance().get(&DataKey::BatchCount).unwrap();
        let batch_id = batch_count + 1;
        let shard_index = (batch_id - 1) / SHARD_CAPACITY;
        let shard_addr = Self::get_or_create_shard(&env, shard_index)?;

        let anchored_ledger = env.ledger().sequence();
        ShardClient::new(&env, &shard_addr).anchor_batch(
            &batch_id,
            &root,
            &count,
            &period_start,
            &period_end,
        );

        env.storage()
            .instance()
            .set(&DataKey::BatchCount, &batch_id);
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
        .publish(&env);

        Ok(batch_id)
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

    pub fn get_batch_count(env: Env) -> Result<u64, Error> {
        env.storage()
            .instance()
            .get(&DataKey::BatchCount)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the maximum number of receipts allowed in a single `anchor_batch`.
    ///
    /// Clients should call this rather than hard-coding the limit so they stay
    /// in sync if the constant is ever tuned.
    pub fn get_max_batch_size(_env: Env) -> u32 {
        MAX_BATCH_SIZE
    }

    /// Returns how many batch ids each shard holds before the factory spawns
    /// the next one. Clients can use this with `get_batch_count` to compute
    /// which shard address currently serves reads/writes, and to read a shard
    /// directly instead of round-tripping through the router.
    pub fn get_shard_capacity(_env: Env) -> u64 {
        SHARD_CAPACITY
    }

    /// Returns the number of shards the factory has deployed so far.
    pub fn get_shard_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ShardCount)
            .unwrap_or(0)
    }

    /// Returns the deployed address of shard `shard_index`, if it exists.
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

mod fuzz_test;
mod test;

// Tier A soroban-budget-assert gates. Compiled only when the `budget-assert`
// feature is enabled (the budget CI job), so the normal test/clippy runs stay
// free of the prebuilt-WASM requirement and the `budget_macros` dev-dependency.
#[cfg(all(test, feature = "budget-assert"))]
mod budget_test;
