#![cfg(test)]
//! Property-based fuzz tests for `ReceiptAnchor`.
//!
//! ## Approach
//!
//! These tests are *property tests over generated operation sequences*, not a
//! coverage-guided fuzzer. Everything runs inside the Soroban test environment,
//! where each "invocation" is a direct in-process call against a freshly
//! registered contract instance. There is no wasm, no fork, and no feedback
//! loop from execution to input generation, so a libFuzzer-style harness is
//! not applicable. Instead we use [`proptest`] with a seeded PRNG:
//!
//! - Each test case generates a random sequence of contract operations.
//! - The sequence is executed against a fresh `Env`, one operation at a time,
//!   advancing the simulated ledger between operations.
//! - After *every* operation we assert the invariants hold, so a violation is
//!   attributed to the exact operation that broke it.
//! - On failure proptest shrinks the sequence to a minimal counterexample and
//!   prints the seed, which we then reproduce as a permanent regression test
//!   (see the `regression` module at the bottom of this file).
//!
//! ## Budget knobs
//!
//! - `FUZZ_CASES` (default `64`) tunes the number of generated sequences.
//! - `FUZZ_SEQ_LEN` (default `48`) tunes the maximum length of each sequence.
//!
//! CI runs with the defaults (a bounded budget). For a longer local profile:
//!
//! ```sh
//! FUZZ_CASES=2000 FUZZ_SEQ_LEN=256 cargo test -p receipt-anchor -- --ignored
//! ```
//!
//! The `*_long` variants of each test are `#[ignore]`d and use larger budgets.
//!
//! ## Limits
//!
//! - Coverage is bounded by the random generator, not by any execution
//!   feedback: a state transition the generator never produces is never
//!   explored. The operation mix is weighted toward the interesting state
//!   (anchoring, pruning, verifying, TTL extension), but this is sampling, not
//!   exhaustive search.
//! - The ledger is advanced in bounded jumps so persistent entries never cross
//!   the archival threshold mid-sequence; real archival/restore behaviour is
//!   out of scope here (covered by the storage audit and unit tests).
//! - `MAX_PRUNE_BATCHES` (100) is rarely hit at CI budgets; the `*_long`
//!   profile with longer sequences exercises the per-call prune bound.
//! - Snapshot capture at `Env` drop is disabled: every generated case would
//!   otherwise emit a golden ledger-snapshot file, which is meaningless for
//!   random inputs and would flood the tree.
//! - Merkle "rejection" assertions rely on the cryptographic collision
//!   resistance of SHA-256: a mutated leaf/proof hashing to the anchored root
//!   is astronomically unlikely, so the probability of a false positive is
//!   negligible (the tests are deterministic given the seed).

extern crate std;

// Property-based fuzz tests for [`ReceiptAnchor::verify_receipt`] — the
// function the public verifier depends on and the most security-sensitive
// path in this repo.
//
// This file replaces the previous placeholder "fuzz" test, which asserted
// `count > 0` without ever touching the contract, and the empty
// `benchmark_gas_and_cpu_instructions` stub. Both carried names that claimed
// coverage the repository did not have.
//
// Every property builds a *real* Merkle tree over randomly generated leaves
// using the same sorted-pair SHA-256 construction `verify_receipt` implements
// (see [`hash_pair`] and [`build_merkle_tree`]), anchors the resulting root,
// and then exercises [`ReceiptAnchor::verify_receipt`]:
//
// * a valid proof for a randomly chosen leaf verifies as `true` across
//   randomised tree sizes (1..=`MAX_BATCH_SIZE` leaves, odd and even);
// * a proof with one corrupted sibling is rejected;
// * a truncated proof is rejected;
// * an empty proof is rejected against any multi-leaf root (it stays valid
//   for a single-leaf batch, where the leaf *is* the root);
// * a valid proof for a leaf of a different batch is rejected;
// * arbitrary caller-supplied proofs — including over-long ones — never
//   panic. A panic in `verify_receipt` would be a denial-of-service on the
//   public verifier. The proof lengths generated here are bounded so every
//   call stays inside the host resource budget; bounding the proof on the
//   contract side is tracked separately in accensa/accensa-contracts#96;
// * duplicate leaves exercise the equal-hash ordering branch of the
//   sorted-pair construction.
//
// The proptest configuration is pinned — fixed case count and fixed RNG
// seed, see [`fuzz_config`] — so the exact input sequence is identical on
// every run, including in `CI`. If a property ever fails, proptest prints
// the failing input and the same seed reproduces the same sequence locally.

use proptest::prelude::*;
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, EnvTestConfig, Ledger},
    Address, Bytes, BytesN, Env,
};
use std::{format, string::String, vec};

use super::{DataKey, Error, ReceiptAnchor, ReceiptAnchorClient};

/// Logical shard used by the fuzz model (single-stream). Cross-shard behavior
/// is covered by dedicated unit tests.
const DEFAULT_SHARD: u64 = 0;

