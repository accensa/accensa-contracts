//! Streaming micro-disbursement schedules (issue #410).
//!
//! A buyer escrows a `deposit` in the vault and it streams linearly to the
//! merchant at `rate_per_ledger` between `start_ledger` and `stop_ledger`
//! (e.g. $1/minute of API or SaaS consumption). At any ledger the amount
//! streamed so far is
//!
//! ```text
//! min(deposit, (current_ledger - start_ledger) * rate_per_ledger)
//! ```
//!
//! with the clock clamped to `[start_ledger, stop_ledger]` and frozen while
//! the stream is paused. The merchant's claimable balance is the streamed
//! amount less what has already been claimed.
//!
//! - `claim_stream` is permissionless: it can only ever pay the merchant
//!   recorded on the stream, so a keeper may call it. Once the stop ledger is
//!   reached the claim pays out the full deposit and the stream closes
//!   automatically (its record is removed).
//! - The buyer may `pause_stream` / `resume_stream` at any time. Resuming
//!   shifts the schedule forward by the paused duration, so the buyer is
//!   never charged for paused ledgers.
//! - The buyer may `cancel_stream` at any time: the merchant receives what
//!   has streamed but not been claimed, and the unspent principal is
//!   returned to the buyer.
//!
//! Escrowed stream principal is tracked in [`DataKey::StreamEscrow`] and is
//! excluded from the merchant's refund float, so refunds, withdrawals and
//! yield deployment can never spend a buyer's unstreamed deposit.

use accensa_common::{storage::extend_instance_ttl, Error};
use soroban_sdk::{contractevent, contracttype, token, Address, Env};

use crate::{acquire_reentrancy_lock, release_reentrancy_lock, DataKey, TTL_EXTEND, TTL_THRESHOLD};

// Doc comments on the exported types below are embedded in the WASM spec, so
// they are kept to one line; field details use plain comments instead. All
// events are topic-keyed by `stream_id`.

/// A linear disbursement schedule from a buyer to the merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementStream {
    // Funder; the only party that may pause, resume or cancel the stream,
    // and the recipient of unspent principal.
    pub buyer: Address,
    // Recipient of streamed funds: the vault merchant at creation time.
    pub merchant: Address,
    // First ledger at which funds begin to stream.
    pub start_ledger: u32,
    // Ledger at which the stream is fully disbursed.
    pub stop_ledger: u32,
    // Amount streamed per elapsed ledger, in the token's smallest unit.
    pub rate_per_ledger: i128,
    // Total deposit commitment escrowed by the buyer.
    pub deposit: i128,
    // Cumulative amount already paid out to the merchant.
    pub claimed: i128,
    // Ledger at which the buyer paused the stream; `None` while running.
    pub paused_at: Option<u32>,
}

/// A buyer opened a stream.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCreatedEvent {
    #[topic]
    pub stream_id: u64,
    pub buyer: Address,
    pub merchant: Address,
    pub start_ledger: u32,
    pub stop_ledger: u32,
    pub rate_per_ledger: i128,
    pub deposit: i128,
}

/// Streamed funds were paid to the merchant; `closed` if fully disbursed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamClaimedEvent {
    #[topic]
    pub stream_id: u64,
    pub amount: i128,
    pub total_claimed: i128,
    pub closed: bool,
}

/// The buyer paused a stream.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamPausedEvent {
    #[topic]
    pub stream_id: u64,
    pub ledger: u32,
}

/// The buyer resumed a stream; `stop_ledger` is the shifted stop ledger.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamResumedEvent {
    #[topic]
    pub stream_id: u64,
    pub ledger: u32,
    pub stop_ledger: u32,
}

/// The buyer cancelled a stream.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCancelledEvent {
    #[topic]
    pub stream_id: u64,
    // Streamed-but-unclaimed amount paid to the merchant.
    pub merchant_amount: i128,
    // Unspent principal returned to the buyer.
    pub buyer_amount: i128,
}

/// Amount of `stream` streamed as of ledger `now`:
/// `min(deposit, (clock - start_ledger) * rate_per_ledger)`, where the clock
/// is `now` (or the pause ledger while paused) clamped to
/// `[start_ledger, stop_ledger]`.
pub fn streamed_amount(stream: &DisbursementStream, now: u32) -> i128 {
    let clock = stream.paused_at.unwrap_or(now).min(stream.stop_ledger);
    let elapsed = clock.saturating_sub(stream.start_ledger);
    // Overflow means the product exceeds any representable deposit.
    (elapsed as i128)
        .checked_mul(stream.rate_per_ledger)
        .map_or(stream.deposit, |v| v.min(stream.deposit))
}

