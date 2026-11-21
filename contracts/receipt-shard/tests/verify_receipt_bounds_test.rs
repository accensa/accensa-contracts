//! Proof-length bounds coverage for `ReceiptShard::verify_receipt` (issue #992).
//!
//! `verify_receipt` rejects a proof longer than `MAX_PROOF_LEN` *before* it
//! loads the batch, so the ordering is client-visible: an over-long proof for
//! an unanchored batch id returns `ProofTooLong`, not `BatchNotFound`. These
//! tests pin that ordering plus the empty-proof edges `fold_proof` implies for
//! a one-leaf batch.

use accensa_common::Error;
use receipt_shard::{ReceiptShard, ReceiptShardClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec};

fn setup() -> (Env, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64, token));
    let client = ReceiptShardClient::new(&env, &contract_id);
    (env, client)
}

/// `len` distinct dummy siblings. Their values are irrelevant for the length
/// rows: the guard runs before any proof byte is folded.
fn siblings(env: &Env, len: u32) -> Vec<BytesN<32>> {
    let mut proof = Vec::new(env);
    for i in 0..len {
        proof.push_back(BytesN::from_array(env, &[i as u8; 32]));
    }
    proof
}

/// A one-leaf tree has depth zero, so an empty proof folds to the leaf itself
/// and verifies against a root equal to that leaf.
#[test]
fn empty_proof_verifies_a_one_leaf_batch() {
    let (env, client) = setup();
    let leaf = BytesN::from_array(&env, &[0x42; 32]);
    client.anchor_batch(&1, &leaf, &1, &0, &100);

    assert!(
        client.verify_receipt(&1, &leaf, &Vec::new(&env)),
        "an empty proof must fold to the leaf and match the root"
    );
}

/// A different leaf folds to a different root, so the result is `false` —
/// not an error.
#[test]
fn empty_proof_with_a_different_leaf_returns_false() {
    let (env, client) = setup();
    let leaf = BytesN::from_array(&env, &[0x42; 32]);
    let other = BytesN::from_array(&env, &[0x43; 32]);
    client.anchor_batch(&1, &leaf, &1, &0, &100);

    assert!(
        !client.verify_receipt(&1, &other, &Vec::new(&env)),
        "a mismatched leaf must report false, not an error"
    );
}

/// A proof of exactly `MAX_PROOF_LEN` (10) siblings passes the length guard
/// and reaches storage, so an unanchored batch id surfaces `BatchNotFound`.
#[test]
fn max_length_proof_for_a_missing_batch_returns_batch_not_found() {
    let (env, client) = setup();
    let leaf = BytesN::from_array(&env, &[0x42; 32]);

    assert_eq!(
        client.try_verify_receipt(&2, &leaf, &siblings(&env, 10)),
        Err(Ok(Error::BatchNotFound)),
        "a 10-sibling proof is in bounds and must reach the batch lookup"
    );
}

/// One sibling over the bound is rejected before the storage read, so the
/// same unanchored id answers `ProofTooLong`. This is the ordering pin: it
/// fails if the length guard moves after `get_batch`.
#[test]
fn over_long_proof_for_a_missing_batch_returns_proof_too_long() {
    let (env, client) = setup();
    let leaf = BytesN::from_array(&env, &[0x42; 32]);

    assert_eq!(
        client.try_verify_receipt(&2, &leaf, &siblings(&env, 11)),
        Err(Ok(Error::ProofTooLong)),
        "the length guard must run before the batch read"
    );
}
