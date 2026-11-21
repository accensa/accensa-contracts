#![cfg(test)]

use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env,
};

/// Logical shard used by these single-stream integration tests.
const DEFAULT_SHARD: u64 = 0;

struct TestEnv<'a> {
    env: Env,
    anchor: ReceiptAnchorClient<'a>,
    #[allow(dead_code)]
    merchant: Address,
}

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup<'a>() -> TestEnv<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    env.as_contract(&anchor_id, || {
        env.storage()
            .instance()
            .set(&receipt_anchor::DataKey::BountyAsset, &token);
    });

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
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    let sibling = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    let batch_id = anchor.anchor_batch(&DEFAULT_SHARD, &root, &2, &0, &100);
    assert_eq!(batch_id, 1);
    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 1);

    let record = anchor.get_batch(&DEFAULT_SHARD, &1);
    assert_eq!(record.root, root);
    assert_eq!(record.count, 2);
    assert_eq!(record.period_start, 0);
    assert_eq!(record.period_end, 100);

    let proof = vec![&env, sibling.clone()];
    assert!(anchor.verify_receipt(&DEFAULT_SHARD, &1, &leaf, &proof));
}

#[test]
fn test_integration_multiple_batches_and_count() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 0);

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);
    let root3 = BytesN::from_array(&env, &[3u8; 32]);

    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root1, &1, &0, &10), 1);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root2, &1, &11, &20), 2);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root3, &1, &21, &30), 3);

    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 3);

    assert_eq!(anchor.get_batch(&DEFAULT_SHARD, &1).root, root1);
    assert_eq!(anchor.get_batch(&DEFAULT_SHARD, &2).root, root2);
    assert_eq!(anchor.get_batch(&DEFAULT_SHARD, &3).root, root3);
}

#[test]
#[should_panic]
fn test_integration_batch_not_found() {
    let TestEnv {
        env: _,
        anchor,
        merchant: _,
    } = setup();
    let _ = anchor.get_batch(&DEFAULT_SHARD, &999);
}

#[test]
fn test_integration_verify_receipt_against_external_root() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    // Construct a 4-leaf Merkle tree independently in the test
    let l1 = BytesN::from_array(&env, &[1u8; 32]);
    let l2 = BytesN::from_array(&env, &[2u8; 32]);
    let l3 = BytesN::from_array(&env, &[3u8; 32]);
    let l4 = BytesN::from_array(&env, &[4u8; 32]);

    let h12 = hash_pair(&env, &l1, &l2);
    let h34 = hash_pair(&env, &l3, &l4);
    let root = hash_pair(&env, &h12, &h34);

    anchor.anchor_batch(&DEFAULT_SHARD, &root, &4, &0, &100);

    // Verify l1 using proof [l2, h34]
    let proof_l1 = vec![&env, l2.clone(), h34.clone()];
    assert!(anchor.verify_receipt(&DEFAULT_SHARD, &1, &l1, &proof_l1));

    // Verify l3 using proof [l4, h12]
    let proof_l3 = vec![&env, l4, h12];
    assert!(anchor.verify_receipt(&DEFAULT_SHARD, &1, &l3, &proof_l3));

    // Verify invalid proof should return false
    let bad_proof = vec![&env, l3, h34];
    assert!(!anchor.verify_receipt(&DEFAULT_SHARD, &1, &l2, &bad_proof));
}

#[test]
fn test_integration_prune_batches_round_trip() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let root1 = BytesN::from_array(&env, &[1u8; 32]);
    let root2 = BytesN::from_array(&env, &[2u8; 32]);

    env.ledger().with_mut(|li| li.sequence_number = 50);
    anchor.anchor_batch(&DEFAULT_SHARD, &root1, &1, &0, &40);

    env.ledger().with_mut(|li| li.sequence_number = 150);
    anchor.anchor_batch(&DEFAULT_SHARD, &root2, &1, &41, &140);

    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 2);
    assert!(anchor.try_get_batch(&DEFAULT_SHARD, &1).is_ok());
    assert!(anchor.try_get_batch(&DEFAULT_SHARD, &2).is_ok());

    // Prune batches anchored before ledger 100
    anchor.prune_batches(&DEFAULT_SHARD, &100);

    // Batch 1 should be pruned (missing), Batch 2 should remain
    assert!(anchor.try_get_batch(&DEFAULT_SHARD, &1).is_err());
    assert!(anchor.try_get_batch(&DEFAULT_SHARD, &2).is_ok());
    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 2);
}

// ---------------------------------------------------------------------------
// Error paths, privileged flows and the canonical event schema
// ---------------------------------------------------------------------------

use accensa_common::Error;
use receipt_anchor::events::SCHEMA_VERSION;
use soroban_sdk::testutils::{Events, MockAuth, MockAuthInvoke};
use soroban_sdk::{IntoVal, Map, Symbol, Val};

/// An address with no auth entries attached cannot write a root.
#[test]
fn test_integration_anchor_requires_admin_auth() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    env.set_auths(&[]);
    let root = BytesN::from_array(&env, &[9u8; 32]);
    assert!(
        anchor
            .try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10)
            .is_err(),
        "anchoring without the admin's authorization must fail"
    );
}

/// A root identical to the shard's current one is rejected with `DuplicateRoot`.
#[test]
fn test_integration_duplicate_root_rejected() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let root = BytesN::from_array(&env, &[1u8; 32]);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10), 1);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &1, &11, &20),
        Err(Ok(Error::DuplicateRoot))
    );
    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 1);
}

/// A batch above `MAX_BATCH_SIZE` is rejected before anything is written.
#[test]
fn test_integration_batch_too_large_rejected() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let root = BytesN::from_array(&env, &[1u8; 32]);
    let too_large = anchor.get_max_batch_size() + 1;
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root, &too_large, &0, &10),
        Err(Ok(Error::BatchTooLarge))
    );
    assert_eq!(anchor.get_batch_count(&DEFAULT_SHARD), 0);
}

