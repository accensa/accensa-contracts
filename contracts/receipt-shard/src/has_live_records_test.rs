#![cfg(test)]

use crate::{BatchRecord, DataKey, Error, ReceiptShard};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn sample_batch_record(env: &Env) -> BatchRecord {
    BatchRecord {
        root: BytesN::from_array(env, &[1u8; 32]),
        count: 5,
        period_start: 100,
        period_end: 200,
        anchored_ledger: env.ledger().sequence(),
    }
}

#[test]
fn test_has_live_records_uninitialised_shard_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 5u64));

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::StartBatchId);
        env.storage().instance().remove(&DataKey::EndBatchId);
        assert_eq!(
            ReceiptShard::has_live_records(&env),
            Err(Error::NotInitialized),
            "uninitialised shard must return NotInitialized error arm, not trap"
        );
    });
}

#[test]
fn test_has_live_records_missing_end_batch_id_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 5u64));

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::EndBatchId);
        assert_eq!(
            ReceiptShard::has_live_records(&env),
            Err(Error::NotInitialized),
            "missing EndBatchId must return NotInitialized error arm, not trap"
        );
    });
}

#[test]
fn test_has_live_records_missing_start_batch_id_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 5u64));

    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::StartBatchId);
        assert_eq!(
            ReceiptShard::has_live_records(&env),
            Err(Error::NotInitialized),
            "missing StartBatchId must return NotInitialized error arm, not trap"
        );
    });
}

#[test]
fn test_has_live_records_empty_shard_returns_ok_false() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 5u64));

    env.as_contract(&contract_id, || {
        assert_eq!(
            ReceiptShard::has_live_records(&env),
            Ok(false),
            "initialized empty shard must return Ok(false)"
        );
    });
}

#[test]
fn test_has_live_records_with_batch_returns_ok_true() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 5u64));

    env.as_contract(&contract_id, || {
        let record = sample_batch_record(&env);
        env.storage().persistent().set(&DataKey::Batch(2), &record);
        assert_eq!(
            ReceiptShard::has_live_records(&env),
            Ok(true),
            "shard with persistent batch record must return Ok(true)"
        );
    });
}