/// The `ReceiptShard` wasm, built by `cargo build -p receipt-shard --target
/// wasm32v1-none --release` before these tests run (see
/// `.github/workflows/ci.yml` and the README's "Build and test" section).
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn shard_wasm_hash(env: &Env) -> BytesN<32> {
    env.deployer().upload_contract_wasm(shard_wasm::WASM)
}

/// Resolves the shard address holding `batch_id`, for tests that need to peek
/// at a shard's own storage (e.g. TTLs) directly.
fn shard_for(client: &ReceiptAnchorClient<'static>, batch_id: u64) -> Address {
    client.get_shard_address(&DEFAULT_SHARD, &((batch_id - 1) / super::SHARD_CAPACITY))
}

/// Bounded CI default budgets; override with `FUZZ_CASES` / `FUZZ_SEQ_LEN`.
fn fuzz_cases() -> u32 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn fuzz_seq_len() -> usize {
    std::env::var("FUZZ_SEQ_LEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48)
}

fn proptest_config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

/// An `Env` that does not write golden ledger snapshots on drop (see module
/// docs). Ledger snapshots are meaningless for generated inputs and would
/// create one file per case.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = test_env();
    env.mock_all_auths();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    client.initialize(&merchant, &shard_wasm_hash(&env));
    (env, client, merchant)
}

/// Converts a `std::vec::Vec<BytesN<32>>` into a `soroban_sdk::Vec` (the
/// latter has no `FromIterator` impl).
fn to_svec(env: &Env, items: std::vec::Vec<BytesN<32>>) -> soroban_sdk::Vec<BytesN<32>> {
    let mut out = soroban_sdk::vec![env];
    for item in items {
        out.push_back(item);
    }
    out
}

/// Sorted-pair SHA-256, mirroring the contract's `verify_receipt` folding so a
/// test-built root/proof is a genuine Merkle path.
fn hash_pair(env: &Env, a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(lo);
    combined[32..].copy_from_slice(hi);
    env.crypto()
        .sha256(&Bytes::from_slice(env, &combined))
        .to_array()
}

/// Deterministic 32-byte leaves from a seed (splitmix64), so proptest can
/// shrink a case to a single `u64` while still producing varied leaves.
fn leaves_from_seed(seed: u64, count: usize) -> std::vec::Vec<[u8; 32]> {
    let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
    let mut next = move || {
        s = s.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    let mut leaves = std::vec::Vec::with_capacity(count);
    for _ in 0..count {
        let hi = next();
        let lo = next();
        let mut leaf = [0u8; 32];
        leaf[..8].copy_from_slice(&hi.to_le_bytes());
        leaf[8..16].copy_from_slice(&lo.to_le_bytes());
        leaf[16..24].copy_from_slice(&next().to_le_bytes());
        leaf[24..32].copy_from_slice(&next().to_le_bytes());
        leaves.push(leaf);
    }
    leaves
}

/// Builds a balanced Merkle tree (last leaf duplicated to pad non-power-of-two
/// counts). Returns the root and, for each leaf, its sibling path.
fn build_tree(
    env: &Env,
    leaves: &[[u8; 32]],
) -> ([u8; 32], std::vec::Vec<std::vec::Vec<[u8; 32]>>) {
    let n = leaves.len();
    debug_assert!(n > 0);
    let mut width = 1;
    while width < n {
        width *= 2;
    }
    // Layer 0: padded leaves (last leaf duplicated to the next power of two).
    let mut levels: std::vec::Vec<std::vec::Vec<[u8; 32]>> = std::vec::Vec::new();
    let mut cur: std::vec::Vec<[u8; 32]> = leaves.to_vec();
    while cur.len() < width {
        cur.push(*leaves.last().unwrap());
    }
    levels.push(cur.clone());
    while cur.len() > 1 {
        let mut next = std::vec::Vec::with_capacity(cur.len() / 2);
        for pair in cur.as_chunks::<2>().0 {
            next.push(hash_pair(env, &pair[0], &pair[1]));
        }
        levels.push(next.clone());
        cur = next;
    }
    let root = levels.last().unwrap()[0];
    // Walk each original leaf's index up the stored levels to collect siblings.
    let mut proofs: std::vec::Vec<std::vec::Vec<[u8; 32]>> = vec![std::vec::Vec::new(); n];
    for (li, _) in leaves.iter().enumerate() {
        let mut idx = li;
        for level in &levels[..levels.len() - 1] {
            let sibling_idx = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
            proofs[li].push(level[sibling_idx]);
            idx /= 2;
        }
    }
    (root, proofs)
}

// ── Model ──────────────────────────────────────────────────────────────────

struct BatchModel {
    id: u64,
    root: [u8; 32],
    leaves: std::vec::Vec<[u8; 32]>,
    proofs: std::vec::Vec<std::vec::Vec<[u8; 32]>>,
    anchored_ledger: u32,
}

/// The test's own simulation of the contract's observable state.
struct Model {
    /// Number of successful `anchor_batch` calls (== `BatchCount`).
    anchors: u64,
    /// Anchored batches, oldest first. Pruned batches are removed.
    batches: std::vec::Vec<BatchModel>,
    /// The `PrunedUpTo` cursor, mirrored exactly from `prune_batches`.
    pruned_up_to: u64,
}

impl Model {
    fn new() -> Self {
        Model {
            anchors: 0,
            batches: std::vec::Vec::new(),
            pruned_up_to: 1,
        }
    }

    fn batch(&self, id: u64) -> Option<&BatchModel> {
        self.batches.iter().find(|b| b.id == id)
    }

    /// Mirrors `ReceiptAnchor::prune_batches`, including the `MAX_PRUNE_BATCHES`
    /// cap and the skip-missing behaviour.
    fn prune(&mut self, before_ledger: u32) {
        let mut pruned_count: u64 = 0;
        while self.pruned_up_to <= self.anchors && pruned_count < (super::MAX_PRUNE_BATCHES as u64)
        {
            let anchored = self.batch(self.pruned_up_to).map(|b| b.anchored_ledger);
            match anchored {
                Some(ledger) if ledger < before_ledger => {
                    let id = self.pruned_up_to;
                    self.batches.retain(|x| x.id != id);
                    self.pruned_up_to += 1;
                    pruned_count += 1;
                }
                Some(_) => break,
                None => {
                    // Not present (already pruned or skipped): advance past it.
                    self.pruned_up_to += 1;
                    pruned_count += 1;
                }
            }
        }
    }
}

// ── Generated operation sequences ──────────────────────────────────────────

#[derive(Clone, Debug)]
enum Op {
    /// Anchor a batch with `leaf_count` leaves (1..=8).
    Anchor { seed: u64, leaf_count: u32 },
    /// Prune batches anchored strictly before `before_ledger`.
    Prune { before_ledger: u32 },
    /// Advance the ledger by up to 50 ledgers.
    Advance { ledgers: u32 },
    /// Verify a receipt. `target` is resolved at execution time as
    /// `target % (batch_count + 1)`: 0 maps to a nonexistent batch id, 1..=n
    /// to the corresponding batch.
    Verify { target: u32 },
    /// Extend the TTL of a batch. `target` resolves like `Verify`.
    ExtendTtl { target: u32 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<u64>(), 1u32..=8).prop_map(|(seed, leaf_count)| Op::Anchor { seed, leaf_count }),
        any::<u32>().prop_map(|before_ledger| Op::Prune { before_ledger }),
        (1u32..=20u32).prop_map(|ledgers| Op::Advance { ledgers }),
        any::<u32>().prop_map(|target| Op::Verify { target }),
        any::<u32>().prop_map(|target| Op::ExtendTtl { target }),
    ]
}

