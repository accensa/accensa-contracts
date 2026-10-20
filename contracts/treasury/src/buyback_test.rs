//! Treasury buyback & burn tests (issue #465).

extern crate std;

use crate::buyback::{BuybackConfig, BuybackConfiguredEvent, BuybackExecutedEvent};
use crate::{Error, Treasury, TreasuryClient};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events as _},
    token::{StellarAssetClient, TokenClient},
    Address, Env, Event,
};

const FEE_SUPPLY: i128 = 100_000;
const GOV_SUPPLY: i128 = 1_000_000;

// ── Mock DEX router ────────────────────────────────────────────────────────

/// A minimal router: it swaps fee tokens already sent to it for governance
/// tokens at an admin-set rate (`rate_bps`, where 10_000 = 1:1).
#[contract]
pub struct MockDex;

#[contractimpl]
impl MockDex {
    pub fn set_rate(env: Env, rate_bps: i128) {
        env.storage().instance().set(&0u32, &rate_bps);
    }

    pub fn swap(
        env: Env,
        _token_in: Address,
        token_out: Address,
        amount_in: i128,
        _min_amount_out: i128,
        recipient: Address,
    ) -> i128 {
        let rate: i128 = env.storage().instance().get(&0u32).unwrap_or(10_000);
        let out = amount_in * rate / 10_000;
        soroban_sdk::token::Client::new(&env, &token_out).transfer(
            &env.current_contract_address(),
            &recipient,
            &out,
        );
        out
    }
}

// ── Setup ──────────────────────────────────────────────────────────────────

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    treasury: Address,
    fee_token: Address,
    gov_token: Address,
    router: Address,
    burn_address: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let burn_address = Address::generate(&env);

    let fee_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let gov_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let treasury = env.register(Treasury, (admin.clone(), fee_token.clone()));
    let client = TreasuryClient::new(&env, &treasury);
    StellarAssetClient::new(&env, &fee_token).mint(&treasury, &FEE_SUPPLY);

    let router = env.register(MockDex, ());
    StellarAssetClient::new(&env, &gov_token).mint(&router, &GOV_SUPPLY);

    Ctx {
        env,
        client,
        treasury,
        fee_token,
        gov_token,
        router,
        burn_address,
    }
}

impl Ctx {
    fn configure(&self, min_amount_in: i128) {
        self.client.set_buyback_config(
            &self.fee_token,
            &self.gov_token,
            &self.router,
            &self.burn_address,
            &min_amount_in,
        );
    }

    fn balance(&self, token: &Address, who: &Address) -> i128 {
        TokenClient::new(&self.env, token).balance(who)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn buyback_swaps_fees_and_burns_governance_tokens() {
    let ctx = setup();
    ctx.configure(100);
    // 2x rate: 1,000 fee tokens -> 2,000 governance tokens.
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&20_000);

    let burned = ctx.client.execute_buyback(&1_000, &2_000);

    assert_eq!(burned, 2_000);
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 2_000);
    assert_eq!(
        ctx.balance(&ctx.fee_token, &ctx.treasury),
        FEE_SUPPLY - 1_000
    );
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.router), GOV_SUPPLY - 2_000);
    // Config is stored and readable.
    assert_eq!(
        ctx.client.get_buyback_config(),
        Some(BuybackConfig {
            fee_token: ctx.fee_token.clone(),
            governance_token: ctx.gov_token.clone(),
            router: ctx.router.clone(),
            burn_address: ctx.burn_address.clone(),
            min_amount_in: 100,
        })
    );
}

#[test]
fn buyback_below_threshold_is_rejected() {
    let ctx = setup();
    ctx.configure(500);
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&20_000);

    assert_eq!(
        ctx.client.try_execute_buyback(&499, &1),
        Err(Ok(Error::BelowBuybackThreshold))
    );
    // Nothing moved.
    assert_eq!(ctx.balance(&ctx.fee_token, &ctx.treasury), FEE_SUPPLY);
}

