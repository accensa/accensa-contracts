#![cfg(test)]

use crate::{BatchRecord, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    Address, BytesN, Env, IntoVal, Map, Symbol, Val,
};

fn setup(env: &Env) -> (ReceiptShardClient<'static>, Address) {
    env.mock_all_auths();
    let router = Address::generate(env);
    let id = env.register(ReceiptShard, (router.clone(), 1u64, 201u64));
    (ReceiptShardClient::new(env, &id), router)
}

fn record(env: &Env, root_byte: u8, count: u32) -> BatchRecord {
    BatchRecord {
        root: BytesN::from_array(env, &[root_byte; 32]),
        count,
        period_start: 10,
        period_end: 20,
        anchored_ledger: env.ledger().sequence(),
    }
}

#[test]
fn consolidation_preserves_records_and_roots() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let target = ReceiptShardClient::new(&env, &target_id);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    source.anchor_batch(&1, &root1, &3, &10, &20);
    source.anchor_batch(&2, &root2, &4, &21, &30);

    // The source contract is the invoker of the destination insertion.
    source.consolidate(&target_id, &10, &20, &soroban_sdk::vec![&env, 1u64, 2u64]);

    assert_eq!(target.get_batch(&1).root, root1);
    assert_eq!(target.get_batch(&2).root, root2);
    assert_eq!(
        source.try_get_batch(&1),
        Err(Ok(crate::Error::BatchNotFound))
    );
    assert_eq!(
        source.try_get_batch(&2),
        Err(Ok(crate::Error::BatchNotFound))
    );
    assert!(!source.is_active());

    let source_stats = source.get_shard_diagnostics();
    assert_eq!(source_stats.live_batches, 0);
    assert_eq!(source_stats.live_leaves, 0);
    assert_eq!(source_stats.total_leaves, 7);

    let target_stats = target.get_shard_diagnostics();
    assert_eq!(target_stats.live_batches, 2);
    assert_eq!(target_stats.live_leaves, 7);
    assert_eq!(target_stats.total_leaves, 7);
    assert!(target_stats.consistent);
}

#[test]
fn partial_consolidation_keeps_source_active() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let root1 = BytesN::from_array(&env, &[4u8; 32]);
    let root2 = BytesN::from_array(&env, &[5u8; 32]);
    source.anchor_batch(&1, &root1, &1, &0, &1);
    source.anchor_batch(&2, &root2, &1, &0, &1);

    assert_eq!(
        source.consolidate(&target_id, &11, &12, &soroban_sdk::vec![&env, 1u64]),
        1
    );
    assert!(source.is_active());
    assert_eq!(source.get_batch(&2).root, root2);
    assert_eq!(
        ReceiptShardClient::new(&env, &target_id).get_batch(&1).root,
        root1
    );
}

#[test]
fn destination_rejects_batch_outside_its_range() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 2u64));
    let target = ReceiptShardClient::new(&env, &target_id);

    let record = record(&env, 9, 1);
    let result = target.try_insert_migrated_batch(&source.address, &2, &record);
    assert!(result.is_err());
}

#[test]
fn decommission_requires_empty_shard() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let root = BytesN::from_array(&env, &[7u8; 32]);
    source.anchor_batch(&1, &root, &1, &0, &1);

    assert!(source.try_decommission().is_err());
    assert!(source.is_active());
}

#[test]
fn consolidation_emits_source_and_destination_ids() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let root = BytesN::from_array(&env, &[8u8; 32]);
    source.anchor_batch(&1, &root, &1, &0, &1);

    source.consolidate(&target_id, &7, &9, &soroban_sdk::vec![&env, 1u64]);

    let mut data = Map::<Symbol, Val>::new(&env);
    data.set(Symbol::new(&env, "migrated_count"), 1u32.into_val(&env));
    assert_eq!(
        env.events().all().filter_by_contract(&source.address),
        soroban_sdk::vec![
            &env,
            (
                source.address.clone(),
                (Symbol::new(&env, "shards_consolidated"), 7u64, 9u64).into_val(&env),
                data.into_val(&env)
            )
        ]
    );
}

#[test]
fn consolidation_rejects_same_source_and_destination() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    source.anchor_batch(&1, &root, &1, &0, &1);

    let res = source.try_consolidate(&source.address, &1, &1, &soroban_sdk::vec![&env, 1u64]);
    assert_eq!(res, Err(Ok(crate::Error::DestinationMismatch)));
    // Batch remains in storage
    assert_eq!(source.get_batch(&1).root, root);
}

#[test]
fn consolidation_rejects_missing_batch_with_typed_error() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));

    let res = source.try_consolidate(&target_id, &1, &2, &soroban_sdk::vec![&env, 99u64]);
    assert_eq!(res, Err(Ok(crate::Error::BatchNotFound)));
}

