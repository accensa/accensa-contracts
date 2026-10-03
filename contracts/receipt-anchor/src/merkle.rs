//! Sorted-pair SHA-256 Merkle proof verification (ADR-001).
//!
//! Each step hashes `sha256(min(a, b) || max(a, b))`, so a proof is just the
//! sibling hashes from leaf to root with no left/right flags — and therefore
//! no leaf index. Hashing runs as pure guest-side SHA-256 over stack arrays;
//! the proof is read straight out of its Soroban vector, one element per
//! level, keeping CPU cost linear in proof length (see `docs/BENCHMARKS.md`).

use sha2::{Digest, Sha256};
use soroban_sdk::{BytesN, Vec};

/// Hash two nodes in sorted order: `sha256(min(a, b) || max(a, b))`.
pub fn hash_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(lo);
    combined[32..].copy_from_slice(hi);
    Sha256::digest(combined).into()
}

/// Fold `proof` into `leaf`, returning the root it implies. An empty proof
/// yields the leaf itself (a single-leaf tree).
pub fn fold_proof(leaf: [u8; 32], proof: &Vec<BytesN<32>>) -> [u8; 32] {
    let mut computed = leaf;
    for sibling in proof.iter() {
        computed = hash_pair(&computed, &sibling.to_array());
    }
    computed
}

/// Whether `proof` places `leaf` under `root`.
pub fn verify(root: &BytesN<32>, leaf: &BytesN<32>, proof: &Vec<BytesN<32>>) -> bool {
    fold_proof(leaf.to_array(), proof) == root.to_array()
}

#[cfg(test)]
pub(crate) mod test_tree {
    //! Reference tree builder for tests. A node without a sibling on its
    //! level is promoted unchanged, so its proof simply omits that level.

    extern crate std;
    use super::hash_pair;
    use std::vec::Vec;

    pub struct Tree {
        levels: Vec<Vec<[u8; 32]>>,
    }

    impl Tree {
        pub fn new(leaves: &[[u8; 32]]) -> Self {
            assert!(!leaves.is_empty());
            let mut levels = std::vec![leaves.to_vec()];
            while levels.last().unwrap().len() > 1 {
                let next = levels
                    .last()
                    .unwrap()
                    .chunks(2)
                    .map(|pair| match pair {
                        [a, b] => hash_pair(a, b),
                        [a] => *a,
                        _ => unreachable!(),
                    })
                    .collect();
                levels.push(next);
            }
            Tree { levels }
        }

        pub fn root(&self) -> [u8; 32] {
            self.levels.last().unwrap()[0]
        }

        pub fn proof(&self, mut index: usize) -> Vec<[u8; 32]> {
            let mut proof = Vec::new();
            for level in &self.levels[..self.levels.len() - 1] {
                if let Some(sibling) = level.get(index ^ 1) {
                    proof.push(*sibling);
                }
                index /= 2;
            }
            proof
        }
    }

    pub fn leaves(n: usize) -> Vec<[u8; 32]> {
        (0..n)
            .map(|i| {
                let mut leaf = [0u8; 32];
                leaf[..8].copy_from_slice(&(i as u64).to_be_bytes());
                leaf[31] = 0xAB;
                leaf
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::test_tree::{leaves, Tree};
    use super::*;
    use proptest::prelude::*;
    use soroban_sdk::{Bytes, Env};

    fn to_proof(env: &Env, nodes: &[[u8; 32]]) -> Vec<BytesN<32>> {
        let mut proof = Vec::new(env);
        for n in nodes {
            proof.push_back(BytesN::from_array(env, n));
        }
        proof
    }

    fn check(env: &Env, root: [u8; 32], leaf: [u8; 32], proof: &[[u8; 32]]) -> bool {
        verify(
            &BytesN::from_array(env, &root),
            &BytesN::from_array(env, &leaf),
            &to_proof(env, proof),
        )
    }

    #[test]
    fn hash_pair_matches_host_sha256_and_is_order_independent() {
        let env = Env::default();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(&a);
        combined[32..].copy_from_slice(&b);
        let host = env
            .crypto()
            .sha256(&Bytes::from_slice(&env, &combined))
            .to_array();

        assert_eq!(hash_pair(&a, &b), host);
        assert_eq!(hash_pair(&b, &a), host);
    }

    #[test]
    fn single_leaf_tree_has_empty_proof() {
        let env = Env::default();
        let leaf = [9u8; 32];
        assert!(check(&env, leaf, leaf, &[]));
        assert!(!check(&env, [8u8; 32], leaf, &[]));
    }

    #[test]
    fn every_leaf_verifies_in_max_depth_tree() {
        // 1000 leaves = MAX_BATCH_SIZE → depth 10 = MAX_PROOF_LEN.
        let env = Env::default();
        let ls = leaves(1000);
        let tree = Tree::new(&ls);
        for (i, leaf) in ls.iter().enumerate() {
            let proof = tree.proof(i);
            assert!(proof.len() <= 10);
            assert!(check(&env, tree.root(), *leaf, &proof), "leaf {i}");
        }
    }

    #[test]
    fn corrupted_branches_are_rejected() {
        let env = Env::default();
        let ls = leaves(16);
        let tree = Tree::new(&ls);
        let (leaf, proof) = (ls[5], tree.proof(5));
        assert!(check(&env, tree.root(), leaf, &proof));

        // A sibling from another branch.
        let mut wrong = proof.clone();
        wrong[1] = tree.proof(12)[1];
        assert!(!check(&env, tree.root(), leaf, &wrong));

        // Truncated, extended, reordered.
        assert!(!check(&env, tree.root(), leaf, &proof[..proof.len() - 1]));
        let mut longer = proof.clone();
        longer.push([0u8; 32]);
        assert!(!check(&env, tree.root(), leaf, &longer));
        let mut swapped = proof.clone();
        swapped.swap(0, 1);
        assert!(!check(&env, tree.root(), leaf, &swapped));

        // Another leaf's proof, and an inner node presented as a leaf with
        // the remaining path: the latter verifies — ADR-001 accepts this
        // (no leaf/node domain separation), so callers must only ever pass
        // receipt hashes as leaves.
        assert!(!check(&env, tree.root(), ls[6], &proof));
        let inner = hash_pair(&ls[4], &ls[5]);
        assert!(check(&env, tree.root(), inner, &proof[1..]));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn prop_valid_proofs_verify(n in 1usize..=200, pick in any::<prop::sample::Index>()) {
            let env = Env::default();
            let ls = leaves(n);
            let tree = Tree::new(&ls);
            let i = pick.index(n);
            prop_assert!(check(&env, tree.root(), ls[i], &tree.proof(i)));
        }

        #[test]
        fn prop_any_bit_flip_breaks_proof(
            n in 2usize..=200,
            pick in any::<prop::sample::Index>(),
            node in any::<prop::sample::Index>(),
            byte in 0usize..32,
            bit in 0u8..8,
        ) {
            let env = Env::default();
            let ls = leaves(n);
            let tree = Tree::new(&ls);
            let i = pick.index(n);
            let mut leaf = ls[i];
            let mut proof = tree.proof(i);

            // Flip one bit in the leaf or in one proof element.
            let target = node.index(proof.len() + 1);
            if target == proof.len() {
                leaf[byte] ^= 1 << bit;
            } else {
                proof[target][byte] ^= 1 << bit;
            }
            prop_assert!(!check(&env, tree.root(), leaf, &proof));
        }
    }
}
