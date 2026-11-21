//! #959 — `get_shard_address` is a pure read: a missing index answers
//! `BatchNotFound` and deploys nothing.
//!
//! The getter at `src/lib.rs:980` is one `instance().get(DataKey::Shard(..))`
//! mapped through `ok_or(Error::BatchNotFound)`. The only prior test of it,
//! `src/test.rs`, asks for index 0 of a shard that has already anchored, so the
//! *miss* branch had never been executed: a getter that lazily deployed the
//! shard it was asked about, or that fell back to the previous index, returned
//! an address the test never requested and so passed unchanged.
//!
//! What makes a lazy deploy observable through the public surface is that the
//! router keeps one `(shard_id, shard_index) -> Address` map entry per deployed
//! storage shard. So each case below reads the map key the getter would have had
//! to write (`receipt_anchor::DataKey::Shard`) and asserts it is still absent,
//! and compares the number of instance-storage entries before and after — an
//! `all()`-style census that catches a new key whatever the getter names it.
//!
//! The `BatchNotFound` answer for `(0, 1)` is only interesting next to a
//! populated `0`: shard 0 really is deployed here, so the getter is refusing an
//! index that does not exist rather than refusing a shard it never saw.
//!
//! One limit of this file is worth stating rather than hiding. `env.events()`
//! reports only the *last* top-level invocation, so "the getter emitted nothing"
//! can only be measured as "the buffer is empty right after the getter ran",
//! which a view call satisfies whether or not it published. The load-bearing
//! assertions are therefore the storage census and the counters, not events; the
//! event checks stay because a getter that anchored, transferred, or bumped a
//! counter would trip them from the other direction.

use accensa_common::Error;
use receipt_anchor::{DataKey, ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{storage::Instance as _, Address as _, Events},
    vec, Address, BytesN, Env,
};

/// The router deploys real `ReceiptShard` contracts, so it needs the same wasm
/// `tests/integration_test.rs` uploads. An arbitrary 32-byte hash is accepted by
/// `initialize` — it is only stored (`src/lib.rs:409`) — but the first
/// `anchor_batch` then aborts in the host with `MissingContract`, so a setup that
/// never deploys a shard cannot answer the question this issue asks.
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

const SHARD: u64 = 0;
/// A second logical shard that is never anchored.
const OTHER_SHARD: u64 = 1;

struct Anchor {
    env: Env,
    address: Address,
    client: ReceiptAnchorClient<'static>,
}

fn setup() -> Anchor {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let client = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    client.initialize(&merchant, &shard_wasm_hash);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    client.set_bounty_asset(&token);

    Anchor {
        env,
        address: anchor_id,
        client,
    }
}

fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// Anchor one one-receipt batch in `shard` and return its `batch_id`.
///
/// `count = 1` keeps this the cheapest way to make shard 0 exist: the issue's
/// point is a missing *index*, and 200 batches would only cost budget.
fn anchor_one(a: &Anchor, shard: u64, i: u8) -> u64 {
    a.client
        .anchor_batch(&shard, &root_at(&a.env, i), &1, &0, &10)
}

/// Every instance-storage key the router holds, as a count.
fn instance_entries(a: &Anchor) -> u32 {
    a.env
        .as_contract(&a.address, || a.env.storage().instance().all().len())
}

/// Whether the router's shard map holds `shard`'s `index`, read the way the
/// contract itself reads it.
fn shard_key_is_deployed(a: &Anchor, shard: u64, index: u64) -> bool {
    a.env.as_contract(&a.address, || {
        a.env
            .storage()
            .instance()
            .has(&DataKey::Shard(shard, index))
    })
}

/// Assert the last invocation (here: always a view call) published nothing from
/// the router.
fn assert_router_published_nothing(a: &Anchor) {
    assert_eq!(
        a.env.events().all().filter_by_contract(&a.address),
        vec![&a.env],
        "a read-only getter must not publish",
    );
}

