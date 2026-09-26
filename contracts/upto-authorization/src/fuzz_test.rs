//! Property tests for the slippage-tolerance bound.

#![cfg(test)]

extern crate std;

use crate::test::{setup_in, Setup};
use crate::{max_settleable, Error, MAX_SLIPPAGE_BPS};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig},
    token::TokenClient,
    Address, BytesN, Env,
};

/// Randomized cases would each write (and churn) a ledger snapshot file.
fn setup() -> Setup {
    setup_in(Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    }))
}

/// Independent reference for `cap + floor(cap * bps / 10_000)`: long division
/// over 64-bit limbs, a different decomposition from the contract's
/// quotient/remainder split on 10_000.
fn reference(cap: i128, bps: u32) -> Option<i128> {
    let cap = cap as u128;
    let bps = bps as u128;
    // Split on 2^64 instead of 10_000.
    let hi = cap >> 64;
    let lo = cap & u64::MAX as u128;
    // cap * bps = hi*bps*2^64 + lo*bps; divide by 10_000 exactly via
    // long division over the two limbs.
    let hi_prod = hi * bps; // < 2^63 * 2^14
    let q_hi = hi_prod / 10_000;
    let r_hi = hi_prod % 10_000;
    // (r_hi * 2^64 + lo * bps) / 10_000; r_hi < 10_000, so r_hi * 2^64 < 2^78.
    let low_part = (r_hi << 64) + lo * bps;
    let q_lo = low_part / 10_000;
    let tolerance = (q_hi << 64) + q_lo;
    let total = cap.checked_add(tolerance)?;
    i128::try_from(total).ok()
}

proptest! {
    #[test]
    fn prop_max_settleable_matches_reference(cap in 0i128..=i128::MAX, bps in 0u32..=MAX_SLIPPAGE_BPS) {
        prop_assert_eq!(max_settleable(cap, bps), reference(cap, bps));
    }

    #[test]
    fn prop_max_settleable_bounds(cap in 0i128..=i128::MAX / 2, bps in 0u32..=MAX_SLIPPAGE_BPS) {
        // Never overflows in this range, never below cap, never above 2 * cap.
        let max = max_settleable(cap, bps).unwrap();
        prop_assert!(max >= cap);
        prop_assert!(max - cap <= cap);
        // Monotonic in bps.
        if bps < MAX_SLIPPAGE_BPS {
            prop_assert!(max_settleable(cap, bps + 1).unwrap() >= max);
        }
    }

    #[test]
    fn prop_out_of_range_bps_rejected(cap in 0i128..=i128::MAX, bps in (MAX_SLIPPAGE_BPS + 1)..=u32::MAX) {
        prop_assert_eq!(max_settleable(cap, bps), None);
    }
}

proptest! {
    // Each case spins up a fresh Soroban env; keep the count modest.
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_settle_accepts_iff_within_tolerance(
        cap in 1i128..=1_000_000,
        bps in 0u32..=MAX_SLIPPAGE_BPS,
        actual in 1i128..=2_000_001,
    ) {
        let (env, client, _admin, buyer, _seller, token) = setup();
        let p = BytesN::from_array(&env, &[1; 32]);
        let recipient = Address::generate(&env);

        client.authorize_with_slippage(&p, &buyer, &recipient, &cap, &1000, &bps);
        let max = max_settleable(cap, bps).unwrap();
        let res = client.try_settle(&p, &actual);

        let tc = TokenClient::new(&env, &token);
        if actual <= max {
            prop_assert_eq!(res, Ok(Ok(())));
            prop_assert_eq!(tc.balance(&recipient), actual);
        } else {
            prop_assert_eq!(res, Err(Ok(Error::AmountExceedsCap)));
            prop_assert_eq!(tc.balance(&recipient), 0);
        }
    }
}
