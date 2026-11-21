//! #957 — the anchor token bucket refills one token per interval, and a late
//! refill restarts the clock rather than preserving the original schedule.
//!
//! `refill_bucket` grants `elapsed / refill_interval_secs` tokens, caps them at
//! the burst, and — the part the existing tests leave free — sets
//! `last_refill = now` whenever at least one token was earned. `test.rs` only
//! ever configures a burst of one or spends two full intervals, so both a
//! "refill to capacity on any elapsed time" change and a "advance
//! `last_refill` by whole intervals" change still pass everything on `main`.
//!
//! Every boundary here is derived from the two configured numbers: burst 2 and
//! a 60-second interval.

use accensa_common::Error;
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

/// Logical shard used by these tests.
const DEFAULT_SHARD: u64 = 0;

/// The two configured numbers, and everything the assertions are derived from.
const BURST: u32 = 2;
const INTERVAL: u64 = 60;

/// Ledger timestamp the bucket starts from.
const T0: u64 = 1_000;

mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

fn setup() -> (Env, ReceiptAnchorClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    anchor.initialize(
        &merchant,
        &env.deployer().upload_contract_wasm(shard_wasm::WASM),
    );
    anchor.set_anchor_rate_limit(&BURST, &(INTERVAL as u32));

    (env, anchor)
}

fn at(env: &Env, timestamp: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = timestamp;
        // The sequence advances alongside, so nothing that keys off ledger
        // numbers sees time move without a ledger moving with it.
        li.sequence_number += 1;
    });
}

/// A fresh root per attempt: `DuplicateRoot` is rejected before the bucket is
/// spent, so a reused root would test the dedupe path instead of the limiter.
fn root_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

/// The burst is spendable back-to-back, and the third anchor in the same ledger
/// is the one that is told to wait.
#[test]
fn burst_is_spent_back_to_back_then_the_bucket_is_empty() {
    let (env, anchor) = setup();
    at(&env, T0);

    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10),
        1
    );
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 2), &1, &0, &10),
        2
    );
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 3), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "burst {BURST} spent"
    );
}

/// +59s is rejected and +60s admits exactly one anchor — one token per
/// interval, not a top-up to the burst.
#[test]
fn one_token_returns_per_interval_not_a_full_bucket() {
    let (env, anchor) = setup();
    at(&env, T0);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 2), &1, &0, &10);

    at(&env, T0 + INTERVAL - 1);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 3), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "one second short of an interval earns nothing"
    );

    at(&env, T0 + INTERVAL);
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 3), &1, &0, &10),
        3,
        "the first interval returns exactly one token"
    );
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 4), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "the earned token was the only one available — the bucket was not \
         refilled to the burst of {BURST}"
    );
}

/// The second token arrives one interval after the first (+120s), not on the
/// original schedule from the drained bucket.
#[test]
fn second_token_arrives_one_interval_after_the_first() {
    let (env, anchor) = setup();
    at(&env, T0);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 2), &1, &0, &10);

    at(&env, T0 + INTERVAL);
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 3), &1, &0, &10),
        3
    );

    at(&env, T0 + 2 * INTERVAL - 1);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 4), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "+119s is one second short of the second token"
    );

    at(&env, T0 + 2 * INTERVAL);
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 4), &1, &0, &10),
        4,
        "+120s admits the second anchor"
    );
}

/// The case that pins *how* the clock moves. Two full intervals elapse while the
/// bucket is empty, so the late anchor earns both tokens at once — capped at the
/// burst — and `last_refill` lands on the late call itself. The next token is
/// therefore one full interval after the late call.
///
/// A refill that advanced `last_refill` by whole intervals instead would set it
/// to `T0 + 2 * INTERVAL` (the boundary the earned tokens were charged for), and
/// then the call at `T_late + INTERVAL - 1` below would already be 89 seconds
/// clear of that clock and would be admitted. This is the assertion that fails
/// against such a change, and the reason the burst-boundary assertions above are
/// not enough on their own.
#[test]
fn a_late_refill_restarts_the_interval_clock() {
    let (env, anchor) = setup();
    at(&env, T0);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 1), &1, &0, &10);
    anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 2), &1, &0, &10);

    // 2.5 intervals elapse with the bucket empty.
    let t_late = T0 + (5 * INTERVAL) / 2;
    at(&env, t_late);

    // Both tokens were earned, capped at the burst, so two anchors go through
    // and the third is rejected — the elapsed time beyond the cap earns nothing
    // extra.
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 3), &1, &0, &10),
        3
    );
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 4), &1, &0, &10),
        4
    );
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 5), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "the refill is capped at the burst of {BURST}"
    );

    // The next token is a full interval after the late call, not after the
    // interval boundary it was charged for.
    at(&env, t_late + INTERVAL - 1);
    assert_eq!(
        anchor.try_anchor_batch(&DEFAULT_SHARD, &root_at(&env, 5), &1, &0, &10),
        Err(Ok(Error::AnchorRateLimited)),
        "an interval-advancing clock would have admitted here"
    );

    at(&env, t_late + INTERVAL);
    assert_eq!(
        anchor.anchor_batch(&DEFAULT_SHARD, &root_at(&env, 5), &1, &0, &10),
        5,
        "one full interval after the late call"
    );
}
