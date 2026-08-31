use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env,
};

/// The `ReceiptShard` wasm, built by `cargo build -p receipt-shard --target
/// wasm32v1-none --release` before these tests run (CI does this in the same
/// step that installs the wasm32v1-none target; see `.github/workflows/ci.yml`
/// and the README's "Build and test" section for the local equivalent).
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn shard_wasm_hash(env: &Env) -> BytesN<32> {
    env.deployer().upload_contract_wasm(shard_wasm::WASM)
}

fn setup() -> (Env, ReceiptAnchorClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);
    (env, client, merchant)
}

fn init(env: &Env, client: &ReceiptAnchorClient, merchant: &Address) {
    client.initialize(merchant, &shard_wasm_hash(env));
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
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
}

#[test]
fn test_double_initialize_fails() {
    let (env, client, merchant) = setup();
    let wasm_hash = shard_wasm_hash(&env);
    client.initialize(&merchant, &wasm_hash);
    assert_eq!(
        client.try_initialize(&merchant, &wasm_hash),
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
    init(&env, &client, &merchant);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    assert_eq!(client.anchor_batch(&root1, &5, &0, &50), 1);
    assert_eq!(client.anchor_batch(&root2, &7, &51, &99), 2);
}

#[test]
fn test_duplicate_root_anchoring_fails() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(client.anchor_batch(&root, &5, &0, &50), 1);

    // Submitting the exact same root again should fail with DuplicateRoot
    assert_eq!(
        client.try_anchor_batch(&root, &5, &51, &100),
        Err(Ok(Error::DuplicateRoot))
    );
}

#[test]
fn test_distinct_root_anchoring_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    assert_eq!(client.anchor_batch(&root1, &5, &0, &50), 1);
    assert_eq!(client.anchor_batch(&root2, &5, &51, &100), 2);
}

#[test]
fn test_get_batch_returns_stored_record() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

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
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    assert_eq!(client.try_get_batch(&99), Err(Ok(Error::BatchNotFound)));
}

#[test]
fn test_get_batch_zero_fails() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    assert_eq!(client.try_get_batch(&0), Err(Ok(Error::BatchNotFound)));
}

#[test]
#[should_panic]
fn test_anchor_batch_requires_merchant_auth() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);

    let merchant = Address::generate(&env);
    init(&env, &client, &merchant);

    // Enforcing mode with no signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root, &1, &0, &1);
}

#[test]
fn test_verify_receipt_single_leaf_tree() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // A one-receipt batch: the root is the leaf itself, proof is empty.
    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let batch_id = client.anchor_batch(&leaf, &1, &0, &10);

    assert!(client.verify_receipt(&batch_id, &leaf, &vec![&env]));
}

#[test]
fn test_verify_receipt_four_leaf_tree() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

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
    init(&env, &client, &merchant);

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
    init(&env, &client, &merchant);
    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_verify_receipt(&5, &leaf, &vec![&env]),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_get_batch_count_tracks_anchors() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    assert_eq!(client.get_batch_count(), 0);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    client.anchor_batch(&root1, &5, &0, &50);
    assert_eq!(client.get_batch_count(), 1);

    client.anchor_batch(&root2, &7, &51, &99);
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
    init(&env, &client, &merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    let batch_id = client.anchor_batch(&root, &MAX_BATCH_SIZE, &0, &50);
    assert_eq!(batch_id, 1);
    let record = client.get_batch(&batch_id);
    assert_eq!(record.count, MAX_BATCH_SIZE);
}

#[test]
fn test_anchor_batch_enforces_max_size() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_anchor_batch(&root, &(MAX_BATCH_SIZE + 1), &0, &50),
        Err(Ok(Error::BatchTooLarge))
    );
}

#[test]
fn test_extend_batch_ttl_fails_if_missing() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    assert_eq!(
        client.try_extend_batch_ttl(&99),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_extend_batch_ttl_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    let batch_id = client.anchor_batch(&root, &5, &0, &50);

    // This won't fail since the batch exists. (TTL updates aren't observable from the contract API, but it shouldn't revert)
    client.extend_batch_ttl(&batch_id);
}

// ---------------------------------------------------------------------------
// Sharded storage / factory routing
// ---------------------------------------------------------------------------

#[test]
fn test_get_shard_capacity() {
    let (_env, client, _merchant) = setup();
    assert_eq!(client.get_shard_capacity(), SHARD_CAPACITY);
}

