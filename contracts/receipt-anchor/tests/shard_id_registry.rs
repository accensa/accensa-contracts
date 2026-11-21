//! #954 — the shard registry is a set, and the duplicate check has to be
//! exercised with an actual repeat.
//!
//! `register_shard_id` appends `shard_id` to `DataKey::ShardIds` only when no
//! equal entry is already there, which keeps `get_shard_ids` duplicate-free and
//! in first-use order. Every successful `anchor_batch` calls it.
//!
//! `src/test.rs::test_get_shard_root_and_ids` checks first-use order for shards
//! 5 and 9, but anchors into each shard exactly once, so the `any` guard never
//! sees a repeat. Replace the guard with an unconditional `push_back` and that
//! test stays green while the registry grows a duplicate on every batch — an
//! indexer enumerating `get_shard_ids` would then walk one shard's stream twice
//! and double-count its roots.
//!
//! These tests anchor into the same shards again, with distinct roots so the
//! duplicate-root check on the batch itself never rejects the call.

use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env};

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup() -> (Env, ReceiptAnchorClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    anchor.set_bounty_asset(&token);

    (env, anchor)
}

/// A distinct 32-byte root per anchor, so no two batches in a shard share a
/// root and the duplicate-root guard never fires.
fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// Anchor a one-receipt batch of `root` into `shard`, expecting the shard's own
/// `expected_batch` to be assigned. Asserting the batch id keeps the anchor
/// honest: it has to have actually been written, not rejected.
fn anchor_into(anchor: &ReceiptAnchorClient, shard: u64, root: &BytesN<32>, expected_batch: u64) {
    assert_eq!(
        anchor.try_anchor_batch(&shard, root, &1, &0, &10),
        Ok(Ok(expected_batch)),
        "shard {shard} must accept its batch {expected_batch}"
    );
}

/// The registry holds each shard once, in first-use order, after every shard
/// has been anchored twice. This is the assertion an unconditional `push_back`
/// fails: the list would read `[5, 9, 5, 9]`.
#[test]
fn a_repeat_anchor_does_not_register_the_shard_again() {
    let (env, anchor) = setup();

    anchor_into(&anchor, 5, &root_at(&env, 1), 1);
    anchor_into(&anchor, 9, &root_at(&env, 2), 1);
    assert_eq!(anchor.get_shard_ids(), vec![&env, 5u64, 9u64]);

    // Repeats: second batch in each shard, different roots.
    anchor_into(&anchor, 5, &root_at(&env, 3), 2);
    anchor_into(&anchor, 9, &root_at(&env, 4), 2);

    assert_eq!(
        anchor.get_shard_ids(),
        vec![&env, 5u64, 9u64],
        "four anchors across two shards must still enumerate two shards, in \
         the order they were first used"
    );
    assert_eq!(anchor.get_shard_ids().len(), 2);
}

/// Many repeats behave like one repeat.
#[test]
fn many_repeats_keep_the_registry_the_same_size() {
    let (env, anchor) = setup();

    for i in 1..=5u8 {
        anchor_into(&anchor, 5, &root_at(&env, i), i as u64);
        anchor_into(&anchor, 9, &root_at(&env, 10 + i), i as u64);
    }

    assert_eq!(
        anchor.get_shard_ids(),
        vec![&env, 5u64, 9u64],
        "ten anchors must not grow the registry past the two shards they used"
    );
    // Both shards really did take all five batches.
    assert_eq!(anchor.get_batch_count(&5), 5);
    assert_eq!(anchor.get_batch_count(&9), 5);
}

/// A shard first used after the repeats is appended last, which is what pins
/// the order to first use rather than to numeric id.
#[test]
fn a_new_shard_is_appended_last() {
    let (env, anchor) = setup();

    anchor_into(&anchor, 9, &root_at(&env, 1), 1);
    anchor_into(&anchor, 5, &root_at(&env, 2), 1);
    anchor_into(&anchor, 9, &root_at(&env, 3), 2);

    assert_eq!(
        anchor.get_shard_ids(),
        vec![&env, 9u64, 5u64],
        "order is first-use, so the shard that anchored second stays second \
         even though its id is lower"
    );

    anchor_into(&anchor, 1, &root_at(&env, 4), 1);
    assert_eq!(
        anchor.get_shard_ids(),
        vec![&env, 9u64, 5u64, 1u64],
        "a shard nobody has used lands at the end"
    );
}

/// Anchoring the same root into two different shards registers both, because
/// the duplicate check is on the shard, not on the root.
#[test]
fn the_same_root_in_two_shards_registers_both() {
    let (env, anchor) = setup();
    let shared = root_at(&env, 8);

    anchor_into(&anchor, 5, &shared, 1);
    anchor_into(&anchor, 9, &shared, 1);

    assert_eq!(
        anchor.get_shard_ids(),
        vec![&env, 5u64, 9u64],
        "two shards sharing a root are still two shards"
    );
}
