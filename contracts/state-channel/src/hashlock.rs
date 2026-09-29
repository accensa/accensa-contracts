//! Lightning-style pre-image reveal mechanics for state channels (issue #488).
//!
//! A payment channel state alone cannot express *conditional* payment: the
//! receiver is entitled to `balance` as soon as the sender signs, whether or
//! not the underlying service was delivered. A hashlock payment fixes that
//! the way a Lightning invoice does — the sender locks a slice of the
//! channel's escrow against `H = sha256(preimage)`, and the slice only
//! becomes the receiver's entitlement once they **reveal the preimage**,
//! on-chain, where everyone can check `sha256(preimage) == H`.
//!
//! # What a hashlock payment is
//!
//! [`add`] reserves `amount` of the sender's free escrow against `hashlock`
//! (same accounting shape as a virtual HTLC hop, `crate::htlc`), creating a
//! [`HashlockPayment`] in the [`HashlockPaymentState::Pending`] state. The
//! reservation is excluded from the sender's free escrow but never enters the
//! receiver's balance — until the preimage is revealed.
//!
//! # Revealing
//!
//! [`resolve_payment`] is the single verification point: it recomputes
//! `sha256(preimage)`, rejects any mismatch with [`Error::InvalidPreimage`],
//! rejects an already-resolved payment with [`Error::HtlcNotPending`], moves
//! `amount` from "reserved" into the receiver's channel balance, and marks
//! the payment [`HashlockPaymentState::Resolved`]. It is permissionless —
//! whoever holds the secret can settle, exactly like HTLC resolution.
//!
//! # Closing under an invoice
//!
//! [`crate::StateChannel::close_channel_with_preimage`] ties the mechanism
//! into the close flow the issue asks for: the receiver submits the sender's
//! signed final state *together with* the `payment_id` and `preimage`, the
//! contract verifies both, and the receiver's payout becomes
//! `state.balance + amount` — the signed balance plus the just-revealed
//! payment. The reveal rides the closing signature payload's call rather
//! than a separate transaction, so an invoice is settled atomically with the
//! close.
//!
//! # Escrow conservation
//!
//! Pending reservations (hashlock *and* HTLC, summed by
//! `StateChannel::reserved_escrow`) are part of every ceiling check on state
//! updates and closes, so `balance + reserved ≤ amount` always holds and no
//! combination of states, HTLCs and hashlock payments can overdraw escrow.
//! Resolving releases exactly what it credited: the total locked never
//! changes, matching `crate::htlc`'s invariant.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Bytes, BytesN, Env};

use crate::{ChannelPhase, DataKey};

/// Lifecycle of a hashlock payment.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HashlockPaymentState {
    /// Locked against `hashlock`, awaiting the preimage.
    Pending,
    /// Preimage revealed and verified; the amount joined the receiver's
    /// balance.
    Resolved,
}

/// A Lightning-style conditional payment locked on a channel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashlockPayment {
    pub channel_id: u64,
    pub payment_id: u64,
    /// SHA-256 of the preimage that unlocks this payment.
    pub hashlock: BytesN<32>,
    /// Amount of the channel's escrow reserved by this payment.
    pub amount: i128,
    pub state: HashlockPaymentState,
}

/// Emitted when a hashlock payment is locked against a channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashlockPaymentAddedEvent {
    #[topic]
    pub channel_id: u64,
    #[topic]
    pub payment_id: u64,
    pub hashlock: BytesN<32>,
    pub amount: i128,
}

/// Emitted when the preimage is revealed and verified.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashlockPaymentRevealedEvent {
    #[topic]
    pub channel_id: u64,
    #[topic]
    pub payment_id: u64,
    pub amount: i128,
    /// `sha256(preimage)`, i.e. the hashlock, so indexers can match the
    /// reveal to the invoice without the raw secret.
    pub preimage_sha256: BytesN<32>,
}

fn payment_key(channel_id: u64, payment_id: u64) -> DataKey {
    DataKey::HashlockPayment(channel_id, payment_id)
}

fn count_key(channel_id: u64) -> DataKey {
    DataKey::HashlockPaymentCount(channel_id)
}

fn reserved_key(channel_id: u64) -> DataKey {
    DataKey::HashlockReserved(channel_id)
}

fn bump(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, crate::TTL_THRESHOLD, crate::TTL_EXTEND);
}

/// Total escrow currently reserved by pending hashlock payments on
/// `channel_id`.
pub(crate) fn reserved(env: &Env, channel_id: u64) -> i128 {
    env.storage()
        .persistent()
        .get(&reserved_key(channel_id))
        .unwrap_or(0)
}

fn set_reserved(env: &Env, channel_id: u64, amount: i128) {
    let key = reserved_key(channel_id);
    env.storage().persistent().set(&key, &amount);
    bump(env, &key);
}

fn store(env: &Env, payment: &HashlockPayment) {
    let key = payment_key(payment.channel_id, payment.payment_id);
    env.storage().persistent().set(&key, payment);
    bump(env, &key);
}