fn execute(env: &Env, client: &ReceiptAnchorClient<'static>, ops: &[Op]) -> std::vec::Vec<String> {
    let mut model = Model::new();
    let mut failures: std::vec::Vec<String> = std::vec::Vec::new();
    let mut last_pruned = model.pruned_up_to;

    for op in ops {
        match op {
            Op::Advance { ledgers } => {
                env.ledger().with_mut(|li| li.sequence_number += ledgers);
            }
            Op::Anchor { seed, leaf_count } => {
                let leaves = leaves_from_seed(*seed, *leaf_count as usize);
                let (root, proofs) = build_tree(env, &leaves);
                let anchored_ledger = env.ledger().sequence();
                let id = client.anchor_batch(
                    &DEFAULT_SHARD,
                    &BytesN::from_array(env, &root),
                    leaf_count,
                    &0,
                    &100,
                );
                model.anchors += 1;
                assert_eq!(id, model.anchors, "batch ids must be sequential");
                let stored = client.get_batch(&DEFAULT_SHARD, &id);
                assert_eq!(
                    stored.root,
                    BytesN::from_array(env, &root),
                    "contract stored a different root than we anchored"
                );
                model.batches.push(BatchModel {
                    id,
                    root,
                    leaves,
                    proofs,
                    anchored_ledger,
                });
            }
            Op::Prune { before_ledger } => {
                client.prune_batches(&DEFAULT_SHARD, before_ledger);
                model.prune(*before_ledger);
            }
            Op::Verify { target } => {
                let n = model.anchors;
                let slot = if n == 0 { 0 } else { target % (n as u32 + 1) };
                let batch_id = slot as u64;
                if slot == 0 {
                    // Nonexistent batch id: must be BatchNotFound.
                    let leaf = BytesN::from_array(env, &[0u8; 32]);
                    let res = client.try_verify_receipt(
                        &DEFAULT_SHARD,
                        &(n + 999),
                        &leaf,
                        &soroban_sdk::vec![env],
                    );
                    if res != Err(Ok(Error::BatchNotFound)) {
                        failures.push(format!(
                            "verify against missing batch: expected BatchNotFound, got {res:?}"
                        ));
                    }
                } else if let Some(b) = model.batch(batch_id) {
                    // The sorted-pair convention means a proof carries no
                    // left/right position flags: hashing each pair in sorted
                    // order makes the leaf's side within a pair irrelevant.
                    // It does *not* make the level sequence of the proof
                    // irrelevant — reversing the sequence changes which
                    // siblings fold at which depth and must be rejected.
                    //
                    // For power-of-two trees we also build the tree from the
                    // mirror-reversed leaf array: the root is identical, but
                    // each leaf's sibling values differ, and the mirrored
                    // proofs must still verify against the same anchored root.
                    for (li, leaf) in b.leaves.iter().enumerate() {
                        let proof = b.proofs[li]
                            .iter()
                            .map(|s| BytesN::from_array(env, s))
                            .collect::<std::vec::Vec<_>>();
                        let proof_vec = to_svec(env, proof.clone());

                        // 1. Real leaf, real proof.
                        if !client.verify_receipt(
                            &DEFAULT_SHARD,
                            &batch_id,
                            &BytesN::from_array(env, leaf),
                            &proof_vec,
                        ) {
                            failures.push(format!(
                                "real leaf {li} of batch {batch_id} failed to verify"
                            ));
                        }

                        // 2. Mirrored tree: same root, different sibling
                        //    values, proofs must still verify (position
                        //    within a pair is irrelevant).
                        if b.leaves.len().is_power_of_two() && b.leaves.len() > 1 {
                            let mut mirrored = b.leaves.clone();
                            mirrored.reverse();
                            let (mirror_root, mirror_proofs) = build_tree(env, &mirrored);
                            if mirror_root != b.root {
                                failures.push(format!(
                                    "mirrored tree of batch {batch_id} produced a different root"
                                ));
                            }
                            let mirror_li = b.leaves.len() - 1 - li;
                            let mirror_proof = to_svec(
                                env,
                                mirror_proofs[mirror_li]
                                    .iter()
                                    .map(|s| BytesN::from_array(env, s))
                                    .collect::<std::vec::Vec<_>>(),
                            );
                            if !client.verify_receipt(
                                &DEFAULT_SHARD,
                                &batch_id,
                                &BytesN::from_array(env, leaf),
                                &mirror_proof,
                            ) {
                                failures.push(format!(
                                    "mirrored proof for leaf {li} of batch {batch_id} failed \
                                     (sorted-pair convention should make position irrelevant)"
                                ));
                            }
                        }

                        // 3. Reversed level sequence: must be rejected (the
                        //    convention does not make the level order
                        //    irrelevant). Depth-1 proofs (2-leaf trees) are
                        //    unaffected by reversal, so require depth >= 2.
                        if proof.len() > 1 {
                            let reversed: std::vec::Vec<_> = proof.iter().cloned().rev().collect();
                            let reversed_vec = to_svec(env, reversed);
                            if client.verify_receipt(
                                &DEFAULT_SHARD,
                                &batch_id,
                                &BytesN::from_array(env, leaf),
                                &reversed_vec,
                            ) {
                                failures.push(format!(
                                    "reversed level sequence for leaf {li} of batch {batch_id} \
                                     verified true (level order must matter)"
                                ));
                            }
                        }

                        // 4. Wrong leaf: flip a byte of the real leaf.
                        let mut wrong_leaf = *leaf;
                        wrong_leaf[0] ^= 0xFF;
                        if client.verify_receipt(
                            &DEFAULT_SHARD,
                            &batch_id,
                            &BytesN::from_array(env, &wrong_leaf),
                            &proof_vec,
                        ) {
                            failures.push(format!("wrong leaf for batch {batch_id} verified true"));
                        }

                        // 5. Random leaf not in the tree.
                        let random_leaf = leaves_from_seed(*target as u64 ^ 0xDEADBEEF, 1)[0];
                        if client.verify_receipt(
                            &DEFAULT_SHARD,
                            &batch_id,
                            &BytesN::from_array(env, &random_leaf),
                            &proof_vec,
                        ) {
                            failures
                                .push(format!("random leaf not in batch {batch_id} verified true"));
                        }

                        // 6. Wrong proof: flip a byte in one sibling.
                        if !proof.is_empty() {
                            let mut wrong = proof.clone();
                            let last = wrong.last_mut().unwrap();
                            let mut arr = last.to_array();
                            arr[0] ^= 0x55;
                            *last = BytesN::from_array(env, &arr);
                            let wrong_vec = to_svec(env, wrong);
                            if client.verify_receipt(
                                &DEFAULT_SHARD,
                                &batch_id,
                                &BytesN::from_array(env, leaf),
                                &wrong_vec,
                            ) {
                                failures.push(format!(
                                    "wrong sibling value for leaf {li} of batch {batch_id} verified true"
                                ));
                            }

                            // 7. Wrong length: truncated proof (drop last sibling).
                            let mut truncated = proof.clone();
                            truncated.pop();
                            let trunc_vec = to_svec(env, truncated);
                            if client.verify_receipt(
                                &DEFAULT_SHARD,
                                &batch_id,
                                &BytesN::from_array(env, leaf),
                                &trunc_vec,
                            ) {
                                failures.push(format!(
                                    "truncated proof for leaf {li} of batch {batch_id} verified true"
                                ));
                            }

                            // 8. Wrong length: extended proof (append a random sibling).
                            let mut extended = proof.clone();
                            extended.push(BytesN::from_array(
                                env,
                                &leaves_from_seed(*target as u64 ^ 0xCAFE, 1)[0],
                            ));
                            let ext_vec = to_svec(env, extended);
                            if client.verify_receipt(
                                &DEFAULT_SHARD,
                                &batch_id,
                                &BytesN::from_array(env, leaf),
                                &ext_vec,
                            ) {
                                failures.push(format!(
                                    "extended proof for leaf {li} of batch {batch_id} verified true"
                                ));
                            }
                        }
                    }
                } else {
                    // The batch was pruned: verification must fail cleanly with
                    // BatchNotFound (the record is gone).
                    let leaf = BytesN::from_array(env, &[0u8; 32]);
                    let res = client.try_verify_receipt(
                        &DEFAULT_SHARD,
                        &batch_id,
                        &leaf,
                        &soroban_sdk::vec![env],
                    );
                    if res != Err(Ok(Error::BatchNotFound)) {
                        failures.push(format!(
                            "verify on pruned batch {batch_id}: expected BatchNotFound, got {res:?}"
                        ));
                    }
                }

                // Wrong batch: a valid proof from one batch must not verify
                // against a different batch's root.
                if model.batches.len() >= 2 {
                    let b0 = &model.batches[0];
                    let b1 = &model.batches[1];
                    if b0.leaves.len() > 1 && b1.leaves.len() > 1 {
                        let (leaf, proof) = (&b0.leaves[0], &b0.proofs[0]);
                        let proof_vec = proof
                            .iter()
                            .map(|s| BytesN::from_array(env, s))
                            .collect::<std::vec::Vec<_>>();
                        let proof_vec = to_svec(env, proof_vec);
                        if client.verify_receipt(
                            &DEFAULT_SHARD,
                            &b1.id,
                            &BytesN::from_array(env, leaf),
                            &proof_vec,
                        ) {
                            failures.push(format!(
                                "proof from batch {} verified against batch {}",
                                b0.id, b1.id
                            ));
                        }
                    }
                }
            }
            Op::ExtendTtl { target } => {
                let n = model.anchors;
                let slot = if n == 0 { 0 } else { target % (n as u32 + 1) };
                let batch_id = slot as u64;
                if slot == 0 {
                    let res = client.try_extend_batch_ttl(&DEFAULT_SHARD, &(n + 999));
                    if res != Err(Ok(Error::BatchNotFound)) {
                        failures.push(format!(
                            "extend_ttl on missing batch: expected BatchNotFound, got {res:?}"
                        ));
                    }
                } else if let Some(_b) = model.batch(batch_id) {
                    // TTL extension must never shorten the TTL.
                    let shard_addr = shard_for(client, batch_id);
                    let ttl_before = env.as_contract(&shard_addr, || {
                        env.storage()
                            .persistent()
                            .get_ttl(&receipt_shard::DataKey::Batch(batch_id))
                    });
                    client.extend_batch_ttl(&DEFAULT_SHARD, &batch_id);
                    let ttl_after = env.as_contract(&shard_addr, || {
                        env.storage()
                            .persistent()
                            .get_ttl(&receipt_shard::DataKey::Batch(batch_id))
                    });
                    if ttl_after < ttl_before {
                        failures.push(format!(
                            "extend_batch_ttl shortened TTL of batch {batch_id}: \
                             {ttl_before} -> {ttl_after}"
                        ));
                    }
                } else {
                    // Pruned batch: TTL extension on a missing record errors.
                    let res = client.try_extend_batch_ttl(&DEFAULT_SHARD, &batch_id);
                    if res != Err(Ok(Error::BatchNotFound)) {
                        failures.push(format!(
                            "extend_ttl on pruned batch {batch_id}: expected BatchNotFound, got {res:?}"
                        ));
                    }
                }
            }
        }

        // ── Invariants checked after every operation ─────────────────────
        //
        // 1. `get_batch_count` == number of anchor_batch calls, regardless of
        //    pruning.
        let count = client.get_batch_count(&DEFAULT_SHARD);
        if count != model.anchors {
            failures.push(format!(
                "batch count mismatch: contract {count}, model {} (after {op:?})",
                model.anchors
            ));
        }

        // 2. Pruned batches always form a contiguous prefix. A full sweep of
        //    every batch id is done once at the end of the sequence (cheap and
        //    sound: a contiguity violation cannot self-heal); here we only
        //    probe the cursor boundary so a mid-sequence violation is
        //    attributed to the operation that caused it.
        let cursor_exists = model.pruned_up_to <= model.anchors
            && client
                .try_get_batch(&DEFAULT_SHARD, &model.pruned_up_to)
                .is_ok();
        let before_cursor_exists = model.pruned_up_to > 1
            && client
                .try_get_batch(&DEFAULT_SHARD, &(model.pruned_up_to - 1))
                .is_ok();
        if cursor_exists != (model.pruned_up_to <= model.anchors) {
            failures.push(format!(
                "batch at cursor {} should be readable but is not",
                model.pruned_up_to
            ));
        }
        if before_cursor_exists {
            failures.push(format!(
                "batch {} before cursor is still readable (non-contiguous prefix)",
                model.pruned_up_to - 1
            ));
        }

        // 3. The stored PrunedUpTo cursor matches the model and is monotonic.
        let stored_cursor: u64 = env.as_contract(&client.address, || {
            env.storage()
                .instance()
                .get(&DataKey::ShardPrunedUpTo(DEFAULT_SHARD))
                .unwrap_or(1)
        });
        if stored_cursor != model.pruned_up_to {
            failures.push(format!(
                "stored PrunedUpTo {stored_cursor} != model cursor {}",
                model.pruned_up_to
            ));
        }
        if stored_cursor < last_pruned {
            failures.push(format!(
                "PrunedUpTo went backwards: {last_pruned} -> {stored_cursor}"
            ));
        }
        last_pruned = stored_cursor;
    }

    // Full contiguity sweep: the readable batches are exactly
    // [pruned_up_to, batch_count] — a contiguous suffix, never a middle slice.
    let mut first_existing: Option<u64> = None;
    for id in 1..=model.anchors {
        if client.try_get_batch(&DEFAULT_SHARD, &id).is_ok() {
            if first_existing.is_none() {
                first_existing = Some(id);
            }
        } else if let Some(first) = first_existing {
            failures.push(format!(
                "non-contiguous pruning: batch {id} missing after batch {first} readable"
            ));
        }
    }
    match first_existing {
        Some(first) if first != model.pruned_up_to => failures.push(format!(
            "first readable batch {first} != expected cursor {}",
            model.pruned_up_to
        )),
        None if model.anchors > 0 && model.pruned_up_to != model.anchors + 1 => {
            failures.push(format!(
                "all batches pruned but cursor {} != {}",
                model.pruned_up_to,
                model.anchors + 1
            ))
        }
        _ => {}
    }

    failures
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    /// pruned batches always form a contiguous prefix; PrunedUpTo is
    /// monotonic; get_batch_count tracks anchors regardless of pruning.
    #[test]
    fn test_fuzz_prune_prefix_and_cursor(
        ops in proptest::collection::vec(op_strategy(), 0..=fuzz_seq_len()),
    ) {
        let (env, client, _merchant) = setup();
        let failures = execute(&env, &client, &ops);
        assert!(
            failures.is_empty(),
            "invariants violated:\n{}",
            failures.join("\n")
        );
    }
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    /// Every Merkle rejection case: wrong leaf, wrong proof, wrong length
    /// (truncated/extra), wrong batch, reversed level sequence, plus the
    /// sorted-pair position irrelevance check (mirrored tree).
    #[test]
    fn test_fuzz_verify_receipt_rejection(
        anchors in proptest::collection::vec((any::<u64>(), 2u32..=8), 2..=4),
        verifies in proptest::collection::vec(any::<u32>(), 1..=4),
    ) {
        let (env, client, _merchant) = setup();
        let mut ops = std::vec::Vec::new();
        for (seed, leaf_count) in anchors {
            ops.push(Op::Anchor { seed, leaf_count });
        }
        for target in verifies {
            ops.push(Op::Verify { target });
        }
        let failures = execute(&env, &client, &ops);
        assert!(
            failures.is_empty(),
            "verification invariants violated:\n{}",
            failures.join("\n")
        );
    }
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    /// TTL extension never shortens a batch's TTL; extension on a missing or
    /// pruned record always errors with BatchNotFound.
    #[test]
    fn test_fuzz_ttl_extension(
        anchor_seed in any::<u64>(),
        advances in proptest::collection::vec(1u32..=1500u32, 1..=8),
        missing_id in any::<u64>(),
    ) {
        let (env, client, _merchant) = setup();
        let leaves = leaves_from_seed(anchor_seed, 2);
        let (root, _proofs) = build_tree(&env, &leaves);
        let batch_id = client.anchor_batch(&DEFAULT_SHARD, &BytesN::from_array(&env, &root), &2, &0, &100);

        // Extension on a record that does not exist errors.
        let missing = missing_id.wrapping_add(batch_id + 1000);
        assert_eq!(
            client.try_extend_batch_ttl(&DEFAULT_SHARD, &missing),
            Err(Ok(Error::BatchNotFound))
        );

        let shard_addr = shard_for(&client, batch_id);
        for advance in advances {
            env.ledger().with_mut(|li| li.sequence_number += advance);
            let ttl_before: u32 = env.as_contract(&shard_addr, || {
                env.storage()
                    .persistent()
                    .get_ttl(&receipt_shard::DataKey::Batch(batch_id))
            });
            client.extend_batch_ttl(&DEFAULT_SHARD, &batch_id);
            let ttl_after: u32 = env.as_contract(&shard_addr, || {
                env.storage()
                    .persistent()
                    .get_ttl(&receipt_shard::DataKey::Batch(batch_id))
            });
            assert!(
                ttl_after >= ttl_before,
                "extend_batch_ttl shortened TTL: {ttl_before} -> {ttl_after}"
            );
            // The batch must remain readable after extension.
            client.get_batch(&DEFAULT_SHARD, &batch_id);
        }
    }
}

