//! Partial-refund settlement (issue #473).
//!
//! A merchant can already refund **part** of a payment: `refund` takes an
//! `amount` and a `payment_amount`, and cumulative refunds for a payment are
//! capped at that ceiling (the cumulative model shipped in issue #99, stored
//! under `DataKey::RefundV2`). What was spread across `claim_single` — and
//! therefore invisible to callers and indexers — is the *split* that a partial
//! refund implies:
//!
//! - what the buyer (the refund recipient) receives;
//! - what the configured fee diverts to the fee recipient; and
//! - what **stays in the vault** for the merchant, i.e. the part of the
//!   payment that is not being refunded.
//!
//! This module makes that split explicit and reusable:
//!
//! - [`resolve_ceiling`] and [`split_amount`] are the single implementations of
//!   the ceiling rule and the fee split, called by `claim_single` so the live
//!   refund path and any preview can never disagree; and
//! - [`RefundVault::preview_settlement`] exposes the resulting figures as a
//!   read-only query, so a merchant API or dashboard can show an exact,
//!   dust-accurate breakdown *before* submitting the transaction.
//!
//! "Dust" here is the fractional-token remainder of the fee computation: the
//! fee rounds **up**, so the recipient's payout `recipient_amount` is what is
//! left, and the three amounts always sum back to `amount + merchant_retained`
//! exactly — no tokens are created or lost by the split.

use accensa_common::Error;
use soroban_sdk::{contractimpl, contracttype, BytesN, Env};

use crate::{refund_fee, DataKey, RefundRecord, RefundVault, RefundVaultArgs, RefundVaultClient};

/// The exact breakdown of a proposed (partial) refund against a payment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundSettlement {
    /// Paid to the refund recipient (the buyer): `amount - fee`.
    pub recipient_amount: i128,
    /// Diverted to the fee recipient in this call. `0` when no fee is set.
    pub fee: i128,
    /// The part of the original payment that stays in the vault for the
    /// merchant — the payment's ceiling minus everything refunded once this
    /// settlement is applied.
    pub merchant_retained: i128,
    /// Cumulative amount refunded for the payment *after* this settlement.
    pub cumulative_refunded: i128,
}

/// Resolve the cumulative-refund bookkeeping for a proposed refund of `amount`
/// against a payment whose original size is `payment_amount`.
///
/// Returns `(previous_refunded, ceiling)`: the amount already refunded for the
/// payment (from the stored record, or `0` on the first partial) and the hard
/// ceiling the cumulative total may never exceed.
///
/// The ceiling is the stored record's `payment_amount` once a first partial
/// exists, and the caller-supplied `payment_amount` otherwise — so the ceiling
/// is fixed by the first claim and cannot be raised by a later one.
///
/// # Errors
///
/// [`Error::ExceedsPayment`] when the record ceiling is non-positive or the new
/// cumulative total would pass it (including `i128` overflow).
pub(crate) fn resolve_ceiling(
    env: &Env,
    payment_ref: &BytesN<32>,
    amount: i128,
    payment_amount: i128,
) -> Result<(i128, i128), Error> {
    let existing: Option<RefundRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::RefundV2(payment_ref.clone()));
    let (previous_refunded, record_ceiling) = match existing {
        Some(rec) => (rec.amount_refunded, rec.payment_amount),
        None => (0i128, payment_amount),
    };

    if previous_refunded.checked_add(amount).is_none()
        || record_ceiling <= 0
        || previous_refunded + amount > record_ceiling
    {
        return Err(Error::ExceedsPayment);
    }

    Ok((previous_refunded, record_ceiling))
}

/// Split a refund `amount` into `(fee, payout)` for the given fee rate.
///
/// The fee rounds **up** (the fractional-token remainder goes to the
/// protocol), so `fee + payout == amount` exactly for every input.
pub(crate) fn split_amount(amount: i128, fee_bps: u32) -> (i128, i128) {
    let fee = refund_fee(amount, fee_bps);
    (fee, amount - fee)
}

#[contractimpl]
impl RefundVault {
    /// Read-only: the exact settlement a [`RefundVault::refund`] of `amount`
    /// would produce for `payment_ref`, without changing any state.
    ///
    /// This is the query form of the partial-refund split: it reports the
    /// buyer's payout, the fee, the cumulative total after the refund, and how
    /// much of the original payment the merchant would retain. A caller can
    /// use it to show a merchant an exact breakdown (fees and rounding
    /// included) before the refund is authorized.
    ///
    /// The policy gates (window, deadline, cooldown, VDF, oracle) are **not**
    /// evaluated here — they depend on the ledger a refund lands in and can
    /// only be judged at execution time. This preview answers "how would this
    /// amount be split?", not "would this claim be admitted?".
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidAmount`] when `amount` is not strictly positive;
    /// - [`Error::ExceedsPayment`] when the refund would pass the payment's
    ///   ceiling, or a legacy pre-#99 record exists for the payment.
    pub fn preview_settlement(
        env: Env,
        payment_ref: BytesN<32>,
        amount: i128,
        payment_amount: i128,
    ) -> Result<RefundSettlement, Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // A legacy record denotes a fully-refunded payment; `refund` rejects it
        // outright, so the preview must not report a split for it either.
        if env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::ExceedsPayment);
        }

        let (previous_refunded, ceiling) =
            resolve_ceiling(&env, &payment_ref, amount, payment_amount)?;
        let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
        let (fee, recipient_amount) = split_amount(amount, fee_bps);
        let cumulative_refunded = previous_refunded + amount;

        Ok(RefundSettlement {
            recipient_amount,
            fee,
            merchant_retained: ceiling - cumulative_refunded,
            cumulative_refunded,
        })
    }
}
