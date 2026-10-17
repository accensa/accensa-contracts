//! Client-level coverage for the continuous-anchoring API (issue #561).
//!
//! `incremental_merkle` unit-tests the frontier math; this suite pins the
//! contract entry points that expose it: leaf insertion and count, the root's
//! parity with the batch convention, the empty-tree error, and the
//! min-anchor-interval setter/getter pair.

extern crate std;

use super::{ReceiptAnchor, ReceiptAnchorClient};
use crate::merkle::{hash_pair, test_tree};
use accensa_common::Error;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};
use std::vec::Vec;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    client.initialize(&merchant, &shard_wasm_hash);
    (env, client, merchant)
}

/// Canonical batch root (ADR-001): sorted-pair SHA-256, duplicating the last
/// node at every odd level. This is the convention the app's SDK uses to build
/// the root that `anchor_batch` records.
fn batch_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level
            .chunks(2)
            .map(|pair| hash_pair(&pair[0], &pair[1]))
            .collect();
    }
    level[0]
}

#[test]
fn inserts_return_increasing_zero_based_indices_and_the_count_agrees() {
    let (env, client, _merchant) = setup();
    assert_eq!(client.get_incremental_leaf_count(), 0);

    for (expected_index, tag) in [1u8, 2, 3, 4].iter().enumerate() {
        let leaf = BytesN::from_array(&env, &[*tag; 32]);
        assert_eq!(client.insert_receipt_leaf(&leaf), expected_index as u64);
        assert_eq!(
            client.get_incremental_leaf_count(),
            expected_index as u64 + 1
        );
    }
}

#[test]
fn incremental_root_matches_the_batch_root_anchor_batch_accepts() {
    let (env, client, _merchant) = setup();

    let leaves = test_tree::leaves(5);
    for (expected_index, leaf) in leaves.iter().enumerate() {
        assert_eq!(
            client.insert_receipt_leaf(&BytesN::from_array(&env, leaf)),
            expected_index as u64
        );
    }

    let root = client.get_incremental_root();
    assert_eq!(root.to_array(), batch_root(&leaves));
    assert_eq!(client.anchor_batch(&0, &root, &5, &0, &100), 1);
}

#[test]
fn incremental_root_before_any_insert_is_root_not_found() {
    let (_env, client, _merchant) = setup();

    assert_eq!(client.get_incremental_leaf_count(), 0);
    assert_eq!(
        client.try_get_incremental_root(),
        Err(Ok(Error::RootNotFound))
    );
}

#[test]
fn min_anchor_interval_getter_returns_the_last_set_value() {
    let (_env, client, _merchant) = setup();

    assert_eq!(client.get_min_anchor_interval(), 0);
    client.set_min_anchor_interval(&90);
    assert_eq!(client.get_min_anchor_interval(), 90);
    client.set_min_anchor_interval(&30);
    assert_eq!(client.get_min_anchor_interval(), 30);
    client.set_min_anchor_interval(&0);
    assert_eq!(client.get_min_anchor_interval(), 0);
}