// ── Long local profile ──────────────────────────────────────────────────────
//
// Run with: cargo test -p receipt-anchor -- --ignored
// For an even longer run: FUZZ_CASES=2000 FUZZ_SEQ_LEN=256 cargo test -p
// receipt-anchor fuzz_test::test_fuzz_prune_prefix_and_cursor_long -- --ignored

proptest! {
    #![proptest_config(proptest_config(256))]

    #[ignore]
    #[test]
    fn test_fuzz_prune_prefix_and_cursor_long(
        ops in proptest::collection::vec(op_strategy(), 0..=128),
    ) {
        let (env, client, _merchant) = setup();
        let failures = execute(&env, &client, &ops);
        assert!(
            failures.is_empty(),
            "invariants violated:\n{}",
            failures.join("\n")
        );
    }
}

// ── Regression corpus ──────────────────────────────────────────────────────
//
// Any failure found by the property tests above is frozen here as a permanent
// deterministic example, per the issue's seed-corpus requirement. Each test
// replays a concrete sequence that previously (or plausibly could) break an
// invariant, so a regression can never be silently forgotten.

#[test]
fn test_regression_prune_prefix_stays_contiguous_after_full_prune() {
    // Prune everything, then keep anchoring: the new batches must be readable
    // and the cursor must point at the first new batch.
    let (env, client, _merchant) = setup();
    env.ledger().with_mut(|li| li.sequence_number = 100);
    let leaves = leaves_from_seed(1, 2);
    let (root, _) = build_tree(&env, &leaves);
    let b1 = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root),
        &2,
        &0,
        &100,
    );

    env.ledger().with_mut(|li| li.sequence_number = 200);
    let leaves2 = leaves_from_seed(2, 2);
    let (root2, _) = build_tree(&env, &leaves2);
    let b2 = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root2),
        &2,
        &0,
        &100,
    );

    // Prune everything anchored before 300.
    client.prune_batches(&DEFAULT_SHARD, &300);
    assert_eq!(
        client.try_get_batch(&DEFAULT_SHARD, &b1),
        Err(Ok(Error::BatchNotFound))
    );
    assert_eq!(
        client.try_get_batch(&DEFAULT_SHARD, &b2),
        Err(Ok(Error::BatchNotFound))
    );

    let stored_cursor: u64 = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&DataKey::ShardPrunedUpTo(DEFAULT_SHARD))
            .unwrap_or(1)
    });
    assert_eq!(
        stored_cursor, 3,
        "cursor must point past the last pruned batch"
    );

    // New anchor gets id 3 and must be readable; the prefix property holds.
    env.ledger().with_mut(|li| li.sequence_number = 400);
    let leaves3 = leaves_from_seed(3, 2);
    let (root3, _) = build_tree(&env, &leaves3);
    let b3 = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root3),
        &2,
        &0,
        &100,
    );
    assert_eq!(b3, 3);
    client.get_batch(&DEFAULT_SHARD, &b3); // panics if missing
}

