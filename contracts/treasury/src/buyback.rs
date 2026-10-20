//! Automated governance-token buyback & burn (issue #465).
//!
//! A share of protocol fees is used to market-buy the native governance token
//! on a DEX and burn it, creating deflationary pressure:
//!
//! 1. The admin configures the hook once with [`set_config`]: the fee token to
//!    spend, the governance token to acquire, the DEX router to trade on, the
//!    address the bought tokens are burned at, and a minimum swap size.
//! 2. Anyone (a keeper) may call [`execute`]. It refuses swaps below the
//!    configured minimum ([`Error::BelowBuybackThreshold`]), moves `amount_in`
//!    of the fee token to the router, calls
//!    [`DexRouter::swap`](DexRouter::swap) with a caller-supplied
//!    `min_amount_out` slippage floor, then sends whatever governance tokens
//!    come back to the burn address.
//!
//! # Slippage & sandwich protection
//!
//! The DEX is untrusted. `execute` passes the caller's `min_amount_out`
//! straight to the router *and* re-checks the returned amount against it, so a
//! router that under-fills reverts the whole invocation
//! ([`Error::BuybackSlippageExceeded`]) rather than letting the treasury overpay.
//! Because the burn address is fixed at configuration time it cannot be
//! redirected by the keeper either.

use crate::{require_admin, require_initialized, DataKey, Error};
use soroban_sdk::{contractclient, contractevent, contracttype, token, Address, Env};

/// Minimal DEX router interface the buyback trades against.
///
/// Implemented by any Soroban AMM/router. `swap` receives `amount_in` of
/// `token_in` already transferred by the treasury, trades it for `token_out`,
/// sends the proceeds to `recipient`, and returns the amount sent. The
/// treasury re-checks the return value against the slippage floor.
#[contractclient(name = "DexRouterClient")]
pub trait DexRouter {
    fn swap(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        recipient: Address,
    ) -> i128;
}

/// The admin-configured buyback hook.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuybackConfig {
    /// Token the treasury spends (protocol fees).
    pub fee_token: Address,
    /// Token the treasury acquires and burns.
    pub governance_token: Address,
    /// DEX router the swap is routed through.
    pub router: Address,
    /// Destination of the bought governance tokens.
    pub burn_address: Address,
    /// Smallest swap `execute` will perform, in fee-token units.
    pub min_amount_in: i128,
}

/// Emitted when the admin configures (or replaces) the buyback hook.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuybackConfiguredEvent {
    /// Token the treasury spends; written by `set_buyback_config` and published
    /// as an event topic.
    #[topic]
    pub fee_token: Address,
    /// Token the treasury buys back and burns; written by `set_buyback_config`
    /// and published as an event topic.
    #[topic]
    pub governance_token: Address,
    /// Router the swap is sent to; written by `set_buyback_config` and carried
    /// in the event data.
    pub router: Address,
    /// Destination the bought governance tokens are sent to; written by
    /// `set_buyback_config`.
    pub burn_address: Address,
    /// Smallest swap `execute_buyback` will perform, in fee-token units;
    /// written by `set_buyback_config`.
    pub min_amount_in: i128,
}

/// Emitted when a buyback swaps fees for governance tokens and burns them.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuybackExecutedEvent {
    #[topic]
    pub governance_token: Address,
    pub amount_in: i128,
    pub amount_out: i128,
    pub burn_address: Address,
}

fn key() -> DataKey {
    DataKey::BuybackConfig
}

/// The configured buyback hook, if any.
pub(crate) fn config(env: &Env) -> Option<BuybackConfig> {
    env.storage().instance().get(&key())
}

/// Admin-only: configure the buyback hook.
pub(crate) fn set_config(
    env: &Env,
    fee_token: Address,
    governance_token: Address,
    router: Address,
    burn_address: Address,
    min_amount_in: i128,
) -> Result<(), Error> {
    require_initialized(env)?;
    require_admin(env);

    if min_amount_in <= 0
        || fee_token == governance_token
        || burn_address == env.current_contract_address()
    {
        return Err(Error::InvalidBuybackConfig);
    }

    env.storage().instance().set(
        &key(),
        &BuybackConfig {
            fee_token: fee_token.clone(),
            governance_token: governance_token.clone(),
            router: router.clone(),
            burn_address: burn_address.clone(),
            min_amount_in,
        },
    );

    BuybackConfiguredEvent {
        fee_token,
        governance_token,
        router,
        burn_address,
        min_amount_in,
    }
    .publish(env);

    Ok(())
}

/// Swap `amount_in` fee tokens for the governance token and burn the proceeds.
/// Permissionless. Returns the amount of governance token burned.
pub(crate) fn execute(env: &Env, amount_in: i128, min_amount_out: i128) -> Result<i128, Error> {
    require_initialized(env)?;
    let config = config(env).ok_or(Error::BuybackNotConfigured)?;

    if amount_in <= 0 || amount_in < config.min_amount_in {
        return Err(Error::BelowBuybackThreshold);
    }
    if min_amount_out <= 0 {
        return Err(Error::BuybackSlippageExceeded);
    }

    let contract = env.current_contract_address();
    let fee_client = token::Client::new(env, &config.fee_token);
    if fee_client.balance(&contract) < amount_in {
        return Err(Error::InsufficientBuybackFloat);
    }

    // Observe the governance-token balance before the swap so we can measure
    // what the router actually delivered (the DEX is untrusted).
    let gov_client = token::Client::new(env, &config.governance_token);
    let before_out = gov_client.balance(&contract);

    // Push the fees to the router, then ask it to swap on our behalf.
    fee_client.transfer(&contract, &config.router, &amount_in);
    let amount_out = DexRouterClient::new(env, &config.router).swap(
        &config.fee_token,
        &config.governance_token,
        &amount_in,
        &min_amount_out,
        &contract,
    );
    let received = gov_client.balance(&contract) - before_out;

    // Re-check the untrusted router's result against the slippage floor and
    // its own claim: reject when the delivery is below the floor or below the
    // router's report.
    if received < min_amount_out || received < amount_out {
        return Err(Error::BuybackSlippageExceeded);
    }

    gov_client.transfer(&contract, &config.burn_address, &received);

    BuybackExecutedEvent {
        governance_token: config.governance_token,
        amount_in,
        amount_out: received,
        burn_address: config.burn_address,
    }
    .publish(env);

    Ok(received)
}

