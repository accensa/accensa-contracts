//! Treasury vesting tests (issue #467).
//!
//! Covers the release curve (nothing before the cliff, a linear middle, the
//! full allocation at the end), the claim path against a real SEP-41 token,
//! and the admin/validation guards.

extern crate std;

use crate::vesting::{VestingSchedule, FOUR_YEARS_SECS, ONE_YEAR_SECS};
use crate::{Error, Treasury, TreasuryClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

/// Arbitrary but fixed Unix timestamp the schedules start from.
const START: u64 = 1_700_000_000;
/// Tokens minted to the treasury up front, enough for every schedule below.
const SUPPLY: i128 = 100_000_000;

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    token: Address,
    treasury: Address,
    #[allow(dead_code)]
    admin: Address,
    alice: Address,
    bob: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);
    StellarAssetClient::new(&env, &token).mint(&treasury, &SUPPLY);

    Ctx {
        env,
        client,
        token,
        treasury,
        admin,
        alice,
        bob,
    }
}

fn set_time(ctx: &Ctx, timestamp: u64) {
    ctx.env.ledger().with_mut(|li| li.timestamp = timestamp);
}

fn balance(ctx: &Ctx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(who)
}

// ── Vesting math ─────────────────────────────────────────────────────────

#[test]
fn vesting_curve_is_flat_then_linear_then_flat() {
    let env = Env::default();
    let schedule = VestingSchedule {
        beneficiary: Address::generate(&env),
        total: 4_000_000,
        claimed: 0,
        start: 0,
        cliff: ONE_YEAR_SECS,
        duration: FOUR_YEARS_SECS,
    };

    // Nothing unlocks until the cliff has fully elapsed.
    assert_eq!(schedule.vested(0), Ok(0));
    assert_eq!(schedule.vested(ONE_YEAR_SECS - 1), Ok(0));
    // The cliff is the first unlock: a quarter of the allocation.
    assert_eq!(schedule.vested(ONE_YEAR_SECS), Ok(1_000_000));
    assert_eq!(schedule.vested(2 * ONE_YEAR_SECS), Ok(2_000_000));
    assert_eq!(schedule.vested(3 * ONE_YEAR_SECS), Ok(3_000_000));
    // Everything is unlocked at the end and stays unlocked.
    assert_eq!(schedule.vested(FOUR_YEARS_SECS), Ok(4_000_000));
    assert_eq!(schedule.vested(FOUR_YEARS_SECS * 10), Ok(4_000_000));
}

#[test]
fn a_zero_cliff_vests_from_the_first_second() {
    let env = Env::default();
    let schedule = VestingSchedule {
        beneficiary: Address::generate(&env),
        total: 1_000,
        claimed: 0,
        start: 0,
        cliff: 0,
        duration: 1_000,
    };

    assert_eq!(schedule.vested(0), Ok(0));
    assert_eq!(schedule.vested(250), Ok(250));
    assert_eq!(schedule.vested(1_000), Ok(1_000));
}

#[test]
fn vesting_rejects_arithmetic_overflow() {
    let env = Env::default();
    let schedule = VestingSchedule {
        beneficiary: Address::generate(&env),
        total: i128::MAX,
        claimed: 0,
        start: 0,
        cliff: 1,
        duration: FOUR_YEARS_SECS,
    };

    // `i128::MAX * (duration / 2)` cannot fit in an `i128`; the scaling step
    // must report that instead of wrapping.
    assert_eq!(
        schedule.vested(FOUR_YEARS_SECS / 2),
        Err(Error::MathOverflow)
    );
}

#[test]
fn schedule_validity_rejects_the_degenerate_shapes() {
    let env = Env::default();
    let beneficiary = Address::generate(&env);
    let base = VestingSchedule {
        beneficiary: beneficiary.clone(),
        total: 1_000,
        claimed: 0,
        start: 0,
        cliff: ONE_YEAR_SECS,
        duration: FOUR_YEARS_SECS,
    };
    assert!(base.is_valid());

    let zero_total = VestingSchedule {
        total: 0,
        ..base.clone()
    };
    assert!(!zero_total.is_valid());

    let zero_duration = VestingSchedule {
        duration: 0,
        cliff: 0,
        ..base.clone()
    };
    assert!(!zero_duration.is_valid());

    let cliff_past_end = VestingSchedule {
        cliff: FOUR_YEARS_SECS + 1,
        ..base
    };
    assert!(!cliff_past_end.is_valid());
}

// ── Claim path ───────────────────────────────────────────────────────────

#[test]
fn nothing_can_be_claimed_before_the_cliff() {
    let ctx = setup();
    ctx.client.add_team_schedule(&ctx.alice, &4_000_000, &START);

    set_time(&ctx, START + ONE_YEAR_SECS - 1);
    assert_eq!(ctx.client.vested_amount(&ctx.alice), 0);
    assert_eq!(ctx.client.claimable(&ctx.alice), 0);
    assert_eq!(
        ctx.client.try_claim_vested(&ctx.alice),
        Err(Ok(Error::NothingToClaim))
    );
    assert_eq!(balance(&ctx, &ctx.alice), 0);
}