#[test]
fn test_regression_verify_wrong_batch_rejected() {
    let (env, client, _merchant) = setup();
    let leaves_a = leaves_from_seed(10, 4);
    let (root_a, proofs_a) = build_tree(&env, &leaves_a);
    let ba = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root_a),
        &4,
        &0,
        &100,
    );

    let leaves_b = leaves_from_seed(20, 4);
    let (root_b, _proofs_b) = build_tree(&env, &leaves_b);
    let bb = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root_b),
        &4,
        &0,
        &100,
    );

    let proof_vec = proofs_a[0]
        .iter()
        .map(|s| BytesN::from_array(&env, s))
        .collect::<std::vec::Vec<_>>();
    let proof = to_svec(&env, proof_vec);
    assert!(client.verify_receipt(
        &DEFAULT_SHARD,
        &ba,
        &BytesN::from_array(&env, &leaves_a[0]),
        &proof
    ));
    // Same leaf+proof must not verify against the other batch's root.
    assert!(!client.verify_receipt(
        &DEFAULT_SHARD,
        &bb,
        &BytesN::from_array(&env, &leaves_a[0]),
        &proof
    ));
}

#[test]
fn test_regression_mirrored_tree_position_irrelevant() {
    // The sorted-pair convention makes a leaf's position within a pair
    // irrelevant: the mirror-reversed tree has the same root and every
    // mirrored proof still verifies against the anchored root.
    let (env, client, _merchant) = setup();
    let leaves = leaves_from_seed(7, 4);
    let (root, _proofs) = build_tree(&env, &leaves);
    let batch = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root),
        &4,
        &0,
        &100,
    );

    let mut mirrored = leaves.clone();
    mirrored.reverse();
    let (mirror_root, mirror_proofs) = build_tree(&env, &mirrored);
    assert_eq!(mirror_root, root, "mirror tree must share the root");

    for (li, leaf) in leaves.iter().enumerate() {
        let mirror_li = leaves.len() - 1 - li;
        let mirror_proof = to_svec(
            &env,
            mirror_proofs[mirror_li]
                .iter()
                .map(|s| BytesN::from_array(&env, s))
                .collect::<std::vec::Vec<_>>(),
        );
        assert!(
            client.verify_receipt(
                &DEFAULT_SHARD,
                &batch,
                &BytesN::from_array(&env, leaf),
                &mirror_proof
            ),
            "mirrored proof for leaf {li} must verify"
        );
    }
}

