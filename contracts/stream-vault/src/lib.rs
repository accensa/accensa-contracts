//! Streaming micro-disbursement schedules (issue #410).
//!
//! A buyer escrows a `deposit` in this contract and it streams linearly to
//! the merchant at `rate_per_ledger` between `start_ledger` and
//! `stop_ledger` (e.g. $1/minute of API or SaaS consumption). At any ledger
//! the amount streamed so far is
//!
//! ```text
//! min(deposit, (current_ledger - start_ledger) * rate_per_ledger)
//! ```
//!
//! with the clock clamped to `[start_ledger, stop_ledger]` and frozen while
//! the stream is paused. The merchant's claimable balance is the streamed
//! amount less what has already been claimed.
//!
//! - [`StreamVault::claim_stream`] is permissionless: it can only ever pay
//!   the merchant, so a keeper may call it. Once the stop ledger is reached
//!   the claim pays out the full deposit and the stream closes automatically
//!   (its record is removed).
//! - The buyer may [`StreamVault::pause_stream`] /
//!   [`StreamVault::resume_stream`] at any time. Resuming shifts the schedule
//!   forward by the paused duration, so the buyer is never charged for
//!   paused ledgers.
//! - The buyer may [`StreamVault::cancel_stream`] at any time: the merchant
//!   receives what has streamed but not been claimed, and the unspent
//!   principal is returned to the buyer.
//!
//! # Why a separate contract
//!
//! Streaming was first built into `RefundVault`, but the vault is already
//! close to Soroban's 128 KiB contract size limit and the extra code pushed
//! it over. A dedicated contract also keeps buyer escrow structurally
//! separate from the merchant's refund float: this contract's balance is
//! exactly the unclaimed principal of its open streams, so no refund,
//! withdrawal or yield deployment can ever touch it.
//!
//! One instance serves one merchant and one token, mirroring `RefundVault`.
//!
//! # Ordering
//!
//! Every entry point writes its state before making a token transfer
//! (checks-effects-interactions), so a re-entrant call from a non-standard
//! token observes the post-update state.

#![no_std]

use accensa_common::storage::extend_instance_ttl;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, Env,
};

contractmeta!(key = "name", val = "StreamVault");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Approximately 30 days of ledgers at ~5 seconds per ledger.
const TTL_EXTEND: u32 = 518_400;
/// Remaining TTL below which the instance TTL is bumped.
const TTL_THRESHOLD: u32 = 100;

/// Errors are local to this contract rather than added to
/// `accensa_common::Error`: every contract that exposes the shared enum
/// embeds all of its variants in its WASM spec, so growing it would enlarge
/// unrelated contracts.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// A deposit or rate is not positive, or the schedule is empty,
    /// backdated, or cannot fully stream the deposit by its stop ledger.
    InvalidAmount = 1,
    /// The buyer is this contract.
    SelfTransfer = 2,
    /// No open stream has this id. A stream that was cancelled or ran to
    /// completion is removed, so it also reports this error.
    StreamNotFound = 3,
    /// The stream is not in the state this operation requires, e.g. pausing
    /// an already-paused stream or resuming a running one.
    StreamNotActive = 4,
    /// Nothing has streamed since the last claim.
    NothingToWithdraw = 5,
    /// Resuming would shift the schedule past `u32::MAX`.
    MathOverflow = 6,
}

#[contracttype]
pub enum DataKey {
    /// Recipient of every stream's disbursements.
    Merchant,
    /// Token every stream is denominated in.
    Token,
    /// Next stream id to assign.
    StreamCount,
    /// An open stream, keyed by id. Persistent storage, TTL kept past the
    /// stop ledger. Removed once the stream closes or is cancelled.
    Stream(u64),
}

/// A linear disbursement schedule from a buyer to the merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementStream {
    /// Funder of the stream; the only party that may pause, resume or cancel
    /// it, and the recipient of unspent principal.
    pub buyer: Address,
    /// First ledger at which funds begin to stream.
    pub start_ledger: u32,
    /// Ledger at which the stream is fully disbursed.
    pub stop_ledger: u32,
    /// Amount streamed per elapsed ledger, in the token's smallest unit.
    pub rate_per_ledger: i128,
    /// Total deposit commitment escrowed by the buyer.
    pub deposit: i128,
    /// Cumulative amount already paid out to the merchant.
    pub claimed: i128,
    /// Ledger at which the buyer paused the stream; `None` while running.
    pub paused_at: Option<u32>,
}

