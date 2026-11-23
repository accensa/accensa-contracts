#![cfg(test)]

//! Absent-state coverage for `ReceiptShard::consolidate`.
//!
//! A `consolidate` call for a batch ID whose persistent storage entry has not
//! been initialized yields `Error::BatchNotFound` rather than aborting the host
//! call with a panic or `WasmRuntimeError`.

use accensa_common::Error;
use receipt_shard::{ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env};

#[test]
fn consolidate_missing_batch_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let destination = Address::generate(&env);
    let contract_id = env.register(
        ReceiptShard,
        (router, 1u64, 1001u64, Address::generate(&env)),
    );
    let client = ReceiptShardClient::new(&env, &contract_id);

    let res = client.try_consolidate(&destination, &1u64, &2u64, &vec![&env, 99u64]);
    assert_eq!(
        res,
        Err(Ok(Error::BatchNotFound)),
        "consolidating an uninitialized batch must return BatchNotFound, not abort"
    );
}