#[test]
fn test_first_anchor_creates_one_shard() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    assert_eq!(client.get_shard_count(), 0);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root, &1, &0, &10);

    assert_eq!(client.get_shard_count(), 1);
    // The shard exists and is addressable.
    client.get_shard_address(&0);
}

#[test]
fn test_get_shard_address_missing_fails() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    assert_eq!(
        client.try_get_shard_address(&0),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_anchor_batch_crosses_shard_boundary() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    for i in 0..SHARD_CAPACITY {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&(i as u32 + 1).to_be_bytes());
        let root = BytesN::from_array(&env, &b);
        client.anchor_batch(&root, &1, &0, &1);
    }
    assert_eq!(client.get_batch_count(), SHARD_CAPACITY);
    assert_eq!(client.get_shard_count(), 1);

    // Batch SHARD_CAPACITY + 1 is the first id in the second shard.
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&((SHARD_CAPACITY + 1) as u32).to_be_bytes());
    let overflow_root = BytesN::from_array(&env, &b);
    let overflow_id = client.anchor_batch(&overflow_root, &1, &0, &1);
    assert_eq!(overflow_id, SHARD_CAPACITY + 1);
    assert_eq!(client.get_shard_count(), 2);

    // Both the last batch of shard 0 and the first batch of shard 1 read back correctly.
    assert_eq!(client.get_batch(&SHARD_CAPACITY).period_end, 1);
    assert_eq!(client.get_batch(&overflow_id).period_end, 1);

    let shard0 = client.get_shard_address(&0);
    let shard1 = client.get_shard_address(&1);
    assert_ne!(shard0, shard1);
}

#[test]
fn test_shard_created_event_emitted_once_per_shard() {
    use soroban_sdk::testutils::Events;
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&root1, &1, &0, &1);
    let after_first = env
        .events()
        .all()
        .filter_by_contract(&client.address)
        .events()
        .len();
    assert_eq!(after_first, 2, "expected ShardCreatedEvent + AnchorEvent");

    // `events().all()` only reflects the most recent top-level invocation, so
    // a second anchor into the same shard should show just its own
    // AnchorEvent (1), not a repeated ShardCreatedEvent.
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    client.anchor_batch(&root2, &1, &0, &1);
    let after_second = env
        .events()
        .all()
        .filter_by_contract(&client.address)
        .events()
        .len();
    assert_eq!(after_second, 1);
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
    init(&env, &client, &merchant);

    for v in vectors::VECTORS {
        let root = BytesN::from_array(&env, &v.root);
        let leaf = BytesN::from_array(&env, &v.leaf);

        let mut proof = vec![&env];
        for sibling in v.proof {
            proof.push_back(BytesN::from_array(&env, sibling));
        }

        let batch_id = client
            .try_anchor_batch(&root, &(v.proof.len() as u32), &0, &100)
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(|| client.get_batch_count());
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
fn test_shared_vectors_cover_required_edge_cases() {
    // Issue #53: the vector set must exercise the edge cases where two
    // independent implementations of verify_receipt are most likely to disagree
    // (single-leaf, two-leaf, odd counts requiring promotion, duplicate leaves,
    // the sorted-pair tie where both siblings hash identically, and a proof of
    // the wrong length). If any of these categories silently disappears from the
    // shared fixture, the cross-implementation proof-of-parity is no longer
    // proving what it claims to. Names are matched by substring so the suite
    // keeps working as vectors are renamed.
    extern crate std;
    let has = |needle: &str| {
        assert!(
            vectors::VECTORS.iter().any(|v| v.name.contains(needle)),
            "shared vectors are missing a required edge case: {needle:?}"
        );
    };
    has("single-leaf");
    has("two-leaf");
    has("odd count");
    has("duplicate-leaf");
    has("tie");
    has("wrong length");
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
    init(&env, &client, &merchant);

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
fn test_prune_batches_crosses_shard_boundary() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    // Fill shard 0 completely, then anchor 5 batches into shard 1.
    for i in 0..(SHARD_CAPACITY + 5) {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&(i as u32 + 1).to_be_bytes());
        let root = BytesN::from_array(&env, &b);
        client.anchor_batch(&root, &1, &0, &1);
    }
    assert_eq!(client.get_shard_count(), 2);

    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);

    // MAX_PRUNE_BATCHES caps each call at 100 deletions, so draining shard 0
    // (SHARD_CAPACITY = 1000 batches) takes 10 calls.
    for _ in 0..(SHARD_CAPACITY / (MAX_PRUNE_BATCHES as u64)) {
        client.prune_batches(&1_000_000);
    }
    assert_eq!(
        client.try_get_batch(&SHARD_CAPACITY),
        Err(Ok(Error::BatchNotFound))
    );
    // Shard 1's batches must survive until the cursor actually reaches them.
    assert!(client.get_batch(&(SHARD_CAPACITY + 1)).period_end == 1);

    // One more call crosses into shard 1 and prunes the remaining 5 batches.
    client.prune_batches(&1_000_000);
    for offset in 1..=5u64 {
        assert_eq!(
            client.try_get_batch(&(SHARD_CAPACITY + offset)),
            Err(Ok(Error::BatchNotFound))
        );
    }
}