#[soroban_sdk::contract]
struct MismatchedTarget;

#[soroban_sdk::contractimpl]
impl MismatchedTarget {
    pub fn insert_migrated_batch(
        _env: Env,
        _source: Address,
        _batch_id: u64,
        mut record: BatchRecord,
    ) -> Result<BatchRecord, crate::Error> {
        // Return a mutated record to simulate round-trip mismatch
        record.count += 1;
        Ok(record)
    }
}

#[test]
fn consolidation_round_trip_mismatch_returns_error_and_leaves_storage_unchanged() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let mock_target_id = env.register(MismatchedTarget, ());

    let root = BytesN::from_array(&env, &[3u8; 32]);
    source.anchor_batch(&1, &root, &5, &10, &20);

    let res = source.try_consolidate(&mock_target_id, &1, &2, &soroban_sdk::vec![&env, 1u64]);
    assert_eq!(res, Err(Ok(crate::Error::RoundTripMismatch)));

    // Verify storage in source shard is unchanged
    let stored_batch = source.get_batch(&1);
    assert_eq!(stored_batch.root, root);
    assert_eq!(stored_batch.count, 5);
    assert!(source.is_active());
}

// ── Decommission and diagnostics coverage (#699) ─────────────────────────────

#[test]
fn test_decommission_on_empty_shard() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    assert!(source.is_active());

    // Shard is empty, decommission by router succeeds
    env.as_contract(&source.address, || {
        ReceiptShard::decommission(env.clone());
    });
    assert!(!source.is_active());
}

#[test]
#[should_panic(expected = "shard is inactive")]
fn test_decommission_already_inactive_fails() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    env.as_contract(&source.address, || {
        ReceiptShard::decommission(env.clone());
    });
    assert!(!source.is_active());
    env.as_contract(&source.address, || {
        ReceiptShard::decommission(env.clone());
    });
}

#[test]
#[should_panic(expected = "shard is not drained")]
fn test_decommission_not_drained_fails() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let record = BatchRecord {
        root: BytesN::from_array(&env, &[1u8; 32]),
        count: 10,
        period_start: 10,
        period_end: 20,
        anchored_ledger: env.ledger().sequence(),
    };
    env.as_contract(&source.address, || {
        env.storage()
            .persistent()
            .set(&crate::DataKey::Batch(1), &record);
        ReceiptShard::decommission(env.clone());
    });
}

#[test]
fn test_decommission_has_live_records_uninitialized_returns_error() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let res = env.as_contract(&source.address, || {
        env.storage()
            .instance()
            .remove(&crate::DataKey::StartBatchId);
        ReceiptShard::has_live_records(&env)
    });
    assert_eq!(res, Err(crate::Error::NotInitialized));
}

#[test]
fn test_diagnostics_record_anchor_and_removals() {
    let env = Env::default();
    let (source, _router) = setup(&env);

    let before = source.get_shard_diagnostics();
    assert_eq!(before.live_batches, 0);
    assert_eq!(before.live_leaves, 0);

    // Initial anchor (previous = None)
    env.as_contract(&source.address, || {
        crate::diagnostics::record_anchor(&env, 5, None, 40);
    });

    let d1 = source.get_shard_diagnostics();
    assert_eq!(d1.live_batches, 1);
    assert_eq!(d1.live_leaves, 40);
    assert_eq!(d1.total_leaves, 40);
    assert_eq!(d1.high_water_batch_id, 6);

    // Overwrite anchor (previous = Some(old))
    let old_rec = BatchRecord {
        root: BytesN::from_array(&env, &[1u8; 32]),
        count: 40,
        period_start: 10,
        period_end: 20,
        anchored_ledger: env.ledger().sequence(),
    };
    env.as_contract(&source.address, || {
        crate::diagnostics::record_anchor(&env, 5, Some(&old_rec), 60);
    });

    let d2 = source.get_shard_diagnostics();
    assert_eq!(d2.live_batches, 1);
    assert_eq!(d2.live_leaves, 60);
    assert_eq!(d2.total_leaves, 100);

    // Record removals with count == 0 is no-op
    env.as_contract(&source.address, || {
        crate::diagnostics::record_removals(&env, 0, 0);
    });
    let d3 = source.get_shard_diagnostics();
    assert_eq!(d3.live_batches, 1);
    assert_eq!(d3.live_leaves, 60);

    // Record removals with count > 0
    env.as_contract(&source.address, || {
        crate::diagnostics::record_removals(&env, 1, 60);
    });
    let d4 = source.get_shard_diagnostics();
    assert_eq!(d4.live_batches, 0);
    assert_eq!(d4.live_leaves, 0);
    assert_eq!(d4.total_leaves, 100);
}