/// Emitted when a buyer opens a stream.
///
/// Topics: `("stream_created_event", stream_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCreatedEvent {
    #[topic]
    pub stream_id: u64,
    pub buyer: Address,
    pub start_ledger: u32,
    pub stop_ledger: u32,
    pub rate_per_ledger: i128,
    pub deposit: i128,
}

/// Emitted when streamed funds are paid out to the merchant.
///
/// Topics: `("stream_claimed_event", stream_id)`. `closed` is `true` when
/// this claim fully disbursed the stream and its record was removed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamClaimedEvent {
    #[topic]
    pub stream_id: u64,
    pub amount: i128,
    pub total_claimed: i128,
    pub closed: bool,
}

/// Emitted when the buyer pauses a stream.
///
/// Topics: `("stream_paused_event", stream_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamPausedEvent {
    #[topic]
    pub stream_id: u64,
    pub ledger: u32,
}

/// Emitted when the buyer resumes a paused stream. `stop_ledger` is the
/// schedule's new stop ledger after shifting by the paused duration.
///
/// Topics: `("stream_resumed_event", stream_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamResumedEvent {
    #[topic]
    pub stream_id: u64,
    pub ledger: u32,
    pub stop_ledger: u32,
}

/// Emitted when the buyer cancels a stream.
///
/// Topics: `("stream_cancelled_event", stream_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCancelledEvent {
    #[topic]
    pub stream_id: u64,
    /// Streamed-but-unclaimed amount paid to the merchant.
    pub merchant_amount: i128,
    /// Unspent principal returned to the buyer.
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

fn merchant(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Merchant)
        .expect("initialized in the constructor")
}

fn token_client(env: &Env) -> token::Client<'_> {
    let token: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .expect("initialized in the constructor");
    token::Client::new(env, &token)
}

fn load(env: &Env, stream_id: u64) -> Result<DisbursementStream, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Stream(stream_id))
        .ok_or(Error::StreamNotFound)
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

#[contract]
pub struct StreamVault;

#[contractimpl]
impl StreamVault {
    /// Bind this instance to the `merchant` that receives every stream's
    /// disbursements and the `token` streams are denominated in.
    pub fn __constructor(env: Env, merchant: Address, token: Address) {
        env.storage().instance().set(&DataKey::Merchant, &merchant);
        env.storage().instance().set(&DataKey::Token, &token);
        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
    }

    /// Open a stream: `buyer` escrows `deposit`, which streams to the
    /// merchant at `rate_per_ledger` from `start_ledger` until
    /// `stop_ledger`. Returns the new stream id.
    ///
    /// # Errors
    /// - `SelfTransfer`: `buyer` is this contract.
    /// - `InvalidAmount`: `deposit` or `rate_per_ledger` is not positive,
    ///   `start_ledger` is in the past, `stop_ledger <= start_ledger`, or the
    ///   deposit exceeds `(stop_ledger - start_ledger) * rate_per_ledger`
    ///   (the surplus could never stream and would sit in escrow).
    pub fn create_stream(
        env: Env,
        buyer: Address,
        start_ledger: u32,
        stop_ledger: u32,
        rate_per_ledger: i128,
        deposit: i128,
    ) -> Result<u64, Error> {
        buyer.require_auth();

        let this = env.current_contract_address();
        if buyer == this {
            return Err(Error::SelfTransfer);
        }
        if deposit <= 0 || rate_per_ledger <= 0 {
            return Err(Error::InvalidAmount);
        }
        if start_ledger < env.ledger().sequence() || stop_ledger <= start_ledger {
            return Err(Error::InvalidAmount);
        }
        let capacity = ((stop_ledger - start_ledger) as i128).checked_mul(rate_per_ledger);
        if capacity.is_some_and(|c| c < deposit) {
            return Err(Error::InvalidAmount);
        }

        let stream_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::StreamCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::StreamCount, &(stream_id + 1));
        store(
            &env,
            stream_id,
            &DisbursementStream {
                buyer: buyer.clone(),
                start_ledger,
                stop_ledger,
                rate_per_ledger,
                deposit,
                claimed: 0,
                paused_at: None,
            },
        );

        token_client(&env).transfer(&buyer, &this, &deposit);