#[test]
fn test_regression_reversed_level_sequence_rejected() {
    // The sorted-pair convention does NOT make the level sequence irrelevant:
    // a proof with reversed sibling order folds different levels together and
    // must be rejected.
    let (env, client, _merchant) = setup();
    let leaves = leaves_from_seed(7, 4);
    let (root, proofs) = build_tree(&env, &leaves);
    let batch = client.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &root),
        &4,
        &0,
        &100,
    );

    let proof_vec = proofs[0]
        .iter()
        .map(|s| BytesN::from_array(&env, s))
        .collect::<std::vec::Vec<_>>();
    let proof = to_svec(&env, proof_vec);
    let mut reversed: std::vec::Vec<BytesN<32>> = proof.iter().collect();
    reversed.reverse();
    let reversed = to_svec(&env, reversed);

    assert!(
        !client.verify_receipt(
            &DEFAULT_SHARD,
            &batch,
            &BytesN::from_array(&env, &leaves[0]),
            &reversed
        ),
        "reversed level sequence must be rejected"
    );
}

/// Headroom percentage (15%) chosen to account for minor toolchain/host optimization differences.
const HEADROOM_PERCENT: u64 = 15;

/// Cost baselines for `anchor_batch` (N=1000-leaf batch root, including first-anchor shard contract deployment)
/// Measured via `env.cost_estimate().budget().cpu_instruction_cost()` and `env.cost_estimate().memory_bytes_cost()` on 2026-08-26.
const ANCHOR_BATCH_BASELINE_CPU: u64 = 1_591_284;
const ANCHOR_BATCH_BASELINE_MEM: u64 = 3_819_993;

