//! Boundary tests for the guarded final addition in [`apply_decay`]
//! (issue #724).
//!
//! `apply_decay` returns a plain `u64`, so its failure arm cannot surface a
//! typed error from this crate's enums. The guard instead rests on a stated
//! precondition: `decayed` is the remainder of the headroom
//! `score - SCORE_FLOOR`, so `SCORE_FLOOR + decayed <= score` for every
//! input. These tests drive the largest score the sum can ever see, the edge
//! where the remainder is the whole headroom, and the saturated-interval
//! case, pinning that the checked addition returns the exact value instead
//! of trapping the host call or wrapping.

extern crate std;

use super::*;

#[test]
fn decay_at_u64_max_returns_the_exact_score_without_overflow() {
    // `u64::MAX` is the largest value the final addition can see. One
    // interval decays 1% of the headroom, so the remainder is the headroom
    // minus that share and the sum must be strict inside `u64`.
    let score = u64::MAX;
    let headroom = score - SCORE_FLOOR;
    let decayed = apply_decay(score, 0, DECAY_INTERVAL_LEDGERS);

    // decay = headroom * DECAY_RATE_BPS / 10_000 = headroom / 100.
    assert_eq!(decayed, score - headroom / DECAY_RATE_BPS);
    assert!(
        decayed <= score,
        "the guarded sum must stay <= score, got {decayed}"
    );
}

#[test]
fn zero_remainder_reaches_the_score_bound_without_wrapping() {
    // A headroom below the bps denominator decays by zero, so `decayed` is
    // the whole headroom and the sum lands exactly on `score`. This is the
    // tightest in-range case: the precondition `SCORE_FLOOR + decayed <=
    // score` is met with equality.
    let score = SCORE_FLOOR + DECAY_RATE_BPS - 1;
    assert_eq!(apply_decay(score, 0, DECAY_INTERVAL_LEDGERS), score);
}

#[test]
fn maximal_elapsed_ledgers_saturates_to_the_floor_without_wrapping() {
    // `u32::MAX` ledgers is ~24,855 intervals, so the decay product exceeds
    // the headroom: the remainder is zero and the guarded sum is the floor,
    // never a wrapped `SCORE_FLOOR + huge`.
    assert_eq!(apply_decay(u64::MAX, 0, u32::MAX), SCORE_FLOOR);
}
