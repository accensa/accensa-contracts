//! Insurance pool tests (issue #445).

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

const FUNDS: i128 = 10_000_000;
const START: u64 = 1_700_000_000;
const ONE_YEAR: u64 = SECONDS_PER_YEAR as u64;

struct Setup {
    env: Env,
    client: InsurancePoolClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    alice: Address,
    bob: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&alice, &FUNDS);
    StellarAssetClient::new(&env, &token).mint(&bob, &FUNDS);

    let pool = env.register(InsurancePool, (admin.clone(), token.clone()));
    let client = InsurancePoolClient::new(&env, &pool);

    Setup {
        token: TokenClient::new(&env, &token),
        env,
        client,
        admin,
        alice,
        bob,
    }
}

fn at(s: &Setup, timestamp: u64) {
    s.env.ledger().with_mut(|li| li.timestamp = timestamp);
}

// ── Interest math (pure) ─────────────────────────────────────────────────

#[test]
fn test_utilization_bounds() {
    assert_eq!(utilization_ratio(0, 0), 0);
    assert_eq!(utilization_ratio(0, 1_000), 0);
    assert_eq!(utilization_ratio(500, 1_000), 5_000);
    assert_eq!(utilization_ratio(1_000, 1_000), 10_000);
    assert_eq!(utilization_ratio(2_000, 1_000), 10_000);
    assert_eq!(utilization_ratio(1_000, 0), 10_000);
    assert_eq!(utilization_ratio(-1, 1_000), 0);
}

#[test]
fn test_borrow_rate_increases_with_utilization() {
    let r0 = borrow_rate(0);
    let r_mid = borrow_rate(5_000);
    let r_full = borrow_rate(10_000);
    assert!(r0 > 0);
    assert!(r_mid > r0);
    assert!(r_full > r_mid);
    // At 0% utilization the rate is exactly the base annual rate per second.
    let expected_base = BASE_RATE_BPS * PRECISION / (10_000 * SECONDS_PER_YEAR);
    assert_eq!(r0, expected_base);
    // Clamped: rates never decrease past the model endpoints.
    assert_eq!(borrow_rate(20_000), r_full);
    assert_eq!(borrow_rate(-5), r0);
}

#[test]
fn test_supply_rate_scales_with_utilization() {
    let br = borrow_rate(10_000);
    assert_eq!(supply_rate(br, 0), 0);
    let s = supply_rate(br, 10_000);
    assert!(s > 0);
    assert!(s < br);
}

#[test]
fn test_accrue_interest_math() {
    let idx = PRECISION;
    let rate = borrow_rate(0);
    // One second of base-rate accrual.
    let after_1s = accrue_interest(idx, rate, 1).unwrap();
    assert!(after_1s > idx);
    // Zero dt is a no-op.
    assert_eq!(accrue_interest(idx, rate, 0).unwrap(), idx);
    assert_eq!(accrue_interest(idx, rate, -5).unwrap(), idx);
    // A full year at the base rate is slightly above 1.0 + 2%.
    let after_year = accrue_interest(idx, rate, SECONDS_PER_YEAR).unwrap();
    assert!(after_year > idx + idx * 200 / 10_000 / 10);
}

// ── Contract: accrual over time ──────────────────────────────────────────

#[test]
fn test_deposit_accrues_interest() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &500);
    s.client.borrow(&s.bob, &400);

    let supply_before = s.client.get_supply_index();
    let borrow_before = s.client.get_borrow_index();
    assert_eq!(supply_before, PRECISION);
    assert_eq!(borrow_before, PRECISION);

    at(&s, START + ONE_YEAR);
    s.client.deposit(&s.alice, &100);

    assert!(s.client.get_supply_index() > supply_before);
    assert!(s.client.get_borrow_index() > borrow_before);
}

#[test]
fn test_interest_accumulates_over_time() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &1_000);
    s.client.borrow(&s.bob, &500);

    let mut prev_supply = s.client.get_supply_index();
    let mut prev_borrow = s.client.get_borrow_index();

    for i in 1..=5u64 {
        at(&s, START + i * (ONE_YEAR / 4));
        s.client.accrue();
        let supply = s.client.get_supply_index();
        let borrow = s.client.get_borrow_index();
        assert!(supply > prev_supply, "supply index must grow each period");
        assert!(borrow > prev_borrow, "borrow index must grow each period");
        prev_supply = supply;
        prev_borrow = borrow;
    }
}

#[test]
fn test_accrue_no_change_when_zero_time() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &500);
    s.client.borrow(&s.bob, &400);

    // Force an accrual so indexes are current, then accrue again at the
    // same timestamp.
    at(&s, START + 10_000);
    s.client.accrue();
    let supply = s.client.get_supply_index();
    let borrow = s.client.get_borrow_index();

    s.client.accrue();
    assert_eq!(s.client.get_supply_index(), supply);
    assert_eq!(s.client.get_borrow_index(), borrow);
}

