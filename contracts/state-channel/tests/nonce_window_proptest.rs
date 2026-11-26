//! Property-based tests for state channel nonce window boundaries (issue #1009).
//!
//! Verifies sliding bitmap replay protection across boundary transitions,
//! out-of-order permutation arrivals, and forward window shifts in `NonceWindow`.

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use proptest::prelude::*;
use soroban_sdk::{testutils::EnvTestConfig, Env};
use state_channel::nonce::{NonceWindow, WINDOW_SIZE};
use std::collections::HashSet;

/// Helper to construct an isolated environment without recording golden snapshot files.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// Strategy for aligned window base nonces (multiples of WINDOW_SIZE = 256).
fn aligned_base_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => Just(0u64),
        2 => (1u64..100u64).prop_map(|k| k * WINDOW_SIZE),
        1 => (100u64..10_000u64).prop_map(|k| k * WINDOW_SIZE),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Arbitrary permutation sequences of in-window nonce insertions and queries.
    ///
    /// Proves that:
    /// - Any nonce within [base, base + WINDOW_SIZE) can be consumed in any order.
    /// - Replays of consumed nonces fail unconditionally with `Error::StaleState`.
    /// - Consuming one nonce does not prematurely evict or block siblings.
    /// - `is_consumed` reflects exact consumption state without bitmap corruption.
    #[test]
    fn prop_in_window_permutation_consumption_and_replays(
        base in aligned_base_strategy(),
        offsets in prop::collection::vec(0u64..WINDOW_SIZE, 1..=64),
    ) {
        let env = test_env();
        let mut window = NonceWindow {
            base,
            bitmap: soroban_sdk::BytesN::from_array(&env, &[0u8; 32]),
        };

        let mut consumed_set = HashSet::new();

        for offset in offsets {
            let nonce = base + offset;

            if consumed_set.contains(&nonce) {
                // Replay attempt must fail.
                prop_assert!(window.is_consumed(nonce), "already consumed nonce must report is_consumed = true");
                prop_assert_eq!(
                    window.consume(&env, nonce),
                    Err(Error::StaleState),
                    "duplicate consumption of nonce {} must fail with StaleState",
                    nonce
                );
            } else {
                // Fresh nonce must succeed.
                prop_assert!(!window.is_consumed(nonce), "fresh nonce must report is_consumed = false");
                prop_assert_eq!(
                    window.consume(&env, nonce),
                    Ok(()),
                    "first consumption of nonce {} must succeed",
                    nonce
                );
                prop_assert!(window.is_consumed(nonce), "newly consumed nonce must report is_consumed = true");

                // Immediate replay must fail.
                prop_assert_eq!(
                    window.consume(&env, nonce),
                    Err(Error::StaleState),
                    "immediate replay of nonce {} must fail with StaleState",
                    nonce
                );

                consumed_set.insert(nonce);
            }
        }

        // Final verification across the entire window.
        for offset in 0..WINDOW_SIZE {
            let nonce = base + offset;
            let expected = consumed_set.contains(&nonce);
            prop_assert_eq!(
                window.is_consumed(nonce),
                expected,
                "is_consumed mismatch for nonce {}",
                nonce
            );
            if expected {
                prop_assert_eq!(
                    window.consume(&env, nonce),
                    Err(Error::StaleState),
                    "subsequent consumption of already consumed nonce {} must fail",
                    nonce
                );
            }
        }

        // Base must remain unshifted.
        prop_assert_eq!(window.base, base);
    }

    /// Critical window edge boundaries: exact boundary limits, shifts, and one-unit offsets.
    ///
    /// Proves that:
    /// - `base + WINDOW_SIZE - 1` (last element) stays within window without shifting base.
    /// - `base + WINDOW_SIZE` (threshold boundary) shifts window forward by 1 bucket.
    /// - Post-slide, all nonces `< new_base` produce typed boundary error `Error::StaleState`.
    /// - Immediate predecessor `new_base - 1` is permanently stale.
    #[test]
    fn prop_exact_boundary_thresholds_and_shifts(
        base in aligned_base_strategy(),
    ) {
        let env = test_env();
        let mut window = NonceWindow {
            base,
            bitmap: soroban_sdk::BytesN::from_array(&env, &[0u8; 32]),
        };

        // 1. Lower boundary: `base` itself.
        prop_assert!(!window.is_consumed(base));
        prop_assert_eq!(window.consume(&env, base), Ok(()));
        prop_assert!(window.is_consumed(base));
        prop_assert_eq!(window.consume(&env, base), Err(Error::StaleState));
        prop_assert_eq!(window.base, base);

        // 2. Upper in-window boundary: `base + WINDOW_SIZE - 1`.
        let upper_in_window = base + WINDOW_SIZE - 1;
        prop_assert!(!window.is_consumed(upper_in_window));
        prop_assert_eq!(window.consume(&env, upper_in_window), Ok(()));
        prop_assert!(window.is_consumed(upper_in_window));
        prop_assert_eq!(window.consume(&env, upper_in_window), Err(Error::StaleState));
        // Base must still not shift.
        prop_assert_eq!(window.base, base);

        // 3. Sliding window threshold: exactly `base + WINDOW_SIZE`.
        let threshold_nonce = base + WINDOW_SIZE;
        prop_assert!(!window.is_consumed(threshold_nonce));
        prop_assert_eq!(window.consume(&env, threshold_nonce), Ok(()));

        // Window shifted by WINDOW_SIZE.
        let new_base = base + WINDOW_SIZE;
        prop_assert_eq!(window.base, new_base);
        prop_assert!(window.is_consumed(threshold_nonce));
        prop_assert_eq!(window.consume(&env, threshold_nonce), Err(Error::StaleState));

        // 4. Boundary checks on evicted / stale nonces:
        // Immediate unit before the new boundary (`new_base - 1`) must fail.
        let just_evicted = new_base - 1;
        prop_assert!(window.is_consumed(just_evicted), "evicted boundary nonce must be considered consumed");
        prop_assert_eq!(
            window.consume(&env, just_evicted),
            Err(Error::StaleState),
            "evicted boundary nonce must fail with StaleState"
        );

        // The old lower boundary `base` is also < new_base and must fail.
        prop_assert!(window.is_consumed(base));
        prop_assert_eq!(
            window.consume(&env, base),
            Err(Error::StaleState),
            "nonce before new window base must fail with StaleState"
        );

        // Nonce 0 if base > 0.
        if base > 0 {
            prop_assert!(window.is_consumed(0));
            prop_assert_eq!(window.consume(&env, 0), Err(Error::StaleState));
        }

        // 5. Remaining fresh nonces in the new window are still valid and uncorrupted.
        let next_fresh = new_base + 1;
        prop_assert!(!window.is_consumed(next_fresh));
        prop_assert_eq!(window.consume(&env, next_fresh), Ok(()));
        prop_assert!(window.is_consumed(next_fresh));
        prop_assert_eq!(window.consume(&env, next_fresh), Err(Error::StaleState));
    }

    /// Multi-bucket forward window sliding and historical replay rejection.
    ///
    /// Proves that jumping forward by multiple window buckets cleanly slides the
    /// base, marks intermediate skipped buckets as stale, and permanently rejects
    /// replays of nonces from all previous windows.
    #[test]
    fn prop_multi_bucket_window_sliding_preserves_replay_protection(
        initial_base in aligned_base_strategy(),
        in_window_offsets in prop::collection::vec(0u64..WINDOW_SIZE, 1..=16),
        jump in 1u64..20u64,
        forward_offset in 0u64..WINDOW_SIZE,
    ) {
        let env = test_env();
        let mut window = NonceWindow {
            base: initial_base,
            bitmap: soroban_sdk::BytesN::from_array(&env, &[0u8; 32]),
        };

        // Consume initial nonces in the first window.
        let mut consumed_initial = HashSet::new();
        for off in in_window_offsets {
            let n = initial_base + off;
            if consumed_initial.insert(n) {
                prop_assert_eq!(window.consume(&env, n), Ok(()));
            }
        }

        // Slide forward to a new bucket.
        let forward_nonce = initial_base + jump * WINDOW_SIZE + forward_offset;
        let expected_new_base = forward_nonce - (forward_nonce % WINDOW_SIZE);
        prop_assert!(expected_new_base >= initial_base + WINDOW_SIZE);

        prop_assert_eq!(window.consume(&env, forward_nonce), Ok(()));
        prop_assert_eq!(window.base, expected_new_base);
        prop_assert!(window.is_consumed(forward_nonce));

        // Replay of forward nonce must fail.
        prop_assert_eq!(window.consume(&env, forward_nonce), Err(Error::StaleState));

        // Every previously consumed nonce is now strictly < expected_new_base and must fail.
        for &old_nonce in &consumed_initial {
            prop_assert!(
                window.is_consumed(old_nonce),
                "previously consumed nonce {} must report is_consumed = true after slide",
                old_nonce
            );
            prop_assert_eq!(
                window.consume(&env, old_nonce),
                Err(Error::StaleState),
                "previously consumed nonce {} must fail with StaleState after slide",
                old_nonce
            );
        }

        // Unconsumed nonces in old / skipped windows are also < expected_new_base and must fail.
        let old_unconsumed = initial_base + (WINDOW_SIZE / 2);
        if !consumed_initial.contains(&old_unconsumed) {
            prop_assert!(
                window.is_consumed(old_unconsumed),
                "unconsumed nonce {} from older window must be deemed consumed",
                old_unconsumed
            );
            prop_assert_eq!(
                window.consume(&env, old_unconsumed),
                Err(Error::StaleState),
                "unconsumed nonce {} from older window must be rejected with StaleState",
                old_unconsumed
            );
        }

        // The edge boundary immediately before expected_new_base.
        let immediate_predecessor = expected_new_base - 1;
        prop_assert!(window.is_consumed(immediate_predecessor));
        prop_assert_eq!(window.consume(&env, immediate_predecessor), Err(Error::StaleState));

        // Queries for unconsumed nonces beyond the new forward window threshold return false.
        let future_nonce = expected_new_base + WINDOW_SIZE + 10;
        prop_assert!(
            !window.is_consumed(future_nonce),
            "uncovered nonce beyond new forward window must report is_consumed = false"
        );
    }
}

