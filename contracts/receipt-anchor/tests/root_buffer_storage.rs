//! #958 — the per-shard root buffer must not live in the router's instance
//! entry.
//!
//! `push_shard_root` keeps a 100-entry ring buffer of historical roots per
//! logical shard. A full buffer is 3,200 bytes of root bytes plus vector
//! framing, and instance storage is a single ledger entry that every call to
//! this contract loads and archives as a unit — so as written, one entry grew
//! with every shard and every 200 batches, and the root history aged with the
//! config it had nothing to do with.
//!
//! These tests read the router's own storage through `env.as_contract`, the way
//! `src/fuzz_test.rs` reads a shard's prune cursor, and assert the storage class
//! and the rent behaviour directly instead of inferring them from the API.

use receipt_anchor::{DataKey, ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    vec, Address, BytesN, Env, Vec,
};

/// Logical shard used by these tests.
const DEFAULT_SHARD: u64 = 0;

/// The ring buffer retains at most this many roots (`ROOT_BUFFER_SIZE`, which
/// is private; pinned here through the public accessor).
const BUFFER_CAPACITY: u32 = 100;

/// How far an anchor write pushes the buffer entry's expiry forward
/// (`TTL_EXTEND`, private; pinned here through the observed TTL).
const RETENTION_HORIZON: u32 = 518_400;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup() -> (Env, ReceiptAnchorClient<'static>) {
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
    anchor.set_bounty_asset(&token);

    (env, anchor)
}

/// A distinct 32-byte root per anchor index, so buffer content is traceable to
/// the call that produced it.
fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// Remaining TTL of the shard's buffer entry, read from inside the contract.
fn buffer_ttl(env: &Env, anchor: &ReceiptAnchorClient, shard: u64) -> u32 {
    let key = DataKey::ShardRootBuffer(shard);
    env.as_contract(&anchor.address, || env.storage().persistent().get_ttl(&key))
}

/// The buffer is its own persistent entry keyed by shard, and is no longer part
/// of the instance entry. This fails against the pre-fix code on both halves:
/// the key was only ever written to `instance()`.
#[test]
fn anchored_roots_are_written_to_persistent_storage() {
    let (env, anchor) = setup();
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10);

    let key = DataKey::ShardRootBuffer(DEFAULT_SHARD);
    let in_persistent: bool =
        env.as_contract(&anchor.address, || env.storage().persistent().has(&key));
    let in_instance: bool = env.as_contract(&anchor.address, || env.storage().instance().has(&key));

    assert!(
        in_persistent,
        "the root buffer must be its own persistent entry, keyed by shard"
    );
    assert!(
        !in_instance,
        "the root buffer must not sit in the router's instance entry"
    );
    assert_eq!(anchor.get_root_buffer(&DEFAULT_SHARD).len(), 1);
}

/// The public shape is untouched: same roots, same order, same eviction, and
/// still one buffer per logical shard.
#[test]
fn ordering_and_eviction_survive_the_move() {
    let (env, anchor) = setup();

    // 101 distinct anchors: the buffer fills at capacity and evicts root 1.
    for i in 1..=101u8 {
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, i), &1, &0, &10);
    }

    assert_eq!(anchor.get_root_buffer_size(), BUFFER_CAPACITY);
    let buffer = anchor.get_root_buffer(&DEFAULT_SHARD);
    assert_eq!(
        buffer.len(),
        BUFFER_CAPACITY,
        "the buffer stays capped at capacity"
    );

    let expected: Vec<BytesN<32>> = (2..=101u8).fold(Vec::new(&env), |mut v, i| {
        v.push_back(root_at(&env, i));
        v
    });
    assert_eq!(
        buffer, expected,
        "roots stay oldest-first, with the oldest dropped on wrap"
    );

    // Per-shard isolation must survive the move to per-shard persistent keys.
    anchor.anchor_batch(&1, &root_at(&env, 7), &1, &0, &10);
    assert_eq!(
        anchor.get_root_buffer(&1),
        vec![&env, root_at(&env, 7)],
        "shard 1 keeps its own history"
    );
    assert_eq!(
        anchor.get_root_buffer(&DEFAULT_SHARD).len(),
        BUFFER_CAPACITY
    );
}

/// Anchoring pays rent on the buffer entry and reading it does not.
///
/// A fresh entry lands on the host default lifetime (4095 ledgers), which is
/// below `TTL_EXTEND`, so `extend_ttl` is a no-op on the first write; it only
/// bites once the entry is inside `TTL_THRESHOLD` (100) ledgers of expiry. The
/// observable window is therefore the tail of the entry's life: left alone it
/// ages down and stays aged, and the next anchor pushes it to the retention
/// horizon. Without the write-time extension the last assertion fails.
#[test]
fn each_anchor_pays_rent_and_a_read_does_not() {
    let (env, anchor) = setup();
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10);

    let written = buffer_ttl(&env, &anchor, DEFAULT_SHARD);
    assert!(written > 0, "the write must leave the entry live");

    // Age the entry to 50 ledgers short of expiry, reading it on the way.
    env.ledger()
        .with_mut(|li| li.sequence_number += written - 50);
    assert_eq!(
        buffer_ttl(&env, &anchor, DEFAULT_SHARD),
        50,
        "a read must not pay rent: the entry should have aged, not been \
         re-extended by the observation"
    );

    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 2), &1, &0, &10);
    assert_eq!(
        buffer_ttl(&env, &anchor, DEFAULT_SHARD),
        RETENTION_HORIZON,
        "the anchor write must re-extend the buffer to its retention horizon, \
         past the {written} it was born with"
    );
}

/// The property the shared instance entry could not provide: each shard's root
/// history carries its own lifetime. Two shards anchored side by side age
/// independently, so an active shard re-pays for its own history while a quiet
/// shard stays on its own rent schedule — and the quiet shard's root still
/// verifies while it is live.
#[test]
fn each_shard_buffer_carries_its_own_lifetime() {
    let (env, anchor) = setup();
    let root_a = root_at(&env, 3);
    let root_b = root_at(&env, 4);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_a, &1, &0, &10);
    anchor.anchor_batch(&1, &root_b, &1, &0, &10);

    let written = buffer_ttl(&env, &anchor, DEFAULT_SHARD);
    assert_eq!(
        buffer_ttl(&env, &anchor, 1),
        written,
        "both shards start on the same horizon"
    );

    env.ledger()
        .with_mut(|li| li.sequence_number += written - 50);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 5), &1, &0, &10);

    assert_eq!(
        buffer_ttl(&env, &anchor, DEFAULT_SHARD),
        RETENTION_HORIZON,
        "the shard that anchored re-extended its own entry"
    );
    assert_eq!(
        buffer_ttl(&env, &anchor, 1),
        50,
        "the untouched shard's buffer ages on its own clock, not with the \
         router's instance entry"
    );

    assert!(
        anchor.verify_receipt_by_root(&1, &root_b, &root_b, &vec![&env]),
        "the quiet shard's history is still verifiable while it is live"
    );
}