// ── Contract: liquidity and collateral ───────────────────────────────────

#[test]
fn test_withdraw_rejects_insufficient_liquidity() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &1_000);
    // Bob draws his full collateral, leaving Alice's deposit as the float.
    s.client.borrow(&s.bob, &1_000);

    // Advance a year so borrow interest outgrows supply interest: the
    // pool's liquid float falls below Alice's settled balance.
    at(&s, START + ONE_YEAR);
    s.client.accrue();

    let alice = s.client.get_user_info(&s.alice);
    assert!(alice.supplied > 1_000);
    assert_eq!(
        s.client.try_withdraw(&s.alice, &alice.supplied),
        Err(Ok(Error::InsufficientLiquidity))
    );

    // Anything above her own balance is InvalidAmount, checked first.
    assert_eq!(
        s.client.try_withdraw(&s.alice, &(alice.supplied + 1)),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_borrow_rejects_exceeds_collateral() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &300);

    // Bob's collateral is 300; borrowing 400 would exceed it.
    assert_eq!(
        s.client.try_borrow(&s.bob, &400),
        Err(Ok(Error::ExceedsCollateral))
    );
    // Borrowing exactly the collateral is allowed.
    s.client.borrow(&s.bob, &300);
    assert_eq!(s.client.get_user_info(&s.bob).borrowed, 300);
}

#[test]
fn test_withdraw_rejects_borrower_withdrawing_collateral() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &500);
    s.client.borrow(&s.bob, &500);

    // Bob's debt equals his supplied collateral; withdrawing any of it
    // would leave the debt unbacked.
    assert_eq!(
        s.client.try_withdraw(&s.bob, &500),
        Err(Ok(Error::ExceedsCollateral))
    );
}

#[test]
fn test_repay_reduces_borrowed_amount() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &500);
    s.client.borrow(&s.bob, &300);

    assert_eq!(s.client.get_user_info(&s.bob).borrowed, 300);

    s.client.repay(&s.bob, &100);
    assert_eq!(s.client.get_user_info(&s.bob).borrowed, 200);

    s.client.repay(&s.bob, &500);
    assert_eq!(s.client.get_user_info(&s.bob).borrowed, 0);

    // Nothing left to repay.
    assert_eq!(
        s.client.try_repay(&s.bob, &1),
        Err(Ok(Error::InvalidAmount))
    );
}

// ── Contract: settlement and bookkeeping ──────────────────────────────────

#[test]
fn test_deposit_settles_accrued_interest_into_user_balance() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &1_000);
    s.client.borrow(&s.bob, &500);

    at(&s, START + ONE_YEAR);
    s.client.deposit(&s.alice, &100);

    let alice = s.client.get_user_info(&s.alice);
    // Principal 1100 plus whatever supply interest accrued over the year.
    assert!(alice.supplied > 1_100);
    assert_eq!(alice.borrowed, 0);

    let bob = s.client.get_user_info(&s.bob);
    // Principal 1000 borrowed 500; debt grew with the borrow index.
    assert!(bob.borrowed > 500);
    assert!(bob.borrowed < 1_000);
}

#[test]
fn test_constructor_binds_admin_and_token() {
    let s = setup();
    assert_eq!(s.client.get_admin(), s.admin);
    assert_eq!(s.client.get_token(), s.token.address);
    assert_eq!(s.client.get_utilization(), 0);
    assert_eq!(s.client.get_supply_index(), PRECISION);
    assert_eq!(s.client.get_borrow_index(), PRECISION);
}

#[test]
fn test_deposit_requires_positive_amount() {
    let s = setup();
    assert_eq!(
        s.client.try_deposit(&s.alice, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_deposit(&s.alice, &-1),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_utilization_reflects_borrows() {
    let s = setup();
    s.client.deposit(&s.alice, &1_000);
    s.client.deposit(&s.bob, &1_000);
    assert_eq!(s.client.get_utilization(), 0);

    s.client.borrow(&s.bob, &500);
    assert_eq!(s.client.get_utilization(), 2_500);

    s.client.borrow(&s.bob, &500);
    assert_eq!(s.client.get_utilization(), 5_000);
}

#[test]
fn test_token_balances_move_with_pool() {
    let s = setup();
    let pool = s.client.address.clone();

    s.client.deposit(&s.alice, &1_000);
    assert_eq!(s.token.balance(&pool), 1_000);
    assert_eq!(s.token.balance(&s.alice), FUNDS - 1_000);

    s.client.deposit(&s.bob, &400);
    s.client.borrow(&s.bob, &300);
    assert_eq!(s.token.balance(&pool), 1_100);

    s.client.repay(&s.bob, &300);
    assert_eq!(s.token.balance(&pool), 1_400);

    s.client.withdraw(&s.alice, &1_000);
    assert_eq!(s.token.balance(&pool), 400);
    assert_eq!(s.token.balance(&s.alice), FUNDS);
}
