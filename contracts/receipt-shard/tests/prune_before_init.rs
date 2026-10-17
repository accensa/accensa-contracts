#![cfg(test)]

//! Before-initialisation coverage for `ReceiptShard::prune_batches` (issue #679).
//!
//! `prune_batches` read its three construction-time instance-storage keys —
//! `Router`, `EndBatchId` and `PrunedUpTo` — with `.unwrap()`, so a caller
//! reaching the shard before it was constructed (or after the instance entry
//! had aged out) aborted the host call with a `WasmRuntimeError` instead of
//! reporting the missing state. These tests drop each key in turn and pin the
//! typed `Error::NotInitialized` the rewritten reads now answer with.

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

/// Registers a shard, runs `remove` against its instance storage, and hands
/// back the client. The constructor runs atomically with registration, so
/// removing a construction-time key models exactly the state a caller
/// observes before that key's write has landed (or after the instance entry
/// itself has aged out): the read sees `None`.
fn shard_with_key_removed(
    remove: impl FnOnce(&Env, &Address),
) -> (Env, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.as_contract(&contract_id, || remove(&env, &contract_id));
    (env, client)
}

/// A `prune_batches` call that reaches the function with no `DataKey::Router`
/// in instance storage — a caller reaching the contract before it is
/// initialised — yields the crate's typed error, not a host abort.
#[test]
fn prune_batches_before_initialisation_returns_typed_error() {
    let (_env, client) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::Router);
        });
    });
    assert_eq!(
        client.try_prune_batches(&1_000_000u32, &10u32, &1u64),
        Err(Ok(Error::NotInitialized)),
        "a missing Router must surface as NotInitialized, not a host abort"
    );
}

/// Second read site in `prune_batches`: with the router present but the
/// shard's assigned range gone, the former `EndBatchId` unwrap answers the
/// same typed error.
#[test]
fn prune_batches_missing_end_batch_id_returns_typed_error() {
    let (_env, client) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::EndBatchId);
        });
    });
    assert_eq!(
        client.try_prune_batches(&1_000_000u32, &10u32, &1u64),
        Err(Ok(Error::NotInitialized)),
        "a missing EndBatchId must surface as NotInitialized, not a host abort"
    );
}

/// Third read site in `prune_batches`: with router and range present but the
/// prune cursor gone, the former `PrunedUpTo` unwrap answers the same typed
/// error rather than reaching `unwrap` on the cursor.
#[test]
fn prune_batches_missing_pruned_up_to_returns_typed_error() {
    let (_env, client) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::PrunedUpTo);
        });
    });
    assert_eq!(
        client.try_prune_batches(&1_000_000u32, &10u32, &1u64),
        Err(Ok(Error::NotInitialized)),
        "a missing PrunedUpTo must surface as NotInitialized, not a host abort"
    );
}
