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
    /// Per-logical-shard batch count. Each `shard_id` owns an independent
    /// batch stream that starts at `1`, so one region/asset type can anchor
    /// concurrently with another without sharing numbering or the
    /// duplicate-root check.
    ShardBatchCount(u64),
    /// Per-logical-shard prune cursor (`PrunedUpTo`), mirroring the
    /// contiguous-prefix guarantee independently for each `shard_id`.
    ShardPrunedUpTo(u64),
    /// Per-logical-shard historical root ring buffer.
    ShardRootBuffer(u64),
    /// The set of logical `shard_id`s that have anchored at least one batch,
    /// in first-use (insertion) order. Lets callers enumerate the live shards
    /// via `get_shard_ids`.
    ShardIds,
    /// Admin-configured token-bucket rate limit for `anchor_batch`.
    /// `{0, 0}` (the default) disables rate limiting.
    RateLimitConfig,
    /// Per-identity token-bucket state, keyed by the anchoring identity
    /// (the merchant). Written only while rate limiting is enabled.
    RateLimitBucket(Address),
    /// The installed `ReceiptShard` wasm hash, set at `initialize` and used by
    /// the router to deploy every subsequent storage shard.
    ShardWasmHash,
    /// Admin-configured minimum interval, in seconds, between anchors.
    /// `0` (the default) disables the check.
    MinAnchorInterval,
    /// Maps a storage-shard index (`(batch_id-1) % SHARD_CAPACITY`, computed
    /// within a logical `shard_id`'s own stream) to the deployed storage
    /// shard's contract address. Keyed by `(logical shard_id, storage index)`
    /// so logical shards isolate their storage shards from one another.
    Shard(u64, u64),
}

/// Admin-configurable token-bucket rate limit applied to `anchor_batch`.
///
/// `burst_capacity` is the maximum number of anchors an identity may submit
/// back-to-back before the bucket empties; the bucket then refills at one
/// token per `refill_interval_secs` seconds, capped at `burst_capacity`. A
/// config of `{0, 0}` disables rate limiting entirely (the default). This
/// subsumes the previous fixed "minimum interval" limiter: that behaviour is
/// exactly `{burst_capacity: 1, refill_interval_secs: <interval>}`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLimitConfig {
    pub burst_capacity: u32,
    pub refill_interval_secs: u32,
}

/// Per-identity token-bucket state: tokens currently in the bucket and the
/// ledger timestamp of the last refill. Packed into a single 12-byte
/// persistent entry per identity, the only tracking storage the rate limiter
/// needs.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BucketState {
    pub tokens: u32,
    pub last_refill: u64,
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
/// Topics: `("anchor_event", shard_id, batch_id)`. The data map mirrors
/// [`BatchRecord`], so indexers can decode it with the same shape returned by
/// `get_batch`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorEvent {
    #[topic]
    pub shard_id: u64,
    #[topic]
    pub batch_id: u64,
    pub root: BytesN<32>,
    pub count: u32,
    pub period_start: u64,
    pub period_end: u64,
    pub anchored_ledger: u32,
}

/// Emitted when a merchant prunes a contiguous prefix of a shard's batch
/// stream.
///
/// Topics: `("prune_event", shard_id, start_batch_id)`. Pruning is per logical
/// shard, so `shard_id` is required to disambiguate the range.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneEvent {
    #[topic]
    pub shard_id: u64,
    #[topic]
    pub start_batch_id: u64,
    pub end_batch_id: u64,
}

/// Emitted when the router spawns a new storage shard to hold a fresh capacity
/// range within a logical `shard_id`'s batch stream.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardCreatedEvent {
    #[topic]
    pub shard_id: u64,
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
#[allow(dead_code)]
const MAX_PRUNE_BATCHES: u32 = 100;

/// Maximum number of historical roots retained in the ring buffer.
/// Proofs are valid against any root still in the buffer.
const ROOT_BUFFER_SIZE: u32 = 100;

