//! Eviction drops only the by-root index (issue #950).
//!
//! Anchoring 101 roots evicts root 0 from the ring buffer: lookup by root
//! reports [`Error::RootNotFound`], but the batch path still verifies because
//! the `BatchRecord` stays in its shard. Re-anchoring the evicted root is
//! legal — the duplicate guard compares only the latest batch — and makes it
//! verifiable by root again, evicting the next-oldest root.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env};

/// Logical shard used by this single-stream test.
const DEFAULT_SHARD: u64 = 0;

struct TestEnv<'a> {
    env: Env,
    anchor: ReceiptAnchorClient<'a>,
    #[allow(dead_code)]
    merchant: Address,
}

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup<'a>() -> TestEnv<'a> {
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
    // Shards deploy with the router's configured bounty asset (issue #998).
    env.as_contract(&anchor_id, || {
        env.storage()
            .instance()
            .set(&receipt_anchor::DataKey::BountyAsset, &token);
    });

    TestEnv {
        env,
        anchor,
        merchant,
    }
}

fn root(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

#[test]
fn evicted_root_is_not_found_by_root_but_verifies_by_batch() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    // Single-leaf batches: root == leaf, empty proof.
    let cap = anchor.get_root_buffer_size();
    assert_eq!(cap, 100);
    for i in 0..=cap {
        let batch_id = anchor.anchor_batch(&DEFAULT_SHARD, &root(&env, i as u8), &1, &0, &1);
        assert_eq!(batch_id, (i + 1) as u64);
    }

    // By-root lookup: root 0 was evicted by the 101st anchor.
    let root0 = root(&env, 0);
    assert_eq!(
        anchor.try_verify_receipt_by_root(&DEFAULT_SHARD, &root0, &root0, &vec![&env]),
        Err(Ok(Error::RootNotFound))
    );

    // Batch path beside it: batch 1's record is untouched in its shard.
    assert_eq!(
        anchor.try_verify_receipt(&DEFAULT_SHARD, &1, &root0, &vec![&env]),
        Ok(Ok(true))
    );

    // Re-anchoring the evicted root is legal — the duplicate guard compares
    // only the latest batch (root 100) — and lands as a fresh batch.
    let batch_id = anchor.anchor_batch(&DEFAULT_SHARD, &root0, &1, &0, &1);
    assert_eq!(batch_id, (cap + 2) as u64);

    // The re-anchored root verifies by root again ...
    assert!(anchor.verify_receipt_by_root(&DEFAULT_SHARD, &root0, &root0, &vec![&env]));

    // ... the buffer holds exactly 100 entries, the next-oldest root (root 1)
    // was evicted, and the re-anchored root is newest-last.
    let buffer = anchor.get_root_buffer(&DEFAULT_SHARD);
    assert_eq!(buffer.len(), cap);
    assert_eq!(buffer.get(0).unwrap(), root(&env, 2));
    assert_eq!(buffer.get(cap - 1).unwrap(), root0);
    let root1 = root(&env, 1);
    assert_eq!(
        anchor.try_verify_receipt_by_root(&DEFAULT_SHARD, &root1, &root1, &vec![&env]),
        Err(Ok(Error::RootNotFound))
    );
}
