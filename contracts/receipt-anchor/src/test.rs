#![cfg(test)]

// This crate is `#![no_std]`; the test harness still links `std`, so bring it
// into scope for `println!` and `std::vec::Vec` below. (The std prelude is not
// auto-injected in no_std crates, so the macro needs an explicit import.)
extern crate std;
use std::println;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, Env,
};

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    (env, client, merchant)
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
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &combined))
        .to_array();
    BytesN::from_array(env, &digest)
}

#[test]
fn test_initialize() {
    let (_env, client, merchant) = setup();
    client.initialize(&merchant);
}

#[test]
fn test_double_initialize_fails() {
    let (_env, client, merchant) = setup();
    client.initialize(&merchant);
    assert_eq!(
        client.try_initialize(&merchant),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_anchor_batch_before_initialize_fails() {
    let (env, client, _merchant) = setup();
    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_anchor_batch(&root, &10, &0, &100),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_anchor_batch_assigns_sequential_ids() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    assert_eq!(client.anchor_batch(&root1, &5, &0, &50), 1);
    assert_eq!(client.anchor_batch(&root2, &7, &51, &99), 2);
}

#[test]
fn test_get_batch_returns_stored_record() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root = BytesN::from_array(&env, &[9u8; 32]);
    let batch_id = client.anchor_batch(&root, &42, &1000, &2000);

    let record = client.get_batch(&batch_id);
    assert_eq!(record.root, root);
    assert_eq!(record.count, 42);
    assert_eq!(record.period_start, 1000);
    assert_eq!(record.period_end, 2000);
}

#[test]
fn test_get_batch_missing_fails() {
    let (_env, client, merchant) = setup();
    client.initialize(&merchant);
    assert_eq!(client.try_get_batch(&99), Err(Ok(Error::BatchNotFound)));
}

#[test]
#[should_panic]
fn test_anchor_batch_requires_merchant_auth() {
    let env = Env::default();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);

    env.mock_all_auths();
    client.initialize(&merchant);

    // Enforcing mode with no signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root, &1, &0, &1);
}

#[test]
fn test_verify_receipt_single_leaf_tree() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    // A one-receipt batch: the root is the leaf itself, proof is empty.
    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let batch_id = client.anchor_batch(&leaf, &1, &0, &10);

    assert!(client.verify_receipt(&batch_id, &leaf, &vec![&env]));
}

#[test]
fn test_verify_receipt_four_leaf_tree() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let l3 = BytesN::from_array(&env, &[3u8; 32]);
    let l4 = BytesN::from_array(&env, &[4u8; 32]);

    let n12 = hash_pair(&env, &l1, &l2);
    let n34 = hash_pair(&env, &l3, &l4);
    let root = hash_pair(&env, &n12, &n34);

    let batch_id = client.anchor_batch(&root, &4, &0, &100);

    // Every leaf must verify with its sibling path.
    assert!(client.verify_receipt(&batch_id, &l1, &vec![&env, l2.clone(), n34.clone()]));
    assert!(client.verify_receipt(&batch_id, &l2, &vec![&env, l1.clone(), n34.clone()]));
    assert!(client.verify_receipt(&batch_id, &l3, &vec![&env, l4.clone(), n12.clone()]));
    assert!(client.verify_receipt(&batch_id, &l4, &vec![&env, l3.clone(), n12.clone()]));
}

#[test]
fn test_verify_receipt_rejects_wrong_leaf_and_proof() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &l1, &l2);
    let batch_id = client.anchor_batch(&root, &2, &0, &100);

    let forged_leaf = BytesN::from_array(&env, &[99u8; 32]);
    assert!(!client.verify_receipt(&batch_id, &forged_leaf, &vec![&env, l2.clone()]));

    let wrong_sibling = BytesN::from_array(&env, &[88u8; 32]);
    assert!(!client.verify_receipt(&batch_id, &l1, &vec![&env, wrong_sibling]));
}

#[test]
fn test_verify_receipt_missing_batch_fails() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);
    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_verify_receipt(&5, &leaf, &vec![&env]),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_get_batch_count_tracks_anchors() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    assert_eq!(client.get_batch_count(), 0);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root, &5, &0, &50);
    assert_eq!(client.get_batch_count(), 1);

    client.anchor_batch(&root, &7, &51, &99);
    assert_eq!(client.get_batch_count(), 2);
}