/// The token-bucket limiter rejects the burst-exhausted identity and admits it
/// again once the refill interval has elapsed.
#[test]
fn test_integration_rate_limit_then_refill() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    anchor.set_anchor_rate_limit(&2, &60);

    let r1 = BytesN::from_array(&env, &[1u8; 32]);
    let r2 = BytesN::from_array(&env, &[2u8; 32]);
    let r3 = BytesN::from_array(&env, &[3u8; 32]);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &r1, &1, &0, &10), 1);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &r2, &1, &11, &20), 2);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &r3, &1, &21, &30),
        Err(Ok(Error::AnchorRateLimited)),
        "the burst of two is spent"
    );

    env.ledger().with_mut(|li| li.timestamp += 60);
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &r3, &1, &21, &30),
        3,
        "one token refills after the interval"
    );
}

/// Admin hand-off is two-step: proposing changes nothing until the proposed
/// admin accepts, and only then does anchoring follow the new admin.
#[test]
fn test_integration_admin_transfer_flow() {
    let TestEnv {
        env,
        anchor,
        merchant,
    } = setup();

    let new_admin = Address::generate(&env);
    anchor.transfer_admin(&new_admin);
    assert_eq!(anchor.get_admin(), merchant, "still the old admin");
    assert_eq!(anchor.get_pending_admin(), new_admin);

    anchor.accept_admin();
    assert_eq!(anchor.get_admin(), new_admin);
    assert_eq!(
        anchor.try_get_pending_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "the pending proposal is consumed"
    );
    assert_eq!(
        anchor.try_accept_admin(),
        Err(Ok(Error::NoPendingTransfer)),
        "double-accept is rejected once the proposal is consumed"
    );

    let root = BytesN::from_array(&env, &[4u8; 32]);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10), 1);
    assert_eq!(
        anchor.get_admin(),
        new_admin,
        "anchoring did not move the admin"
    );
}

/// After the hand-off the contract demands the *new* admin's authorization:
/// a valid auth entry from the former admin is no longer enough.
#[test]
fn test_integration_former_admin_cannot_anchor() {
    let TestEnv {
        env,
        anchor,
        merchant,
    } = setup();

    let new_admin = Address::generate(&env);
    anchor.transfer_admin(&new_admin);
    anchor.accept_admin();

    let root = BytesN::from_array(&env, &[5u8; 32]);
    let args: soroban_sdk::Vec<Val> = vec![
        &env,
        DEFAULT_SHARD.into_val(&env),
        root.clone().into_val(&env),
        1u32.into_val(&env),
        0u64.into_val(&env),
        10u64.into_val(&env),
    ];
    env.mock_auths(&[MockAuth {
        address: &merchant,
        invoke: &MockAuthInvoke {
            contract: &anchor.address,
            fn_name: "anchor_batch",
            args,
            sub_invokes: &[],
        },
    }]);
    assert!(
        anchor
            .try_anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10)
            .is_err(),
        "the former admin must not be able to write roots"
    );
}

/// Pruning removes a batch from `verify_receipt`, while the ring buffer keeps
/// the root verifiable by `verify_receipt_by_root`.
#[test]
fn test_integration_pruned_batch_no_longer_verifiable() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    let leaf = BytesN::from_array(&env, &[1u8; 32]);
    let sibling = BytesN::from_array(&env, &[2u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    env.ledger().with_mut(|li| li.sequence_number = 50);
    anchor.anchor_batch(&DEFAULT_SHARD, &root, &2, &0, &40);

    env.ledger().with_mut(|li| li.sequence_number = 150);
    anchor.prune_batches(&DEFAULT_SHARD, &100);

    let proof = vec![&env, sibling.clone()];
    assert_eq!(
        anchor.try_verify_receipt(&DEFAULT_SHARD, &1, &leaf, &proof),
        Err(Ok(Error::BatchNotFound)),
        "the pruned batch record is gone"
    );
    assert!(
        anchor.verify_receipt_by_root(&DEFAULT_SHARD, &root, &leaf, &proof),
        "the root survives in the ring buffer"
    );
}

/// The end-to-end shape indexers subscribe to: `(receipt, "anchor",
/// anchor_id)` with a payload that leads with `schema_version` and
/// `timestamp`.
#[test]
fn test_integration_receipt_event_uses_canonical_topics() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    // The first anchor also deploys a storage shard, so seed it first; the
    // second anchor is the only event of its own invocation.
    anchor.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &[1u8; 32]),
        &1,
        &0,
        &10,
    );
    env.ledger().with_mut(|li| {
        li.sequence_number = 42;
        li.timestamp = 1_000;
    });
    let root = BytesN::from_array(&env, &[2u8; 32]);
    anchor.anchor_batch(&DEFAULT_SHARD, &root, &7, &11, &20);

    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "schema_version").into_val(&env),
        SCHEMA_VERSION.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "timestamp").into_val(&env),
        1_000u64.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "root").into_val(&env),
        root.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "shard_id").into_val(&env),
        DEFAULT_SHARD.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "count").into_val(&env),
        7u32.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "period_start").into_val(&env),
        11u64.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "period_end").into_val(&env),
        20u64.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "anchored_ledger").into_val(&env),
        42u32.into_val(&env),
    );
    assert_eq!(
        env.events().all().filter_by_contract(&anchor.address),
        vec![
            &env,
            (
                anchor.address.clone(),
                (
                    Symbol::new(&env, "receipt"),
                    Symbol::new(&env, "anchor"),
                    2u64
                )
                    .into_val(&env),
                data.into_val(&env)
            )
        ]
    );
}
