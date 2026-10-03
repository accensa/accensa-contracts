//! Automated fee-liquidation tests (issue #444).
//!
//! Covers the happy path (fees swapped into the primary stablecoin), the two
//! ways a swap can short-change the treasury (the AMM delivering below the
//! oracle floor, and an AMM that reports more than it sent), fail-closed
//! behaviour before configuration, the admin-only gate, and malformed
//! requests.

#![cfg(test)]

use crate::liquidation::{BPS_DENOMINATOR, PRICE_SCALE};
use crate::{Error, Treasury, TreasuryClient};
use accensa_common::Error as CommonError;
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

// ── Mock AMM ──────────────────────────────────────────────────────────────

/// A minimal AMM: it holds a reserve of the output token and pays
/// `amount_in * rate_bps / 10_000` for whatever input the treasury forwarded
/// to it. `report_override`, when non-zero, is the amount it *claims* to have
/// sent — letting a test model an AMM that lies about its output.
#[contract]
pub struct MockAmm;

#[contractimpl]
impl MockAmm {
    pub fn __constructor(env: Env, rate_bps: u32, report_override: i128) {
        env.storage()
            .instance()
            .set(&symbol_short!("rate"), &rate_bps);
        env.storage()
            .instance()
            .set(&symbol_short!("reported"), &report_override);
    }

    pub fn swap(
        env: Env,
        _token_in: Address,
        token_out: Address,
        amount_in: i128,
        _min_out: i128,
        to: Address,
    ) -> Result<i128, CommonError> {
        let rate: u32 = env
            .storage()
            .instance()
            .get(&symbol_short!("rate"))
            .unwrap_or(10_000);
        let out = amount_in * rate as i128 / BPS_DENOMINATOR as i128;
        if out > 0 {
            TokenClient::new(&env, &token_out).transfer(&env.current_contract_address(), &to, &out);
        }
        let claimed: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("reported"))
            .unwrap_or(0);
        Ok(if claimed > 0 { claimed } else { out })
    }
}

// ── Mock price feed ───────────────────────────────────────────────────────

/// A fixed-price oracle: one `base` unit is worth `price / PRICE_SCALE`
/// `quote` units.
#[contract]
pub struct MockPriceFeed;

#[contractimpl]
impl MockPriceFeed {
    pub fn __constructor(env: Env, price: i128) {
        env.storage()
            .instance()
            .set(&symbol_short!("price"), &price);
    }

    pub fn price(env: Env, _base: Address, _quote: Address) -> Result<i128, CommonError> {
        Ok(env
            .storage()
            .instance()
            .get(&symbol_short!("price"))
            .unwrap_or(0))
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────

const FEE_AMOUNT: i128 = 1_000_000;

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    admin: Address,
    treasury: Address,
    fee: Address,
    stable: Address,
    amm: Address,
    feed: Address,
}

/// Treasury vesting a separate token, holding `FEE_AMOUNT` of a fee token,
/// with liquidation configured against a mock AMM and price feed.
fn setup(rate_bps: u32, price: i128, report_override: i128) -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vesting = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), vesting));
    let client = TreasuryClient::new(&env, &treasury);

    let fee = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let stable = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    StellarAssetClient::new(&env, &fee).mint(&treasury, &FEE_AMOUNT);

    let amm = env.register(MockAmm, (rate_bps, report_override));
    let feed = env.register(MockPriceFeed, (price,));
    // The mock AMM needs a reserve of the stablecoin to pay out.
    StellarAssetClient::new(&env, &stable).mint(&amm, &1_000_000_000);

    client.whitelist_amm(&amm);
    client.set_stable_token(&stable);
    client.set_price_feed(&feed);

    Ctx {
        env,
        client,
        admin,
        treasury,
        fee,
        stable,
        amm,
        feed,
    }
}

fn balance(ctx: &Ctx, token: &Address, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, token).balance(who)
}

// ── Happy path ────────────────────────────────────────────────────────────

#[test]
fn liquidating_fees_swaps_into_the_stablecoin() {
    let ctx = setup(10_000, PRICE_SCALE, 0);

    let received = ctx.client.liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100);

    assert_eq!(received, FEE_AMOUNT);
    assert_eq!(balance(&ctx, &ctx.stable, &ctx.treasury), FEE_AMOUNT);
    assert_eq!(balance(&ctx, &ctx.fee, &ctx.treasury), 0);
    assert_eq!(balance(&ctx, &ctx.fee, &ctx.amm), FEE_AMOUNT);
}

#[test]
fn a_better_oracle_rate_still_clears_a_generous_slippage_bound() {
    // Oracle says 1:1, AMM pays 1% better: the floor is comfortably met.
    let ctx = setup(10_100, PRICE_SCALE, 0);
    let received = ctx.client.liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100);
    assert_eq!(received, 1_010_000);
}

