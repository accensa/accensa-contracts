//! TTL coverage for `ReceiptShard::extend_batch_ttl` (issue #991).
//!
//! The existing unit test for the success branch asserts nothing about
//! storage, so a regression turning `extend_batch_ttl` into a no-op returning
//! `Ok(())` would pass the suite. These tests read the persistent batch entry's
//! TTL before and after the call, and pin the `BatchNotFound` branch.

use accensa_common::Error;
use receipt_shard::{DataKey, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    Address, BytesN, Env,
};

/// Mirrors the shard's private TTL constants in `src/lib.rs`: an entry is
/// extended to `TTL_EXTEND` ledgers only once fewer than `TTL_THRESHOLD`
/// remain.
const TTL_EXTEND: u32 = 518_400;
const TTL_THRESHOLD: u32 = 100;

/// Base ledger sequence the fixture anchors at (see `pruning_test.rs`).
const BASE_LEDGER: u32 = 100_000;

fn setup() -> (Env, ReceiptShardClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64, token));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);
    (env, client, contract_id)
}

fn batch_ttl(env: &Env, shard: &Address, batch_id: u64) -> u32 {
    env.as_contract(shard, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Batch(batch_id))
    })
}

/// A live batch whose entry is inside the extension threshold gets extended
/// back to `TTL_EXTEND`, from an unauthorized caller.
#[test]
fn extend_batch_ttl_extends_a_live_batch_without_auth() {
    let (env, client, shard) = setup();
    let root = BytesN::from_array(&env, &[7u8; 32]);
    client.anchor_batch(&1, &root, &1, &0, &100);

    // A freshly written persistent entry starts with the host's minimum
    // persistent TTL, which is above TTL_THRESHOLD — so `extend_ttl` inside
    // `anchor_batch` leaves it alone and the value below is that host
    // minimum, not TTL_EXTEND.
    let fresh = batch_ttl(&env, &shard, 1);
    assert!(
        fresh > TTL_THRESHOLD,
        "a live record should start above the extension threshold, got {fresh}"
    );

    // Advance until fewer than TTL_THRESHOLD ledgers remain: only then does
    // extend_ttl actually rewrite the live-until ledger, so the delta we
    // measure cannot be the one anchor_batch already applied.
    let seq = env.ledger().sequence();
    env.ledger().with_mut(|li| {
        li.sequence_number = seq + fresh - TTL_THRESHOLD + 1;
    });
    let before = batch_ttl(&env, &shard, 1);
    assert_eq!(
        before,
        TTL_THRESHOLD - 1,
        "the record should be within the extension threshold before the call"
    );

    // No authorization is mocked: the entry point is permissionless.
    env.mock_auths(&[]);
    client.extend_batch_ttl(&1);

    let after = batch_ttl(&env, &shard, 1);
    assert!(
        after > before,
        "extend_batch_ttl must raise the TTL: {before} -> {after}"
    );
    assert!(
        after >= TTL_EXTEND - 1,
        "extend_batch_ttl must extend back to TTL_EXTEND, got {after}"
    );
}

/// A never-anchored id reports `BatchNotFound` and creates no entry.
#[test]
fn extend_batch_ttl_missing_batch_errors_and_creates_nothing() {
    let (env, client, shard) = setup();

    assert_eq!(
        client.try_extend_batch_ttl(&1),
        Err(Ok(Error::BatchNotFound)),
        "an id with no persistent record must report BatchNotFound"
    );
    assert!(
        !env.as_contract(&shard, || env
            .storage()
            .persistent()
            .has(&DataKey::Batch(1))),
        "a failed extension must not create a record"
    );
}
