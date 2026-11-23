#![cfg(test)]

//! Tests for `ReceiptShard::insert_migrated_batch` error handling when
//! shard instance bounds are not initialized.

use accensa_common::Error;
use receipt_shard::{BatchRecord, DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, BytesN, Env};

fn dummy_record(env: &Env) -> BatchRecord {
    BatchRecord {
        root: BytesN::from_array(env, &[1u8; 32]),
        count: 10,
        period_start: 1,
        period_end: 2,
        anchored_ledger: env.ledger().sequence(),
    }
}

#[test]
fn insert_migrated_batch_missing_start_batch_id_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let source = Address::generate(&env);
    let contract_id = env.register(
        ReceiptShard,
        (router, 1u64, 1001u64, Address::generate(&env)),
    );
    let client = ReceiptShardClient::new(&env, &contract_id);

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::StartBatchId);
    });

    let res = client.try_insert_migrated_batch(&source, &10u64, &dummy_record(&env));
    assert_eq!(
        res,
        Err(Ok(Error::NotInitialized)),
        "missing StartBatchId must return NotInitialized, not a host trap"
    );
}

#[test]
fn insert_migrated_batch_missing_end_batch_id_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let source = Address::generate(&env);
    let contract_id = env.register(
        ReceiptShard,
        (router, 1u64, 1001u64, Address::generate(&env)),
    );
    let client = ReceiptShardClient::new(&env, &contract_id);

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::EndBatchId);
    });

    let res = client.try_insert_migrated_batch(&source, &10u64, &dummy_record(&env));
    assert_eq!(
        res,
        Err(Ok(Error::NotInitialized)),
        "missing EndBatchId must return NotInitialized, not a host trap"
    );
}
