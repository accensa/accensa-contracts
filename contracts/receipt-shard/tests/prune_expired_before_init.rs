#![cfg(test)]

//! Before-initialisation coverage for `ReceiptShard::prune_expired_receipts`.
//!
//! `prune_expired_receipts` read its construction-time instance-storage keys —
//! `EndBatchId` and `PrunedUpTo` — with `.unwrap()`, so a caller reaching the
//! shard before it was constructed (or after the instance entry had aged out)
//! aborted the host call with a `WasmRuntimeError` instead of reporting the
//! missing state. These tests drop each key in turn and pin the typed
//! `Error::NotInitialized` the rewritten reads now answer with.

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

fn shard_with_key_removed(
    remove: impl FnOnce(&Env, &Address),
) -> (Env, ReceiptShardClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let bounty_asset = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64, bounty_asset));
    let client = ReceiptShardClient::new(&env, &contract_id);
    let pruner = Address::generate(&env);
    env.as_contract(&contract_id, || remove(&env, &contract_id));
    (env, client, pruner)
}

#[test]
fn prune_expired_receipts_missing_end_batch_id_returns_typed_error() {
    let (_env, client, pruner) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::EndBatchId);
        });
    });
    assert_eq!(
        client.try_prune_expired_receipts(&pruner, &10u32),
        Err(Ok(Error::NotInitialized)),
        "a missing EndBatchId must surface as NotInitialized, not a host abort"
    );
}

#[test]
fn prune_expired_receipts_missing_pruned_up_to_returns_typed_error() {
    let (_env, client, pruner) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::PrunedUpTo);
        });
    });
    assert_eq!(
        client.try_prune_expired_receipts(&pruner, &10u32),
        Err(Ok(Error::NotInitialized)),
        "a missing PrunedUpTo must surface as NotInitialized, not a host abort"
    );
}
