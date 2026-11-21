//! Property-based tests for treasury split_by_weights invariant and dust allocation (issue #1018).

#![cfg(test)]

extern crate std;

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};
use treasury::strategies::{split_by_weights, MAX_STRATEGIES};
use treasury::Error;

fn valid_weights_strategy() -> impl Strategy<Value = std::vec::Vec<u32>> {
    (1usize..=(MAX_STRATEGIES as usize)).prop_flat_map(|n| {
        if n == 1 {
            Just(std::vec![10_000u32]).boxed()
        } else {
            proptest::collection::btree_set(1u32..10_000u32, n - 1)
                .prop_map(move |cuts| {
                    let mut weights = std::vec::Vec::with_capacity(n);
                    let mut prev = 0u32;
                    for cut in cuts {
                        weights.push(cut - prev);
                        prev = cut;
                    }
                    weights.push(10_000u32 - prev);
                    weights
                })
                .boxed()
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_split_by_weights_conserves_sum_and_bounds_shares(
        weights in valid_weights_strategy(),
        total in 0i128..=1_000_000_000_000_000i128,
    ) {
        let env = Env::default();
        let mut weights_vec: Vec<(Address, u32)> = Vec::new(&env);
        for &w in &weights {
            let addr = Address::generate(&env);
            weights_vec.push_back((addr, w));
        }

        let shares = split_by_weights(&env, total, &weights_vec)
            .expect("split_by_weights must succeed for valid weights and non-negative total");

        let mut sum: i128 = 0;
        for (_, share) in shares.iter() {
            prop_assert!(share >= 0, "calculated share must be non-negative");
            prop_assert!(share <= total, "calculated share cannot exceed total");
            sum = sum.checked_add(share).expect("sum should not overflow");
        }

        prop_assert_eq!(sum, total, "shares must sum exactly to total without remainder");
    }
}

#[test]
fn edge_case_single_strategy_takes_entire_total() {
    let env = Env::default();
    let addr = Address::generate(&env);
    let mut weights: Vec<(Address, u32)> = Vec::new(&env);
    weights.push_back((addr.clone(), 10_000));

    let total = 987_654_321i128;
    let shares = split_by_weights(&env, total, &weights).unwrap();

    assert_eq!(shares.len(), 1);
    let (strategy, share) = shares.get(0).unwrap();
    assert_eq!(strategy, addr);
    assert_eq!(share, total);
}

#[test]
fn edge_case_equal_weights_across_two_strategies_awards_dust_to_first() {
    let env = Env::default();
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let mut weights: Vec<(Address, u32)> = Vec::new(&env);
    weights.push_back((a1.clone(), 5_000));
    weights.push_back((a2.clone(), 5_000));

    // total = 3: 5000 bps is 1 each (assigned 2), dust 1 awarded to first index (a1)
    let total = 3i128;
    let shares = split_by_weights(&env, total, &weights).unwrap();

    assert_eq!(shares.get(0).unwrap(), (a1, 2));
    assert_eq!(shares.get(1).unwrap(), (a2, 1));
}

#[test]
fn edge_case_equal_weights_across_four_strategies_awards_dust_to_first() {
    let env = Env::default();
    let addrs: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(&env)).collect();
    let mut weights: Vec<(Address, u32)> = Vec::new(&env);
    for addr in &addrs {
        weights.push_back((addr.clone(), 2_500));
    }

    // total = 5: 2500 bps is 1 each (assigned 4), dust 1 goes to index 0
    let total = 5i128;
    let shares = split_by_weights(&env, total, &weights).unwrap();

    assert_eq!(shares.get(0).unwrap(), (addrs[0].clone(), 2));
    assert_eq!(shares.get(1).unwrap(), (addrs[1].clone(), 1));
    assert_eq!(shares.get(2).unwrap(), (addrs[2].clone(), 1));
    assert_eq!(shares.get(3).unwrap(), (addrs[3].clone(), 1));
}

#[test]
fn edge_case_equal_weights_across_eight_strategies_awards_dust_to_first() {
    let env = Env::default();
    let addrs: std::vec::Vec<Address> = (0..8).map(|_| Address::generate(&env)).collect();
    let mut weights: Vec<(Address, u32)> = Vec::new(&env);
    for addr in &addrs {
        weights.push_back((addr.clone(), 1_250));
    }

    // total = 11: 1250 bps is 1 each (assigned 8), dust 3 goes to index 0 (1 + 3 = 4)
    let total = 11i128;
    let shares = split_by_weights(&env, total, &weights).unwrap();

    assert_eq!(shares.get(0).unwrap(), (addrs[0].clone(), 4));
    for (i, addr) in addrs.iter().enumerate().skip(1) {
        assert_eq!(shares.get(i as u32).unwrap(), (addr.clone(), 1));
    }

    let mut sum: i128 = 0;
    for (_, s) in shares.iter() {
        sum += s;
    }
    assert_eq!(sum, total);
}

#[test]
fn edge_case_ten_strategies_rejected_as_exceeding_max_strategies() {
    let env = Env::default();
    let addrs: std::vec::Vec<Address> = (0..10).map(|_| Address::generate(&env)).collect();
    let mut weights: Vec<(Address, u32)> = Vec::new(&env);
    for addr in &addrs {
        weights.push_back((addr.clone(), 1_000));
    }

    // Even though sum == 10_000, 10 strategies exceeds MAX_STRATEGIES (8)
    let total = 10_000i128;
    let res = split_by_weights(&env, total, &weights);
    assert_eq!(res, Err(Error::InvalidAllocations));
}
