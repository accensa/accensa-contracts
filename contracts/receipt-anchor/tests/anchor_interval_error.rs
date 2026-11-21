//! #953 — an out-of-range anchor interval is a bad argument, not a bad batch.
//!
//! `set_min_anchor_interval` rejects an interval above `MAX_ANCHOR_INTERVAL`
//! before it reads the previous value, writes the new one, or publishes its
//! event. The guard used to answer `Error::BatchTooLarge`, which is the code
//! `anchor_batch` returns when `count > MAX_BATCH_SIZE` — so a mistyped
//! interval read back as a receipt-count problem, and a client switching on
//! the code retried with a smaller batch instead of a shorter interval.
//!
//! The ceiling is inclusive: 86,400 is the largest accepted interval, so the
//! rejection has to be proven one above it rather than at it. Nothing on `main`
//! calls the setter outside `src/test.rs`, which only records the event payload
//! for 90 and 0 — the `> MAX_ANCHOR_INTERVAL` branch was never reached.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    Address, Env,
};

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// The ceiling the setter accepts (`MAX_ANCHOR_INTERVAL`, private; pinned here
/// from the range documented on the function).
const MAX_ANCHOR_INTERVAL: u32 = 86_400;

fn setup() -> (Env, ReceiptAnchorClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);

    (env, anchor)
}

/// How many events the router published in the most recent top-level
/// invocation. `events().all()` reflects only that invocation, so this is a
/// per-call observation — and a failed invocation publishes nothing, which is
/// what makes the zero side of the assertion real rather than tautological.
fn published_by(env: &Env, anchor: &ReceiptAnchorClient) -> usize {
    env.events()
        .all()
        .filter_by_contract(&anchor.address)
        .events()
        .len()
}

/// The boundary itself is inside the range: the ceiling is accepted and stored.
#[test]
fn the_ceiling_is_accepted_and_stored() {
    let (env, anchor) = setup();

    assert_eq!(
        anchor.try_set_min_anchor_interval(&MAX_ANCHOR_INTERVAL),
        Ok(Ok(())),
        "{MAX_ANCHOR_INTERVAL} is the largest accepted interval, so the guard \
         must be a strict greater-than"
    );
    assert_eq!(
        published_by(&env, &anchor),
        1,
        "the accepted call publishes exactly one AnchorIntervalUpdatedEvent"
    );
    assert_eq!(
        anchor.get_min_anchor_interval(),
        MAX_ANCHOR_INTERVAL,
        "the stored value must be the ceiling it was given"
    );
}

/// One above the ceiling is rejected, and with the code for a bad argument.
/// `BatchTooLarge` is the batch-count failure and must not be reachable here.
#[test]
fn one_above_the_ceiling_is_rejected_as_an_invalid_amount() {
    let (_env, anchor) = setup();

    let rejected = anchor.try_set_min_anchor_interval(&(MAX_ANCHOR_INTERVAL + 1));
    assert_eq!(
        rejected,
        Err(Ok(Error::InvalidAmount)),
        "an out-of-range interval is a rejected scalar argument, not a batch \
         that is too large"
    );
    assert_ne!(
        rejected,
        Err(Ok(Error::BatchTooLarge)),
        "the batch-count code must not be reachable from the interval setter"
    );
}

/// The rejection is a no-op: the interval in force is untouched and the shard
/// publishes nothing to say it changed.
#[test]
fn a_rejected_interval_changes_nothing_and_publishes_nothing() {
    let (env, anchor) = setup();
    anchor.set_min_anchor_interval(&3_600);
    assert_eq!(anchor.get_min_anchor_interval(), 3_600);

    assert_eq!(
        anchor.try_set_min_anchor_interval(&(MAX_ANCHOR_INTERVAL + 1)),
        Err(Ok(Error::InvalidAmount))
    );
    // Measured immediately, before any read: a later invocation would overwrite
    // the event window this assertion is about.
    assert_eq!(
        published_by(&env, &anchor),
        0,
        "a rejected set_min_anchor_interval must not publish an \
         AnchorIntervalUpdatedEvent"
    );
    assert_eq!(
        anchor.get_min_anchor_interval(),
        3_600,
        "the interval in force before the rejected call must still be in force"
    );
}

/// Far above the ceiling behaves like one above it, so the guard is the range
/// check and not an overflow or a wrap-around at the boundary.
#[test]
fn far_above_the_ceiling_is_rejected_the_same_way() {
    let (_env, anchor) = setup();
    anchor.set_min_anchor_interval(&90);

    assert_eq!(
        anchor.try_set_min_anchor_interval(&u32::MAX),
        Err(Ok(Error::InvalidAmount)),
        "the widest possible interval is still out of range"
    );
    assert_eq!(
        anchor.get_min_anchor_interval(),
        90,
        "and it still changes nothing"
    );
}
