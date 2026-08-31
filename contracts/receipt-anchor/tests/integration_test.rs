#![cfg(test)]

use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env,
};

struct TestEnv<'a> {
    env: Env,
    anchor: ReceiptAnchorClient<'a>,
    merchant: Address,
}

fn setup<'a>() -> TestEnv<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    anchor.initialize(&merchant);

    TestEnv {
        env,
        anchor,
        merchant,
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
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &combined))
        .to_array();
    BytesN::from_array(env, &digest)
}

#[test]
fn test_integration_initialize_anchor_and_read_back() {
    let TestEnv { env, anchor, merchant: _ } = setup();

    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    let sibling = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    let batch_id = anchor.anchor_batch(&root, &2, &0, &100);
    assert_eq!(batch_id, 1);
    assert_eq!(anchor.get_batch_count(), 1);

    let record = anchor.get_batch(&1);
    assert_eq!(record.root, root);
    assert_eq!(record.count, 2);
    assert_eq!(record.period_start, 0);
    assert_eq!(record.period_end, 100);

    let proof = vec![&env, sibling.clone()];
    assert!(anchor.verify_receipt(&1, &leaf, &proof));
}

#[test]
fn test_integration_multiple_batches_and_count() {
    let TestEnv { env, anchor, merchant: _ } = setup();

    assert_eq!(anchor.get_batch_count(), 0);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    let root3 = BytesN::from_array(&env, &[3u8; 32]);

    assert_eq!(anchor.anchor_batch(&root1, &1, &0, &10), 1);
    assert_eq!(anchor.anchor_batch(&root2, &1, &11, &20), 2);
    assert_eq!(anchor.anchor_batch(&root3, &1, &21, &30), 3);

    assert_eq!(anchor.get_batch_count(), 3);

    assert_eq!(anchor.get_batch(&1).root, root1);
    assert_eq!(anchor.get_batch(&2).root, root2);
    assert_eq!(anchor.get_batch(&3).root, root3);
}

#[test]
#[should_panic]
fn test_integration_batch_not_found() {
    let TestEnv { env: _, anchor, merchant: _ } = setup();
    let _ = anchor.get_batch(&999);
}

#[test]
fn test_integration_verify_receipt_against_external_root() {
    let TestEnv { env, anchor, merchant: _ } = setup();

    // Construct a 4-leaf Merkle tree independently in the test
    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let l3 = BytesN::from_array(&env, &[3u8; 32]);
    let l4 = BytesN::from_array(&env, &[4u8; 32]);

    let h12 = hash_pair(&env, &l1, &l2);
    let h34 = hash_pair(&env, &l3, &l4);
    let root = hash_pair(&env, &h12, &h34);

    anchor.anchor_batch(&root, &4, &0, &100);

    // Verify l1 using proof [l2, h34]
    let proof_l1 = vec![&env, l2, h34.clone()];
    assert!(anchor.verify_receipt(&1, &l1, &proof_l1));

    // Verify l3 using proof [l4, h12]
    let proof_l3 = vec![&env, l4, h12];
    assert!(anchor.verify_receipt(&1, &l3, &proof_l3));

    // Verify invalid proof should return false
    let bad_proof = vec![&env, l1, h34];
    assert!(!anchor.verify_receipt(&1, &l2, &bad_proof));
}

#[test]
fn test_integration_prune_batches_round_trip() {
    let TestEnv { env, anchor, merchant: _ } = setup();

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    env.ledger().with_mut(|li| li.sequence_number = 50);
    anchor.anchor_batch(&root1, &1, &0, &40);

    env.ledger().with_mut(|li| li.sequence_number = 150);
    anchor.anchor_batch(&root2, &1, &41, &140);

    assert_eq!(anchor.get_batch_count(), 2);
    assert!(anchor.try_get_batch(&1).is_ok());
    assert!(anchor.try_get_batch(&2).is_ok());

    // Prune batches anchored before ledger 100
    anchor.prune_batches(&100);

    // Batch 1 should be pruned (missing), Batch 2 should remain
    assert!(anchor.try_get_batch(&1).is_err());
    assert!(anchor.try_get_batch(&2).is_ok());
    assert_eq!(anchor.get_batch_count(), 2);
}
