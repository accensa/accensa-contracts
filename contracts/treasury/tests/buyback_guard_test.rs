extern crate std;

use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};
use treasury::{Error, Treasury, TreasuryClient};

const FEE_SUPPLY: i128 = 100_000;
const GOV_SUPPLY: i128 = 1_000_000;

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

#[test]
fn buyback_below_threshold_is_rejected_and_leaves_balances_untouched() {
    let ctx = setup();
    let min_amount_in = 1_000i128;
    ctx.client.set_buyback_config(
        &ctx.fee_token,
        &ctx.gov_token,
        &ctx.router,
        &ctx.burn_address,
        &min_amount_in,
    );

    let fee_client = TokenClient::new(&ctx.env, &ctx.fee_token);
    let gov_client = TokenClient::new(&ctx.env, &ctx.gov_token);

    let treasury_fee_before = fee_client.balance(&ctx.treasury);
    let router_fee_before = fee_client.balance(&ctx.router);
    let burn_gov_before = gov_client.balance(&ctx.burn_address);

    // Attempt buyback below min_amount_in
    let below_min = 500i128;
    let res = ctx.client.try_execute_buyback(&below_min, &400);
    assert_eq!(res, Err(Ok(Error::BelowBuybackThreshold)));

    // Verify token balances remain completely untouched
    assert_eq!(fee_client.balance(&ctx.treasury), treasury_fee_before);
    assert_eq!(fee_client.balance(&ctx.router), router_fee_before);
    assert_eq!(gov_client.balance(&ctx.burn_address), burn_gov_before);
}

#[test]
fn buyback_violating_slippage_is_rejected_and_leaves_balances_untouched() {
    let ctx = setup();
    let min_amount_in = 1_000i128;
    ctx.client.set_buyback_config(
        &ctx.fee_token,
        &ctx.gov_token,
        &ctx.router,
        &ctx.burn_address,
        &min_amount_in,
    );

    let fee_client = TokenClient::new(&ctx.env, &ctx.fee_token);
    let gov_client = TokenClient::new(&ctx.env, &ctx.gov_token);

    let treasury_fee_before = fee_client.balance(&ctx.treasury);
    let router_fee_before = fee_client.balance(&ctx.router);
    let burn_gov_before = gov_client.balance(&ctx.burn_address);

    // Swap 2000 fee tokens. At 1:1 rate, router outputs 2000 gov tokens.
    // Specifying min_amount_out = 3000 violates slippage floor.
    let amount_in = 2_000i128;
    let impossible_min_out = 3_000i128;
    let res = ctx
        .client
        .try_execute_buyback(&amount_in, &impossible_min_out);
    assert_eq!(res, Err(Ok(Error::BuybackSlippageExceeded)));

    // Balances must remain completely unmodified
    assert_eq!(fee_client.balance(&ctx.treasury), treasury_fee_before);
    assert_eq!(fee_client.balance(&ctx.router), router_fee_before);
    assert_eq!(gov_client.balance(&ctx.burn_address), burn_gov_before);

    // Non-positive min_amount_out also rejected
    let res_zero = ctx.client.try_execute_buyback(&amount_in, &0);
    assert_eq!(res_zero, Err(Ok(Error::BuybackSlippageExceeded)));

    assert_eq!(fee_client.balance(&ctx.treasury), treasury_fee_before);
    assert_eq!(fee_client.balance(&ctx.router), router_fee_before);
    assert_eq!(gov_client.balance(&ctx.burn_address), burn_gov_before);
}
