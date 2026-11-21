//! #997 — pin how `prune_batches` treats ids with no stored batch.
//!
//! The shard's pruning cursor is a contiguous prefix, but "no batch stored at
//! this id" is not "nothing to prune": an id that was never anchored (or was
//! already consumed) still consumes one unit of `max_batches` and advances the
//! cursor, even though no record and no diagnostics counters move. These tests
//! pin that accounting with an unanchored id sitting *between* two anchored
//! ones, so a future change cannot silently make absent ids free cursor
//! movement or phantom deletions.

#![cfg(test)]

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

const START: u64 = 1;
const END: u64 = 5;

fn setup(env: &Env) -> ReceiptShardClient<'static> {
    env.mock_all_auths();
    let id = env.register(
        ReceiptShard,
        (Address::generate(env), START, END, Address::generate(env)),
    );
    ReceiptShardClient::new(env, &id)
}

fn anchor_at(env: &Env, client: &ReceiptShardClient, batch_id: u64, count: u32) {
    env.ledger().with_mut(|li| li.sequence_number = 100);
    let root = BytesN::from_array(env, &[batch_id as u8; 32]);
    client.anchor_batch(&batch_id, &root, &count, &0, &10);
}

fn stored_cursor(env: &Env, client: &ReceiptShardClient) -> u64 {
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&DataKey::PrunedUpTo)
            .expect("PrunedUpTo is set by the constructor")
    })
}

#[test]
fn absent_id_consumes_budget_and_is_skipped_not_counted_as_removal() {
    let env = Env::default();
    let client = setup(&env);

    anchor_at(&env, &client, 1, 3);
    // Batch id 2 is deliberately never anchored.
    anchor_at(&env, &client, 3, 4);
    anchor_at(&env, &client, 4, 1);

    let before = client.get_shard_diagnostics();
    assert_eq!(before.live_batches, 3);
    assert_eq!(before.live_leaves, 8);
    assert_eq!(stored_cursor(&env, &client), START);

    // `max_batches = 2` is exhausted by batch 1 (deleted) plus the absent id 2
    // (walked over, not deleted): the cursor stops at 3, before batch 3.
    assert_eq!(client.prune_batches(&101, &2, &END), (3, 2));

    assert_eq!(client.try_get_batch(&1), Err(Ok(Error::BatchNotFound)));
    assert_eq!(client.try_get_batch(&2), Err(Ok(Error::BatchNotFound)));
    assert_eq!(client.get_batch(&3).count, 4, "batch 3 must be untouched");
    assert_eq!(client.get_batch(&4).count, 1, "batch 4 must be untouched");
    assert_eq!(
        stored_cursor(&env, &client),
        3,
        "the cursor must advance over the absent id"
    );

    // Only batch 1 was a real removal: the absent id never reaches the
    // diagnostics counters and is not a phantom deletion.
    let after = client.get_shard_diagnostics();
    assert_eq!(after.live_batches, before.live_batches - 1);
    assert_eq!(after.live_leaves, before.live_leaves - 3);
    assert_eq!(after.total_leaves, before.total_leaves);

    // The next call runs entirely over present records and returns
    // `(cursor, pruned)` with one deletion per id.
    assert_eq!(client.prune_batches(&101, &10, &END), (5, 2));
    assert_eq!(client.try_get_batch(&3), Err(Ok(Error::BatchNotFound)));
    assert_eq!(client.try_get_batch(&4), Err(Ok(Error::BatchNotFound)));
    assert_eq!(stored_cursor(&env, &client), END);

    // A drained shard is an empty no-op: no cursor movement, no deletions.
    assert_eq!(client.prune_batches(&101, &10, &END), (END, 0));
}