/// Cost baselines for `verify_receipt` (4-leaf Merkle proof, including cross-contract shard routing)
/// Measured via `env.cost_estimate().budget().cpu_instruction_cost()` and `env.cost_estimate().memory_bytes_cost()`.
/// Re-measured on 2026-08-29: the pure-WASM SHA-256 folding merged in #250 (08-27)
/// moved hashing out of the host into WASM, which raised the host CPU instruction
/// count for this path (~569.9k -> ~780.8k) while cutting WASM instruction usage.
const VERIFY_RECEIPT_BASELINE_CPU: u64 = 780_762;
const VERIFY_RECEIPT_BASELINE_MEM: u64 = 1_378_946;

#[test]
fn benchmark_gas_and_cpu_instructions() {
    let (env, client, _merchant) = setup();

    let root = BytesN::from_array(&env, &[1u8; 32]);

    env.cost_estimate().budget().reset_default();
    let batch_id = client.anchor_batch(&DEFAULT_SHARD, &root, &1000, &0, &100);
    let cpu_anchor = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_anchor = env.cost_estimate().budget().memory_bytes_cost();

    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    let proof = soroban_sdk::vec![&env, BytesN::from_array(&env, &[2u8; 32])];

    env.cost_estimate().budget().reset_default();
    let _verified = client.verify_receipt(&DEFAULT_SHARD, &batch_id, &leaf, &proof);
    let cpu_verify = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_verify = env.cost_estimate().budget().memory_bytes_cost();

    let max_cpu_anchor =
        ANCHOR_BATCH_BASELINE_CPU + (ANCHOR_BATCH_BASELINE_CPU * HEADROOM_PERCENT / 100);
    let max_mem_anchor =
        ANCHOR_BATCH_BASELINE_MEM + (ANCHOR_BATCH_BASELINE_MEM * HEADROOM_PERCENT / 100);

    assert!(
        cpu_anchor <= max_cpu_anchor,
        "anchor_batch CPU cost regression! Function: anchor_batch, Limit: {}, Measured: {}",
        max_cpu_anchor,
        cpu_anchor
    );
    assert!(
        mem_anchor <= max_mem_anchor,
        "anchor_batch Memory cost regression! Function: anchor_batch, Limit: {}, Measured: {}",
        max_mem_anchor,
        mem_anchor
    );

    let max_cpu_verify =
        VERIFY_RECEIPT_BASELINE_CPU + (VERIFY_RECEIPT_BASELINE_CPU * HEADROOM_PERCENT / 100);
    let max_mem_verify =
        VERIFY_RECEIPT_BASELINE_MEM + (VERIFY_RECEIPT_BASELINE_MEM * HEADROOM_PERCENT / 100);

    assert!(
        cpu_verify <= max_cpu_verify,
        "verify_receipt CPU cost regression! Function: verify_receipt, Limit: {}, Measured: {}",
        max_cpu_verify,
        cpu_verify
    );
    assert!(
        mem_verify <= max_mem_verify,
        "verify_receipt Memory cost regression! Function: verify_receipt, Limit: {}, Measured: {}",
        max_mem_verify,
        mem_verify
    );
}
