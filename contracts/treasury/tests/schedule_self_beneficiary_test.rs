//! Integration tests verifying that the treasury rejects itself as a schedule beneficiary (issue #1019).

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env};
use treasury::{Error, Treasury, TreasuryClient};

#[test]
fn add_schedule_rejects_treasury_as_own_beneficiary() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    let total: i128 = 100_000;
    let start: u64 = 1_000;
    let cliff: u64 = 100;
    let duration: u64 = 500;

    // Passing the treasury contract address as the schedule beneficiary must fail with Error::InvalidSchedule
    let result = client.try_add_schedule(&treasury, &total, &start, &cliff, &duration);
    assert_eq!(result, Err(Ok(Error::InvalidSchedule)));

    // Verify schedule was not created
    assert_eq!(client.get_schedule(&treasury), None);
}

#[test]
fn add_schedule_succeeds_with_external_beneficiary() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    let total: i128 = 100_000;
    let start: u64 = 1_000;
    let cliff: u64 = 100;
    let duration: u64 = 500;

    // Normal schedule creation with external beneficiary succeeds
    client.add_schedule(&beneficiary, &total, &start, &cliff, &duration);

    let schedule = client
        .get_schedule(&beneficiary)
        .expect("schedule must exist");
    assert_eq!(schedule.beneficiary, beneficiary);
    assert_eq!(schedule.total, total);
    assert_eq!(schedule.claimed, 0);
    assert_eq!(schedule.start, start);
    assert_eq!(schedule.cliff, cliff);
    assert_eq!(schedule.duration, duration);
}
