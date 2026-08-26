#![cfg(test)]

//! Property-based fuzz tests for [`ReceiptAnchor::verify_receipt`] — the
//! function the public verifier depends on and the most security-sensitive
//! path in this repo.
//!
//! This file replaces the previous placeholder "fuzz" test, which asserted
//! `count > 0` without ever touching the contract, and the empty
//! `benchmark_gas_and_cpu_instructions` stub. Both carried names that claimed
//! coverage the repository did not have.
//!
//! Every property builds a *real* Merkle tree over randomly generated leaves
//! using the same sorted-pair SHA-256 construction `verify_receipt` implements
//! (see [`hash_pair`] and [`build_merkle_tree`]), anchors the resulting root,
//! and then exercises [`ReceiptAnchor::verify_receipt`]:
//!
//! * a valid proof for a randomly chosen leaf verifies as `true` across
//!   randomised tree sizes (1..=`MAX_BATCH_SIZE` leaves, odd and even);
//! * a proof with one corrupted sibling is rejected;
//! * a truncated proof is rejected;
//! * an empty proof is rejected against any multi-leaf root (it stays valid
//!   for a single-leaf batch, where the leaf *is* the root);
//! * a valid proof for a leaf of a different batch is rejected;
//! * arbitrary caller-supplied proofs — including over-long ones — never
//!   panic. A panic in `verify_receipt` would be a denial-of-service on the
//!   public verifier. The proof lengths generated here are bounded so every
//!   call stays inside the host resource budget; bounding the proof on the
//!   contract side is tracked separately in accensa/accensa-contracts#96;
//! * duplicate leaves exercise the equal-hash ordering branch of the
//!   sorted-pair construction.
//!
//! The proptest configuration is pinned — fixed case count and fixed RNG
//! seed, see [`fuzz_config`] — so the exact input sequence is identical on
//! every run, including in `CI`. If a property ever fails, proptest prints
//! the failing input and the same seed reproduces the same sequence locally.

extern crate std;

use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use soroban_sdk::{testutils::Address as _, vec, Address, Bytes, BytesN, Env};

use super::*;

/// Pinned RNG seed for every property in this file (chosen so a failure is
/// reproducible from the test log rather than a one-off nobody can re-run).
const FUZZ_SEED: u64 = 0x0000_0000_5EED_0085;

/// Number of generated cases each property runs before passing.
const FUZZ_CASES: u32 = 128;

/// Upper bound on the number of leaves in a generated batch. Equal to the
/// contract's own `MAX_BATCH_SIZE`, so every generated batch is anchorable
/// and the deepest legitimate proof depth is exercised.
const MAX_FUZZ_LEAVES: usize = MAX_BATCH_SIZE as usize;

/// Proptest configuration shared by every property in this file.
///
/// Deterministic on purpose: both the case count and the RNG seed are pinned
/// (and failure persistence is disabled, so `CI` never writes
/// `.proptest-regressions` files). A failing property can always be
/// reproduced locally by re-running the test binary.
fn fuzz_config() -> ProptestConfig {
    ProptestConfig {
        cases: FUZZ_CASES,
        rng_seed: RngSeed::Fixed(FUZZ_SEED),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Registers and initializes a fresh `ReceiptAnchor` with all auths mocked,
/// returning the environment and its client.
fn fresh_client() -> (Env, ReceiptAnchorClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    client.initialize(&merchant);
    (env, client)
}

/// Hashes two nodes with the sorted-pair SHA-256 convention the contract and
/// the accensa-app SDK share: siblings are concatenated smaller-hash-first, so
/// proofs carry no left/right position flags.
fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() {
        (a.to_array(), b.to_array())
    } else {
        (b.to_array(), a.to_array())
    };
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(&lo);
    combined[32..].copy_from_slice(&hi);
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &combined))
        .to_array();
    BytesN::from_array(env, &digest)
}

