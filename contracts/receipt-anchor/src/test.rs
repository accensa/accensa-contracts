#![cfg(test)]

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
fn test_anchor_batch_enforces_max_size() {
    let (env, client, merchant) = setup();
    client.initialize(&merchant);

    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_anchor_batch(&root, &1001, &0, &50),
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
    extern crate std;
    let emitted = std::format!("{:?}", env.events().all().events().last().unwrap());
    assert!(emitted.contains("StringM(anchor_event)"), "Expected anchor_event, got {:?}", emitted);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    client.prune_batches(&150);

    assert_eq!(
        env.events()
            .all()
            .filter_by_contract(&client.address)
            .events()
            .len(),
        1,
        "PruneEvent missing"
    );
    let emitted = std::format!("{:?}", env.events().all().events().last().unwrap());
    assert!(emitted.contains("StringM(prune_event)"), "Expected prune_event, got {:?}", emitted);
}