        StreamCreatedEvent {
            stream_id,
            buyer,
            start_ledger,
            stop_ledger,
            rate_per_ledger,
            deposit,
        }
        .publish(&env);

        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
        Ok(stream_id)
    }

    /// Pay the merchant everything streamed but not yet claimed.
    /// Permissionless, since funds only ever go to the merchant. Once the
    /// stop ledger is reached this disburses the remaining deposit and
    /// closes the stream. Returns the amount paid.
    ///
    /// # Errors
    /// - `StreamNotFound`: no open stream has this id.
    /// - `NothingToWithdraw`: nothing has streamed since the last claim.
    pub fn claim_stream(env: Env, stream_id: u64) -> Result<i128, Error> {
        let mut stream = load(&env, stream_id)?;
        let amount = streamed_amount(&stream, env.ledger().sequence()) - stream.claimed;
        if amount <= 0 {
            return Err(Error::NothingToWithdraw);
        }

        stream.claimed += amount;
        let closed = stream.claimed == stream.deposit;
        if closed {
            env.storage()
                .persistent()
                .remove(&DataKey::Stream(stream_id));
        } else {
            store(&env, stream_id, &stream);
        }

        token_client(&env).transfer(&env.current_contract_address(), &merchant(&env), &amount);

        StreamClaimedEvent {
            stream_id,
            amount,
            total_claimed: stream.claimed,
            closed,
        }
        .publish(&env);

        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
        Ok(amount)
    }

    /// Freeze accrual at the current ledger. Buyer only.
    ///
    /// # Errors
    /// - `StreamNotFound`: no open stream has this id.
    /// - `StreamNotActive`: already paused, or the stop ledger has passed.
    pub fn pause_stream(env: Env, stream_id: u64) -> Result<(), Error> {
        let mut stream = load(&env, stream_id)?;
        stream.buyer.require_auth();

        let now = env.ledger().sequence();
        if stream.paused_at.is_some() || now >= stream.stop_ledger {
            return Err(Error::StreamNotActive);
        }
        stream.paused_at = Some(now);
        store(&env, stream_id, &stream);

        StreamPausedEvent {
            stream_id,
            ledger: now,
        }
        .publish(&env);
        Ok(())
    }

    /// Resume a paused stream, shifting its schedule forward by the paused
    /// duration. Buyer only.
    ///
    /// # Errors
    /// - `StreamNotFound`: no open stream has this id.
    /// - `StreamNotActive`: the stream is not paused.
    /// - `MathOverflow`: the shifted schedule would pass `u32::MAX`.
    pub fn resume_stream(env: Env, stream_id: u64) -> Result<(), Error> {
        let mut stream = load(&env, stream_id)?;
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
        store(&env, stream_id, &stream);

        StreamResumedEvent {
            stream_id,
            ledger: now,
            stop_ledger: stream.stop_ledger,
        }
        .publish(&env);
        Ok(())
    }

    /// Close a stream early. Buyer only. The merchant receives the
    /// streamed-but-unclaimed balance and the unspent principal is returned
    /// to the buyer. Returns `(merchant_amount, buyer_amount)`.
    ///
    /// # Errors
    /// - `StreamNotFound`: no open stream has this id.
    pub fn cancel_stream(env: Env, stream_id: u64) -> Result<(i128, i128), Error> {
        let stream = load(&env, stream_id)?;
        stream.buyer.require_auth();

        let streamed = streamed_amount(&stream, env.ledger().sequence());
        let merchant_amount = streamed - stream.claimed;
        let buyer_amount = stream.deposit - streamed;
        env.storage()
            .persistent()
            .remove(&DataKey::Stream(stream_id));

        let token = token_client(&env);
        let this = env.current_contract_address();
        if merchant_amount > 0 {
            token.transfer(&this, &merchant(&env), &merchant_amount);
        }
        if buyer_amount > 0 {
            token.transfer(&this, &stream.buyer, &buyer_amount);
        }

        StreamCancelledEvent {
            stream_id,
            merchant_amount,
            buyer_amount,
        }
        .publish(&env);

        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
        Ok((merchant_amount, buyer_amount))
    }

    /// The open stream `stream_id`, or `None` once it is closed or cancelled.
    pub fn get_stream(env: Env, stream_id: u64) -> Option<DisbursementStream> {
        env.storage().persistent().get(&DataKey::Stream(stream_id))
    }

    /// Amount the merchant could claim on `stream_id` at the current ledger.
    ///
    /// # Errors
    /// - `StreamNotFound`: no open stream has this id.
    pub fn get_stream_claimable(env: Env, stream_id: u64) -> Result<i128, Error> {
        let stream = load(&env, stream_id)?;
        Ok(streamed_amount(&stream, env.ledger().sequence()) - stream.claimed)
    }

    pub fn get_merchant(env: Env) -> Address {
        merchant(&env)
    }

    pub fn get_token(env: Env) -> Address {
        token_client(&env).address
    }
}

#[cfg(test)]
mod test;
