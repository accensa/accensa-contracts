#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `ReceiptAnchor` (via the `ReceiptShard` it
//! deploys), using Tollcraft's `soroban-budget-assert` `#[budget_cpu_lt(N)]`
//! macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`). The measured baselines live in `docs/BENCHMARKS.md`; update
//! them deliberately after re-measuring, never automatically.
//!
//! The contracts must be compiled to WASM first:
//! `cargo build -p receipt-anchor -p receipt-shard --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`/`std::vec::Vec`; bring `std` in explicitly.
extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env, Vec,
};

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "contract wasm not found; run `cargo build -p receipt-anchor -p receipt-shard \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Deploys the real `receipt_anchor` and `receipt_shard` WASM and initializes the
/// router with the uploaded shard wasm hash, so the scaling entry points behave
/// exactly as they do on-chain.
fn setup_router(env: &Env) -> (ReceiptAnchorClient<'static>, Address) {
    let shard_wasm = load_wasm(env, "../../target/wasm32v1-none/release/receipt_shard.wasm");
    let anchor_wasm = load_wasm(
        env,
        "../../target/wasm32v1-none/release/receipt_anchor.wasm",
    );
    #[allow(deprecated)]
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm);
    #[allow(deprecated)]
    let id = env.register_contract_wasm(None, anchor_wasm);
    let client = ReceiptAnchorClient::new(env, &id);
    env.mock_all_auths();
    let merchant = Address::generate(env);
    client.initialize(&merchant, &shard_wasm_hash);
    (client, merchant)
}

fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() {
        (a.to_array(), b.to_array())
    } else {
        (b.to_array(), a.to_array())
    };
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(&lo);
    combined[32..].copy_from_slice(&hi);
    BytesN::from_array(
        env,
        &env.crypto()
            .sha256(&Bytes::from_slice(env, &combined))
            .to_array(),
    )
}

/// Builds a `2^depth`-leaf sorted-pair Merkle tree and returns the root plus the
/// proof for leaf 0. `count` is passed separately to `anchor_batch` and is not
/// validated against the tree, so we can anchor a depth-10 (1024-leaf) proof
/// under `count = MAX_BATCH_SIZE = 1000`.
fn build_tree(env: &Env, depth: u32) -> (BytesN<32>, Vec<BytesN<32>>) {
    let n = 1usize << depth;
    let mut layer: Vec<BytesN<32>> = Vec::new(env);
    for i in 0..n {
        layer.push_back(BytesN::from_array(env, &[i as u8; 32]));
    }
    let mut proof = vec![env];
    let mut idx = 0usize;
    while layer.len() > 1 {
        let sibling = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        proof.push_back(layer.get(sibling as u32).unwrap());
        let mut next: Vec<BytesN<32>> = Vec::new(env);
        let mut i = 0;
        while i < layer.len() as usize {
            next.push_back(hash_pair(
                env,
                &layer.get(i as u32).unwrap(),
                &layer.get(i as u32 + 1).unwrap(),
            ));
            i += 2;
        }
        idx /= 2;
        layer = next;
    }
    (layer.get(0).unwrap(), proof)
}

#[test]
#[budget_cpu_lt(2_780_000)]
fn budget_anchor_batch_count_1() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    env.cost_estimate().budget().reset_unlimited();
    client.anchor_batch(&root, &1, &0, &10);
}

#[test]
#[budget_cpu_lt(2_780_000)]
fn budget_anchor_batch_count_500() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    let root = BytesN::from_array(&env, &[2u8; 32]);
    env.cost_estimate().budget().reset_unlimited();
    client.anchor_batch(&root, &500, &0, &10);
}

#[test]
#[budget_cpu_lt(2_780_000)]
fn budget_anchor_batch_count_1000() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    let root = BytesN::from_array(&env, &[3u8; 32]);
    env.cost_estimate().budget().reset_unlimited();
    client.anchor_batch(&root, &MAX_BATCH_SIZE, &0, &10);
}

#[test]
#[budget_cpu_lt(1_473_000)]
fn budget_verify_receipt_depth_1() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    let (root, proof) = build_tree(&env, 1);
    let leaf = BytesN::from_array(&env, &[0u8; 32]);
    let batch_id = client.anchor_batch(&root, &2, &0, &100);
    env.cost_estimate().budget().reset_unlimited();
    assert!(client.verify_receipt(&batch_id, &leaf, &proof));
}

#[test]
#[budget_cpu_lt(3_621_000)]
fn budget_verify_receipt_depth_10() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    let (root, proof) = build_tree(&env, 10);
    let leaf = BytesN::from_array(&env, &[0u8; 32]);
    // A 1024-leaf tree yields a 10-element proof; anchor it under the maximum
    // batch size to prove the worst-case `verify_receipt` path.
    let batch_id = client.anchor_batch(&root, &MAX_BATCH_SIZE, &0, &100);
    env.cost_estimate().budget().reset_unlimited();
    assert!(client.verify_receipt(&batch_id, &leaf, &proof));
}

#[test]
#[budget_cpu_lt(22_212_000)]
fn budget_prune_batches_100() {
    let env = Env::default();
    let (client, _) = setup_router(&env);
    // Anchor 100 batches into shard 0 (SHARD_CAPACITY = 200), all old. Each
    // anchor needs a distinct root (DuplicateRoot = 103 otherwise).
    env.ledger().with_mut(|li| li.sequence_number = 100);
    for i in 0..100u8 {
        client.anchor_batch(&BytesN::from_array(&env, &[i; 32]), &1, &0, &1);
    }
    // Jump the ledger far forward and delete up to MAX_PRUNE_BATCHES (100).
    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);
    env.cost_estimate().budget().reset_unlimited();
    client.prune_batches(&500_000);
}
