//! Merkle-depth bounds coverage for shard diagnostics (issue #990).

use receipt_shard::{ReceiptShard, ReceiptShardClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn anchor_case(count: u32, root_byte: u8) -> receipt_shard::diagnostics::ShardDiagnostics {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    let root = BytesN::from_array(&env, &[root_byte; 32]);
    client.anchor_batch(&1, &root, &count, &0, &100);
    client.get_shard_diagnostics()
}

#[test]
fn merkle_depth_is_pinned_at_key_bounds() {
    // (count, expected_depth, expected_consistent)
    let cases: &[(u32, u32, bool)] = &[
        (0, 0, true),
        (1, 0, true),
        (2, 1, true),
        (1000, 10, true),
        (2049, 12, false),
        (u32::MAX, 32, false),
    ];

    for (i, &(count, expected_depth, expected_consistent)) in cases.iter().enumerate() {
        let d = anchor_case(count, (i + 1) as u8);
        assert_eq!(d.max_merkle_depth, expected_depth, "count={count}");
        assert_eq!(d.consistent, expected_consistent, "count={count}");
    }
}
