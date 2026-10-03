//! Inactivity auto-cancellation for lapsed payment authorizations
//! (issue #435).
//!
//! A `upto` authorization is a lock on the buyer's tokens: [`authorize`]
//! records the cap and grants this contract a SEP-41 allowance up to it, and
//! the facilitator later [`settle`]s the actual amount. If the facilitator
//! walks away, the record and its allowance linger in storage long after the
//! payment is dead. That is a slow storage-rent leak and, worse, an
//! open-ended claim on the buyer's funds until `expiry` passes. A buyer should
//! be able to unilaterally put that dead authorization to rest — without
//! needing the merchant/facilitator to co-sign a cancellation they have no
//! incentive to send.
//!
//! # Model
//!
//! - **Dormancy = unclaimed for the full window.** An authorization is
//!   *inactive* when at least [`inactivity_timeout`] ledgers have elapsed since
//!   it was *created* with no `settle` (the default is
//!   [`DEFAULT_INACTIVITY_LEDGERS`], ~30 days at five seconds a ledger). The
//!   clock runs from creation, not from the authorization's `expiry`, so a
//!   buyer locked out of a long-dated authorization is not forced to wait for
//!   the expiry as well.
//! - **The buyer triggers it.** Cancelling releases the buyer's own tokens, so
//!   it requires `record.from`'s authorization; the merchant/facilitator's
//!   signature is never needed.
//! - **Release = revoke the allowance, delete the record.** The buyer's tokens
//!   never left their account — the "locked escrow" is the outstanding
//!   allowance. Cancellation zeroes that allowance (so the dead authorization
//!   can never be settled) and removes the storage entry, reclaiming its rent.
//! - **The timeout is governance-set.** Only the admin may change it; it
//!   defaults to ~30 days.

use soroban_sdk::{contractevent, token, Address, BytesN, Env};

use crate::types::AuthorizationRecord;
use crate::{DataKey, Error};

/// Default inactivity window: ~30 days of ledgers at five seconds per ledger
/// (`60 * 60 * 24 * 30 / 5`). Matches the contract's TTL horizon.
pub const DEFAULT_INACTIVITY_LEDGERS: u32 = 518_400;

/// The inactivity window, in ledgers. Defaults to
/// [`DEFAULT_INACTIVITY_LEDGERS`] until governance changes it.
pub fn inactivity_timeout(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::InactivityTimeout)
        .unwrap_or(DEFAULT_INACTIVITY_LEDGERS)
}

/// Admin-only: set the inactivity window, in ledgers. `0` cancels an
/// authorization as soon as its own expiry passes.
pub fn set_inactivity_timeout(env: &Env, ledgers: u32) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;
    admin.require_auth();

    env.storage()
        .instance()
        .set(&DataKey::InactivityTimeout, &ledgers);
    env.storage()
        .instance()
        .extend_ttl(crate::TTL_THRESHOLD, crate::TTL_EXTEND);
    Ok(())
}

/// The ledger after which an authorization created at `created_ledger` becomes
/// cancellable: `created_ledger + timeout` (saturating at `u32::MAX`, where no
/// further ledger can ever make it cancellable).
fn cancellation_deadline(created_ledger: u32, timeout: u32) -> u32 {
    created_ledger.saturating_add(timeout)
}

/// Buyer-triggered cancellation of a dormant authorization.
///
/// Requires `record.from`'s authorization. Releases the outstanding allowance
/// (zeroes it) and deletes the record, returning the cap that was released.
/// The merchant/facilitator is not involved.
///
/// # Errors
///
/// - [`Error::AuthorizationNotFound`] if there is no record for `payment_id`;
/// - [`Error::AlreadySettled`] if it was already settled;
/// - [`Error::NotInactive`] if `created_ledger + inactivity_timeout` has not
///   passed.
///
/// # Events emitted on success
/// - [`EscrowCancelledInactivity`]
pub fn cancel_inactive_escrow(env: &Env, payment_id: BytesN<32>) -> Result<i128, Error> {
    let record: AuthorizationRecord = env
        .storage()
        .persistent()
        .get(&DataKey::Authorization(payment_id.clone()))
        .ok_or(Error::AuthorizationNotFound)?;

    if record.consumed {
        return Err(Error::AlreadySettled);
    }

    let current_ledger = env.ledger().sequence();
    if current_ledger <= cancellation_deadline(record.created_ledger, inactivity_timeout(env)) {
        return Err(Error::NotInactive);
    }

    // Releasing the buyer's own allowance needs the buyer's authorization.
    record.from.require_auth();

    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    // Zero the outstanding allowance so the dead authorization can never be
    // settled. `live_until_ledger = 0` means "no expiry" on a zero amount.
    token::Client::new(env, &token_addr).approve(
        &record.from,
        &env.current_contract_address(),
        &0i128,
        &0u32,
    );

    // Drop the record: the rent tied to it is reclaimed.
    env.storage()
        .persistent()
        .remove(&DataKey::Authorization(payment_id.clone()));

    EscrowCancelledInactivity {
        payment_id,
        from: record.from.clone(),
        released: record.cap,
        ledger: current_ledger,
    }
    .publish(env);

    env.storage()
        .instance()
        .extend_ttl(crate::TTL_THRESHOLD, crate::TTL_EXTEND);

    Ok(record.cap)
}

/// Emitted when a dormant authorization is cancelled and its allowance
/// released back to the buyer.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCancelledInactivity {
    #[topic]
    pub payment_id: BytesN<32>,
    /// The buyer whose allowance was released.
    pub from: Address,
    /// The cap that was locked (and is now free).
    pub released: i128,
    /// The ledger at which cancellation was executed.
    pub ledger: u32,
}