#[test]
fn partial_claims_track_the_release_curve() {
    let ctx = setup();
    ctx.client.add_team_schedule(&ctx.alice, &4_000_000, &START);

    // Two years in: half unlocked, claim it.
    set_time(&ctx, START + 2 * ONE_YEAR_SECS);
    assert_eq!(ctx.client.vested_amount(&ctx.alice), 2_000_000);
    assert_eq!(ctx.client.claim_vested(&ctx.alice), 2_000_000);
    assert_eq!(balance(&ctx, &ctx.alice), 2_000_000);

    // A repeat claim in the same ledger has nothing new to pay.
    assert_eq!(
        ctx.client.try_claim_vested(&ctx.alice),
        Err(Ok(Error::NothingToClaim))
    );

    // Three years in: another year's worth unlocked.
    set_time(&ctx, START + 3 * ONE_YEAR_SECS);
    assert_eq!(ctx.client.claimable(&ctx.alice), 1_000_000);
    assert_eq!(ctx.client.claim_vested(&ctx.alice), 1_000_000);
    assert_eq!(balance(&ctx, &ctx.alice), 3_000_000);
}

#[test]
fn the_full_allocation_can_be_claimed_at_the_end() {
    let ctx = setup();
    ctx.client.add_team_schedule(&ctx.alice, &4_000_000, &START);

    // Half-way through, draw the first half.
    set_time(&ctx, START + 2 * ONE_YEAR_SECS);
    assert_eq!(ctx.client.claim_vested(&ctx.alice), 2_000_000);

    // At the end, everything remaining is available and nothing is left over.
    set_time(&ctx, START + FOUR_YEARS_SECS);
    assert_eq!(ctx.client.vested_amount(&ctx.alice), 4_000_000);
    assert_eq!(ctx.client.claim_vested(&ctx.alice), 2_000_000);
    assert_eq!(balance(&ctx, &ctx.alice), 4_000_000);
    assert_eq!(ctx.client.claimable(&ctx.alice), 0);
    assert_eq!(
        ctx.client.try_claim_vested(&ctx.alice),
        Err(Ok(Error::NothingToClaim))
    );
}

#[test]
fn one_schedule_never_pays_out_more_than_its_total() {
    let ctx = setup();

    // Two beneficiaries with different allocations, claimed interleaved: each
    // is capped by its own total, not the treasury's balance.
    ctx.client.add_team_schedule(&ctx.alice, &4_000_000, &START);
    ctx.client.add_team_schedule(&ctx.bob, &8_000_000, &START);

    set_time(&ctx, START + FOUR_YEARS_SECS);
    assert_eq!(ctx.client.claim_vested(&ctx.alice), 4_000_000);
    assert_eq!(ctx.client.claim_vested(&ctx.bob), 8_000_000);

    assert_eq!(balance(&ctx, &ctx.alice), 4_000_000);
    assert_eq!(balance(&ctx, &ctx.bob), 8_000_000);
    assert_eq!(balance(&ctx, &ctx.treasury), SUPPLY - 12_000_000);
}

#[test]
fn unknown_beneficiaries_have_no_schedule() {
    let ctx = setup();

    assert_eq!(ctx.client.get_schedule(&ctx.bob), None);
    assert_eq!(
        ctx.client.try_vested_amount(&ctx.bob),
        Err(Ok(Error::ScheduleNotFound))
    );
    assert_eq!(
        ctx.client.try_claim_vested(&ctx.bob),
        Err(Ok(Error::ScheduleNotFound))
    );
}

// ── Admin & validation ───────────────────────────────────────────────────

#[test]
fn only_the_admin_can_create_schedules() {
    let ctx = setup();

    ctx.env.set_auths(&[]);
    assert!(ctx
        .client
        .try_add_team_schedule(&ctx.alice, &1_000, &START)
        .is_err());
    ctx.env.mock_all_auths();

    ctx.client.add_team_schedule(&ctx.alice, &1_000, &START);
    assert!(ctx.client.get_schedule(&ctx.alice).is_some());
}

#[test]
fn invalid_schedules_are_rejected() {
    let ctx = setup();

    assert_eq!(
        ctx.client
            .try_add_schedule(&ctx.alice, &0, &START, &ONE_YEAR_SECS, &FOUR_YEARS_SECS),
        Err(Ok(Error::InvalidSchedule))
    );
    assert_eq!(
        ctx.client
            .try_add_schedule(&ctx.alice, &1_000, &START, &0, &0),
        Err(Ok(Error::InvalidSchedule))
    );
    assert_eq!(
        ctx.client
            .try_add_schedule(&ctx.alice, &1_000, &START, &FOUR_YEARS_SECS, &ONE_YEAR_SECS),
        Err(Ok(Error::InvalidSchedule))
    );
}

#[test]
fn a_beneficiary_cannot_have_two_schedules() {
    let ctx = setup();
    ctx.client.add_team_schedule(&ctx.alice, &1_000, &START);

    assert_eq!(
        ctx.client.try_add_team_schedule(&ctx.alice, &2_000, &START),
        Err(Ok(Error::ScheduleAlreadyExists))
    );
}

#[test]
fn the_team_schedule_uses_the_four_year_one_year_cliff_defaults() {
    let ctx = setup();
    ctx.client
        .add_team_schedule(&ctx.alice, &12_000_000, &START);

    let schedule = ctx.client.get_schedule(&ctx.alice).unwrap();
    assert_eq!(schedule.cliff, ONE_YEAR_SECS);
    assert_eq!(schedule.duration, FOUR_YEARS_SECS);
    assert_eq!(schedule.total, 12_000_000);
    assert_eq!(schedule.claimed, 0);
    assert_eq!(schedule.start, START);
    assert_eq!(schedule.beneficiary, ctx.alice);
}

#[test]
fn initialization_is_recorded_and_single_shot() {
    let ctx = setup();

    assert_eq!(ctx.client.get_admin(), Some(ctx.admin.clone()));
    assert_eq!(ctx.client.get_token(), Some(ctx.token.clone()));

    // A second `initialize` on the same instance is refused.
    assert_eq!(
        ctx.client.try_initialize(&ctx.admin, &ctx.token),
        Err(Ok(Error::AlreadyInitialized))
    );
}
