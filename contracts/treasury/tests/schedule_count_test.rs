use soroban_sdk::{testutils::Address as _, Address, Env};
use treasury::{Error, Treasury, TreasuryClient};

#[test]
fn schedule_count_tracks_schedules_monotonically() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    // Initialized contract with no schedules returns 0
    assert_eq!(client.get_schedule_count(), 0);

    // Add schedule 1 for Alice
    let alice = Address::generate(&env);
    client.add_schedule(&alice, &10_000, &0, &100, &1_000);
    assert_eq!(client.get_schedule_count(), 1);

    // Add schedule 2 (team schedule) for Bob
    let bob = Address::generate(&env);
    client.add_team_schedule(&bob, &50_000, &0);
    assert_eq!(client.get_schedule_count(), 2);

    // Failed schedule addition (duplicate beneficiary) does NOT increment count
    assert_eq!(
        client.try_add_schedule(&alice, &20_000, &0, &100, &1_000),
        Err(Ok(Error::ScheduleAlreadyExists))
    );
    assert_eq!(client.get_schedule_count(), 2);

    // Failed schedule addition (invalid parameters: duration = 0) does NOT increment count
    let charlie = Address::generate(&env);
    assert_eq!(
        client.try_add_schedule(&charlie, &10_000, &0, &100, &0),
        Err(Ok(Error::InvalidSchedule))
    );
    assert_eq!(client.get_schedule_count(), 2);

    // Add schedule 3 for Charlie
    client.add_schedule(&charlie, &30_000, &0, &50, &500);
    assert_eq!(client.get_schedule_count(), 3);

    // Verify existing address-based lookups remain accurate
    let alice_sched = client.get_schedule(&alice).unwrap();
    assert_eq!(alice_sched.total, 10_000);
    assert_eq!(alice_sched.cliff, 100);

    let bob_sched = client.get_schedule(&bob).unwrap();
    assert_eq!(bob_sched.total, 50_000);

    let charlie_sched = client.get_schedule(&charlie).unwrap();
    assert_eq!(charlie_sched.total, 30_000);
}
