//! Receipt topic shapes pinned per deployed version (issue #942).
//!
//! The live testnet deployment still emits 0.1.0's `("anchored", batch_id)`
//! while current source publishes `(receipt, action, anchor_id)`; the shapes
//! live in `contracts/receipt-anchor/event-topic-vectors.json`, and these
//! tests drive the real entry points and assert the published topics equal the
//! current-source entry. A second assertion pins that the two arms differ in
//! arity or lead symbol, so a topic filter cannot match the wrong deployment.
//! `prune_batches` did not exist in 0.1.0, so the prune shape pins its absent
//! legacy arm as `None`.

#[path = "fixtures/event_topic_vectors.rs"]
mod event_topic_vectors;

use event_topic_vectors::{ANCHOR_CURRENT, ANCHOR_LEGACY_0_1_0, PRUNE_CURRENT, PRUNE_LEGACY_0_1_0};
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    xdr::{ContractEventBody, ScVal},
    Address, BytesN, Env,
};

/// Logical shard used by these single-stream tests.
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
    // Shards deploy with the router's configured bounty asset (issue #998).
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

/// Resolve vector tags to the topic `ScVal`s an indexer filters on: bare
/// words are Symbols, "u64" is the scalar the test chose. Unknown tags fail
/// loudly so a vector typo cannot silently pass.
fn resolve_topics(shape: &[&str], scalar: u64) -> std::vec::Vec<ScVal> {
    shape
        .iter()
        .map(|tag| match *tag {
            "u64" => ScVal::U64(scalar),
            sym => ScVal::Symbol(sym.try_into().expect("vector symbol tag")),
        })
        .collect()
}

/// Topic tuples the last invocation published on `contract`, one entry per
/// event. `env.events()` only reflects the last top-level call, so this runs
/// immediately after the entry point under test.
fn published_topic_sets(env: &Env, contract: &Address) -> std::vec::Vec<std::vec::Vec<ScVal>> {
    env.events()
        .all()
        .filter_by_contract(contract)
        .events()
        .iter()
        .map(|event| match &event.body {
            ContractEventBody::V0(v0) => v0.topics.to_vec(),
        })
        .collect()
}

#[test]
fn anchor_publishes_current_topics_and_differs_from_legacy() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    // The first anchor also deploys a storage shard; the second anchor's
    // invocation carries the receipt event under test.
    anchor.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &[1u8; 32]),
        &1,
        &0,
        &10,
    );
    anchor.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &[2u8; 32]),
        &1,
        &11,
        &20,
    );

    let expected = resolve_topics(ANCHOR_CURRENT, 2);
    let published = published_topic_sets(&env, &anchor.address);
    assert_eq!(
        published
            .iter()
            .filter(|topics| **topics == expected)
            .count(),
        1,
        "anchor_batch must publish exactly the current vector topics"
    );

    // The arms differ in both arity and lead symbol, so neither deployment's
    // events decode as the other's.
    let legacy = ANCHOR_LEGACY_0_1_0.expect("legacy anchor arm pinned");
    assert_ne!(
        ANCHOR_CURRENT.len(),
        legacy.len(),
        "current (receipt, anchor, id) vs legacy (anchored, id) arity"
    );
    assert_ne!(
        ANCHOR_CURRENT[0], legacy[0],
        "current vs legacy lead symbol"
    );
}

#[test]
fn prune_publishes_current_topics_and_has_no_legacy_arm() {
    let TestEnv {
        env,
        anchor,
        merchant: _,
    } = setup();

    env.ledger().with_mut(|li| li.sequence_number = 50);
    anchor.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &[1u8; 32]),
        &1,
        &0,
        &40,
    );
    env.ledger().with_mut(|li| li.sequence_number = 150);
    anchor.anchor_batch(
        &DEFAULT_SHARD,
        &BytesN::from_array(&env, &[2u8; 32]),
        &1,
        &41,
        &140,
    );

    // Prune the batch anchored before ledger 100: start_batch_id is 1.
    anchor.prune_batches(&DEFAULT_SHARD, &100);

    let expected = resolve_topics(PRUNE_CURRENT, 1);
    let published = published_topic_sets(&env, &anchor.address);
    assert_eq!(
        published
            .iter()
            .filter(|topics| **topics == expected)
            .count(),
        1,
        "prune_batches must publish exactly the current vector topics"
    );

    assert!(
        PRUNE_LEGACY_0_1_0.is_none(),
        "prune_batches did not exist in 0.1.0: no legacy arm may appear"
    );
}
