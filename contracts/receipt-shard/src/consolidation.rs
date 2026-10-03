//! Storage-shard consolidation and defragmentation (issue #437).
//!
//! A router-authorized source shard can move its exact [`BatchRecord`] values
//! into another shard. The destination accepts a migrated record only from
//! the source contract that invoked it, so the existing router trust boundary
//! remains the sole authority for starting a migration. The source deletes a
//! record only after the destination has accepted and returned the same record.

use crate::{
    BatchRecord, DataKey, ReceiptShard, ReceiptShardArgs, ReceiptShardClient, TTL_EXTEND,
    TTL_THRESHOLD,
};
use accensa_common::Error;
use soroban_sdk::{contractclient, contractevent, contractimpl, Address, Env, Vec};

#[contractclient(name = "ConsolidationTargetClient")]
pub trait ConsolidationTargetInterface {
    fn insert_migrated_batch(
        env: Env,
        source: Address,
        batch_id: u64,
        record: BatchRecord,
    ) -> Result<BatchRecord, Error>;
}

/// Emitted after one or more batch records have been moved from a drained
/// source shard to a destination shard.
///
/// Topics: `("shards_consolidated", source_shard_id, destination_shard_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardsConsolidated {
    #[topic]
    pub source_shard_id: u64,
    #[topic]
    pub destination_shard_id: u64,
    pub migrated_count: u32,
}

#[contractimpl]
impl ReceiptShard {
    /// Moves the selected batch records to `destination`.
    ///
    /// The caller must be this shard's configured router. Each destination
    /// write is authorized by this source contract, and the source record is
    /// removed only after the destination returns the identical record. The
    /// whole operation is atomic, so a failed destination write leaves the
    /// source unchanged.
    ///
    /// If all records have been drained, the source is automatically marked
    /// inactive. Its contract address remains valid for historical discovery,
    /// while state-changing entry points reject further writes.
    pub fn consolidate(
        env: Env,
        destination: Address,
        source_shard_id: u64,
        destination_shard_id: u64,
        batch_ids: Vec<u64>,
    ) -> u32 {
        Self::require_active_and_router(&env);
        assert!(
            destination != env.current_contract_address(),
            "destination must differ from source"
        );

        let mut migrated = 0u32;
        let source = env.current_contract_address();
        let target = ConsolidationTargetClient::new(&env, &destination);

        for batch_id in batch_ids.iter() {
            let record: BatchRecord = env
                .storage()
                .persistent()
                .get(&DataKey::Batch(batch_id))
                .unwrap_or_else(|| panic!("batch not found"));

            let stored = target.insert_migrated_batch(&source, &batch_id, &record);
            assert!(stored == record, "migrated batch record mismatch");

            env.storage().persistent().remove(&DataKey::Batch(batch_id));
            crate::diagnostics::record_removals(&env, 1, record.count as u64);
            migrated += 1;
        }

        if migrated > 0 && !Self::has_live_records(&env) {
            Self::decommission_internal(&env);
        }

        if migrated > 0 {
            ShardsConsolidated {
                source_shard_id,
                destination_shard_id,
                migrated_count: migrated,
            }
            .publish(&env);
        }

        migrated
    }

    /// Inserts an exact batch record into a destination shard.
    ///
    /// Only a source shard contract can authorize this call: `source` must be
    /// the direct invoker. This prevents an external caller from injecting an
    /// arbitrary record while still allowing a router-authorized source shard
    /// to make the nested migration call.
    pub fn insert_migrated_batch(
        env: Env,
        source: Address,
        batch_id: u64,
        record: BatchRecord,
    ) -> Result<BatchRecord, Error> {
        assert!(Self::is_active(env.clone()), "shard is inactive");
        source.require_auth();

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
        assert!(
            !env.storage().persistent().has(&DataKey::Batch(batch_id)),
            "batch already exists in destination shard"
        );

        env.storage()
            .persistent()
            .set(&DataKey::Batch(batch_id), &record);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Batch(batch_id), TTL_THRESHOLD, TTL_EXTEND);
        crate::diagnostics::record_migration(&env, batch_id, record.count);

        Ok(record)
    }

    /// Marks a drained shard inactive. No batch records may remain.
    pub fn decommission(env: Env) {
        Self::require_active_and_router(&env);
        assert!(!Self::has_live_records(&env), "shard is not drained");
        Self::decommission_internal(&env);
    }

    pub fn is_active(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Active)
            .unwrap_or(true)
    }

    fn require_active_and_router(env: &Env) {
        assert!(Self::is_active(env.clone()), "shard is inactive");
        let router: Address = env.storage().instance().get(&DataKey::Router).unwrap();
        router.require_auth();
    }

    fn has_live_records(env: &Env) -> bool {
        let start: u64 = env
            .storage()
            .instance()
            .get(&DataKey::StartBatchId)
            .unwrap();
        let end: u64 = env.storage().instance().get(&DataKey::EndBatchId).unwrap();
        (start..end).any(|batch_id| env.storage().persistent().has(&DataKey::Batch(batch_id)))
    }

    fn decommission_internal(env: &Env) {
        env.storage().instance().set(&DataKey::Active, &false);
    }
}
