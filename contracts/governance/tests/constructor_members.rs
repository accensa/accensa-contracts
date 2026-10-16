//! `Governance::__constructor` pairs members with deposits (issue #670).
//!
//! The constructor used to walk `members` and `deposits` by index with
//! `get(i).unwrap()`. It now zips the two vectors. These tests pin the
//! behaviour the rewrite must keep: every member keeps its own deposit and
//! weight, order is preserved, the member cap is inclusive, and the existing
//! rejections still reject.

use governance::{Governance, GovernanceClient};
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

fn register(env: &Env, members: &Vec<Address>, deposits: &Vec<u64>) -> Address {
    env.register(
        Governance,
        (members.clone(), deposits.clone(), 5000u32, 10u32),
    )
}

#[test]
fn constructor_pairs_each_member_with_its_own_deposit() {
    let env = Env::default();
    let members: Vec<Address> = Vec::from_array(
        &env,
        [
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ],
    );
    // Distinct perfect squares, so a swapped pair would change a weight.
    let deposits = Vec::from_array(&env, [1u64, 4u64, 9u64, 16u64]);

    let id = register(&env, &members, &deposits);
    let gov = GovernanceClient::new(&env, &id);

    for (index, expected_deposit) in [1u64, 4, 9, 16].into_iter().enumerate() {
        let member = members.get(index as u32).unwrap();
        assert_eq!(gov.get_member_deposit(&member), expected_deposit);
        assert_eq!(gov.get_member_weight(&member), (index as u64) + 1);
        assert!(gov.is_member(&member));
    }
    assert_eq!(gov.get_total_weight(), 1 + 2 + 3 + 4);
    assert_eq!(gov.get_total_deposits(), 1 + 4 + 9 + 16);
}

#[test]
fn constructor_accepts_exactly_the_maximum_member_count() {
    let env = Env::default();
    let mut members = Vec::new(&env);
    let mut deposits = Vec::new(&env);
    for _ in 0..32 {
        members.push_back(Address::generate(&env));
        deposits.push_back(1u64);
    }

    let id = register(&env, &members, &deposits);
    let gov = GovernanceClient::new(&env, &id);

    assert_eq!(gov.get_total_weight(), 32);
    assert_eq!(gov.get_total_deposits(), 32);
    assert!(gov.is_member(&members.get(31).unwrap()));
}

fn assert_rejected(members: Vec<Address>, deposits: Vec<u64>, env: &Env) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register(Governance, (members, deposits, 5000u32, 10u32))
    }));
    assert!(outcome.is_err(), "construction must be rejected");
}

#[test]
fn constructor_still_rejects_bad_input() {
    let env = Env::default();
    let a = Address::generate(&env);
    let b = Address::generate(&env);

    // Mismatched lengths, either way round.
    assert_rejected(
        Vec::from_array(&env, [a.clone()]),
        Vec::from_array(&env, [1u64, 2u64]),
        &env,
    );
    assert_rejected(
        Vec::from_array(&env, [a.clone(), b.clone()]),
        Vec::from_array(&env, [1u64]),
        &env,
    );
    // No members.
    assert_rejected(Vec::new(&env), Vec::new(&env), &env);
    // A zero deposit, in any position.
    assert_rejected(
        Vec::from_array(&env, [a.clone(), b.clone()]),
        Vec::from_array(&env, [4u64, 0u64]),
        &env,
    );
    // The same member twice.
    assert_rejected(
        Vec::from_array(&env, [a.clone(), a.clone()]),
        Vec::from_array(&env, [1u64, 1u64]),
        &env,
    );
    // One member over the cap.
    let mut members = Vec::new(&env);
    let mut deposits = Vec::new(&env);
    for _ in 0..33 {
        members.push_back(Address::generate(&env));
        deposits.push_back(1u64);
    }
    assert_rejected(members, deposits, &env);
}