/// Builds a sorted-pair SHA-256 Merkle tree over `leaves` — the same
/// convention `verify_receipt` implements — and returns `(root, proof)`,
/// where `proof` is the sibling path proving the leaf at `index`.
///
/// Odd nodes are carried up unchanged; a carried node contributes no sibling
/// to the proof at that level. Building happens off-chain in the test
/// harness, so it is not part of any metered contract cost.
fn build_merkle_tree(
    env: &Env,
    leaves: &[BytesN<32>],
    index: usize,
) -> (BytesN<32>, std::vec::Vec<BytesN<32>>) {
    assert!(!leaves.is_empty(), "merkle tree needs at least one leaf");
    assert!(index < leaves.len(), "proven leaf index out of range");

    let mut level: std::vec::Vec<BytesN<32>> = leaves.to_vec();
    let mut proof: std::vec::Vec<BytesN<32>> = std::vec::Vec::new();
    let mut idx = index;

    while level.len() > 1 {
        let mut next: std::vec::Vec<BytesN<32>> =
            std::vec::Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            if i + 1 < level.len() {
                if i == idx {
                    proof.push(level[i + 1].clone());
                } else if i + 1 == idx {
                    proof.push(level[i].clone());
                }
                next.push(hash_pair(env, &level[i], &level[i + 1]));
            } else {
                // Odd node at this level: carried up without a sibling.
                next.push(level[i].clone());
            }
            i += 2;
        }
        idx /= 2;
        level = next;
    }

    let root = level.pop().expect("merkle tree has a root");
    (root, proof)
}

/// Converts a `std::vec::Vec` of siblings into the soroban [`Vec`] type
/// [`ReceiptAnchor::verify_receipt`] takes.
fn to_proof_vec(env: &Env, proof: &[BytesN<32>]) -> soroban_sdk::Vec<BytesN<32>> {
    let mut path = vec![env];
    for sibling in proof {
        path.push_back(sibling.clone());
    }
    path
}

/// Strategy for a non-empty leaf list together with a valid index into it, so
/// the property always proves an actual member of the generated batch.
fn leaf_list_and_index() -> impl Strategy<Value = (std::vec::Vec<[u8; 32]>, usize)> {
    prop::collection::vec(any::<[u8; 32]>(), 1..=MAX_FUZZ_LEAVES).prop_flat_map(|leaves| {
        let len = leaves.len();
        (Just(leaves), 0..len)
    })
}

