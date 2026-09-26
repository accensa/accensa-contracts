//! Automated fee liquidation through a whitelisted AMM (issue #444).
//!
//! Protocol fees and strategy yield accumulate in whatever token they were
//! earned in. Treasury accounting, however, is denominated in a single primary
//! stablecoin, so idle fee tokens have to be swapped. Doing that by hand is an
//! operational burden and a timing risk; `liquidate_fees` lets governance turn
//! a whitelisted token balance into the primary stablecoin in one call.
//!
//! # Model
//!
//! - **Whitelist first.** Only an admin-approved [`Amm`] may be swapped
//!   through, and the destination stablecoin and the price feed are set by the
//!   admin too. An unconfigured treasury cannot liquidate at all
//!   ([`Error::LiquidationNotConfigured`]).
//! - **Slippage floor from the oracle.** `liquidate_fees` reads the price feed
//!   for `token_in/stable`, derives the expected output, and reduces it by the
//!   caller's tolerance. The AMM must deliver at least that much. The oracle
//!   value is a *floor*, never a settlement price: the AMM's actual output is
//!   what gets booked, and it is cross-checked against the treasury's own
//!   token-balance delta, so a lying AMM cannot be credited for tokens it
//!   never sent (the same untrusted-external-call posture as
//!   [`crate::strategies`]).
//! - **Transferred before the swap.** The treasury moves `amount_in` to the
//!   AMM and only then calls `swap`, mirroring the strategy `deposit` flow.
//! - **One call, one lock.** Every liquidation runs under the treasury's
//!   reentrancy lock, so a hostile AMM cannot re-enter the portfolio or a
//!   second liquidation mid-swap.
//!
//! Prices are integers scaled by [`PRICE_SCALE`]: a price of `p` means one
//! unit of `base` is worth `p / PRICE_SCALE` units of `quote`.

use accensa_common::Error as CommonError;
use soroban_sdk::{contractclient, contractevent, token, Address, Env};

use crate::DataKey;
use crate::Error;

/// Price scale. A feed price is "quote units per base unit" multiplied by this,
/// so `PRICE_SCALE` itself means a 1:1 rate.
pub const PRICE_SCALE: i128 = 10_000_000;

/// Basis-point denominator: `10_000` bps = 100%.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Interface for a whitelisted Soroban AMM (issue #444).
///
/// The treasury transfers `amount_in` of `token_in` to the AMM *before* calling
/// `swap`; `swap` is the AMM's cue to send `token_out` to `to` (the treasury)
/// and report how much it sent. `min_out` is the treasury's oracle-derived
/// slippage floor — an AMM that cannot meet it should return an error rather
/// than short-change the caller, and either way the treasury re-checks the
/// amount it actually received.
///
/// As with [`crate::strategies::Strategy`], the trait exists so a typed
/// `AmmClient` is generated from a single definition; errors use the shared
/// [`CommonError`] code space.
#[contractclient(name = "AmmClient")]
pub trait Amm {
    /// Swap `amount_in` of `token_in` for `token_out`, sending the output to
    /// `to`. Returns the amount of `token_out` sent.
    fn swap(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_out: i128,
        to: Address,
    ) -> Result<i128, CommonError>;
}

/// Interface for the price feed the slippage floor is derived from
/// (issue #444).
///
/// `price(base, quote)` returns the value of one `base` unit in `quote` units,
/// scaled by [`PRICE_SCALE`]. A feed is trusted only as a *bound*: its output
/// gates slippage, while the AMM's actual delivery is what the treasury books.
#[contractclient(name = "PriceFeedClient")]
pub trait PriceFeed {
    /// Price of one `base` unit in `quote` units, scaled by [`PRICE_SCALE`].
    fn price(env: Env, base: Address, quote: Address) -> Result<i128, CommonError>;
}

/// Emitted when accumulated fee tokens are swapped into the primary
/// stablecoin.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeesLiquidatedEvent {
    #[topic]
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: i128,
    /// Amount actually received, after the swap settled.
    pub amount_out: i128,
    /// The oracle-derived floor the swap had to clear.
    pub min_out: i128,
}

// ── Configuration ─────────────────────────────────────────────────────────

/// The primary stablecoin liquidated fees are swapped into, if configured.
pub fn stable_token(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::StableToken)
}

/// The admin-approved AMM, if configured.
pub fn amm(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Amm)
}

/// The oracle price feed used to bound slippage, if configured.
pub fn price_feed(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::PriceFeed)
}

/// Admin-only body: approve `amm` as the liquidation venue.
pub fn whitelist_amm(env: &Env, amm: Address) -> Result<(), Error> {
    env.storage().instance().set(&DataKey::Amm, &amm);
    Ok(())
}