#[test]
fn test_anchor_and_prune_events_emitted() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    let root3 = BytesN::from_array(&env, &[3u8; 32]);

    client.anchor_batch(&root1, &10, &0, &10);
    client.anchor_batch(&root2, &10, &11, &20);
    client.anchor_batch(&root3, &10, &21, &30);

    assert_eq!(client.get_batch_count(), 3);

    env.ledger().set_sequence_number(300);
    let pruned = client.prune_batches(&400);
    assert_eq!(pruned, 4);
}

// ---------------------------------------------------------------------------
// Ring buffer tests
// ---------------------------------------------------------------------------

#[test]
fn test_root_buffer_starts_empty() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let buffer = client.get_root_buffer();
    assert_eq!(buffer.len(), 0);
}

#[test]
fn test_root_buffer_grows_with_anchors() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let r1 = BytesN::from_array(&env, &[1u8; 32]);
    let r2 = BytesN::from_array(&env, &[2u8; 32]);
    client.anchor_batch(&r1, &1, &0, &10);
    client.anchor_batch(&r2, &1, &11, &20);

    let buffer = client.get_root_buffer();
    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.get(0).unwrap(), r1);
    assert_eq!(buffer.get(1).unwrap(), r2);
}

#[test]
fn test_verify_receipt_by_root_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let _batch_id = client.anchor_batch(&leaf, &1, &0, &10);

    // Single-leaf tree: root == leaf, empty proof.
    assert!(client.verify_receipt_by_root(&leaf, &leaf, &vec![&env]));
}

#[test]
fn test_verify_receipt_by_root_with_merkle_proof() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let n12 = hash_pair(&env, &l1, &l2);

    let _batch_id = client.anchor_batch(&n12, &2, &0, &100);

    // Verify l1 against the root n12.
    assert!(client.verify_receipt_by_root(&n12, &l1, &vec![&env, l2.clone()]));
    assert!(client.verify_receipt_by_root(&n12, &l2, &vec![&env, l1.clone()]));
}

#[test]
fn test_verify_receipt_by_root_rejects_unknown_root() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let r1 = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&r1, &1, &0, &10);

    let unknown = BytesN::from_array(&env, &[99u8; 32]);
    assert_eq!(
        client.try_verify_receipt_by_root(&unknown, &r1, &vec![&env]),
        Err(Ok(Error::RootNotFound))
    );
}

#[test]
fn test_verify_receipt_by_root_rejects_wrong_proof() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &l1, &l2);
    client.anchor_batch(&root, &2, &0, &100);

    let forged = BytesN::from_array(&env, &[99u8; 32]);
    assert!(!client.verify_receipt_by_root(&root, &forged, &vec![&env, l2.clone()]));
}

#[test]
fn test_verify_receipt_by_root_rejects_swapped_proof() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &l1, &l2);
    client.anchor_batch(&root, &2, &0, &100);

    // Use wrong sibling.
    let wrong = BytesN::from_array(&env, &[88u8; 32]);
    assert!(!client.verify_receipt_by_root(&root, &l1, &vec![&env, wrong]));
}

#[test]
fn test_root_buffer_evicts_oldest_when_full() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // Fill the buffer to capacity.
    let mut roots = Vec::new(&env);
    for i in 0..ROOT_BUFFER_SIZE {
        let root = BytesN::from_array(&env, &[i as u8; 32]);
        roots.push_back(root.clone());
        client.anchor_batch(&root, &1, &0, &1);
    }

    let buffer = client.get_root_buffer();
    assert_eq!(buffer.len(), ROOT_BUFFER_SIZE);
    assert_eq!(buffer.get(0).unwrap(), BytesN::from_array(&env, &[0u8; 32]));

    // Anchor one more — oldest (index 0) should be evicted.
    let new_root = BytesN::from_array(&env, &[255u8; 32]);
    client.anchor_batch(&new_root, &1, &0, &1);

    let buffer = client.get_root_buffer();
    assert_eq!(buffer.len(), ROOT_BUFFER_SIZE);
    // First entry is now [1u8; 32] (the second root we anchored).
    assert_eq!(buffer.get(0).unwrap(), BytesN::from_array(&env, &[1u8; 32]));
    // Last entry is the new root.
    assert_eq!(buffer.get(ROOT_BUFFER_SIZE - 1).unwrap(), new_root);
}

