#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Bytes, Env,
};

fn setup() -> (Env, ReceiptShardClient<'static>, Address, u64, u64) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let (start, end) = (1u64, 1001u64);
    let contract_id = env.register(ReceiptShard, (router.clone(), start, end));
    let client = ReceiptShardClient::new(&env, &contract_id);
    (env, client, router, start, end)
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
fn test_constructor_sets_range_and_router() {
    let (_env, client, router, start, end) = setup();
    assert_eq!(client.get_router(), router);
    assert_eq!(client.get_range(), (start, end));
}

#[test]
fn test_anchor_batch_and_get_batch_roundtrip() {
    let (env, client, ..) = setup();
    let root = BytesN::from_array(&env, &[9u8; 32]);
    client.anchor_batch(&1, &root, &42, &1000, &2000);

    let record = client.get_batch(&1);
    assert_eq!(record.root, root);
    assert_eq!(record.count, 42);
    assert_eq!(record.period_start, 1000);
    assert_eq!(record.period_end, 2000);
}

#[test]
fn test_get_batch_missing_fails() {
    let (_env, client, ..) = setup();
    assert_eq!(client.try_get_batch(&1), Err(Ok(Error::BatchNotFound)));
}

#[test]
#[should_panic]
fn test_anchor_batch_requires_router_auth() {
    let (env, client, ..) = setup();
    env.set_auths(&[]);
    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&1, &root, &1, &0, &1);
}

#[test]
#[should_panic(expected = "batch_id out of shard range")]
fn test_anchor_batch_rejects_out_of_range_id() {
    let (env, client, ..) = setup();
    let root = BytesN::from_array(&env, &[1u8; 32]);
    // This shard's range is [1, 1001); 1001 belongs to the next shard.
    client.anchor_batch(&1001, &root, &1, &0, &1);
}

#[test]
fn test_verify_receipt_four_leaf_tree() {
    let (env, client, ..) = setup();

    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let l3 = BytesN::from_array(&env, &[3u8; 32]);
    let l4 = BytesN::from_array(&env, &[4u8; 32]);

    let n12 = hash_pair(&env, &l1, &l2);
    let n34 = hash_pair(&env, &l3, &l4);
    let root = hash_pair(&env, &n12, &n34);

    client.anchor_batch(&1, &root, &4, &0, &100);

    assert!(client.verify_receipt(&1, &l1, &vec![&env, l2.clone(), n34.clone()]));
    assert!(!client.verify_receipt(&1, &l1, &vec![&env, n34.clone(), l2.clone()]));
}

#[test]
fn test_verify_receipt_missing_batch_fails() {
    let (env, client, ..) = setup();
    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(
        client.try_verify_receipt(&5, &leaf, &vec![&env]),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_extend_batch_ttl_fails_if_missing() {
    let (_env, client, ..) = setup();
    assert_eq!(
        client.try_extend_batch_ttl(&1),
        Err(Ok(Error::BatchNotFound))
    );
}

#[test]
fn test_extend_batch_ttl_succeeds() {
    let (env, client, ..) = setup();
    let root = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&1, &root, &5, &0, &50);
    client.extend_batch_ttl(&1);
}

#[test]
fn test_prune_batches_deletes_old_records_within_range() {
    let (env, client, ..) = setup();

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    client.anchor_batch(&1, &root1, &10, &0, &10);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    client.anchor_batch(&2, &root2, &10, &11, &20);

    // Only batches 1..3 have been written; the router passes 3 as the
    // high-water mark so the shard never treats unwritten ids as prunable.
    let (cursor, pruned) = client.prune_batches(&200, &100, &3);
    assert_eq!(pruned, 1);
    assert_eq!(cursor, 2);
    assert_eq!(client.try_get_batch(&1), Err(Ok(Error::BatchNotFound)));
    assert!(client.get_batch(&2).period_end == 20);

    let (cursor2, pruned2) = client.prune_batches(&300, &100, &3);
    assert_eq!(pruned2, 1);
    assert_eq!(cursor2, 3);
    assert_eq!(client.try_get_batch(&2), Err(Ok(Error::BatchNotFound)));
}

#[test]
#[should_panic]
fn test_prune_batches_requires_router_auth() {
    let (env, client, ..) = setup();
    env.set_auths(&[]);
    client.prune_batches(&100, &10, &1);
}
