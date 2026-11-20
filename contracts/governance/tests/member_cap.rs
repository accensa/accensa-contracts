use governance::{Error, Governance, GovernanceClient};
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

#[test]
fn governance_initializes_with_32_members_and_returns_correct_allocations() {
    let env = Env::default();
    env.mock_all_auths();

    let mut members = Vec::new(&env);
    let mut deposits = Vec::new(&env);

    // Use perfect squares (i * i) so integer_sqrt gives exact weight i
    for i in 1..=32u64 {
        let member = Address::generate(&env);
        members.push_back(member);
        deposits.push_back(i * i);
    }

    let gov_id = env.register(
        Governance,
        (members.clone(), deposits.clone(), 5_000u32, 10u32),
    );
    let client = GovernanceClient::new(&env, &gov_id);

    // Verify each of the 32 members is recognized with correct deposit and weight
    for (idx, member) in members.iter().enumerate() {
        let i = idx as u64 + 1;
        assert!(client.is_member(&member));
        assert_eq!(client.get_member_deposit(&member), i * i);
        assert_eq!(client.get_member_weight(&member), i);
    }

    // Verify total weight is sum of 1..=32 = 32 * 33 / 2 = 528
    let expected_total_weight: u64 = (1..=32u64).sum();
    assert_eq!(client.get_total_weight(), expected_total_weight);
    assert_eq!(client.get_total_weight(), 528);
}

#[test]
fn governance_rejects_initialization_with_33_members() {
    let env = Env::default();
    env.mock_all_auths();

    let mut members = Vec::new(&env);
    let mut deposits = Vec::new(&env);

    for i in 1..=33u64 {
        let member = Address::generate(&env);
        members.push_back(member);
        deposits.push_back(i * i);
    }

    // A valid instance gives as_contract an address to run against so the constructor Result is observable
    let dummy_member = Address::generate(&env);
    let gov_id = env.register(
        Governance,
        (
            Vec::from_array(&env, [dummy_member]),
            Vec::from_array(&env, [1u64]),
            5_000u32,
            10u32,
        ),
    );

    let res = env.as_contract(&gov_id, || {
        Governance::__constructor(env.clone(), members, deposits, 5_000u32, 10u32)
    });

    assert_eq!(res, Err(Error::InvalidMembers));
}
