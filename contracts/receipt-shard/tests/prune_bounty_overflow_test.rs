#![cfg(test)]

//! Boundary and overflow tests for guarded bounty accrual in `prune_expired_receipts` (issue #715).

use receipt_shard::pruning::{PRUNE_BOUNTY_PER_BATCH, RETENTION_LEDGERS};
use receipt_shard::{BatchRecord, DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, BytesN, Env};

const BASE_LEDGER: u32 = 100_000;

fn setup_shard() -> (Env, Address, Address, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let pruner = Address::generate(&env);
    let token_address = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64, token_address));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);
    (env, contract_id, pruner, client)
}

fn add_expired_batch(env: &Env, contract_id: &Address, batch_id: u64) {
    env.as_contract(contract_id, || {
        let record = BatchRecord {
            root: BytesN::from_array(env, &[1u8; 32]),
            count: 5,
            period_start: 1000,
            period_end: 2000,
            anchored_ledger: BASE_LEDGER,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Batch(batch_id), &record);
    });
}

#[test]
fn test_prune_bounty_accrues_using_checked_add() {
    let (env, contract_id, pruner, client) = setup_shard();
    add_expired_batch(&env, &contract_id, 1);

    // Advance past retention window
    env.ledger().with_mut(|li| {
        li.sequence_number = BASE_LEDGER + RETENTION_LEDGERS + 10;
    });

    let pruned = client.prune_expired_receipts(&pruner, &10u32);
    assert_eq!(pruned, 1);

    let accrued: i128 = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::PruneBounty(pruner.clone()))
            .unwrap_or(0)
    });
    assert_eq!(accrued, PRUNE_BOUNTY_PER_BATCH);
}

#[test]
#[should_panic(expected = "bounty accrual within i128 bounds")]
fn test_prune_bounty_overflow_at_i128_max_panics_with_explicit_message() {
    let (env, contract_id, pruner, client) = setup_shard();
    add_expired_batch(&env, &contract_id, 1);

    // Preset accrued bounty to i128::MAX so adding another batch's bounty overflows
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::PruneBounty(pruner.clone()), &i128::MAX);
    });

    // Advance past retention window
    env.ledger().with_mut(|li| {
        li.sequence_number = BASE_LEDGER + RETENTION_LEDGERS + 10;
    });

    // This must trigger add_amounts overflow check and explicit panic
    client.prune_expired_receipts(&pruner, &10u32);
}