fn load(env: &Env, channel_id: u64, payment_id: u64) -> Result<HashlockPayment, Error> {
    env.storage()
        .persistent()
        .get(&payment_key(channel_id, payment_id))
        .ok_or(Error::HtlcNotFound)
}

/// Verify `sha256(preimage) == hashlock` — the issue's core requirement,
/// kept in exactly one place so every reveal path checks identically.
fn verify_preimage(env: &Env, hashlock: &BytesN<32>, preimage: &Bytes) -> Result<(), Error> {
    let digest: BytesN<32> = env.crypto().sha256(preimage).into();
    if digest != *hashlock {
        return Err(Error::InvalidPreimage);
    }
    Ok(())
}

/// Lock `amount` of `channel_id`'s free escrow against `hashlock`. Returns
/// the new `payment_id`.
///
/// Requires the channel sender's authorization (it is their free escrow
/// being committed) and a [`ChannelPhase::Open`] channel. The reservation
/// must fit alongside the receiver's committed balance and every other
/// pending reservation (HTLC + hashlock), or it fails with
/// [`Error::HtlcInsufficientEscrow`].
pub(crate) fn add(
    env: &Env,
    channel_id: u64,
    hashlock: BytesN<32>,
    amount: i128,
) -> Result<u64, Error> {
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }
    channel.sender.require_auth();

    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    // The payment must fit inside the sender's still-free remainder:
    // committed balance + all pending reservations + this payment <= escrow.
    let htlc_reserved = crate::htlc::reserved(env, channel_id);
    let already_reserved = reserved(env, channel_id);
    let committed = channel
        .balance
        .checked_add(htlc_reserved)
        .ok_or(Error::MathOverflow)?
        .checked_add(already_reserved)
        .ok_or(Error::MathOverflow)?;
    if committed
        .checked_add(amount)
        .is_none_or(|total| total > channel.amount)
    {
        return Err(Error::HtlcInsufficientEscrow);
    }

    let payment_id: u64 = env
        .storage()
        .persistent()
        .get(&count_key(channel_id))
        .unwrap_or(0)
        + 1;
    let ck = count_key(channel_id);
    env.storage().persistent().set(&ck, &payment_id);
    bump(env, &ck);

    store(
        env,
        &HashlockPayment {
            channel_id,
            payment_id,
            hashlock: hashlock.clone(),
            amount,
            state: HashlockPaymentState::Pending,
        },
    );
    set_reserved(env, channel_id, already_reserved + amount);

    HashlockPaymentAddedEvent {
        channel_id,
        payment_id,
        hashlock,
        amount,
    }
    .publish(env);

    Ok(payment_id)
}

/// Reveal `preimage` for a pending payment and credit the receiver's
/// balance. Permissionless; requires a [`ChannelPhase::Open`] channel.
///
/// The credit rides on top of the last signed state: the reservation drops
/// by `amount` and `channel.balance` rises by the same `amount`, so
/// `balance + reserved` and the total locked against escrow are unchanged.
pub(crate) fn resolve_payment(
    env: &Env,
    channel_id: u64,
    payment_id: u64,
    preimage: &Bytes,
) -> Result<i128, Error> {
    let mut payment = load(env, channel_id, payment_id)?;
    if payment.state != HashlockPaymentState::Pending {
        return Err(Error::HtlcNotPending);
    }
    verify_preimage(env, &payment.hashlock, preimage)?;

    let mut channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }

    set_reserved(env, channel_id, reserved(env, channel_id) - payment.amount);
    channel.balance = channel
        .balance
        .checked_add(payment.amount)
        .ok_or(Error::MathOverflow)?;
    payment.state = HashlockPaymentState::Resolved;

    store(env, &payment);
    env.storage()
        .instance()
        .set(&DataKey::Channel(channel_id), &channel);

    HashlockPaymentRevealedEvent {
        channel_id,
        payment_id,
        amount: payment.amount,
        preimage_sha256: payment.hashlock,
    }
    .publish(env);

    Ok(payment.amount)
}

/// Reveal the preimage as part of a channel close (issue #488: "require
/// preimage in closing signature payload").
///
/// Called by [`crate::StateChannel::close_channel_with_preimage`] *after*
/// the sender's signature over the final state has been verified and the
/// escrow ceiling checked, *before* the close is recorded. The revealed
/// payment's amount is added to the receiver's final payout on top of the
/// signed `state.balance`, and the reservation is released so the close's
/// conservation math stays exact.
pub(crate) fn resolve_for_close(
    env: &Env,
    channel_id: u64,
    payment_id: u64,
    preimage: &Bytes,
) -> Result<i128, Error> {
    // Same verification and effects as a standalone reveal; the caller
    // enforces the close-specific ordering and phase checks.
    resolve_payment(env, channel_id, payment_id, preimage)
}

/// Read a single hashlock payment.
pub(crate) fn get(env: &Env, channel_id: u64, payment_id: u64) -> Result<HashlockPayment, Error> {
    load(env, channel_id, payment_id)
}