proptest! {
    #![proptest_config(fuzz_config())]

    /// The core property: a real tree, a real proof, and both the positive
    /// and every negative outcome the verifier must produce.
    #[test]
    fn test_fuzz_merkle_verification(
        (leaves, index) in leaf_list_and_index(),
        junk_proof in prop::collection::vec(any::<[u8; 32]>(), 0..=17),
    ) {
        let (env, client) = fresh_client();
        let leaves: std::vec::Vec<BytesN<32>> =
            leaves.iter().map(|l| BytesN::from_array(&env, l)).collect();
        let leaf_count = leaves.len();
        let leaf = &leaves[index];

        // Build the tree and a valid proof for the leaf at `index`, then
        // anchor the root — exactly the flow an off-chain indexer performs.
        let (root, proof) = build_merkle_tree(&env, &leaves, index);
        let batch_id = client.anchor_batch(&root, &(leaf_count as u32), &0, &100);

        // 1. A valid proof for a randomly chosen leaf verifies as true.
        prop_assert!(
            client.verify_receipt(&batch_id, leaf, &to_proof_vec(&env, &proof)),
            "a valid proof for leaf {index} of a {leaf_count}-leaf batch must verify"
        );

        // 2. A proof with one corrupted sibling is rejected.
        if !proof.is_empty() {
            let mut corrupted = proof.clone();
            let mut bad = corrupted[0].to_array();
            bad[0] ^= 0xFF;
            corrupted[0] = BytesN::from_array(&env, &bad);
            prop_assert!(
                !client.verify_receipt(&batch_id, leaf, &to_proof_vec(&env, &corrupted)),
                "a corrupted sibling must be rejected for a {leaf_count}-leaf batch"
            );
        }

        // 3. A truncated proof (deepest sibling removed) is rejected.
        if !proof.is_empty() {
            let mut truncated = proof.clone();
            truncated.pop();
            prop_assert!(
                !client.verify_receipt(&batch_id, leaf, &to_proof_vec(&env, &truncated)),
                "a truncated proof must be rejected for a {leaf_count}-leaf batch"
            );
        }

        // 4. An empty proof is rejected against any multi-leaf root.
        if leaf_count >= 2 {
            prop_assert!(
                !client.verify_receipt(&batch_id, leaf, &vec![&env]),
                "an empty proof must be rejected against a {leaf_count}-leaf root"
            );
        }

        // 5. A valid proof for a leaf of a *different* batch is rejected.
        {
            let mut foreign_leaves = leaves.clone();
            let mut tweaked = foreign_leaves[index].to_array();
            tweaked[0] ^= 0xFF;
            foreign_leaves[index] = BytesN::from_array(&env, &tweaked);

            let (foreign_root, foreign_proof) =
                build_merkle_tree(&env, &foreign_leaves, index);
            prop_assert_ne!(
                &foreign_root, &root,
                "mutating a leaf must change the batch root"
            );

            let foreign_batch_id =
                client.anchor_batch(&foreign_root, &(leaf_count as u32), &0, &100);
            prop_assert!(
                client.verify_receipt(
                    &foreign_batch_id,
                    &foreign_leaves[index],
                    &to_proof_vec(&env, &foreign_proof),
                ),
                "the mutated leaf must verify within its own batch"
            );
            prop_assert!(
                !client.verify_receipt(
                    &batch_id,
                    &foreign_leaves[index],
                    &to_proof_vec(&env, &foreign_proof),
                ),
                "a proof for a leaf of another batch must be rejected"
            );
        }

        // 6. Arbitrary caller-supplied proofs — including over-long ones, up
        //    to 18 siblings against batches whose legitimate proofs are at
        //    most 10 — must never panic. Bounded here so every call stays
        //    inside the host resource budget; the contract-side proof bound
        //    is tracked in accensa/accensa-contracts#96.
        {
            let junk: std::vec::Vec<BytesN<32>> =
                junk_proof.iter().map(|b| BytesN::from_array(&env, b)).collect();
            let _ = client.verify_receipt(&batch_id, leaf, &to_proof_vec(&env, &junk));
        }
    }

    /// Duplicate leaves: the sorted-pair convention must hash an equal pair
    /// deterministically (`a <= b` when `a == b`), and both occurrences of
    /// the duplicated leaf must verify.
    #[test]
    fn test_fuzz_merkle_duplicate_leaves(
        base in any::<[u8; 32]>(),
        extra in prop::collection::vec(any::<[u8; 32]>(), 0..=30),
    ) {
        let (env, client) = fresh_client();

        // Guaranteed duplicate: `base` appears at indices 0 and 1.
        let mut leaves: std::vec::Vec<BytesN<32>> = std::vec::Vec::with_capacity(extra.len() + 2);
        leaves.push(BytesN::from_array(&env, &base));
        leaves.push(BytesN::from_array(&env, &base));
        for l in &extra {
            leaves.push(BytesN::from_array(&env, l));
        }

        let (root, proof_first) = build_merkle_tree(&env, &leaves, 0);
        let (_, proof_second) = build_merkle_tree(&env, &leaves, 1);
        let batch_id = client.anchor_batch(&root, &(leaves.len() as u32), &0, &100);

        prop_assert!(
            client.verify_receipt(&batch_id, &leaves[0], &to_proof_vec(&env, &proof_first)),
            "the first occurrence of a duplicated leaf must verify"
        );
        prop_assert!(
            client.verify_receipt(&batch_id, &leaves[1], &to_proof_vec(&env, &proof_second)),
            "the second occurrence of a duplicated leaf must verify"
        );
    }
}
