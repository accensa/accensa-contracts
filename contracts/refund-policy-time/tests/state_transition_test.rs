//! Every boundary of `determine_refund_state`'s four states (issue #981).
//!
//! The function walks a strict-less-than chain over
//! `start_time`, `start_time + grace_period` and
//! `start_time + grace_period + cooldown`, so each of the three instants is a
//! decision point whose *inclusive* side belongs to the later state. Nothing
//! in the names or the comments settles that, and the function is re-exported
//! from the crate root for integrators, so a later refactor that turns one
//! `<` into a `<=` moves a boundary by a second and is invisible to the rest
//! of the suite, which only ever calls `evaluate`.
//!
//! The table below is the whole contract: both sides of each instant, the
//! interior of each state, and the two degenerate offsets. Flipping any single
//! comparison to `<=` breaks at least one labelled row:
//!
//! | mutation on `state_transition.rs`      | rows that fail |
//! |----------------------------------------|----------------|
//! | first `<` becomes `<=`                 | `before start` |
//! | second `<` becomes `<=`                | `at start` |
//! | third `<` becomes `<=`                 | `at grace end` |
//! | (chain reordered so `Expired` wins)    | `at cooldown end` and the two zero-offset rows |

use refund_policy_time::{determine_refund_state, RefundState};
use soroban_sdk::Env;

const START: u64 = 1_000_000;
const GRACE: u64 = 100_000;
const COOLDOWN: u64 = 50_000;
const GRACE_END: u64 = START + GRACE;
const COOLDOWN_END: u64 = GRACE_END + COOLDOWN;

/// One row of the table: a claim instant and the offsets it is measured against.
struct Row {
    label: &'static str,
    at: u64,
    grace: u64,
    cooldown: u64,
    expected: RefundState,
}

const ROWS: &[Row] = &[
    // ── Full offsets: both sides of all three instants, plus an interior
    //    point per state so a wholesale rewrite of the chain cannot pass. ──
    Row {
        label: "one second before start",
        at: START - 1,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Active,
    },
    Row {
        label: "well before start",
        at: 0,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Active,
    },
    Row {
        label: "at start",
        at: START,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::GracePeriod,
    },
    Row {
        label: "last second of grace",
        at: GRACE_END - 1,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::GracePeriod,
    },
    Row {
        label: "middle of grace",
        at: START + GRACE / 2,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::GracePeriod,
    },
    Row {
        label: "at grace end",
        at: GRACE_END,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Cooldown,
    },
    Row {
        label: "last second of cooldown",
        at: COOLDOWN_END - 1,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Cooldown,
    },
    Row {
        label: "middle of cooldown",
        at: GRACE_END + COOLDOWN / 2,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Cooldown,
    },
    Row {
        label: "at cooldown end",
        at: COOLDOWN_END,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Expired,
    },
    Row {
        label: "long after cooldown end",
        at: COOLDOWN_END + 1,
        grace: GRACE,
        cooldown: COOLDOWN,
        expected: RefundState::Expired,
    },
    // ── `grace = 0`: the grace window collapses, so `start` itself is the
    //    first instant of cooldown rather than of grace. ──
    Row {
        label: "zero grace, before start",
        at: START - 1,
        grace: 0,
        cooldown: COOLDOWN,
        expected: RefundState::Active,
    },
    Row {
        label: "zero grace, at start",
        at: START,
        grace: 0,
        cooldown: COOLDOWN,
        expected: RefundState::Cooldown,
    },
    Row {
        label: "zero grace, last second of cooldown",
        at: START + COOLDOWN - 1,
        grace: 0,
        cooldown: COOLDOWN,
        expected: RefundState::Cooldown,
    },
    Row {
        label: "zero grace, at cooldown end",
        at: START + COOLDOWN,
        grace: 0,
        cooldown: COOLDOWN,
        expected: RefundState::Expired,
    },
    // ── `cooldown = 0`: reaching the end of grace expires the claim outright. ──
    Row {
        label: "zero cooldown, last second of grace",
        at: GRACE_END - 1,
        grace: GRACE,
        cooldown: 0,
        expected: RefundState::GracePeriod,
    },
    Row {
        label: "zero cooldown, at grace end",
        at: GRACE_END,
        grace: GRACE,
        cooldown: 0,
        expected: RefundState::Expired,
    },
    // ── Both offsets zero: `start` is the only boundary left, and the instant
    //    it arrives the claim is expired. ──
    Row {
        label: "both offsets zero, at start",
        at: START,
        grace: 0,
        cooldown: 0,
        expected: RefundState::Expired,
    },
];

#[test]
fn every_boundary_instant_maps_to_the_documented_state() {
    let env = Env::default();
    for row in ROWS {
        let got = determine_refund_state(&env, row.at, START, row.grace, row.cooldown);
        assert_eq!(
            got, row.expected,
            "{}: t={} grace={} cooldown={}",
            row.label, row.at, row.grace, row.cooldown
        );
    }
}

/// The state sequence over the full-offset timeline is exactly
/// `Active, GracePeriod, Cooldown, Expired` with no state skipped, held for
/// zero seconds, or entered twice. The table pins the instants; this pins that
/// the four states partition the timeline, which a reordered chain would break
/// while leaving individual rows plausible.
#[test]
fn the_four_states_partition_the_timeline() {
    let env = Env::default();
    let mut visits = [0u32; 4];
    let mut last: Option<RefundState> = None;
    for t in [
        0,
        START - 1,
        START,
        GRACE_END - 1,
        GRACE_END,
        COOLDOWN_END - 1,
        COOLDOWN_END,
        u64::MAX,
    ] {
        let state = determine_refund_state(&env, t, START, GRACE, COOLDOWN);
        visits[index_of(&state)] += 1;
        // The chain is monotonic in `t`: once it advances it never comes back.
        if let Some(previous) = last {
            assert!(
                index_of(&state) >= index_of(&previous),
                "state went backwards between t={} ({:?}) and t={} ({:?})",
                t - 1,
                previous,
                t,
                state
            );
        }
        last = Some(state);
    }
    assert_eq!(visits, [2, 2, 2, 2]);
}

fn index_of(state: &RefundState) -> usize {
    match state {
        RefundState::Active => 0,
        RefundState::GracePeriod => 1,
        RefundState::Cooldown => 2,
        RefundState::Expired => 3,
    }
}
