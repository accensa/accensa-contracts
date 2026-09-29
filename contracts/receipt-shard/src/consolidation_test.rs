#![cfg(test)]

use crate::{BatchRecord, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events, Ledger},
    Address, BytesN, Env, IntoVal, Map, Symbol, Val,
};

#[contract]
struct TestRouter;

#[contracttype]
enum TestRouterKey {
    Shard(Address),
}

#[contractimpl]
impl TestRouter {
    pub fn register_shard(env: Env, address: Address) {
        env.storage()
            .instance()
            .set(&TestRouterKey::Shard(address), &true);
    }

    pub fn is_registered_shard(env: Env, address: Address) -> bool {
        env.storage()
            .instance()
            .get(&TestRouterKey::Shard(address))
            .unwrap_or(false)
    }
}

fn setup(env: &Env) -> (ReceiptShardClient<'static>, Address) {
    env.mock_all_auths();
    let router = env.register(TestRouter, ());
    let id = env.register(ReceiptShard, (router.clone(), 1u64, 201u64));
    TestRouterClient::new(env, &router).register_shard(&id);
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
    assert!(source.is_active());
    source.decommission();
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
fn destination_rejects_authenticated_unregistered_source() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let target = ReceiptShardClient::new(&env, &target_id);
    let attacker = Address::generate(&env);

    assert!(target
        .try_insert_migrated_batch(&attacker, &1, &record(&env, 9, 1))
        .is_err());
}

#[test]
fn sparse_migrated_expired_record_is_pruned_after_a_gap() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let target = ReceiptShardClient::new(&env, &target_id);
    source.anchor_batch(&3, &BytesN::from_array(&env, &[3u8; 32]), &1, &0, &1);
    source.consolidate(&target_id, &1, &2, &soroban_sdk::vec![&env, 3u64]);

    env.ledger()
        .with_mut(|ledger| ledger.sequence_number = crate::pruning::RETENTION_LEDGERS + 1);
    let pruner = Address::generate(&env);
    assert_eq!(target.prune_expired_receipts(&pruner, &1), 1);
    assert_eq!(
        target.get_shard_diagnostics().oldest_unpruned_batch_id,
        1,
        "a sparse leading gap must remain available for a future anchor"
    );
    assert_eq!(target.get_shard_diagnostics().live_batches, 0);
}

#[test]
fn diagnostics_count_retained_sparse_migrated_records() {
    let env = Env::default();
    let (source, _router) = setup(&env);
    let target_id = env.register(ReceiptShard, (source.get_router(), 1u64, 201u64));
    let target = ReceiptShardClient::new(&env, &target_id);
    source.anchor_batch(&3, &BytesN::from_array(&env, &[4u8; 32]), &1, &0, &1);
    source.consolidate(&target_id, &1, &2, &soroban_sdk::vec![&env, 3u64]);

    assert_eq!(target.get_shard_diagnostics().active_dispute_count, 1);
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