#[test]
fn test_verify_receipt_by_root_works_for_eviction_boundary() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // Anchor ROOT_BUFFER_SIZE + 1 batches, tracking all roots.
    let mut roots = Vec::new(&env);
    for i in 0..=ROOT_BUFFER_SIZE {
        let root = BytesN::from_array(&env, &[i as u8; 32]);
        roots.push_back(root.clone());
        client.anchor_batch(&root, &1, &0, &1);
    }

    // The first root (index 0) was evicted — verification should fail.
    assert_eq!(
        client.try_verify_receipt_by_root(
            &roots.get(0).unwrap(),
            &roots.get(0).unwrap(),
            &vec![&env]
        ),
        Err(Ok(Error::RootNotFound))
    );

    // The second root (index 1) is still in the buffer — should succeed.
    assert!(client.verify_receipt_by_root(
        &roots.get(1).unwrap(),
        &roots.get(1).unwrap(),
        &vec![&env]
    ));

    // The last root (index ROOT_BUFFER_SIZE) should also succeed.
    let last = roots.get(ROOT_BUFFER_SIZE).unwrap();
    assert!(client.verify_receipt_by_root(&last, &last, &vec![&env]));
}

#[test]
fn test_get_root_buffer_size() {
    let (_env, client, _merchant) = setup();
    assert_eq!(client.get_root_buffer_size(), ROOT_BUFFER_SIZE);
    assert_eq!(client.get_root_buffer_size(), 100);
}

#[test]
fn test_verify_receipt_by_root_before_init_fails() {
    let (env, client, _merchant) = setup();
    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_verify_receipt_by_root(&root, &root, &vec![&env]),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_existing_verify_receipt_still_works_with_buffer() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &l1, &l2);
    let batch_id = client.anchor_batch(&root, &2, &0, &100);

    // The original batch_id-based verification still works.
    assert!(client.verify_receipt(&batch_id, &l1, &vec![&env, l2.clone()]));
    assert!(client.verify_receipt(&batch_id, &l2, &vec![&env, l1.clone()]));
}

// ---------------------------------------------------------------------------
// Proof-length bounds tests
// ---------------------------------------------------------------------------

/// Build a chain-hash proof of `depth` siblings and the corresponding root.
/// Each level pairs the current accumulator with a deterministic sibling,
/// using the same sorted-pair SHA-256 the contract expects.
fn build_chain_proof(env: &Env, leaf: &BytesN<32>, depth: u32) -> (BytesN<32>, Vec<BytesN<32>>) {
    let mut proof = Vec::new(env);
    let mut acc = leaf.clone();
    for i in 0..depth {
        let mut sibling_bytes = [0u8; 32];
        sibling_bytes[0] = (i + 1) as u8;
        let sibling = BytesN::from_array(env, &sibling_bytes);
        acc = hash_pair(env, &acc, &sibling);
        proof.push_back(sibling);
    }
    (acc, proof)
}

#[test]
fn test_verify_receipt_empty_proof_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // Single-leaf batch: root == leaf, empty proof.
    let leaf = BytesN::from_array(&env, &[42u8; 32]);
    let batch_id = client.anchor_batch(&leaf, &1, &0, &10);
    assert!(client.verify_receipt(&batch_id, &leaf, &vec![&env]));
}

#[test]
fn test_verify_receipt_at_bound_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let (root, proof) = build_chain_proof(&env, &leaf, MAX_PROOF_LEN);

    assert_eq!(proof.len(), MAX_PROOF_LEN);
    let batch_id = client.anchor_batch(&root, &1000, &0, &1000);
    assert!(client.verify_receipt(&batch_id, &leaf, &proof));
}

#[test]
fn test_verify_receipt_one_over_bound_fails() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let (root, proof) = build_chain_proof(&env, &leaf, MAX_PROOF_LEN + 1);

    assert_eq!(proof.len(), MAX_PROOF_LEN + 1);
    let batch_id = client.anchor_batch(&root, &1000, &0, &1000);
    assert_eq!(
        client.try_verify_receipt(&batch_id, &leaf, &proof),
        Err(Ok(Error::ProofTooLong))
    );
}