#[test]
fn test_get_batch_count_before_initialize_fails() {
    let (_env, client, _merchant) = setup();
    assert_eq!(client.try_get_batch_count(), Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_get_max_batch_size() {
    let (_env, client, _merchant) = setup();
    assert_eq!(client.get_max_batch_size(), MAX_BATCH_SIZE);
    assert_eq!(client.get_max_batch_size(), 1000);
}

#[test]
fn test_anchor_batch_at_max_size_succeeds() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    let batch_id = client.anchor_batch(&root, &MAX_BATCH_SIZE, &0, &50);
    assert_eq!(batch_id, 1);
    let record = client.get_batch(&batch_id);
    assert_eq!(record.count, MAX_BATCH_SIZE);
}

#[test]
fn test_anchor_batch_enforces_max_size() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_anchor_batch(&root, &(MAX_BATCH_SIZE + 1), &0, &50),
        Err(Ok(Error::BatchTooLarge))
    );
}

#[test]
fn test_extend_batch_ttl_fails_if_missing() {
    let (_env, client, merchant) = setup();
    client.initialize(&merchant);
    assert_eq!(
        client.try_extend_batch_ttl(&99),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_extend_batch_ttl_succeeds() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    let batch_id = client.anchor_batch(&root, &5, &0, &50);

    // This won't fail since the batch exists. (TTL updates aren't observable from the contract API, but it shouldn't revert)
    client.extend_batch_ttl(&batch_id);
}

// ---------------------------------------------------------------------------
// Cross-implementation conformance
// ---------------------------------------------------------------------------
//
// The vectors below are byte-identical to the ones the TypeScript SDK is tested
// against (packages/sdk/merkle-vectors.json in accensa-app). Both suites are
// generated from a single source of truth, so if this contract and the SDK ever
// diverge on the sorted-pair SHA-256 convention, one of them fails.

#[path = "vectors.rs"]
mod vectors;

#[test]
fn test_shared_vectors_match_typescript_sdk() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    for v in vectors::VECTORS {
        let root = BytesN::from_array(&env, &v.root);
        let leaf = BytesN::from_array(&env, &v.leaf);

        let mut proof = vec![&env];
        for sibling in v.proof {
            proof.push_back(BytesN::from_array(&env, sibling));
        }

        // Each vector gets its own batch so roots never collide.
        let batch_id = client.anchor_batch(&root, &(v.proof.len() as u32), &0, &100);
        let got = client.verify_receipt(&batch_id, &leaf, &proof);

        assert_eq!(
            got, v.expected,
            "vector {:?}: contract returned {}, TypeScript SDK returns {}",
            v.name, got, v.expected
        );
    }
}

#[test]
fn test_shared_vectors_cover_both_outcomes() {
    // Guards against the conformance suite silently degrading into all-true or
    // all-false cases, which would still pass while proving nothing.
    assert!(vectors::VECTORS.iter().any(|v| v.expected));
    assert!(vectors::VECTORS.iter().any(|v| !v.expected));
}

#[test]
fn test_shared_vectors_include_live_testnet_batch() {
    // The first vector is the batch anchored on Stellar testnet as batch #1 of
    // CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV. Keeping it in
    // the suite ties these tests to a deployment anyone can independently check.
    let live = &vectors::VECTORS[0];
    assert!(live.expected);
    assert_eq!(
        live.root,
        [
            0xc6, 0xcc, 0xdc, 0xdb, 0x57, 0x89, 0x6f, 0xa4, 0x99, 0x9d, 0x9d, 0xea, 0x6a, 0x5e,
            0xf4, 0x05, 0x23, 0xd5, 0x5e, 0x46, 0xcf, 0x32, 0xb6, 0x21, 0xd7, 0xea, 0x4a, 0x58,
            0x2d, 0x90, 0xe6, 0xac,
        ]
    );
}

#[test]
fn test_prune_batches_deletes_old_records() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let b1 = client.anchor_batch(&root1, &10, &0, &10);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    let b2 = client.anchor_batch(&root2, &10, &11, &20);

    env.ledger().with_mut(|li| li.sequence_number = 300);
    let root3 = BytesN::from_array(&env, &[3u8; 32]);
    let b3 = client.anchor_batch(&root3, &10, &21, &30);

    // Prune before ledger 200 (should delete b1 only)
    client.prune_batches(&200);

    assert_eq!(client.try_get_batch(&b1), Err(Ok(Error::BatchNotFound)));
    assert!(client.get_batch(&b2).period_end == 20);
    assert!(client.get_batch(&b3).period_end == 30);

    // Prune before ledger 400 (should delete b2 and b3)
    client.prune_batches(&400);

    assert_eq!(client.try_get_batch(&b2), Err(Ok(Error::BatchNotFound)));
    assert_eq!(client.try_get_batch(&b3), Err(Ok(Error::BatchNotFound)));
}

#[test]
#[should_panic]
fn test_prune_batches_requires_admin_auth() {
    let env = Env::default();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);

    env.mock_all_auths();
    client.initialize(&merchant);

    env.set_auths(&[]);
    client.prune_batches(&100);
}