/// Action types for model-based state machine fuzzing.
#[derive(Clone, Debug)]
enum WindowAction {
    ConsumeInWindow(u64),
    ConsumeBehind(u64),
    SlideForward { jump: u64, offset: u64 },
    Query(u64),
}

fn window_action_strategy() -> impl Strategy<Value = WindowAction> {
    prop_oneof![
        4 => (0u64..WINDOW_SIZE).prop_map(WindowAction::ConsumeInWindow),
        2 => (1u64..500u64).prop_map(WindowAction::ConsumeBehind),
        3 => (1u64..10u64, 0u64..WINDOW_SIZE).prop_map(|(jump, offset)| WindowAction::SlideForward { jump, offset }),
        2 => (0u64..50_000u64).prop_map(WindowAction::Query),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Model-based state machine fuzzing against shadow tracker.
    ///
    /// Interleaves in-window insertions, out-of-order queries, out-of-range
    /// attempts, and sliding jumps while checking all NonceWindow invariants.
    #[test]
    fn prop_interleaved_operations_shadow_model(
        actions in prop::collection::vec(window_action_strategy(), 1..=128),
    ) {
        let env = test_env();
        let mut window = NonceWindow::empty(&env);

        let mut shadow_base = 0u64;
        let mut shadow_consumed_in_window = HashSet::new();
        let mut shadow_all_consumed = HashSet::new();

        for action in actions {
            match action {
                WindowAction::ConsumeInWindow(offset) => {
                    let nonce = shadow_base + offset;
                    let was_consumed = shadow_consumed_in_window.contains(&nonce);

                    prop_assert_eq!(window.is_consumed(nonce), was_consumed);

                    if was_consumed {
                        prop_assert_eq!(
                            window.consume(&env, nonce),
                            Err(Error::StaleState),
                            "replay of {} must fail",
                            nonce
                        );
                    } else {
                        prop_assert_eq!(
                            window.consume(&env, nonce),
                            Ok(()),
                            "consumption of fresh nonce {} must succeed",
                            nonce
                        );
                        shadow_consumed_in_window.insert(nonce);
                        shadow_all_consumed.insert(nonce);
                        prop_assert!(window.is_consumed(nonce));
                    }
                }
                WindowAction::ConsumeBehind(delta) => {
                    if shadow_base > 0 {
                        let stale_nonce = shadow_base.saturating_sub(delta);
                        if stale_nonce < shadow_base {
                            prop_assert!(window.is_consumed(stale_nonce));
                            prop_assert_eq!(
                                window.consume(&env, stale_nonce),
                                Err(Error::StaleState),
                                "nonce {} < base {} must be rejected",
                                stale_nonce,
                                shadow_base
                            );
                        }
                    }
                }
                WindowAction::SlideForward { jump, offset } => {
                    let slide_nonce = shadow_base + jump * WINDOW_SIZE + offset;
                    let new_base = slide_nonce - (slide_nonce % WINDOW_SIZE);
                    prop_assert!(new_base >= shadow_base + WINDOW_SIZE);

                    prop_assert_eq!(window.consume(&env, slide_nonce), Ok(()));

                    shadow_base = new_base;
                    shadow_consumed_in_window.clear();
                    shadow_consumed_in_window.insert(slide_nonce);
                    shadow_all_consumed.insert(slide_nonce);

                    prop_assert_eq!(window.base, shadow_base);
                    prop_assert!(window.is_consumed(slide_nonce));
                    prop_assert_eq!(window.consume(&env, slide_nonce), Err(Error::StaleState));
                }
                WindowAction::Query(query_nonce) => {
                    let is_cons = window.is_consumed(query_nonce);
                    if query_nonce < shadow_base {
                        prop_assert!(is_cons, "nonces < base must report is_consumed = true");
                    } else if query_nonce >= shadow_base + WINDOW_SIZE {
                        prop_assert!(!is_cons, "nonces >= base + WINDOW_SIZE must report is_consumed = false");
                    } else {
                        prop_assert_eq!(
                            is_cons,
                            shadow_consumed_in_window.contains(&query_nonce),
                            "in-window query mismatch for {}",
                            query_nonce
                        );
                    }
                }
            }

            // Invariant assertions after every action:
            prop_assert_eq!(window.base, shadow_base);
            prop_assert_eq!(window.base % WINDOW_SIZE, 0, "base must always be a multiple of WINDOW_SIZE");

            // Replay protection invariant: every consumed nonce ever tracked must fail if re-submitted.
            for &consumed_nonce in &shadow_all_consumed {
                if consumed_nonce < window.base {
                    prop_assert!(window.is_consumed(consumed_nonce));
                    prop_assert_eq!(window.consume(&env, consumed_nonce), Err(Error::StaleState));
                } else if consumed_nonce < window.base + WINDOW_SIZE {
                    prop_assert!(window.is_consumed(consumed_nonce));
                    prop_assert_eq!(window.consume(&env, consumed_nonce), Err(Error::StaleState));
                }
            }
        }
    }
}