/// Total stream principal still held in escrow (deposits less claims across
/// all open streams).
pub(crate) fn escrowed(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::StreamEscrow)
        .unwrap_or(0)
}

fn adjust_escrow(env: &Env, delta: i128) -> Result<(), Error> {
    let next = escrowed(env)
        .checked_add(delta)
        .ok_or(Error::MathOverflow)?;
    env.storage().instance().set(&DataKey::StreamEscrow, &next);
    Ok(())
}

fn ensure_not_paused(env: &Env) -> Result<(), Error> {
    if env
        .storage()
        .instance()
        .get(&DataKey::IsPaused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    Ok(())
}

fn token_client(env: &Env) -> Result<token::Client<'_>, Error> {
    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    Ok(token::Client::new(env, &token_addr))
}

pub(crate) fn get_stream(env: &Env, stream_id: u64) -> Option<DisbursementStream> {
    env.storage().persistent().get(&DataKey::Stream(stream_id))
}

fn load(env: &Env, stream_id: u64) -> Result<DisbursementStream, Error> {
    get_stream(env, stream_id).ok_or(Error::StreamNotFound)
}

/// Persist `stream`, keeping its record live until well past its stop ledger.
fn store(env: &Env, stream_id: u64, stream: &DisbursementStream) {
    let key = DataKey::Stream(stream_id);
    env.storage().persistent().set(&key, stream);
    let extend_to = stream
        .stop_ledger
        .saturating_sub(env.ledger().sequence())
        .saturating_add(TTL_EXTEND)
        .min(env.storage().max_ttl());
    env.storage()
        .persistent()
        .extend_ttl(&key, extend_to, extend_to);
}

pub(crate) fn claimable(env: &Env, stream_id: u64) -> Result<i128, Error> {
    let stream = load(env, stream_id)?;
    Ok(streamed_amount(&stream, env.ledger().sequence()) - stream.claimed)
}

/// Open a stream: `buyer` escrows `deposit`, which streams to the merchant
/// at `rate_per_ledger` from `start_ledger` until `stop_ledger`. Returns the
/// new stream id.
///
/// # Errors
/// - `Paused`: the vault is paused.
/// - `SelfTransfer`: `buyer` is the vault itself.
/// - `InvalidAmount`: `deposit` or `rate_per_ledger` is not positive,
///   `start_ledger` is in the past, `stop_ledger <= start_ledger`, or the
///   deposit exceeds `(stop_ledger - start_ledger) * rate_per_ledger`.
pub(crate) fn create_stream(
    env: &Env,
    buyer: Address,
    start_ledger: u32,
    stop_ledger: u32,
    rate_per_ledger: i128,
    deposit: i128,
) -> Result<u64, Error> {
    acquire_reentrancy_lock(env)?;
    ensure_not_paused(env)?;
    buyer.require_auth();

    let vault = env.current_contract_address();
    if buyer == vault {
        return Err(Error::SelfTransfer);
    }
    if deposit <= 0 || rate_per_ledger <= 0 {
        return Err(Error::InvalidAmount);
    }
    if start_ledger < env.ledger().sequence() || stop_ledger <= start_ledger {
        return Err(Error::InvalidAmount);
    }
    // The deposit must be fully streamable by the stop ledger, otherwise the
    // surplus would sit in escrow after the schedule ends.
    let capacity = ((stop_ledger - start_ledger) as i128).checked_mul(rate_per_ledger);
    if capacity.is_some_and(|c| c < deposit) {
        return Err(Error::InvalidAmount);
    }

    let merchant: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;

    token_client(env)?.transfer(&buyer, &vault, &deposit);
    adjust_escrow(env, deposit)?;

    let stream_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::StreamCount)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&DataKey::StreamCount, &(stream_id + 1));

    let stream = DisbursementStream {
        buyer: buyer.clone(),
        merchant: merchant.clone(),
        start_ledger,
        stop_ledger,
        rate_per_ledger,
        deposit,
        claimed: 0,
        paused_at: None,
    };
    store(env, stream_id, &stream);

    StreamCreatedEvent {
        stream_id,
        buyer,
        merchant,
        start_ledger,
        stop_ledger,
        rate_per_ledger,
        deposit,
    }
    .publish(env);

    extend_instance_ttl(env, TTL_THRESHOLD, TTL_EXTEND);
    release_reentrancy_lock(env);
    Ok(stream_id)
}