/// Admin-only body: remove the approved AMM, disabling liquidation.
pub fn revoke_amm(env: &Env) -> Result<(), Error> {
    if amm(env).is_none() {
        return Err(Error::LiquidationNotConfigured);
    }
    env.storage().instance().remove(&DataKey::Amm);
    Ok(())
}

/// Admin-only body: set the primary stablecoin.
pub fn set_stable_token(env: &Env, token: Address) -> Result<(), Error> {
    env.storage().instance().set(&DataKey::StableToken, &token);
    Ok(())
}

/// Admin-only body: set the price feed.
pub fn set_price_feed(env: &Env, feed: Address) -> Result<(), Error> {
    env.storage().instance().set(&DataKey::PriceFeed, &feed);
    Ok(())
}

// ── Liquidation ───────────────────────────────────────────────────────────

/// The minimum output a swap of `amount_in` must deliver, derived from the
/// feed's `price` and the caller's `max_slippage_bps` tolerance.
///
/// `floor(expected * (BPS - max_slippage_bps) / BPS)`, where
/// `expected = amount_in * price / PRICE_SCALE`.
fn min_out_from_price(amount_in: i128, price: i128, max_slippage_bps: u32) -> Result<i128, Error> {
    if price <= 0 || amount_in <= 0 {
        return Err(Error::InvalidPrice);
    }
    let expected = amount_in.checked_mul(price).ok_or(Error::MathOverflow)? / PRICE_SCALE;
    if expected <= 0 {
        return Err(Error::InvalidPrice);
    }
    let floor = expected
        .checked_mul((BPS_DENOMINATOR - max_slippage_bps) as i128)
        .ok_or(Error::MathOverflow)?
        / BPS_DENOMINATOR as i128;
    if floor <= 0 {
        return Err(Error::InvalidPrice);
    }
    Ok(floor)
}

/// Swap `amount_in` of `token_in` into the configured stablecoin through the
/// whitelisted AMM, enforcing an oracle-derived minimum output.
///
/// Callers must have checked initialization/admin and must hold the treasury's
/// reentrancy lock (see [`crate::strategies::with_lock`]). Returns the amount
/// of stablecoin actually received.
///
/// # Errors
///
/// - [`Error::LiquidationNotConfigured`] if the AMM, stablecoin, or price feed
///   is unset;
/// - [`Error::InvalidLiquidation`] for a non-positive amount, a slippage
///   tolerance of 100% or more, or `token_in` equal to the stablecoin;
/// - [`Error::InsufficientBalance`] if the treasury does not hold `amount_in`;
/// - [`Error::InvalidPrice`] if the feed returns a non-positive price;
/// - [`Error::MathOverflow`] on scaled-price overflow;
/// - [`Error::SlippageExceeded`] if the AMM delivers less than the floor.
pub fn liquidate_fees(
    env: &Env,
    token_in: Address,
    amount_in: i128,
    max_slippage_bps: u32,
) -> Result<i128, Error> {
    let stable = stable_token(env).ok_or(Error::LiquidationNotConfigured)?;
    let amm_addr = amm(env).ok_or(Error::LiquidationNotConfigured)?;
    let feed = price_feed(env).ok_or(Error::LiquidationNotConfigured)?;

    if amount_in <= 0 || max_slippage_bps >= BPS_DENOMINATOR || token_in == stable {
        return Err(Error::InvalidLiquidation);
    }

    let treasury = env.current_contract_address();
    let in_client = token::Client::new(env, &token_in);
    if in_client.balance(&treasury) < amount_in {
        return Err(Error::InsufficientBalance);
    }

    // The feed only bounds the trade; the AMM's delivery is what is booked.
    let price = PriceFeedClient::new(env, &feed).price(&token_in, &stable);
    let min_out = min_out_from_price(amount_in, price, max_slippage_bps)?;

    // Move the input to the AMM before asking it to swap (same shape as a
    // strategy deposit), then observe the stablecoin we actually receive.
    let stable_client = token::Client::new(env, &stable);
    let before_out = stable_client.balance(&treasury);
    in_client.transfer(&treasury, &amm_addr, &amount_in);
    let reported =
        AmmClient::new(env, &amm_addr).swap(&token_in, &stable, &amount_in, &min_out, &treasury);
    let received = stable_client.balance(&treasury) - before_out;

    if reported < min_out || received < min_out || received < reported {
        return Err(Error::SlippageExceeded);
    }

    FeesLiquidatedEvent {
        token_in,
        token_out: stable,
        amount_in,
        amount_out: received,
        min_out,
    }
    .publish(env);

    Ok(received)
}