/// Maximum allowed burst capacity for the anchor rate limiter. Caps how many
/// back-to-back anchors a single identity can submit before the bucket
/// refills, so an admin cannot configure the burst so large that the
/// protection is meaningless.
/// Upper bound on the configurable minimum anchor interval (24 h).
const MAX_ANCHOR_INTERVAL: u32 = 86_400;

const MAX_RATE_BURST: u32 = 1000;

/// Maximum allowed refill interval for the anchor rate limiter (24 hours in
/// seconds). Prevents the admin from setting an interval so long that
/// legitimate anchoring becomes impossible.
const MAX_RATE_REFILL_INTERVAL: u32 = 86_400;

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
        env.storage()
            .instance()
            .set(&DataKey::ShardIds, &Vec::<u64>::new(&env));
        env.storage().instance().set(
            &DataKey::RateLimitConfig,
            &RateLimitConfig {
                burst_capacity: 0,
                refill_interval_secs: 0,
            },
        );
        env.storage()
            .instance()
            .set(&DataKey::ShardWasmHash, &shard_wasm_hash);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Anchors a batch of receipts for `shard_id` using a state root.
    ///
    /// `shard_id` is a logical partition of the merchant's receipts (region,
    /// asset type, etc.). Each `shard_id` owns an independent batch stream, so
    /// different shards can be anchored concurrently and share no batch
    /// numbering, no duplicate-root check, and no root history.
    pub fn anchor_batch(
        env: Env,
        shard_id: u64,
        root: BytesN<32>,
        count: u32,
        period_start: u64,
        period_end: u64,
    ) -> Result<u64, Error> {
        Self::anchor_batch_internal(&env, shard_id, root, count, period_start, period_end)
    }

    /// Anchors a batch of receipts for `shard_id` by verifying a ZK validity
    /// proof of the state root. Returns the assigned `batch_id` (in that
    /// shard's own stream) upon successful verification.
    pub fn anchor_batch_zk(
        env: Env,
        shard_id: u64,
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
        Self::anchor_batch_internal(&env, shard_id, state_root, count, period_start, period_end)
    }

    /// Internal batch anchoring helper.
    fn anchor_batch_internal(
        env: &Env,
        shard_id: u64,
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

        // Token-bucket rate limit, enforced only when the admin has configured
        // one. Phase 1 (here) is a read-only admission check: it refills the
        // bucket with tokens earned over the elapsed refill intervals and
        // rejects the anchor when the bucket is empty, without writing any
        // state. The token itself is consumed in phase 2 after the anchor has
        // been written, so a failed anchor (e.g. a duplicate root) does not
        // spend a token. When rate limiting is disabled this costs exactly one
        // instance-storage read — nothing is written and no bucket entry is
        // ever created. The limiter is global per merchant (v1 scope), not per
        // shard.
        let rate_limit: RateLimitConfig = env
            .storage()
            .instance()
            .get(&DataKey::RateLimitConfig)
            .unwrap_or(RateLimitConfig {
                burst_capacity: 0,
                refill_interval_secs: 0,
            });
        let rate_limit_active =
            rate_limit.burst_capacity > 0 && rate_limit.refill_interval_secs > 0;
        let bucket_key = if rate_limit_active {
            let key = DataKey::RateLimitBucket(merchant.clone());
            Self::rate_limit_admitted(env, &key, &rate_limit)?;
            Some(key)
        } else {
            None
        };

        // The duplicate-root check is scoped to this shard's own stream: the
        // latest batch of *this* shard, not a global last batch.
        let batch_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardBatchCount(shard_id))
            .unwrap_or(0);
        if batch_count > 0 {
            if let Ok(last_batch) = Self::get_batch(env.clone(), shard_id, batch_count) {
                if last_batch.root == root {
                    return Err(Error::DuplicateRoot);
                }
            }
        }
        let batch_id = batch_count + 1;
        let shard_index = (batch_id - 1) / SHARD_CAPACITY;
        let shard_addr = Self::get_or_create_shard(env, shard_id, shard_index)?;

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
            .set(&DataKey::ShardBatchCount(shard_id), &batch_id);
        Self::register_shard_id(env, shard_id);

        // Phase 2 of the rate limit: spend the token now that the anchor
        // succeeded, persisting the bucket alongside the batch.
        if let Some(key) = bucket_key {
            Self::rate_limit_consume(env, &key, &rate_limit);
        }

        // Push root into this shard's ring buffer, evicting the oldest if full.
        let mut buffer: Vec<BytesN<32>> = env
            .storage()
            .instance()
            .get(&DataKey::ShardRootBuffer(shard_id))
            .unwrap_or_else(|| Vec::new(env));
        if buffer.len() >= ROOT_BUFFER_SIZE {
            buffer.remove(0);
        }
        buffer.push_back(root.clone());
        env.storage()
            .instance()
            .set(&DataKey::ShardRootBuffer(shard_id), &buffer);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        AnchorEvent {
            shard_id,
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

    pub fn get_batch(env: Env, shard_id: u64, batch_id: u64) -> Result<BatchRecord, Error> {
        let shard_addr = Self::shard_for_batch(&env, shard_id, batch_id)?;
        Self::unwrap_shard_result(ShardClient::new(&env, &shard_addr).try_get_batch(&batch_id))
    }

    pub fn verify_receipt(
        env: Env,
        shard_id: u64,
        batch_id: u64,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        let shard_addr = Self::shard_for_batch(&env, shard_id, batch_id)?;
        Self::unwrap_shard_result(
            ShardClient::new(&env, &shard_addr).try_verify_receipt(&batch_id, &leaf, &proof),
        )
    }

    /// Verify a receipt against any root in `shard_id`'s historical ring
    /// buffer. Returns `true` if the root is in the buffer AND the Merkle
    /// proof is valid. Roots are isolated per shard, so a root anchored in one
    /// shard is not verifiable by root in another.
    pub fn verify_receipt_by_root(
        env: Env,
        shard_id: u64,
        root: BytesN<32>,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        if proof.len() > MAX_PROOF_LEN {
            return Err(Error::ProofTooLong);
        }
        Self::require_initialized(&env)?;
        // A shard with no anchored roots has an empty root history, so any
        // root is unknown.
        let buffer: Vec<BytesN<32>> = env
            .storage()
            .instance()
            .get(&DataKey::ShardRootBuffer(shard_id))
            .unwrap_or_else(|| Vec::new(&env));

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

    /// Returns the current ring buffer of historical roots for `shard_id`
    /// (read-only).
    pub fn get_root_buffer(env: Env, shard_id: u64) -> Vec<BytesN<32>> {
        env.storage()
            .instance()
            .get(&DataKey::ShardRootBuffer(shard_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the maximum number of historical roots retained in the ring buffer.
    pub fn get_root_buffer_size(_env: Env) -> u32 {
        ROOT_BUFFER_SIZE
    }

    /// Returns the latest root anchored for `shard_id`, if any.
    pub fn get_shard_root(env: Env, shard_id: u64) -> Result<BytesN<32>, Error> {
        Self::require_initialized(&env)?;
        let count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardBatchCount(shard_id))
            .unwrap_or(0);
        if count == 0 {
            return Err(Error::BatchNotFound);
        }
        let last = Self::get_batch(env, shard_id, count)?;
        Ok(last.root)
    }

    /// Returns the logical `shard_id`s that have anchored at least one batch,
    /// in first-use order.
    pub fn get_shard_ids(env: Env) -> Vec<u64> {
        env.storage()
            .instance()
            .get(&DataKey::ShardIds)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the number of batches anchored for `shard_id`. Returns `0` for
    /// an as-yet-unused shard once the contract is initialized; `NotInitialized`
    /// before the contract itself has been initialized.
    pub fn get_batch_count(env: Env, shard_id: u64) -> Result<u64, Error> {
        Self::require_initialized(&env)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::ShardBatchCount(shard_id))
            .unwrap_or(0))
    }

    /// Configures the token-bucket rate limit applied to `anchor_batch`.
    ///
    /// `burst_capacity` anchors may be submitted back-to-back before the
    /// bucket empties; it then refills one token every
    /// `refill_interval_secs` seconds, capped at `burst_capacity`. Setting
    /// both to `0` disables rate-limiting entirely (the default).
    ///
    /// Caps: `burst_capacity <= MAX_RATE_BURST` (1000) and
    /// `refill_interval_secs <= MAX_RATE_REFILL_INTERVAL` (86,400 / 24 h). A
    /// config with exactly one zeroed parameter (or either above its cap) is
    /// rejected with `InvalidRateLimitConfig`.
    pub fn set_anchor_rate_limit(
        env: Env,
        burst_capacity: u32,
        refill_interval_secs: u32,
    ) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let config = RateLimitConfig {
            burst_capacity,
            refill_interval_secs,
        };
        if !Self::is_valid_rate_limit(&config) {
            return Err(Error::InvalidRateLimitConfig);
        }

        env.storage()
            .instance()
            .set(&DataKey::RateLimitConfig, &config);
        Ok(())
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

    pub fn get_shard_capacity(_env: Env) -> u64 {
        SHARD_CAPACITY
    }

    /// Returns the number of storage shards currently holding `shard_id`'s
    /// batch stream. Returns `0` for an as-yet-unused shard once the contract
    /// is initialized.
    pub fn get_shard_count(env: Env, shard_id: u64) -> Result<u64, Error> {
        Self::require_initialized(&env)?;
        let batch_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardBatchCount(shard_id))
            .unwrap_or(0);
        if batch_count == 0 {
            Ok(0)
        } else {
            Ok((batch_count - 1) / SHARD_CAPACITY + 1)
        }
    }

    /// Returns the storage-shard address holding `shard_id`'s
    /// `shard_index`-th capacity chunk.
    pub fn get_shard_address(env: Env, shard_id: u64, shard_index: u64) -> Result<Address, Error> {
        let key = DataKey::Shard(shard_id, shard_index);
        env.storage()
            .instance()
            .get(&key)
            .ok_or(Error::BatchNotFound)
    }

    /// Returns the current anchor rate-limit configuration (read-only).
    /// Returns `{0, 0}` (rate limiting disabled) if unset or the contract is
    /// not yet initialized.
    pub fn get_anchor_rate_limit(env: Env) -> RateLimitConfig {
        env.storage()
            .instance()
            .get(&DataKey::RateLimitConfig)
            .unwrap_or(RateLimitConfig {
                burst_capacity: 0,
                refill_interval_secs: 0,
            })
    }

    /// Whether a config is acceptable: `{0, 0}` disables, otherwise both
    /// parameters must be positive and within their caps.
    fn is_valid_rate_limit(config: &RateLimitConfig) -> bool {
        if config.burst_capacity == 0 && config.refill_interval_secs == 0 {
            return true;
        }
        config.burst_capacity > 0
            && config.refill_interval_secs > 0
            && config.burst_capacity <= MAX_RATE_BURST
            && config.refill_interval_secs <= MAX_RATE_REFILL_INTERVAL
    }

    /// Phase 1 of the token bucket: refill the identity's bucket with the
    /// tokens it has earned over the elapsed refill intervals (capped at the
    /// burst capacity) and reject the anchor if the bucket is empty. Read-only
    /// — no state is written here, so a later failure does not spend a token.
    /// A missing bucket (first anchor) is treated as full, allowing the first
    /// `burst_capacity` anchors through back-to-back.
    fn rate_limit_admitted(
        env: &Env,
        key: &DataKey,
        config: &RateLimitConfig,
    ) -> Result<(), Error> {
        let now = env.ledger().timestamp();
        let mut state = env
            .storage()
            .persistent()
            .get::<_, BucketState>(key)
            .unwrap_or(BucketState {
                tokens: config.burst_capacity,
                last_refill: now,
            });

        Self::refill_bucket(&mut state, now, config);

        if state.tokens == 0 {
            return Err(Error::AnchorRateLimited);
        }
        Ok(())
    }

    /// Phase 2 of the token bucket: spend one token and persist the bucket
    /// after the anchor has been written successfully. The entry's TTL is
    /// extended alongside so an actively-anchoring identity never has its
    /// bucket archived mid-burst.
    fn rate_limit_consume(env: &Env, key: &DataKey, config: &RateLimitConfig) {
        let now = env.ledger().timestamp();
        let mut state = env
            .storage()
            .persistent()
            .get::<_, BucketState>(key)
            .unwrap_or(BucketState {
                tokens: config.burst_capacity,
                last_refill: now,
            });

        Self::refill_bucket(&mut state, now, config);
        state.tokens = state.tokens.saturating_sub(1);
        env.storage().persistent().set(key, &state);
        env.storage()
            .persistent()
            .extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND);
    }

    /// Adds the tokens earned over the elapsed refill intervals since
    /// `last_refill` (one token per `refill_interval_secs`, integer division),
    /// capped at `burst_capacity`. Resets `last_refill` to `now` only when at
    /// least one token was actually earned, so sub-interval time is never
    /// discarded while the bucket is still empty.
    fn refill_bucket(state: &mut BucketState, now: u64, config: &RateLimitConfig) {
        let elapsed = now.saturating_sub(state.last_refill);
        if elapsed >= config.refill_interval_secs as u64 {
            let refilled = elapsed / config.refill_interval_secs as u64;
            // Saturating: an outlandish `elapsed` (dormant contract, hostile
            // test ledger) must clamp to the burst, never overflow.
            state.tokens = (state.tokens as u64)
                .saturating_add(refilled)
                .min(config.burst_capacity as u64) as u32;
            state.last_refill = now;
        }
    }
    /// Returns the admin (merchant) address, or `NotInitialized` if the
    /// contract has not been initialized.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    /// Returns `NotInitialized` unless the contract has been initialized
    /// (i.e. an admin is set). Per-shard state is lazily created, so a shard's
    /// absent keys must not be mistaken for an uninitialized contract.
    fn require_initialized(env: &Env) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            Ok(())
        } else {
            Err(Error::NotInitialized)
        }
    }

    /// Returns the pruned-up-to batch ID for `shard_id`. Batches in that
    /// shard's stream with IDs less than or equal to this value have been
    /// pruned and are no longer verifiable on-chain. Returns `1` (nothing
    /// pruned) for an as-yet-unused shard once the contract is initialized.
    pub fn get_pruned_up_to(env: Env, shard_id: u64) -> Result<u64, Error> {
        Self::require_initialized(&env)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::ShardPrunedUpTo(shard_id))
            .unwrap_or(1))
    }

    /// Returns the maximum batch size supported by an anchor.
    pub fn get_max_batch_size(_env: Env) -> u32 {
        MAX_BATCH_SIZE
    }

    /// Extends the persistent TTL of a batch record in `shard_id`'s stream.
    pub fn extend_batch_ttl(env: Env, shard_id: u64, batch_id: u64) -> Result<(), Error> {
        let shard_addr = Self::shard_for_batch(&env, shard_id, batch_id)?;
        Self::unwrap_shard_result(
            ShardClient::new(&env, &shard_addr).try_extend_batch_ttl(&batch_id),
        )
    }

    /// Returns the maximum proof length supported by the verifier.
    pub fn get_max_proof_len(_env: Env) -> u32 {
        MAX_PROOF_LEN
    }

    /// Prunes `shard_id`'s batches anchored prior to `before_ledger`. Each
    /// logical shard prunes independently against its own cursor.
    pub fn prune_batches(env: Env, shard_id: u64, before_ledger: u32) -> Result<u64, Error> {
        let mut pruned_count: u64 = 0;
        let mut cursor: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardPrunedUpTo(shard_id))
            .unwrap_or(1);
        let batch_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ShardBatchCount(shard_id))
            .unwrap_or(0);

        while cursor <= batch_count && (pruned_count as u32) < MAX_PRUNE_BATCHES {
            let shard_addr = match Self::shard_for_batch(&env, shard_id, cursor) {
                Ok(addr) => addr,
                Err(_) => {
                    cursor += 1;
                    pruned_count += 1;
                    continue;
                }
            };

            let shard_client = ShardClient::new(&env, &shard_addr);
            let max_to_prune = MAX_PRUNE_BATCHES.saturating_sub(pruned_count as u32);
            let (next_cursor, count) =
                shard_client.prune_batches(&before_ledger, &max_to_prune, &(batch_count + 1));
            pruned_count += count;
            if next_cursor == cursor {
                break;
            }
            cursor = next_cursor;
        }

        env.storage()
            .instance()
            .set(&DataKey::ShardPrunedUpTo(shard_id), &cursor);

        Ok(cursor)
    }

    /// Returns the storage-shard address that owns `batch_id` in `shard_id`'s
    /// stream, deploying it if this is the first batch to land in its capacity
    /// range.
    fn get_or_create_shard(env: &Env, shard_id: u64, shard_index: u64) -> Result<Address, Error> {
        let key = DataKey::Shard(shard_id, shard_index);
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

        // The salt composes both the logical `shard_id` (high 8 bytes) and the
        // storage index (low 8 bytes), so (shard_id, index) always resolves to
        // the same address and distinct pairs never collide.
        let salt = Self::shard_salt(env, shard_id, shard_index);
        let shard_addr = env.deployer().with_current_contract(salt).deploy_v2(
            wasm_hash,
            (env.current_contract_address(), start_batch_id, end_batch_id),
        );

        env.storage().instance().set(&key, &shard_addr);

        ShardCreatedEvent {
            shard_id,
            shard_index,
            shard_address: shard_addr.clone(),
            start_batch_id,
            end_batch_id,
        }
        .publish(env);

        Ok(shard_addr)
    }

    /// Deterministic per-(shard_id, storage-index) deploy salt: the logical
    /// `shard_id` in the high 8 bytes and the storage `shard_index` in the low
    /// 8 bytes, zero-padded. Deterministic so the same pair always resolves to
    /// the same address, and distinct across pairs so storage shards never
    /// collide — even across logical shards (e.g. shard 1 index 0 vs shard 0
    /// index 1).
    fn shard_salt(env: &Env, shard_id: u64, shard_index: u64) -> BytesN<32> {
        let mut bytes = [0u8; 32];
        bytes[16..24].copy_from_slice(&shard_id.to_be_bytes());
        bytes[24..32].copy_from_slice(&shard_index.to_be_bytes());
        BytesN::from_array(env, &bytes)
    }

    fn shard_for_batch(env: &Env, shard_id: u64, batch_id: u64) -> Result<Address, Error> {
        if batch_id == 0 {
            return Err(Error::BatchNotFound);
        }
        let shard_index = (batch_id - 1) / SHARD_CAPACITY;
        env.storage()
            .instance()
            .get(&DataKey::Shard(shard_id, shard_index))
            .ok_or(Error::BatchNotFound)
    }

    /// Records `shard_id` in the `ShardIds` enumeration set if it is not
    /// already there, preserving first-use order.
    fn register_shard_id(env: &Env, shard_id: u64) {
        let mut ids: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::ShardIds)
            .unwrap_or_else(|| Vec::new(env));
        if !ids.iter().any(|id| id == shard_id) {
            ids.push_back(shard_id);
            env.storage().instance().set(&DataKey::ShardIds, &ids);
        }
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

// Tier A soroban-budget-assert gates. Compiled only when the `budget-assert`
// feature is enabled (the budget CI job), so the normal test/clippy runs stay
// free of the prebuilt-WASM requirement and the `budget_macros` dev-dependency.
#[cfg(all(test, feature = "budget-assert"))]
mod budget_test;
