#![cfg(test)]

//! Before-initialisation coverage for `ReceiptShard::require_active_and_router` (issue #677).
//!
//! When `DataKey::Router` is missing from instance storage (i.e. before the contract
//! is initialised), `require_active_and_router` returns `Error::NotInitialized` rather
//! than aborting the host call with an untyped trap.

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env};

fn shard_with_key_removed(
    remove: impl FnOnce(&Env, &Address),
) -> (Env, Address, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(
        ReceiptShard,
        (router, 1u64, 1001u64, Address::generate(&env)),
    );
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.as_contract(&contract_id, || remove(&env, &contract_id));
    (env, contract_id, client)
}

#[test]
fn require_active_and_router_before_initialisation_returns_not_initialized() {
    let (env, contract_id, _client) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::Router);
        });
    });

    let res = env.as_contract(&contract_id, || {
        ReceiptShard::require_active_and_router(&env)
    });

    assert_eq!(
        res,
        Err(Error::NotInitialized),
        "require_active_and_router without Router must return NotInitialized"
    );
}

#[test]
fn consolidate_before_initialisation_returns_typed_error() {
    let (env, _contract_id, client) = shard_with_key_removed(|env, contract_id| {
        env.as_contract(contract_id, || {
            env.storage().instance().remove(&DataKey::Router);
        });
    });

    let dest = Address::generate(&env);
    let batch_ids = vec![&env, 1u64];
    assert_eq!(
        client.try_consolidate(&dest, &1u64, &2u64, &batch_ids),
        Err(Ok(Error::NotInitialized)),
        "consolidate before initialisation must return typed NotInitialized error"
    );
}