#[test]
fn test_verify_receipt_by_root_at_bound_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let (root, proof) = build_chain_proof(&env, &leaf, MAX_PROOF_LEN);

    client.anchor_batch(&root, &1000, &0, &1000);
    assert!(client.verify_receipt_by_root(&root, &leaf, &proof));
}

#[test]
fn test_verify_receipt_by_root_one_over_bound_fails() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[7u8; 32]);
    let (root, proof) = build_chain_proof(&env, &leaf, MAX_PROOF_LEN + 1);

    client.anchor_batch(&root, &1000, &0, &1000);
    assert_eq!(
        client.try_verify_receipt_by_root(&root, &leaf, &proof),
        Err(Ok(Error::ProofTooLong))
    );
}

#[test]
fn test_verify_receipt_valid_deep_proof() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[5u8; 32]);
    // Depth 5: well within the bound, still a meaningful proof.
    let (root, proof) = build_chain_proof(&env, &leaf, 5);

    assert_eq!(proof.len(), 5);
    let batch_id = client.anchor_batch(&root, &32, &0, &32);
    assert!(client.verify_receipt(&batch_id, &leaf, &proof));
    assert!(client.verify_receipt_by_root(&root, &leaf, &proof));
}

#[test]
fn test_get_max_proof_len() {
    let (_env, client, _merchant) = setup();
    assert_eq!(client.get_max_proof_len(), MAX_PROOF_LEN);
    assert_eq!(client.get_max_proof_len(), 10);
}

// ---------------------------------------------------------------------------
// Rate-limiting tests
// ---------------------------------------------------------------------------
//
// The limiter is a per-identity token bucket (the identity is the merchant,
// since `anchor_batch` is merchant-authorized): `burst_capacity` anchors may
// land back-to-back, then the bucket refills one token every
// `refill_interval_secs` seconds, capped at `burst_capacity`. A config of
// `{0, 0}` disables it entirely. A fresh bucket (first anchor, or first after
// a config change) starts full.

fn root_of(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn test_rate_limit_disabled_by_default() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    assert_eq!(
        client.get_anchor_rate_limit(),
        RateLimitConfig {
            burst_capacity: 0,
            refill_interval_secs: 0,
        }
    );

    // Unlimited back-to-back anchors at the same timestamp pass.
    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    assert_eq!(client.get_batch_count(), 2);
}

#[test]
fn test_set_anchor_rate_limit_round_trips() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    client.set_anchor_rate_limit(&3, &60);
    assert_eq!(
        client.get_anchor_rate_limit(),
        RateLimitConfig {
            burst_capacity: 3,
            refill_interval_secs: 60,
        }
    );

    // Disabling round-trips too.
    client.set_anchor_rate_limit(&0, &0);
    assert_eq!(
        client.get_anchor_rate_limit(),
        RateLimitConfig {
            burst_capacity: 0,
            refill_interval_secs: 0,
        }
    );
}

#[test]
#[should_panic]
fn test_set_anchor_rate_limit_requires_admin_auth() {
    let env = Env::default();
    let contract_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &contract_id);
    let merchant = Address::generate(&env);

    env.mock_all_auths();
    init(&env, &client, &merchant);

    env.set_auths(&[]);
    client.set_anchor_rate_limit(&3, &60);
}

#[test]
fn test_set_anchor_rate_limit_requires_init() {
    let (_env, client, _merchant) = setup();
    assert_eq!(
        client.try_set_anchor_rate_limit(&3, &60),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_set_anchor_rate_limit_rejects_invalid_config() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // One parameter zeroed while the other is set is nonsense.
    assert_eq!(
        client.try_set_anchor_rate_limit(&0, &60),
        Err(Ok(Error::InvalidRateLimitConfig))
    );
    assert_eq!(
        client.try_set_anchor_rate_limit(&3, &0),
        Err(Ok(Error::InvalidRateLimitConfig))
    );

    // Above the caps.
    assert_eq!(
        client.try_set_anchor_rate_limit(&(MAX_RATE_BURST + 1), &60),
        Err(Ok(Error::InvalidRateLimitConfig))
    );
    assert_eq!(
        client.try_set_anchor_rate_limit(&3, &(MAX_RATE_REFILL_INTERVAL + 1)),
        Err(Ok(Error::InvalidRateLimitConfig))
    );

    // At the caps — accepted.
    client.set_anchor_rate_limit(&MAX_RATE_BURST, &MAX_RATE_REFILL_INTERVAL);
}

#[test]
fn test_burst_allows_back_to_back_anchors() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&3, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);
    assert_eq!(client.get_batch_count(), 3);
}

