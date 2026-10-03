//! Dust sweep for orphaned refund escrows (issue #427).
//!
//! Every payment the vault refunds against keeps a cumulative
//! [`RefundRecord`] whose unrefunded remainder is `payment_amount -
//! amount_refunded`. Once the refund escrow for that payment has closed and
//! stayed closed for [`DUST_SWEEP_DELAY_LEDGERS`] (~90 days), a remainder
//! strictly below the configured dust threshold (rounding leftovers) can no
//! longer be claimed in practice. `sweep_dust` moves that remainder to the
//! designated treasury and deletes the record, reclaiming its storage.
//!
//! An escrow is considered closed at `max(paid_at_ledger + refund_window,
//! last_refund_ledger)`: the refund window has elapsed *and* no refund has
//! been processed since. With `refund_window == 0` (no window) the escrow
//! closes at its last refund.

use accensa_common::Error;
use soroban_sdk::{contractevent, token, Address, BytesN, Env};

use crate::{active_fee_recipient, DataKey, RefundRecord};

/// Default dust threshold in the token's smallest unit (stroops for XLM).
pub const DEFAULT_DUST_THRESHOLD: i128 = 100;

/// Ledgers an escrow must stay closed before its dust may be swept:
/// 90 days at ~5 seconds per ledger (90 * 17_280).
pub const DUST_SWEEP_DELAY_LEDGERS: u32 = 1_555_200;

/// Emitted when a refund record's dust is swept to the treasury.
///
/// Topics: `("dust_swept_event", payment_ref: BytesN<32>)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DustSweptEvent {
    #[topic]
    pub payment_ref: BytesN<32>,
    /// Residual amount transferred to the treasury (`0` for a fully
    /// refunded record, which is only reclaimed).
    pub amount: i128,
    pub treasury: Address,
}

pub(crate) fn dust_threshold(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::DustThreshold)
        .unwrap_or(DEFAULT_DUST_THRESHOLD)
}

/// The treasury receiving swept dust: the configured dust treasury, falling
/// back to the fee recipient (and from there to the merchant).
pub(crate) fn dust_treasury(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::DustTreasury)
        .unwrap_or_else(|| active_fee_recipient(env))
}

fn admin(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)
}

pub(crate) fn set_dust_config(env: &Env, threshold: i128, treasury: Address) -> Result<(), Error> {
    admin(env)?.require_auth();
    if threshold <= 0 {
        return Err(Error::InvalidAmount);
    }
    if treasury == env.current_contract_address() {
        return Err(Error::SelfTransfer);
    }
    env.storage()
        .instance()
        .set(&DataKey::DustThreshold, &threshold);
    env.storage()
        .instance()
        .set(&DataKey::DustTreasury, &treasury);
    Ok(())
}

/// Ledger at which the escrow for `record` closed.
fn closed_at(record: &RefundRecord, window: u32) -> u32 {
    record
        .paid_at_ledger
        .saturating_add(window)
        .max(record.ledger)
}

pub(crate) fn sweep_dust(env: &Env, payment_ref: BytesN<32>) -> Result<i128, Error> {
    accensa_common::reentrancy::ReentrancyGuard::acquire(env)?;

    if env
        .storage()
        .instance()
        .get(&DataKey::IsPaused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    admin(env)?.require_auth();

    let key = DataKey::RefundV2(payment_ref.clone());
    let record: RefundRecord = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::RefundNotFound)?;

    let residual = record
        .payment_amount
        .checked_sub(record.amount_refunded)
        .ok_or(Error::MathOverflow)?;
    if residual < 0 || residual >= dust_threshold(env) {
        return Err(Error::InvalidAmount);
    }

    let window: u32 = env
        .storage()
        .instance()
        .get(&DataKey::RefundWindow)
        .unwrap_or(0);
    let eligible_after = closed_at(&record, window).saturating_add(DUST_SWEEP_DELAY_LEDGERS);
    if env.ledger().sequence() <= eligible_after {
        return Err(Error::TimelockNotExpired);
    }

    let treasury = dust_treasury(env);
    if residual > 0 {
        if treasury == env.current_contract_address() {
            return Err(Error::SelfTransfer);
        }
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::Client::new(env, &token_addr);
        if token_client.balance(&env.current_contract_address()) < residual {
            return Err(Error::InsufficientFloat);
        }
        token_client.transfer(&env.current_contract_address(), &treasury, &residual);
    }

    env.storage().persistent().remove(&key);

    DustSweptEvent {
        payment_ref,
        amount: residual,
        treasury,
    }
    .publish(env);

    accensa_common::reentrancy::ReentrancyGuard::release(env);
    Ok(residual)
}
