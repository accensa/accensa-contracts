//! #951 — root history is per shard, and both shards have to be non-empty.
//!
//! `verify_receipt_by_root` reads `DataKey::ShardRootBuffer(shard_id)`, so the
//! root a shard anchored is the only root that shard can verify against.
//! `src/test.rs::test_root_history_is_per_shard` proves the isolation with a
//! single anchor: it writes to shard 1 and queries shard 2, which has never
//! anchored. That read falls into the empty-buffer case, so the comparison loop
//! never runs against a populated foreign buffer. Drop `shard_id` from the key —
//! one shared buffer for every shard — and the foreign root is found and the
//! call answers `Ok(true)`; that bug passes the current test.
//!
//! Here both shards anchor first, so the cross-shard call has to walk a
//! populated buffer and still refuse the root it contains. Each batch is one
//! receipt (`count = 1`), which makes the leaf equal to the root and the proof
//! empty: the only thing left to decide the answer is which buffer is read.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env, Vec};

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// Two shards, both anchored, so neither query hits the empty-buffer case.
const SHARD_A: u64 = 1;
const SHARD_B: u64 = 2;

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

/// A distinct 32-byte root per anchor, so a buffer's contents name the call
/// that produced them.
fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// Anchor `root` as a one-receipt batch in `shard`. With `count = 1` the Merkle
/// tree is the single leaf, so the leaf the caller verifies is the root itself.
fn anchor_one(anchor: &ReceiptAnchorClient, shard: u64, root: &BytesN<32>) {
    assert_eq!(
        anchor.try_anchor_batch(&shard, root, &1, &0, &10),
        Ok(Ok(1)),
        "setup: shard {shard} must accept the batch"
    );
}

/// The empty sorted-pair proof that a single-leaf batch needs.
fn no_proof(env: &Env) -> Vec<BytesN<32>> {
    Vec::new(env)
}

/// A shard verifies the root it anchored.
#[test]
fn a_shard_verifies_its_own_root() {
    let (env, anchor) = setup();
    let root_a = root_at(&env, 1);
    let root_b = root_at(&env, 2);
    anchor_one(&anchor, SHARD_A, &root_a);
    anchor_one(&anchor, SHARD_B, &root_b);

    assert!(
        anchor.verify_receipt_by_root(&SHARD_A, &root_a, &root_a, &no_proof(&env)),
        "shard {SHARD_A} must verify the root it anchored"
    );
    assert!(
        anchor.verify_receipt_by_root(&SHARD_B, &root_b, &root_b, &no_proof(&env)),
        "shard {SHARD_B} must verify the root it anchored"
    );
}

/// The isolation, proven against a populated buffer: shard B holds root B, and
/// root A is not verifiable in shard B even though some shard anchored it.
/// The answer is the typed `RootNotFound`, not the boolean `Ok(false)` a
/// present-root-with-a-bad-proof would return.
#[test]
fn a_root_anchored_elsewhere_is_not_verifiable_here() {
    let (env, anchor) = setup();
    let root_a = root_at(&env, 3);
    let root_b = root_at(&env, 4);
    anchor_one(&anchor, SHARD_A, &root_a);
    anchor_one(&anchor, SHARD_B, &root_b);

    // Both buffers are populated before the cross-shard calls, so these reach
    // the branch the empty-buffer fallback used to skip.
    assert_eq!(anchor.get_root_buffer(&SHARD_A).len(), 1);
    assert_eq!(anchor.get_root_buffer(&SHARD_B).len(), 1);

    let cross_shard = [(SHARD_B, root_a.clone()), (SHARD_A, root_b.clone())];
    for (shard, foreign_root) in cross_shard {
        assert_eq!(
            anchor.try_verify_receipt_by_root(
                &shard,
                &foreign_root,
                &foreign_root,
                &no_proof(&env)
            ),
            Err(Ok(Error::RootNotFound)),
            "this root was anchored in the other shard, so shard {shard} must \
             report it as unknown rather than evaluate it"
        );
    }
}

/// Each shard's buffer carries its own history and nothing else: one entry, and
/// the two entries differ. This is the assertion a shard-independent key would
/// fail — both roots would land in one buffer, which would then report two.
#[test]
fn each_shard_buffer_holds_only_its_own_root() {
    let (env, anchor) = setup();
    let root_a = root_at(&env, 5);
    let root_b = root_at(&env, 6);
    anchor_one(&anchor, SHARD_A, &root_a);
    anchor_one(&anchor, SHARD_B, &root_b);

    assert_eq!(
        anchor.get_root_buffer(&SHARD_A),
        vec![&env, root_a.clone()],
        "shard {SHARD_A}'s history is exactly its own root"
    );
    assert_eq!(
        anchor.get_root_buffer(&SHARD_B),
        vec![&env, root_b.clone()],
        "shard {SHARD_B}'s history is exactly its own root"
    );
    assert_ne!(
        anchor.get_root_buffer(&SHARD_A),
        anchor.get_root_buffer(&SHARD_B),
        "the two buffers must not be the same list"
    );
}

/// A third shard that never anchored has no history at all, so nothing is
/// verifiable in it — including a root another shard holds.
#[test]
fn an_unused_shard_verifies_nothing() {
    let (env, anchor) = setup();
    let root_a = root_at(&env, 7);
    anchor_one(&anchor, SHARD_A, &root_a);
    const UNUSED_SHARD: u64 = 99;

    assert_eq!(anchor.get_root_buffer(&UNUSED_SHARD).len(), 0);
    assert_eq!(
        anchor.try_verify_receipt_by_root(&UNUSED_SHARD, &root_a, &root_a, &no_proof(&env)),
        Err(Ok(Error::RootNotFound)),
        "a shard with no batches cannot verify another shard's root"
    );
}