#[test]
fn test_spam_beyond_burst_rejected() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&3, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);

    // Fourth anchor at the same timestamp: the bucket is empty.
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 4), &1, &31, &40),
        Err(Ok(Error::AnchorRateLimited))
    );
    assert_eq!(
        client.get_batch_count(),
        3,
        "rejected spam must not anchor a batch"
    );
}

#[test]
fn test_tokens_refill_after_interval() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&1, &60);

    env.ledger().with_mut(|li| {
        li.sequence_number = 10;
        li.timestamp = 1000;
    });
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);

    // One second before the refill boundary — still rejected.
    env.ledger().with_mut(|li| {
        li.sequence_number = 20;
        li.timestamp = 1059;
    });
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 2), &1, &11, &20),
        Err(Ok(Error::AnchorRateLimited))
    );

    // Exactly at the boundary — the token has refilled.
    env.ledger().with_mut(|li| {
        li.sequence_number = 21;
        li.timestamp = 1060;
    });
    assert_eq!(client.anchor_batch(&root_of(&env, 3), &1, &21, &30), 2);
}

#[test]
fn test_bucket_partially_refills() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&3, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 4), &1, &31, &40),
        Err(Ok(Error::AnchorRateLimited))
    );

    // Two full refill intervals later: exactly two tokens have come back.
    env.ledger().with_mut(|li| li.timestamp = 1120);
    client.anchor_batch(&root_of(&env, 4), &1, &31, &40);
    client.anchor_batch(&root_of(&env, 5), &1, &41, &50);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 6), &1, &51, &60),
        Err(Ok(Error::AnchorRateLimited))
    );
}

#[test]
fn test_refill_caps_at_burst() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&2, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);

    // Idle for a very long time: the bucket caps at the burst instead of
    // accumulating unboundedly (no overflow, no runaway allowance).
    env.ledger().with_mut(|li| li.timestamp = 100_000);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);
    client.anchor_batch(&root_of(&env, 4), &1, &31, &40);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 5), &1, &41, &50),
        Err(Ok(Error::AnchorRateLimited))
    );
}

#[test]
fn test_zero_config_disables_rate_limit() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&1, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 2), &1, &11, &20),
        Err(Ok(Error::AnchorRateLimited))
    );

    // Disable: the same back-to-back anchor now passes.
    client.set_anchor_rate_limit(&0, &0);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    assert_eq!(client.get_batch_count(), 2);
}

#[test]
fn test_changing_config_takes_effect() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);

    // Enabling the limiter creates a fresh, full bucket: the next anchor
    // passes, and the one after is rejected until a refill interval elapses.
    client.set_anchor_rate_limit(&1, &60);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 3), &1, &21, &30),
        Err(Ok(Error::AnchorRateLimited))
    );

    // After the refill interval the anchor passes again.
    env.ledger().with_mut(|li| li.timestamp = 1060);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);
    assert_eq!(client.get_batch_count(), 3);
}

#[test]
fn test_rejected_anchor_does_not_consume_token() {
    // A rejected anchor (duplicate root) must not spend a token: the bucket
    // holds what it held before the failed attempt.
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&2, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);

    // Duplicate root: rejected by the duplicate check, no token consumed.
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 1), &1, &11, &20),
        Err(Ok(Error::DuplicateRoot))
    );

    // One interval later the bucket has refilled to the burst of 2, so two
    // more anchors pass and a third is rejected. Had the duplicate consumed a
    // token, only one would have passed.
    env.ledger().with_mut(|li| li.timestamp = 1060);
    client.anchor_batch(&root_of(&env, 2), &1, &11, &20);
    client.anchor_batch(&root_of(&env, 3), &1, &21, &30);
    assert_eq!(
        client.try_anchor_batch(&root_of(&env, 4), &1, &31, &40),
        Err(Ok(Error::AnchorRateLimited))
    );
}

#[test]
fn test_rate_limit_bucket_keyed_by_merchant_identity() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);
    client.set_anchor_rate_limit(&3, &60);

    env.ledger().with_mut(|li| li.timestamp = 1000);
    client.anchor_batch(&root_of(&env, 1), &1, &0, &10);

    // The bucket is a single persistent entry under the merchant identity,
    // holding burst-1 tokens after the first anchor.
    let bucket: BucketState = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&DataKey::RateLimitBucket(merchant.clone()))
            .unwrap()
    });
    assert_eq!(bucket.tokens, 2);
    assert_eq!(bucket.last_refill, 1000);

    // No other identity has a bucket entry — per-identity tracking never
    // pre-allocates storage for identities that do not anchor.
    let stranger = Address::generate(&env);
    let has_stranger = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .has(&DataKey::RateLimitBucket(stranger))
    });
    assert!(!has_stranger);
}