/// Pay the merchant everything streamed but not yet claimed. Permissionless,
/// since funds only ever go to the stream's merchant. Once the stop ledger is
/// reached this disburses the remaining deposit and closes the stream.
/// Returns the amount paid.
///
/// # Errors
/// - `Paused`: the vault is paused.
/// - `StreamNotFound`: no open stream has this id.
/// - `NothingToWithdraw`: nothing has streamed since the last claim.
pub(crate) fn claim_stream(env: &Env, stream_id: u64) -> Result<i128, Error> {
    acquire_reentrancy_lock(env)?;
    ensure_not_paused(env)?;

    let mut stream = load(env, stream_id)?;
    let amount = streamed_amount(&stream, env.ledger().sequence()) - stream.claimed;
    if amount <= 0 {
        return Err(Error::NothingToWithdraw);
    }

    token_client(env)?.transfer(&env.current_contract_address(), &stream.merchant, &amount);
    adjust_escrow(env, -amount)?;

    stream.claimed += amount;
    let closed = stream.claimed == stream.deposit;
    if closed {
        env.storage()
            .persistent()
            .remove(&DataKey::Stream(stream_id));
    } else {
        store(env, stream_id, &stream);
    }

    StreamClaimedEvent {
        stream_id,
        amount,
        total_claimed: stream.claimed,
        closed,
    }
    .publish(env);

    extend_instance_ttl(env, TTL_THRESHOLD, TTL_EXTEND);
    release_reentrancy_lock(env);
    Ok(amount)
}

/// Freeze accrual at the current ledger. Buyer only.
///
/// # Errors
/// - `StreamNotFound`: no open stream has this id.
/// - `StreamNotActive`: already paused, or the stop ledger has passed.
pub(crate) fn pause_stream(env: &Env, stream_id: u64) -> Result<(), Error> {
    let mut stream = load(env, stream_id)?;
    stream.buyer.require_auth();

    let now = env.ledger().sequence();
    if stream.paused_at.is_some() || now >= stream.stop_ledger {
        return Err(Error::StreamNotActive);
    }
    stream.paused_at = Some(now);
    store(env, stream_id, &stream);

    StreamPausedEvent {
        stream_id,
        ledger: now,
    }
    .publish(env);
    Ok(())
}

/// Resume a paused stream, shifting its schedule forward by the paused
/// duration. Buyer only.
///
/// # Errors
/// - `StreamNotFound`: no open stream has this id.
/// - `StreamNotActive`: the stream is not paused.
pub(crate) fn resume_stream(env: &Env, stream_id: u64) -> Result<(), Error> {
    let mut stream = load(env, stream_id)?;
    stream.buyer.require_auth();

    let paused_at = stream.paused_at.ok_or(Error::StreamNotActive)?;
    let now = env.ledger().sequence();
    // Only ledgers that would have accrued are skipped: a pause lodged
    // before the start ledger counts from the start ledger.
    let shift = now.max(stream.start_ledger) - paused_at.max(stream.start_ledger);
    stream.start_ledger = stream
        .start_ledger
        .checked_add(shift)
        .ok_or(Error::MathOverflow)?;
    stream.stop_ledger = stream
        .stop_ledger
        .checked_add(shift)
        .ok_or(Error::MathOverflow)?;
    stream.paused_at = None;
    store(env, stream_id, &stream);

    StreamResumedEvent {
        stream_id,
        ledger: now,
        stop_ledger: stream.stop_ledger,
    }
    .publish(env);
    Ok(())
}

/// Close a stream early. Buyer only, and allowed while the vault is paused.
/// The merchant receives the streamed-but-unclaimed balance and the unspent
/// principal is returned to the buyer. Returns `(merchant_amount,
/// buyer_amount)`.
///
/// # Errors
/// - `StreamNotFound`: no open stream has this id.
pub(crate) fn cancel_stream(env: &Env, stream_id: u64) -> Result<(i128, i128), Error> {
    acquire_reentrancy_lock(env)?;

    let stream = load(env, stream_id)?;
    stream.buyer.require_auth();

    let streamed = streamed_amount(&stream, env.ledger().sequence());
    let merchant_amount = streamed - stream.claimed;
    let buyer_amount = stream.deposit - streamed;

    let token = token_client(env)?;
    let vault = env.current_contract_address();
    if merchant_amount > 0 {
        token.transfer(&vault, &stream.merchant, &merchant_amount);
    }
    if buyer_amount > 0 {
        token.transfer(&vault, &stream.buyer, &buyer_amount);
    }
    adjust_escrow(env, -(merchant_amount + buyer_amount))?;
    env.storage()
        .persistent()
        .remove(&DataKey::Stream(stream_id));

    StreamCancelledEvent {
        stream_id,
        merchant_amount,
        buyer_amount,
    }
    .publish(env);

    extend_instance_ttl(env, TTL_THRESHOLD, TTL_EXTEND);
    release_reentrancy_lock(env);
    Ok((merchant_amount, buyer_amount))
}
