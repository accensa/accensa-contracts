//! Admin-binding and round-staleness verdict tests (issue #652).
//!
//! [`ChainlinkConsumer::get_admin`] is asserted through the contract client in
//! both of its observable states — unset before [`ChainlinkConsumer::initialize`]
//! and bound after — and the double-initialize guard is pinned with its typed
//! error while proving the original admin stays bound. The staleness gate from
//! [`crate::chainlink`] is driven only through the public readers and the
//! ledger sequence: a round exactly `max_staleness_ledgers` old is served (the
//! bound is inclusive), one ledger older is refused.

#![cfg(test)]

use crate::{chainlink::RoundData, ChainlinkConsumer, ChainlinkConsumerClient, Error};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env,
};

/// The freshness bound under test (~4 hours of ledgers at ~5 s/ledger). The
/// boundary assertions below state their expectations in these terms.
const MAX_STALENESS: u32 = 3_600;

/// Ledger sequence the feed is pinned to; far enough above zero that
/// "answered at 1000" reads like a real round id/sequence pair.
const BASE_SEQUENCE: u32 = 1_000;

fn round(round_id: u64, answer: i128, updated_at: u32) -> RoundData {
    RoundData {
        round_id,
        answer,
        started_at: updated_at,
        updated_at,
        answered_in_round: round_id,
    }
}

/// An initialized consumer plus its admin, without any rounds pushed.
fn setup() -> (Env, ChainlinkConsumerClient<'static>, Address) {
    let env = Env::default();
    env.ledger().with_mut(|l| l.sequence_number = BASE_SEQUENCE);
    let id = env.register(ChainlinkConsumer, ());
    let client = ChainlinkConsumerClient::new(&env, &id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client, admin)
}

// ── get_admin binding (issue #652) ───────────────────────────────────────

#[test]
fn get_admin_is_none_before_initialize_and_some_after() {
    let env = Env::default();
    let id = env.register(ChainlinkConsumer, ());
    let client = ChainlinkConsumerClient::new(&env, &id);

    assert_eq!(
        client.get_admin(),
        None,
        "no admin may be bound before initialize"
    );

    let admin = Address::generate(&env);
    client.initialize(&admin);
    assert_eq!(
        client.get_admin(),
        Some(admin),
        "initialize must bind exactly the supplied admin"
    );
}

#[test]
fn second_initialize_is_rejected_and_leaves_the_original_admin_bound() {
    let (env, client, admin) = setup();

    let intruder = Address::generate(&env);
    assert_eq!(
        client.try_initialize(&intruder),
        Err(Ok(Error::AlreadyInitialized)),
        "a second initialize must fail with the typed AlreadyInitialized error"
    );
    assert_eq!(
        client.get_admin(),
        Some(admin),
        "the rejected initialize must leave the original admin bound"
    );
}

// ── Round staleness boundary (issue #652) ────────────────────────────────

/// The bound is inclusive: `updated_at + max_staleness == current` is served,
/// `updated_at + max_staleness < current` (one ledger older) is refused.
#[test]
fn staleness_bound_is_accepted_at_exactly_the_bound_and_refused_one_ledger_past_it() {
    let (env, client, _) = setup();
    env.mock_all_auths();

    let answer = 100_000_000;
    let answered_at = env.ledger().sequence();
    client.set_round_data(&round(1, answer, answered_at));

    // Age strictly inside the bound: served.
    env.ledger()
        .with_mut(|l| l.sequence_number = answered_at + MAX_STALENESS - 1);
    assert_eq!(client.latest_answer(&MAX_STALENESS), answer);

    // Age exactly at the bound: still served (boundary is inclusive).
    env.ledger()
        .with_mut(|l| l.sequence_number = answered_at + MAX_STALENESS);
    assert_eq!(
        client.latest_answer(&MAX_STALENESS),
        answer,
        "a round at exactly max_staleness ledgers old must be served"
    );
    assert_eq!(
        client.latest_round_data(&MAX_STALENESS),
        round(1, answer, answered_at)
    );

    // One ledger past the bound: refused as stale through both readers.
    env.ledger()
        .with_mut(|l| l.sequence_number = answered_at + MAX_STALENESS + 1);
    assert_eq!(
        client.try_latest_answer(&MAX_STALENESS),
        Err(Ok(Error::StalePrice)),
        "a round one ledger older than max_staleness must be refused"
    );
    assert_eq!(
        client.try_latest_round_data(&MAX_STALENESS),
        Err(Ok(Error::StalePrice)),
        "latest_round_data must apply the same staleness verdict"
    );
}