#[test]
fn buyback_reverts_when_slippage_floor_is_not_met() {
    let ctx = setup();
    ctx.configure(100);
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&10_000);

    // 1,000 in -> 1,000 out, below the 1,500 floor.
    assert_eq!(
        ctx.client.try_execute_buyback(&1_000, &1_500),
        Err(Ok(Error::BuybackSlippageExceeded))
    );
    // The whole invocation reverted: no fees spent, nothing burned.
    assert_eq!(ctx.balance(&ctx.fee_token, &ctx.treasury), FEE_SUPPLY);
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 0);
}

#[test]
fn buyback_requires_configuration() {
    let ctx = setup();
    assert_eq!(
        ctx.client.try_execute_buyback(&1_000, &1),
        Err(Ok(Error::BuybackNotConfigured))
    );
}

#[test]
fn invalid_buyback_configs_are_rejected() {
    let ctx = setup();

    // Zero / negative minimum swap size.
    assert_eq!(
        ctx.client.try_set_buyback_config(
            &ctx.fee_token,
            &ctx.gov_token,
            &ctx.router,
            &ctx.burn_address,
            &0,
        ),
        Err(Ok(Error::InvalidBuybackConfig))
    );

    // Fee and governance tokens must differ.
    assert_eq!(
        ctx.client.try_set_buyback_config(
            &ctx.fee_token,
            &ctx.fee_token,
            &ctx.router,
            &ctx.burn_address,
            &100,
        ),
        Err(Ok(Error::InvalidBuybackConfig))
    );

    // The burn address must not be the treasury itself.
    assert_eq!(
        ctx.client.try_set_buyback_config(
            &ctx.fee_token,
            &ctx.gov_token,
            &ctx.router,
            &ctx.treasury,
            &100,
        ),
        Err(Ok(Error::InvalidBuybackConfig))
    );
}

// ── Event assertions (issue #606) ──────────────────────────────────────────

#[test]
fn buyback_configured_event_assert_exact_xdr_shape() {
    let ctx = setup();
    let min_amount_in = 250i128;

    ctx.client.set_buyback_config(
        &ctx.fee_token,
        &ctx.gov_token,
        &ctx.router,
        &ctx.burn_address,
        &min_amount_in,
    );

    // `events().all()` covers only the last invocation: the configuration itself.
    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);

    let expected = BuybackConfiguredEvent {
        fee_token: ctx.fee_token.clone(),
        governance_token: ctx.gov_token.clone(),
        router: ctx.router.clone(),
        burn_address: ctx.burn_address.clone(),
        min_amount_in,
    };
    assert_eq!(
        &events.events()[0],
        &expected.to_xdr(&ctx.env, &ctx.treasury)
    );
}

#[test]
fn buyback_executed_event_assert_exact_xdr_shape_and_slippage_guard() {
    let ctx = setup();
    ctx.configure(100);

    // 2x rate: 1,000 fee tokens -> 2,000 governance tokens.
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&20_000);

    let amount_in = 1_000i128;
    let min_amount_out = 1_500i128; // Slippage floor: 2,000 >= 1,500 clears the guard
    let burned = ctx.client.execute_buyback(&amount_in, &min_amount_out);
    assert_eq!(burned, 2_000);

    // `events().all()` covers only the last invocation: the execute_buyback itself.
    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);

    let expected = BuybackExecutedEvent {
        governance_token: ctx.gov_token.clone(),
        amount_in,
        amount_out: burned,
        burn_address: ctx.burn_address.clone(),
    };
    assert_eq!(
        &events.events()[0],
        &expected.to_xdr(&ctx.env, &ctx.treasury)
    );

    // When the DEX delivers exactly the slippage floor, the guard passes and
    // the emitted event reports that exact boundary amount.
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&15_000); // 1.5x rate: 1,000 -> 1,500
    let burned_floor = ctx.client.execute_buyback(&amount_in, &min_amount_out);
    assert_eq!(burned_floor, min_amount_out);

    // `events().all()` covers only the last invocation.
    let events_floor = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events_floor.events().len(), 1);

    let expected_floor = BuybackExecutedEvent {
        governance_token: ctx.gov_token.clone(),
        amount_in,
        amount_out: min_amount_out,
        burn_address: ctx.burn_address.clone(),
    };
    assert_eq!(
        &events_floor.events()[0],
        &expected_floor.to_xdr(&ctx.env, &ctx.treasury)
    );
}

