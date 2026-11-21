//! #960 — `initialize` runs once, and the guard has to be proven against a call
//! that would actually change something.
//!
//! `initialize` returns `AlreadyInitialized` before it writes the merchant or
//! pins the `ReceiptShard` wasm hash, and the pinned hash is what the router
//! later deploys shards from. The test on `main` (`src/test.rs::test_double_initialize_fails`) replays the *same*
//! merchant and the same
//! hash, so a guard moved below those two writes would still pass: rewriting
//! the same values is invisible.
//!
//! Here the replay carries a different admin and an all-zero wasm hash. If the
//! guard sat after the writes, `get_admin` would report the new address and
//! every later shard would be deployed from a hash that is not an installed
//! wasm, so the anchoring call below would fail instead of spawning shard 0.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    Address, BytesN, Env,
};

/// Logical shard used by these tests.
const DEFAULT_SHARD: u64 = 0;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// How many events the router itself published in the most recent top-level
/// invocation. `events().all()` reflects only that invocation, which is what
/// makes this a per-call observation rather than a cumulative log.
fn published_by(env: &Env, anchor: &ReceiptAnchorClient) -> usize {
    env.events()
        .all()
        .filter_by_contract(&anchor.address)
        .events()
        .len()
}

#[test]
fn a_second_initialize_cannot_retarget_the_admin_or_the_shard_wasm() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let real_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&admin_a, &real_hash);

    // The counter has teeth: the accepted call did publish its event.
    assert_eq!(
        published_by(&env, &anchor),
        1,
        "the first initialize publishes exactly one InitializedEvent"
    );
    assert_eq!(anchor.get_shard_count(&DEFAULT_SHARD), 0);

    let zero_hash = BytesN::from_array(&env, &[0u8; 32]);
    assert_eq!(
        anchor.try_initialize(&admin_b, &zero_hash),
        Err(Ok(Error::AlreadyInitialized)),
        "a replay with different arguments is still a replay"
    );
    // Measured immediately: `events().all()` only reflects the most recent
    // top-level invocation, so any read in between would score that call.
    assert_eq!(
        published_by(&env, &anchor),
        0,
        "the rejected initialize published nothing"
    );

    assert_eq!(
        anchor.get_admin(),
        admin_a,
        "the admin must still be the one from the first call"
    );

    // The pinned wasm is still the real shard: an anchor still factory-deploys
    // shard 0. With a stored all-zero hash this call fails to deploy, so this
    // is the assertion that catches the write-then-guard ordering.
    let root = BytesN::from_array(&env, &[9u8; 32]);
    assert_eq!(anchor.anchor_batch(&DEFAULT_SHARD, &root, &1, &0, &10), 1);
    assert_eq!(
        anchor.get_shard_count(&DEFAULT_SHARD),
        1,
        "the first anchor spawns exactly one storage shard from the pinned wasm"
    );
    assert_eq!(anchor.get_shard_root(&DEFAULT_SHARD), root);

    // Replaying with the original arguments is refused for the same reason, so
    // the guard is about initialization state rather than about argument
    // equality.
    assert_eq!(
        anchor.try_initialize(&admin_a, &real_hash),
        Err(Ok(Error::AlreadyInitialized))
    );
    assert_eq!(
        published_by(&env, &anchor),
        0,
        "and it published nothing either"
    );
    assert_eq!(anchor.get_admin(), admin_a);
}
