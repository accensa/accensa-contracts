//! Root buffers evict oldest-first per shard (issue #952).
//!
//! Two shards fill independently to capacity with their own roots in
//! first-anchored-first-returned order. Anchoring 50 more roots in one shard
//! leaves exactly 100 entries, newest last, with the 50 oldest evicted; the
//! untouched shard keeps its original 100 roots in order. The full buffer is
//! compared element by element, so a shared key, a missing eviction, or a
//! back-eviction all fail loudly.

use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec};

/// The two logical shards driven past the cap.
const SHARD_A: u64 = 0;
const SHARD_B: u64 = 1;

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

/// Counter-derived root: byte 0 tags the shard so cross-shard leakage is
/// detectable, bytes 1..3 carry the anchor counter so the expected order is
/// explicit. Consecutive roots always differ, so the duplicate guard (which
/// compares only the latest batch) never fires.
fn root(env: &Env, shard: u8, i: u16) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[0] = shard;
    bytes[1..3].copy_from_slice(&i.to_be_bytes());
    BytesN::from_array(env, &bytes)
}

fn expected_buffer(env: &Env, shard: u8, range: std::ops::Range<u16>) -> Vec<BytesN<32>> {
    let mut expected = Vec::new(env);
    for i in range {
        expected.push_back(root(env, shard, i));
    }
    expected
}

#[test]
fn root_buffers_evict_oldest_first_per_shard() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let cap = anchor.get_root_buffer_size();
    assert_eq!(cap, 100);

    // Fill both shards independently to capacity.
    for i in 0..cap {
        let batch_a = anchor.anchor_batch(&SHARD_A, &root(&env, 0, i as u16), &1, &0, &1);
        let batch_b = anchor.anchor_batch(&SHARD_B, &root(&env, 1, i as u16), &1, &0, &1);
        assert_eq!(batch_a, (i + 1) as u64);
        assert_eq!(batch_b, (i + 1) as u64);
    }

    // Each shard reports its own 100 roots, first-anchored first.
    assert_eq!(
        anchor.get_root_buffer(&SHARD_A),
        expected_buffer(&env, 0, 0..100)
    );
    assert_eq!(
        anchor.get_root_buffer(&SHARD_B),
        expected_buffer(&env, 1, 0..100)
    );

    // Anchor 50 more in shard A only.
    for i in cap..cap + 50 {
        let batch_a = anchor.anchor_batch(&SHARD_A, &root(&env, 0, i as u16), &1, &0, &1);
        assert_eq!(batch_a, (i + 1) as u64);
    }

    // Shard A holds exactly 100 entries: the 50 oldest evicted, newest last.
    let buffer_a = anchor.get_root_buffer(&SHARD_A);
    assert_eq!(buffer_a.len(), cap);
    assert_eq!(buffer_a, expected_buffer(&env, 0, 50..150));

    // Shard B is untouched: still its original 100 roots in order.
    assert_eq!(
        anchor.get_root_buffer(&SHARD_B),
        expected_buffer(&env, 1, 0..100)
    );
}