// ── Tests ──────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use crate::{Error, Treasury, TreasuryClient};
    use soroban_sdk::{
        testutils::Address as _,
        token::{StellarAssetClient, TokenClient},
        Address, Env,
    };

    const FEE_SUPPLY: i128 = 100_000;
    const GOV_SUPPLY: i128 = 1_000_000;

    // ── Mock DEX router: over-reporter ──────────────────────────────────────
    // Sends fewer governance tokens than it reports (reports more than it sends).
    mod mock_dex_over_report {
        use soroban_sdk::{contract, contractimpl, token, Address, Env};

        #[contract]
        pub struct MockDexOverReport;

        #[contractimpl]
        impl MockDexOverReport {
            pub fn swap(
                env: Env,
                _token_in: Address,
                token_out: Address,
                amount_in: i128,
                _min_amount_out: i128,
                recipient: Address,
            ) -> i128 {
                let out = amount_in / 2; // actually sends half
                let reported = amount_in * 2; // reports double (over-report)
                token::Client::new(&env, &token_out).transfer(
                    &env.current_contract_address(),
                    &recipient,
                    &out,
                );
                reported
            }
        }
    }
    use mock_dex_over_report::MockDexOverReport;

    // ── Mock DEX router: under-reporter ─────────────────────────────────────
    // Sends more governance tokens than it reports (reports less than it sends).
    mod mock_dex_under_report {
        use soroban_sdk::{contract, contractimpl, token, Address, Env};

        #[contract]
        pub struct MockDexUnderReport;

        #[contractimpl]
        impl MockDexUnderReport {
            pub fn swap(
                env: Env,
                _token_in: Address,
                token_out: Address,
                amount_in: i128,
                _min_amount_out: i128,
                recipient: Address,
            ) -> i128 {
                let out = amount_in * 2; // actually sends double
                let reported = amount_in / 2; // reports half (under-report)
                token::Client::new(&env, &token_out).transfer(
                    &env.current_contract_address(),
                    &recipient,
                    &out,
                );
                reported
            }
        }
    }
    use mock_dex_under_report::MockDexUnderReport;

    // ── Setup ──────────────────────────────────────────────────────────────
    struct Ctx {
        env: Env,
        client: TreasuryClient<'static>,
        treasury: Address,
        fee_token: Address,
        gov_token: Address,
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

        Ctx {
            env,
            client,
            treasury,
            fee_token,
            gov_token,
            burn_address,
        }
    }

    impl Ctx {
        fn configure_with_router(&self, router: Address, min_amount_in: i128) {
            self.client.set_buyback_config(
                &self.fee_token,
                &self.gov_token,
                &router,
                &self.burn_address,
                &min_amount_in,
            );
        }

        fn balance(&self, token: &Address, who: &Address) -> i128 {
            TokenClient::new(&self.env, token).balance(who)
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────────

    #[test]
    fn buyback_over_reporting_router_is_rejected() {
        let ctx = setup();
        let router = ctx.env.register(MockDexOverReport, ());
        StellarAssetClient::new(&ctx.env, &ctx.gov_token).mint(&router, &GOV_SUPPLY);
        ctx.configure_with_router(router.clone(), 100);

        // Router sends 500 but reports 2000. received(500) < amount_out(2000) → reject.
        assert_eq!(
            ctx.client.try_execute_buyback(&1_000, &100),
            Err(Ok(Error::BuybackSlippageExceeded))
        );
        // Nothing burned.
        assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 0);
        // Fees were not spent (reverted on error).
        assert_eq!(ctx.balance(&ctx.fee_token, &ctx.treasury), FEE_SUPPLY);
    }

    #[test]
    fn buyback_under_reporting_router_burns_delivered_amount() {
        let ctx = setup();
        let router = ctx.env.register(MockDexUnderReport, ());
        StellarAssetClient::new(&ctx.env, &ctx.gov_token).mint(&router, &GOV_SUPPLY);
        ctx.configure_with_router(router.clone(), 100);

        // Router sends 2000 but reports 500. received(2000) >= min_amount_out(100) → pass.
        // received(2000) >= amount_out(500) → pass.
        let burned = ctx.client.execute_buyback(&1_000, &100);
        assert_eq!(burned, 2_000);
        assert_eq!(ctx.balance(&ctx.gov_token, &ctx.burn_address), 2_000);
        // Treasury spent 1,000 fee tokens.
        assert_eq!(
            ctx.balance(&ctx.fee_token, &ctx.treasury),
            FEE_SUPPLY - 1_000
        );
    }
}
