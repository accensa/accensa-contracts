//! Treasury buyback & burn tests (issue #465).

extern crate std;

use crate::buyback::BuybackConfig;
use crate::{Error, Treasury, TreasuryClient};
use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
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
        Err(Ok(Error::SlippageExceeded))
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