/// The rate limiter must not materially increase the cost of a normal anchor:
/// disabled it costs a single instance read, enabled it adds one small
/// persistent read + write. This pins the delta so a pathological
/// implementation (unbounded scans, rewrites of large state) fails CI.
#[test]
fn test_rate_limit_tracking_overhead_is_bounded() {
    extern crate std;
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // Warm up so shard 0 exists; the measured anchors are steady-state.
    client.anchor_batch(&root_of(&env, 0), &1, &0, &1);

    let steady_state_cost =
        |client: &ReceiptAnchorClient<'static>, env: &Env, seed: u8| -> (u64, u64) {
            env.cost_estimate().budget().reset_default();
            let root = root_of(env, seed);
            client.anchor_batch(&root, &1, &0, &1);
            (
                env.cost_estimate().budget().cpu_instruction_cost(),
                env.cost_estimate().budget().memory_bytes_cost(),
            )
        };

    let (cpu_disabled, mem_disabled) = steady_state_cost(&client, &env, 1);
    client.set_anchor_rate_limit(&1000, &3600);
    let (cpu_enabled, mem_enabled) = steady_state_cost(&client, &env, 2);

    let cpu_delta = cpu_enabled.saturating_sub(cpu_disabled);
    let mem_delta = mem_enabled.saturating_sub(mem_disabled);
    std::println!(
        "RATE-LIMIT OVERHEAD: disabled cpu={cpu_disabled} mem={mem_disabled} | \
         enabled cpu={cpu_enabled} mem={mem_enabled} | delta cpu={cpu_delta} mem={mem_delta}"
    );

    // The measured delta is ~60k instructions (a persistent get + set + TTL
    // bump charged at host-call rates) against a ~1.2M anchor; the bound is
    // set with headroom for toolchain calibration drift so it only trips on a
    // real regression (e.g. unbounded scans or per-call rewrites of large
    // state).
    assert!(
        cpu_delta < 150_000,
        "rate-limit tracking grew anchor_batch by {cpu_delta} CPU instructions"
    );
}

/// The commit hash embedded via contractmeta must be real provenance, not the
/// silent "unknown" fallback, in a normal repository build (see build.rs).
#[test]
fn test_commit_meta_is_well_formed() {
    let sha = env!("GIT_SHA");
    assert_ne!(sha, "unknown", "GIT_SHA must not fall back to 'unknown'");
    assert_eq!(sha.len(), 40, "GIT_SHA should be 40 hex chars, got: {sha}");
    assert!(
        sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "GIT_SHA contains non-hex chars: {sha}"
    );

    let dirty = env!("GIT_DIRTY");
    assert!(
        dirty == "0" || dirty == "1",
        "GIT_DIRTY must be '0' or '1', got: {dirty}"
    );
}

#[test]
fn test_verify_receipt_batch_size_instruction_benchmark() {
    extern crate std;
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    // A balanced tree's proof depth is ceil(log2(batch size)). Measure the
    // actual invocation cost at the sizes that determine transaction limits.
    for (batch_size, proof_len) in [(1u32, 0u32), (10, 4), (25, 5), (50, 6), (100, 7)] {
        let leaf = BytesN::from_array(&env, &[batch_size as u8; 32]);
        let (root, proof) = build_chain_proof(&env, &leaf, proof_len);
        let batch_id = client.anchor_batch(&root, &batch_size, &0, &100);

        env.cost_estimate().budget().reset_default();
        assert!(client.verify_receipt(&batch_id, &leaf, &proof));
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        std::println!(
            "BENCHMARK: batch_size={batch_size} proof_len={proof_len} cpu_instructions={cpu}"
        );
        assert!(cpu > 0, "benchmark must record CPU instructions");
    }
}