/// The miss branch is reached at all: with shard 0 deployed, its *next* index is
/// `BatchNotFound` and nothing was written for it.
#[test]
fn the_index_after_the_deployed_one_is_not_found_and_stays_undeployed() {
    let a = setup();
    anchor_one(&a, SHARD, 1);

    let deployed = a.client.get_shard_address(&SHARD, &0);
    let before = instance_entries(&a);
    assert!(shard_key_is_deployed(&a, SHARD, 0));
    assert!(!shard_key_is_deployed(&a, SHARD, 1));

    assert_eq!(
        a.client.try_get_shard_address(&SHARD, &1),
        Err(Ok(Error::BatchNotFound)),
    );
    assert_router_published_nothing(&a);

    // The failed lookup deployed nothing: neither the key it was asked for nor
    // any key at all appeared, and the batch it refused is still the only one.
    assert!(!shard_key_is_deployed(&a, SHARD, 1));
    assert_eq!(instance_entries(&a), before);
    assert_eq!(a.client.get_shard_count(&SHARD), 1);
    assert_eq!(a.client.get_batch_count(&SHARD), 1);
    // No fallback either: index 0 still answers with the address it had before.
    assert_eq!(a.client.get_shard_address(&SHARD, &0), deployed);
}

/// The same miss on indexes further out, so "index 1" is not special: the getter
/// refuses every index above the deployed one instead of wrapping, clamping or
/// deploying the requested one.
#[test]
fn indexes_far_past_the_deployed_one_are_all_not_found() {
    let a = setup();
    anchor_one(&a, SHARD, 1);
    let before = instance_entries(&a);

    for index in [2u64, 3, 199, 200, 1000] {
        assert_eq!(
            a.client.try_get_shard_address(&SHARD, &index),
            Err(Ok(Error::BatchNotFound)),
            "index {index} of a one-batch shard has no address to return",
        );
        assert!(!shard_key_is_deployed(&a, SHARD, index));
    }
    assert_eq!(instance_entries(&a), before);
    assert_eq!(a.client.get_shard_count(&SHARD), 1);
    assert_eq!(a.client.get_batch_count(&SHARD), 1);
}

/// A `shard_id` that has never anchored has no index 0 either — and querying it
/// does not bring the shard into existence.
#[test]
fn an_untouched_shard_has_no_index_zero() {
    let a = setup();
    anchor_one(&a, SHARD, 1);
    let before = instance_entries(&a);

    assert_eq!(a.client.get_shard_count(&OTHER_SHARD), 0);
    assert_eq!(a.client.get_batch_count(&OTHER_SHARD), 0);

    assert_eq!(
        a.client.try_get_shard_address(&OTHER_SHARD, &0),
        Err(Ok(Error::BatchNotFound)),
    );
    assert_router_published_nothing(&a);

    assert!(!shard_key_is_deployed(&a, OTHER_SHARD, 0));
    assert_eq!(instance_entries(&a), before);
    // The shard is still empty by both counters, not just by its map key.
    assert_eq!(a.client.get_shard_count(&OTHER_SHARD), 0);
    assert_eq!(a.client.get_batch_count(&OTHER_SHARD), 0);
    // And the populated shard is untouched by the foreign lookup.
    assert_eq!(a.client.get_batch_count(&SHARD), 1);
}

/// The failure must not consume anything from the stream: the next real anchor
/// still lands in shard 0 / index 0, which is only true if the refused lookups
/// neither deployed a shard nor advanced a counter.
#[test]
fn a_refused_lookup_leaves_the_next_batch_slot_for_its_anchor() {
    let a = setup();
    anchor_one(&a, SHARD, 1);
    let index_zero = a.client.get_shard_address(&SHARD, &0);

    assert_eq!(
        a.client.try_get_shard_address(&SHARD, &1),
        Err(Ok(Error::BatchNotFound)),
    );
    assert_eq!(
        a.client.try_get_shard_address(&OTHER_SHARD, &0),
        Err(Ok(Error::BatchNotFound)),
    );

    assert_eq!(anchor_one(&a, SHARD, 2), 2, "the stream resumed");
    assert_eq!(a.client.get_batch_count(&SHARD), 2);
    assert_eq!(a.client.get_shard_count(&SHARD), 1);
    assert_eq!(a.client.get_shard_address(&SHARD, &0), index_zero);
    assert!(!shard_key_is_deployed(&a, SHARD, 1));
    assert!(!shard_key_is_deployed(&a, OTHER_SHARD, 0));
}