#[test]
fn buyback_reconfiguration_updates_stored_amm_and_emits_event() {
    let ctx = setup();
    ctx.configure(100);

    // Verify initial configuration is stored with the initial router AMM.
    let initial_config = ctx.client.get_buyback_config().expect("initial config");
    assert_eq!(initial_config.router, ctx.router);

    // Register a second AMM router reusing the existing MockDex contract.
    let new_router = ctx.env.register(MockDex, ());
    StellarAssetClient::new(&ctx.env, &ctx.gov_token).mint(&new_router, &GOV_SUPPLY);
    assert_ne!(new_router, ctx.router);

    // Set rate on the new router (3x: 1,000 fee tokens -> 3,000 governance tokens).
    MockDexClient::new(&ctx.env, &new_router).set_rate(&30_000);

    // Reconfigure buyback with the new AMM router address.
    let new_min_in = 200i128;
    ctx.client.set_buyback_config(
        &ctx.fee_token,
        &ctx.gov_token,
        &new_router,
        &ctx.burn_address,
        &new_min_in,
    );

    // `events().all()` covers only the last invocation: the reconfiguration itself.
    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);

    let expected_config_event = BuybackConfiguredEvent {
        fee_token: ctx.fee_token.clone(),
        governance_token: ctx.gov_token.clone(),
        router: new_router.clone(),
        burn_address: ctx.burn_address.clone(),
        min_amount_in: new_min_in,
    };
    assert_eq!(
        &events.events()[0],
        &expected_config_event.to_xdr(&ctx.env, &ctx.treasury)
    );

    // Confirm stored value follows the new config rather than the stale entry.
    let updated_config = ctx.client.get_buyback_config().expect("updated config");
    assert_eq!(updated_config.router, new_router);
    assert_ne!(updated_config.router, ctx.router);
    assert_eq!(updated_config.min_amount_in, new_min_in);

    // Execute buyback: swap occurs through the new AMM router, not the stale one.
    let burned = ctx.client.execute_buyback(&1_000, &3_000);
    assert_eq!(burned, 3_000);

    // `events().all()` covers only the last invocation: the execution itself.
    // Must capture events before calling ctx.balance(), which runs another invocation.
    let exec_events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(exec_events.events().len(), 1);

    let expected_exec_event = BuybackExecutedEvent {
        governance_token: ctx.gov_token.clone(),
        amount_in: 1_000,
        amount_out: 3_000,
        burn_address: ctx.burn_address.clone(),
    };
    assert_eq!(
        &exec_events.events()[0],
        &expected_exec_event.to_xdr(&ctx.env, &ctx.treasury)
    );

    // Tokens were drawn from new_router while the stale ctx.router remains untouched.
    assert_eq!(ctx.balance(&ctx.gov_token, &new_router), GOV_SUPPLY - 3_000);
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.router), GOV_SUPPLY);
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 3_000);
}

#[test]
fn rejected_buyback_below_min_amount_out_emits_no_executed_event() {
    let ctx = setup();
    ctx.configure(100);

    // Router rate is 1:1 (10_000 bps) -> 1,000 fee tokens yields 1,000 gov tokens.
    MockDexClient::new(&ctx.env, &ctx.router).set_rate(&10_000);

    // Caller requires min_amount_out of 1_500, but router only yields 1,000.
    assert_eq!(
        ctx.client.try_execute_buyback(&1_000, &1_500),
        Err(Ok(Error::BuybackSlippageExceeded))
    );

    // `events().all()` covers only the last invocation.
    // Pin the 'no swap, no event' contract: rejection emits no BuybackExecutedEvent.
    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 0);

    // Rejection below min_amount_in also emits no event.
    assert_eq!(
        ctx.client.try_execute_buyback(&50, &1),
        Err(Ok(Error::BelowBuybackThreshold))
    );
    let events_below = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events_below.events().len(), 0);

    // Treasury float and burn address remain unchanged.
    assert_eq!(ctx.balance(&ctx.fee_token, &ctx.treasury), FEE_SUPPLY);
    assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 0);
}