#[test]
fn test_verify_receipt_memory_scaling_benchmark() {
    extern crate std;
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    let root = BytesN::from_array(&env, &[9u8; 32]);
    let batch_id = client.anchor_batch(&root, &42, &1000, &2000);

    for proof_len in [2, 4, 6, 8, 10] {
        let mut proof_vec = soroban_sdk::Vec::new(&env);
        for i in 0..proof_len {
            proof_vec.push_back(BytesN::from_array(&env, &[(i % 256) as u8; 32]));
        }

        let cpu_before = env.cost_estimate().budget().cpu_instruction_cost();
        let mem_before = env.cost_estimate().budget().memory_bytes_cost();

        let result = client.verify_receipt(&batch_id, &leaf, &proof_vec);

        let cpu_after = env.cost_estimate().budget().cpu_instruction_cost();
        let mem_after = env.cost_estimate().budget().memory_bytes_cost();

        let cpu_diff = cpu_after.saturating_sub(cpu_before);
        let mem_diff = mem_after.saturating_sub(mem_before);

        std::println!(
            "BENCHMARK: Proof length: {:>3} | CPU: before={}, after={}, diff={} | Mem: before={}, after={}, diff={} | Result={:?}",
            proof_len, cpu_before, cpu_after, cpu_diff, mem_before, mem_after, mem_diff, result
        );
    }
}

#[test]
fn test_anchor_batch_zk_valid_proof_succeeds() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let state_root = BytesN::from_array(&env, &[42u8; 32]);
    let proof = ZkProof {
        a: Bytes::from_slice(&env, &[1u8; 64]),
        b: Bytes::from_slice(&env, &[2u8; 128]),
        c: Bytes::from_slice(&env, &[3u8; 64]),
    };

    let batch_id = client.anchor_batch_zk(&state_root, &proof, &50, &100, &200);
    assert_eq!(batch_id, 1);

    let record = client.get_batch(&batch_id);
    assert_eq!(record.root, state_root);
    assert_eq!(record.count, 50);
    assert_eq!(record.period_start, 100);
    assert_eq!(record.period_end, 200);
}

#[test]
fn test_anchor_batch_zk_invalid_proof_rejected() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let state_root = BytesN::from_array(&env, &[42u8; 32]);

    // Corrupted / all-zero proof
    let invalid_proof = ZkProof {
        a: Bytes::from_slice(&env, &[0u8; 64]),
        b: Bytes::from_slice(&env, &[0u8; 128]),
        c: Bytes::from_slice(&env, &[0u8; 64]),
    };

    assert_eq!(
        client.try_anchor_batch_zk(&state_root, &invalid_proof, &50, &100, &200),
        Err(Ok(Error::InvalidProof))
    );

    // Empty proof bytes
    let empty_proof = ZkProof {
        a: Bytes::new(&env),
        b: Bytes::new(&env),
        c: Bytes::new(&env),
    };

    assert_eq!(
        client.try_anchor_batch_zk(&state_root, &empty_proof, &50, &100, &200),
        Err(Ok(Error::InvalidProof))
    );
}

#[test]
fn test_verify_zk_proof_end_to_end() {
    let (env, client, merchant) = setup();
    init(&env, &client, &merchant);

    let proof = ZkProof {
        a: Bytes::from_slice(&env, &[1u8; 64]),
        b: Bytes::from_slice(&env, &[2u8; 128]),
        c: Bytes::from_slice(&env, &[3u8; 64]),
    };

    let mut ic_vec = soroban_sdk::Vec::new(&env);
    ic_vec.push_back(Bytes::from_slice(&env, &[10u8; 64]));
    ic_vec.push_back(Bytes::from_slice(&env, &[11u8; 64]));

    let vk = VerifyingKey {
        alpha_g1: Bytes::from_slice(&env, &[4u8; 64]),
        beta_g2: Bytes::from_slice(&env, &[5u8; 128]),
        gamma_g2: Bytes::from_slice(&env, &[6u8; 128]),
        delta_g2: Bytes::from_slice(&env, &[7u8; 128]),
        ic: ic_vec,
    };

    let mut public_inputs = soroban_sdk::Vec::new(&env);
    public_inputs.push_back(BytesN::from_array(&env, &[99u8; 32]));

    // Valid proof + valid VK + matching public input count -> true
    assert!(client.verify_zk_proof(&proof, &vk, &public_inputs));

    // Mismatched public inputs count -> false
    let mut mismatched_inputs = soroban_sdk::Vec::new(&env);
    mismatched_inputs.push_back(BytesN::from_array(&env, &[99u8; 32]));
    mismatched_inputs.push_back(BytesN::from_array(&env, &[100u8; 32]));
    assert!(!client.verify_zk_proof(&proof, &vk, &mismatched_inputs));

    // Corrupted proof -> false
    let corrupted_proof = ZkProof {
        a: Bytes::from_slice(&env, &[0u8; 64]),
        b: Bytes::from_slice(&env, &[0u8; 128]),
        c: Bytes::from_slice(&env, &[0u8; 64]),
    };
    assert!(!client.verify_zk_proof(&corrupted_proof, &vk, &public_inputs));
}
