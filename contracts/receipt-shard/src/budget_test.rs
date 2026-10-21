#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `ReceiptShard` (issue #599), using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`). The baselines measured for this crate are recorded on each
//! test below and in the comparison table.
//!
//! Unlike `contracts/receipt-anchor/src/budget_test.rs`, these gates hold no
//! router: the instance is deployed from `receipt_shard.wasm` and runs its own
//! `__constructor`, so the measured call is the shard entry point a client's
//! verification lands on after `ReceiptAnchor::verify_receipt` has routed it —
//! the path `docs/BENCHMARKS.md` does not cover.
//!
//! ## Shard-side vs router-side Merkle verification
//!
//! `docs/BENCHMARKS.md` tabulates the router's root-history path
//! (`verify_receipt_leaf`), which folds the same sorted-pair proof with the same
//! iterative pure-WASM SHA-256 loop but never crosses into a storage shard:
//!
//! | proof depth | router `verify_receipt_leaf` (BENCHMARKS.md) | shard `verify_receipt` (this suite) | shard vs the tabulated router number |
//! |---|---|---|---|
//! | 0  | 680,379 | 497,495 | -26.9% |
//! | 1  | 892,836 | 637,364 | -28.6% |
//! | 2  | 1,105,293 | 777,817 | -29.6% |
//! | 5  | — (not tabulated) | 1,198,008 | — |
//! | 7  | 2,167,578 | 1,478,330 | -31.8% |
//! | 10 (`MAX_PROOF_LEN`) | 2,811,397 | 1,904,505 | -32.3% |
//!
//! Every shard-side number sits more than the 15% tolerance *below* the
//! tabulated router curve, so it is reported here rather than absorbed. The
//! curve, not the fold, is what moved: the doc records ~212k instructions per
//! proof level, while re-running its own
//! `measure_verify_receipt_leaf_by_depth` on this workspace (soroban-sdk
//! 27.0.6, optimized WASM, 2026-10-21) puts the router at 627,423 at depth 0
//! and 2,102,081 at depth 10 — ~147k per level. Measured against those
//! same-build router numbers the shard is 17.7% cheaper at depth 1 and 9.4% at
//! `MAX_PROOF_LEN`, inside the tolerance; against the checked-in table it is
//! not. Nothing here edits `docs/BENCHMARKS.md` or the router's gates: the
//! divergence and the reproduction numbers belong in the PR body.
//!
//! Per-level cost is flat in both directions: shard memory grows by exactly 24
//! bytes per level (1,239,672 at depth 0 to 1,239,912 at depth 10), and CPU is
//! linear at ~140,701 instructions per level, so the `MAX_PROOF_LEN` gate is
//! the only one that can be approached by a deeper proof.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p receipt-shard --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`; bring `std` in explicitly, matching the
// receipt-anchor and refund-policy-vdf suites.
extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{testutils::Address as _, vec, Address, Bytes, BytesN, Env, Vec};

/// Batch id anchored into this shard by the gates below. Any id inside the
/// shard's `[start, end)` range behaves identically — the record key is a
/// `u64` and the fold does not read it.
const BATCH_ID: u64 = 1;

/// The `[start_batch_id, end_batch_id)` bounds the shard is constructed with.
/// A real router assigns a slice `SHARD_CAPACITY` wide (the router's private
/// `SHARD_CAPACITY` is 200, `contracts/receipt-anchor/src/lib.rs:351`, and
/// `shard_index * SHARD_CAPACITY + 1` starts it); this suite uses the wider
/// `[1, 1001)` range that `contracts/receipt-shard/src/test.rs` constructs with,
/// so every batch id the gates below anchor falls inside the accepted window.
/// The bounds only gate whether an id is accepted — the verification fold never
/// reads them — so the measured cost is the same either way.
const START_BATCH_ID: u64 = 1;
const END_BATCH_ID: u64 = 1_001;

/// The router's `MAX_BATCH_SIZE` (1000, `contracts/receipt-anchor/src/lib.rs:308`):
/// the largest leaf count a shard can ever be asked to record, because the router
/// rejects a bigger batch before routing it. A `MAX_PROOF_LEN` tree is 1024 leaves,
/// so `anchor_tree` and the measurement below clamp the anchored count to this
/// value rather than anchoring an unreachable 1024-leaf batch. `anchor_batch`
/// stores `count` and `verify_receipt` never reads it, so the clamped anchor costs
/// the same as an unclamped one — `budget_verify_receipt_max_proof_len` measures
/// 1_904_505 either way.
const MAX_BATCH_SIZE: u32 = 1000;

