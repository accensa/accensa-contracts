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
//! ([`Error::SlippageExceeded`]) rather than letting the treasury overpay.
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
    #[topic]
    pub fee_token: Address,
    #[topic]
    pub governance_token: Address,
    pub router: Address,
    pub burn_address: Address,
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
        return Err(Error::SlippageExceeded);
    }

    let contract = env.current_contract_address();
    let fee_client = token::Client::new(env, &config.fee_token);
    if fee_client.balance(&contract) < amount_in {
        return Err(Error::InsufficientBuybackFloat);
    }

    // Push the fees to the router, then ask it to swap on our behalf.
    fee_client.transfer(&contract, &config.router, &amount_in);
    let amount_out = DexRouterClient::new(env, &config.router).swap(
        &config.fee_token,
        &config.governance_token,
        &amount_in,
        &min_amount_out,
        &contract,
    );

    // Re-check the untrusted router's result against the slippage floor.
    if amount_out < min_amount_out {
        return Err(Error::SlippageExceeded);
    }

    token::Client::new(env, &config.governance_token).transfer(
        &contract,
        &config.burn_address,
        &amount_out,
    );

    BuybackExecutedEvent {
        governance_token: config.governance_token,
        amount_in,
        amount_out,
        burn_address: config.burn_address,
    }
    .publish(env);

    Ok(amount_out)
}