// ── Slippage protection ───────────────────────────────────────────────────

#[test]
fn a_swap_below_the_oracle_floor_is_rejected() {
    // Oracle 1:1, 100 bps tolerance -> floor 990_000, but the AMM only
    // delivers 500_000.
    let ctx = setup(5_000, PRICE_SCALE, 0);
    assert_eq!(
        ctx.client.try_liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100),
        Err(Ok(Error::SlippageExceeded))
    );
}

#[test]
fn an_amm_that_over_reports_its_output_is_rejected() {
    // It delivers the full amount (so the floor is met) but claims far more
    // than it sent.
    let ctx = setup(10_000, PRICE_SCALE, 1_000_000_000_000);
    assert_eq!(
        ctx.client.try_liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100),
        Err(Ok(Error::SlippageExceeded))
    );
}

#[test]
fn a_non_positive_oracle_price_is_rejected() {
    let ctx = setup(10_000, 0, 0);
    assert_eq!(
        ctx.client.try_liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100),
        Err(Ok(Error::InvalidPrice))
    );
}

// ── Fail closed before configuration ──────────────────────────────────────

#[test]
fn liquidation_before_configuration_fails_closed() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let vesting = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), vesting));
    let client = TreasuryClient::new(&env, &treasury);
    let fee = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    assert_eq!(client.get_amm(), None);
    assert_eq!(client.get_stable_token(), None);
    assert_eq!(client.get_price_feed(), None);
    assert_eq!(
        client.try_liquidate_fees(&fee, &1_000, &100),
        Err(Ok(Error::LiquidationNotConfigured))
    );
}

#[test]
fn revoking_the_amm_disables_liquidation() {
    let ctx = setup(10_000, PRICE_SCALE, 0);
    ctx.client.revoke_amm();
    assert_eq!(ctx.client.get_amm(), None);
    assert_eq!(
        ctx.client.try_liquidate_fees(&ctx.fee, &FEE_AMOUNT, &100),
        Err(Ok(Error::LiquidationNotConfigured))
    );
    assert_eq!(
        ctx.client.try_revoke_amm(),
        Err(Ok(Error::LiquidationNotConfigured))
    );
}

#[test]
fn a_partially_configured_treasury_still_fails_closed() {
    // AMM whitelisted but no stablecoin/feed yet.
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let vesting = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), vesting));
    let client = TreasuryClient::new(&env, &treasury);
    let fee = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let amm = env.register(MockAmm, (10_000u32, 0i128));
    client.whitelist_amm(&amm);

    assert_eq!(
        client.try_liquidate_fees(&fee, &1_000, &100),
        Err(Ok(Error::LiquidationNotConfigured))
    );
}

// ── Malformed requests ────────────────────────────────────────────────────

#[test]
fn malformed_liquidations_are_rejected() {
    let ctx = setup(10_000, PRICE_SCALE, 0);

    // Non-positive amount.
    assert_eq!(
        ctx.client.try_liquidate_fees(&ctx.fee, &0, &100),
        Err(Ok(Error::InvalidLiquidation))
    );
    // 100% slippage tolerance gives no protection at all.
    assert_eq!(
        ctx.client
            .try_liquidate_fees(&ctx.fee, &FEE_AMOUNT, &BPS_DENOMINATOR),
        Err(Ok(Error::InvalidLiquidation))
    );
    // Liquidating the stablecoin into itself is a no-op.
    assert_eq!(
        ctx.client
            .try_liquidate_fees(&ctx.stable, &FEE_AMOUNT, &100),
        Err(Ok(Error::InvalidLiquidation))
    );
}

#[test]
fn liquidating_more_than_the_treasury_holds_is_rejected() {
    let ctx = setup(10_000, PRICE_SCALE, 0);
    assert_eq!(
        ctx.client
            .try_liquidate_fees(&ctx.fee, &(FEE_AMOUNT + 1), &100),
        Err(Ok(Error::InsufficientBalance))
    );
}

// ── Authorization ─────────────────────────────────────────────────────────

#[test]
fn only_the_admin_can_configure_or_liquidate() {
    let ctx = setup(10_000, PRICE_SCALE, 0);
    ctx.env.set_auths(&[]);

    assert!(ctx
        .client
        .try_liquidate_fees(&ctx.fee, &1_000, &100)
        .is_err());
    assert!(ctx.client.try_whitelist_amm(&ctx.amm).is_err());
    assert!(ctx.client.try_revoke_amm().is_err());
    assert!(ctx.client.try_set_stable_token(&ctx.stable).is_err());
    assert!(ctx.client.try_set_price_feed(&ctx.feed).is_err());
    let _ = ctx.admin;
}