/// Batches anchored before the measured `anchor_batch` call, so the gate covers
/// a populated shard: the diagnostics counters then read and rewrite an
/// existing `ShardStats` entry instead of creating one, and the persistent
/// batch map already has depth.
const POPULATE_BATCHES: u64 = 20;

fn load_wasm(env: &Env) -> Bytes {
    let buf = std::fs::read("../../target/wasm32v1-none/release/receipt_shard.wasm").expect(
        "receipt-shard wasm not found; run `cargo build -p receipt-shard \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Deploys the real `receipt_shard.wasm` — no `ReceiptAnchor` router — and runs
/// its constructor with the usual `(router, start, end)` arguments. `router` is
/// the address the shard gates every write on; `mock_all_auths` satisfies it for
/// the harness.
fn setup_shard(env: &Env) -> (ReceiptShardClient<'static>, Address) {
    // Same reasoning as the receipt-anchor suite: the harness uploads the ~54 KB
    // optimized shard WASM inside the same test "transaction" as the call it
    // measures, which no real invocation does, so per-transaction invocation
    // limits are disabled here. The gate this file is responsible for is the
    // `budget_cpu_lt` CPU assertion, plus the explicit `TX_MAX_*` checks in
    // `measure_verify_receipt_by_depth` below; neither relies on host
    // enforcement. Every test also points the budget at just the measured call
    // with `budget().reset_unlimited()`.
    env.cost_estimate().disable_resource_limits();
    env.mock_all_auths();

    let router = Address::generate(env);
    let wasm_hash = env.deployer().upload_contract_wasm(load_wasm(env));
    let id = env
        .deployer()
        .with_address(router.clone(), BytesN::from_array(env, &[0u8; 32]))
        .deploy_v2(wasm_hash, (router.clone(), START_BATCH_ID, END_BATCH_ID));
    (ReceiptShardClient::new(env, &id), router)
}

/// Anchor `n` batches with distinct roots, oldest ledger first, leaving the
/// shard populated.
fn populate(env: &Env, client: &ReceiptShardClient, n: u64) {
    for i in 0..n {
        client.anchor_batch(
            &(START_BATCH_ID + i),
            &BytesN::from_array(env, &[(i as u8) + 1; 32]),
            &1,
            &0,
            &100,
        );
    }
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

/// Builds a `2^depth`-leaf sorted-pair Merkle tree and returns the root plus
/// the proof for leaf 0, exactly as the router suite does, so the two curves
/// measure the same fold.
fn build_tree(env: &Env, depth: u32) -> (BytesN<32>, Vec<BytesN<32>>) {
    let n = 1usize << depth;
    let mut layer: Vec<BytesN<32>> = Vec::new(env);
    for i in 0..n {
        layer.push_back(BytesN::from_array(env, &[i as u8; 32]));
    }
    let mut proof = vec![env];
    let mut idx = 0usize;
    while layer.len() > 1 {
        let sibling = if idx.is_multiple_of(2) {
            idx + 1
        } else {
            idx - 1
        };
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

/// Anchor a `2^depth`-leaf tree as `BATCH_ID` and return `(root, proof, leaf)`
/// for the verification gates. The anchored count is the leaf count clamped to
/// `MAX_BATCH_SIZE`, matching what the router suite does for its `MAX_PROOF_LEN`
/// tree.
fn anchor_tree(
    env: &Env,
    client: &ReceiptShardClient,
    depth: u32,
) -> (BytesN<32>, Vec<BytesN<32>>, BytesN<32>) {
    let (root, proof) = build_tree(env, depth);
    let leaf = BytesN::from_array(env, &[0u8; 32]);
    client.anchor_batch(
        &BATCH_ID,
        &root,
        &(1u32 << depth).min(MAX_BATCH_SIZE),
        &0,
        &100,
    );
    (root, proof, leaf)
}

// ── verify_receipt: the proof-length curve, shard-side ──────────────────────

// Baseline 637_364 (measured 2026-10-21, shard-side depth-1 proof) * 1.15.
#[test]
#[budget_cpu_lt(733_000)]
fn budget_verify_receipt_depth_1() {
    let env = Env::default();
    let (client, _) = setup_shard(&env);
    let (_root, proof, leaf) = anchor_tree(&env, &client, 1);
    env.cost_estimate().budget().reset_unlimited();
    assert!(client.verify_receipt(&BATCH_ID, &leaf, &proof));
}

// Baseline 1_198_008 (measured 2026-10-21, shard-side depth-5 proof) * 1.15.
#[test]
#[budget_cpu_lt(1_378_000)]
fn budget_verify_receipt_depth_5() {
    let env = Env::default();
    let (client, _) = setup_shard(&env);
    let (_root, proof, leaf) = anchor_tree(&env, &client, 5);
    env.cost_estimate().budget().reset_unlimited();
    assert!(client.verify_receipt(&BATCH_ID, &leaf, &proof));
}

// Baseline 1_904_505 (measured 2026-10-21, shard-side `MAX_PROOF_LEN` proof,
// 10 elements / 1024 leaves) * 1.15.
#[test]
#[budget_cpu_lt(2_190_000)]
fn budget_verify_receipt_max_proof_len() {
    let env = Env::default();
    let (client, _) = setup_shard(&env);
    let (_root, proof, leaf) = anchor_tree(&env, &client, MAX_PROOF_LEN);
    assert_eq!(proof.len(), MAX_PROOF_LEN);
    env.cost_estimate().budget().reset_unlimited();
    assert!(client.verify_receipt(&BATCH_ID, &leaf, &proof));
}

// ── anchor_batch on a populated shard ───────────────────────────────────────

// Baseline 771_804 (measured 2026-10-21, `anchor_batch` into a shard already
// holding 20 batches) * 1.15.
#[test]
#[budget_cpu_lt(888_000)]
fn budget_anchor_batch_populated_shard() {
    let env = Env::default();
    let (client, _) = setup_shard(&env);
    populate(&env, &client, POPULATE_BATCHES);
    env.cost_estimate().budget().reset_unlimited();
    client.anchor_batch(
        &(START_BATCH_ID + POPULATE_BATCHES),
        &BytesN::from_array(&env, &[0xAA; 32]),
        &1,
        &0,
        &100,
    );
}

// ── Measurement ─────────────────────────────────────────────────────────────

/// Soroban per-transaction limits (mainnet network config), same constants the
/// router suite uses.
const TX_MAX_CPU_INSTRUCTIONS: u64 = 100_000_000;
const TX_MAX_MEMORY_BYTES: u64 = 41_943_040;

/// Measures shard-side `verify_receipt` CPU and memory at every proof depth up
/// to `MAX_PROOF_LEN` and prints them, so the numbers behind the gates above —
/// and their divergence from the `docs/BENCHMARKS.md` router curve — are
/// reproducible without editing a threshold. Fails if the worst case uses more
/// than 10% of either per-transaction limit, leaving room for the caller's own
/// work in the same transaction.
#[test]
fn measure_verify_receipt_by_depth() {
    for depth in 0..=MAX_PROOF_LEN {
        // Eleven envs, each embedding the shard WASM: skip the snapshot files.
        let env = Env::new_with_config(soroban_sdk::testutils::EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        let (client, _) = setup_shard(&env);
        let (root, proof) = if depth == 0 {
            let leaf = BytesN::from_array(&env, &[0u8; 32]);
            (leaf, vec![&env])
        } else {
            build_tree(&env, depth)
        };
        let leaf = BytesN::from_array(&env, &[0u8; 32]);
        client.anchor_batch(
            &BATCH_ID,
            &root,
            &(1u32 << depth).min(MAX_BATCH_SIZE),
            &0,
            &100,
        );

        env.cost_estimate().budget().reset_unlimited();
        assert!(client.verify_receipt(&BATCH_ID, &leaf, &proof));
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        let mem = env.cost_estimate().budget().memory_bytes_cost();
        std::println!("BENCHMARK receipt_shard verify_receipt depth={depth} cpu={cpu} mem={mem}");

        assert!(
            cpu * 10 < TX_MAX_CPU_INSTRUCTIONS,
            "depth {depth}: cpu {cpu}"
        );
        assert!(mem * 10 < TX_MAX_MEMORY_BYTES, "depth {depth}: mem {mem}");
    }
}