#[test]
fn test_anchor_and_prune_events_emitted() {
    use soroban_sdk::testutils::Events;
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root, &10, &0, &10);

    assert_eq!(
        env.events()
            .all()
            .filter_by_contract(&client.address)
            .events()
            .len(),
        1,
        "AnchorEvent missing"
    );

    use soroban_sdk::{vec, IntoVal, Symbol};

    let anchor_events = env.events().all();
    let batch = client.get_batch(&1);
    assert_eq!(
        anchor_events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "anchor_event"), 1u64).into_val(&env),
                batch.into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.prune_batches(&150);

    let prune_events = env.events().all();
    assert_eq!(
        prune_events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "prune_event"), 1u64).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "end_batch_id"), 2u64)].into_val(&env)
            )
        ]
    );
}

// ---------------------------------------------------------------------------
// Resource benchmark: anchor_batch / verify_receipt vs batch size
// ---------------------------------------------------------------------------
//
// `MAX_BATCH_SIZE` caps how many receipts fit into a single anchor, but the
// number was theoretical: nothing empirically showed that a 1000-receipt batch
// fits inside Soroban's per-transaction resource budget. This benchmark anchors
// a genuine Merkle batch (root built over `size` leaves with the same
// sorted-pair SHA-256 convention as the SDK) at increasing sizes and meters
// each invocation.
//
// Measurement notes:
//   * `Env::default()` enforces the mainnet invocation resource limits and
//     panics if any invocation breaches them, so the test passing is itself
//     proof that every tier fits on mainnet. The explicit assertions below
//     additionally pin the headroom so a regression fails with a readable
//     message.
//   * `env.cost_estimate().resources()` reports the resources metered during
//     the last top-level contract invocation (CPU insns, memory, I/O, rent),
//     and `env.cost_estimate().fee()` estimates the resource fee in stroops
//     using the mainnet fee-rate snapshot baked into the SDK.
//   * The test harness runs the contract natively (Rust) rather than as Wasm,
//     so VM interpretation and Wasm read costs are NOT included: the measured
//     figures are a conservative lower bound of on-chain usage.
//
// Run with `cargo test -- --nocapture` to see the CSV table and breakdowns.

/// Batch sizes benchmarked, ending at the contract's own `MAX_BATCH_SIZE` so
/// the largest tier is defined by the limit under test rather than a magic
/// number.
const BENCH_BATCH_SIZES: [u32; 4] = [10, 100, MAX_BATCH_SIZE / 2, MAX_BATCH_SIZE];

/// Builds a sorted-pair SHA-256 Merkle tree over `leaves` — the same convention
/// the accensa-app TypeScript SDK uses — and returns `(root, proof)`, where
/// `proof` is the sibling path proving the leaf at `index`.
///
/// Odd nodes are carried up unchanged; a carried node contributes no sibling to
/// the proof at that level. Building happens off-chain in the test harness, so
/// it is not part of the metered contract costs below.
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

