//! Integration tests for ReceiptsPruned event emission behavior (issue #993).

#![cfg(test)]

use receipt_shard::pruning::{PRUNE_BOUNTY_PER_BATCH, RETENTION_LEDGERS};
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    Address, BytesN, Env,
};

const BASE_LEDGER: u32 = 100_000;

struct Fixture {
    env: Env,
    client: ReceiptShardClient<'static>,
    pruner: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let pruner = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);
    Fixture {
        env,
        client,
        pruner,
    }
}

fn anchor_batch(fx: &Fixture, batch_id: u64) {
    let root = BytesN::from_array(&fx.env, &[1u8; 32]);
    fx.client.anchor_batch(&batch_id, &root, &1, &0, &100);
}

#[test]
fn test_noop_prune_emits_no_event_and_keeps_state() {
    let fx = setup();
    // No batches are anchored, so pruning removes 0 records.
    let pruned = fx.client.prune_expired_receipts(&fx.pruner, &10);
    assert_eq!(pruned, 0);

    // Assert that no event was emitted.
    let events = fx.env.events().all();
    assert_eq!(events.events().len(), 0);

    // Bounty and cursor remain untouched.
    let bounty: i128 = fx.env.as_contract(&fx.client.address, || {
        fx.env
            .storage()
            .instance()
            .get(&DataKey::PruneBounty(fx.pruner.clone()))
            .unwrap_or(0)
    });
    assert_eq!(bounty, 0);
}

#[test]
fn test_pruning_batches_emits_single_event_with_exact_counts() {
    let fx = setup();
    // Anchor 2 batches at BASE_LEDGER
    anchor_batch(&fx, 1);
    anchor_batch(&fx, 2);

    // Advance ledger past retention window
    fx.env
        .ledger()
        .with_mut(|li| li.sequence_number = BASE_LEDGER + RETENTION_LEDGERS + 10);

    let pruned = fx.client.prune_expired_receipts(&fx.pruner, &10);
    assert_eq!(pruned, 2);

    let events = fx.env.events().all();
    assert_eq!(events.events().len(), 1);

    // Check bounty accrued
    let bounty: i128 = fx.env.as_contract(&fx.client.address, || {
        fx.env
            .storage()
            .instance()
            .get(&DataKey::PruneBounty(fx.pruner.clone()))
            .unwrap_or(0)
    });
    assert_eq!(bounty, PRUNE_BOUNTY_PER_BATCH * 2);
}