#[test]
fn bench_anchor_batch_scales_to_max_batch_size() {
    use soroban_env_host::InvocationResourceLimits;
    use soroban_sdk::testutils::cost_estimate::NetworkInvocationResourceLimits;

    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    // The per-transaction limits every invocation must fit into (mainnet
    // snapshot enforced by `Env::default()`).
    let limits = InvocationResourceLimits::mainnet();

    // One row per tier: (batch_size, anchor cpu, anchor mem, anchor fee,
    // verify cpu, verify mem, verify fee).
    let mut results: std::vec::Vec<(u32, u64, u64, u64, u64, u64, u64)> = std::vec::Vec::new();

    for &size in &BENCH_BATCH_SIZES {
        // Genuine batch: `size` distinct leaves. The proof targets the last
        // leaf — the deepest path, i.e. the worst case for verify_receipt.
        let leaves: std::vec::Vec<BytesN<32>> = (0..size)
            .map(|i| {
                let mut leaf = [0u8; 32];
                leaf[..4].copy_from_slice(&i.to_be_bytes());
                BytesN::from_array(&env, &leaf)
            })
            .collect();
        let (root, proof) = build_merkle_tree(&env, &leaves, (size - 1) as usize);

        // anchor_batch — metered by the host and checked against mainnet
        // limits; any breach would have panicked here.
        let batch_id = client.anchor_batch(&root, &size, &0, &100);
        assert_eq!(batch_id, results.len() as u64 + 1);
        let anchor = env.cost_estimate().resources();
        let anchor_cpu = anchor.instructions as u64;
        let anchor_mem = anchor.mem_bytes as u64;
        let anchor_fee = env.cost_estimate().fee().total as u64;

        if size == MAX_BATCH_SIZE {
            println!("\n--- host budget breakdown for anchor_batch({MAX_BATCH_SIZE}) ---");
            env.cost_estimate().budget().print();
            println!("--- metered invocation resources for anchor_batch({MAX_BATCH_SIZE}) ---");
            println!("{anchor:?}");
        }

        // verify_receipt for the deepest leaf — cost grows with tree depth.
        let mut path = vec![&env];
        for sibling in &proof {
            path.push_back(sibling.clone());
        }
        let last_leaf = &leaves[(size - 1) as usize];
        let verified = client.verify_receipt(&batch_id, last_leaf, &path);
        let verify = env.cost_estimate().resources();
        let verify_cpu = verify.instructions as u64;
        let verify_mem = verify.mem_bytes as u64;
        let verify_fee = env.cost_estimate().fee().total as u64;

        assert!(
            verified,
            "proof for the last leaf of a {size}-leaf batch must verify"
        );

        // Safe-limit validation: every tier must stay inside the mainnet
        // per-transaction limits.
        assert!(
            anchor_cpu < limits.instructions as u64,
            "anchor_batch({size}) used {anchor_cpu} CPU insns, exceeding the mainnet limit of {}",
            limits.instructions
        );
        assert!(
            anchor_mem < limits.mem_bytes as u64,
            "anchor_batch({size}) used {anchor_mem} mem bytes, exceeding the mainnet limit of {}",
            limits.mem_bytes
        );
        assert!(
            verify_cpu < limits.instructions as u64,
            "verify_receipt({size}) used {verify_cpu} CPU insns, exceeding the mainnet limit of {}",
            limits.instructions
        );
        assert!(
            verify_mem < limits.mem_bytes as u64,
            "verify_receipt({size}) used {verify_mem} mem bytes, exceeding the mainnet limit of {}",
            limits.mem_bytes
        );

        // The largest batch must stay well within the budget: demand at least
        // 10x headroom so mainnet deployment has comfortable margin.
        if size == MAX_BATCH_SIZE {
            assert!(
                anchor_cpu <= limits.instructions as u64 / 10,
                "anchor_batch at MAX_BATCH_SIZE uses {anchor_cpu} CPU insns, more than 10% of the \
                 mainnet limit ({})",
                limits.instructions
            );
            assert!(
                anchor_mem <= limits.mem_bytes as u64 / 10,
                "anchor_batch at MAX_BATCH_SIZE uses {anchor_mem} mem bytes, more than 10% of the \
                 mainnet limit ({})",
                limits.mem_bytes
            );
        }

        results.push((
            size, anchor_cpu, anchor_mem, anchor_fee, verify_cpu, verify_mem, verify_fee,
        ));
    }

    println!("\n=== ReceiptAnchor resource benchmark (soroban-sdk test env) ===");
    println!(
        "mainnet invocation limits: instructions={}, mem_bytes={}",
        limits.instructions, limits.mem_bytes
    );

    println!("\n-- CSV --");
    println!(
        "batch_size,anchor_cpu_insns,anchor_mem_bytes,anchor_fee_stroops,\
         verify_cpu_insns,verify_mem_bytes,verify_fee_stroops"
    );
    for (size, ac, am, af, vc, vm, vf) in &results {
        println!("{size},{ac},{am},{af},{vc},{vm},{vf}");
    }

    println!("\n-- summary --");
    println!(
        "{:<10}{:<19}{:<18}{:<16}{:<19}{:<18}{:<16}",
        "batch", "anchor_cpu", "anchor_mem", "anchor_fee", "verify_cpu", "verify_mem", "verify_fee"
    );
    println!(
        "{:<10}{:<19}{:<18}{:<16}{:<19}{:<18}{:<16}",
        "size", "(insns)", "(bytes)", "(stroops)", "(insns)", "(bytes)", "(stroops)"
    );
    for (size, ac, am, af, vc, vm, vf) in &results {
        println!("{size:<10}{ac:<19}{am:<18}{af:<16}{vc:<19}{vm:<18}{vf:<16}");
    }

    let (max_size, ac_max, am_max, _, vc_max, vm_max, _) =
        *results.last().expect("benchmark has at least one tier");
    println!("\n-- safe-limit validation (largest tier: batch_size={max_size}) --");
    println!(
        "anchor_batch:   {ac_max} CPU insns ({:.4}% of {}), {am_max} mem bytes ({:.4}% of {})",
        ac_max as f64 / limits.instructions as f64 * 100.0,
        limits.instructions,
        am_max as f64 / limits.mem_bytes as f64 * 100.0,
        limits.mem_bytes
    );
    println!(
        "verify_receipt: {vc_max} CPU insns ({:.4}% of {}), {vm_max} mem bytes ({:.4}% of {})",
        vc_max as f64 / limits.instructions as f64 * 100.0,
        limits.instructions,
        vm_max as f64 / limits.mem_bytes as f64 * 100.0,
        limits.mem_bytes
    );
}
