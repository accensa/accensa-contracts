//! Hashed-Timelock Contracts for virtual multi-hop state channels (issue #458).
//!
//! An HTLC lets a sender route a payment through intermediaries without
//! opening a direct channel to the final recipient. Each hop locks a slice of
//! a channel's escrow against one SHA-256 `hash_lock`; whoever holds the
//! preimage can claim the locked slice before its timeout, and after the
//! timeout anyone may release it back to the sender.
//!
//! # Linking a route
//!
//! A hop is added with an optional [`HtlcRef`] pointing at the *upstream* hop
//! that funds it (the parent). The new hop's `timeout_ledger` must be
//! **strictly smaller** than its parent's, so as a route is walked downstream
//! (`Alice→Bob→Charlie`) every later hop expires earlier. That ordering is
//! what makes the route safe: Bob, holding the preimage, can always resolve
//! the upstream hop before it times out, so he is never left out of pocket.
//! Adding a hop whose timeout is not strictly smaller fails with
//! [`Error::HtlcTimeoutOutOfOrder`].
//!
//! # Escrow accounting
//!
//! A channel's escrow is split three ways: the receiver's committed balance,
//! the sum of *pending* HTLC amounts (the "reserved" total), and the sender's
//! still-free remainder. Adding a hop can never reserve more than that free
//! remainder ([`Error::HtlcInsufficientEscrow`]). Resolving credits the
//! receiver's balance with the hop's amount and releases the same amount of
//! reservation, so the total locked never changes; refunding simply releases
//! the reservation back to the sender. No tokens move until the channel itself
//! settles, so an HTLC never causes a partial or out-of-band transfer.
//!
//! HTLCs exist only while the channel is [`ChannelPhase::Open`]; a hop cannot
//! be added to, resolved on, or refunded from a closed channel.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Bytes, BytesN, Env};

use crate::{ChannelPhase, DataKey};

/// Lifecycle of a single HTLC hop.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtlcState {
    /// Locked and awaiting a preimage or its timeout.
    Pending,
    /// Claimed with the correct preimage; its amount joined the receiver's
    /// balance.
    Resolved,
    /// Timed out and released back to the sender's free escrow.
    Refunded,
}

/// Identifies an HTLC on a specific channel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcRef {
    pub channel_id: u64,
    pub htlc_id: u64,
}

/// A single hop's locked amount and timeout.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Htlc {
    pub channel_id: u64,
    pub htlc_id: u64,
    /// SHA-256 of the preimage that unlocks this hop.
    pub hash_lock: BytesN<32>,
    /// Amount of the channel's escrow reserved by this hop.
    pub amount: i128,
    /// Ledger after which the hop may be refunded. Strictly smaller than the
    /// parent hop's when this hop is part of a route.
    pub timeout_ledger: u32,
    pub state: HtlcState,
    /// Upstream hop's channel id, or `0` when this is a route's first hop.
    pub parent_channel_id: u64,
    /// Upstream hop's id, or `0` when this is a route's first hop.
    pub parent_htlc_id: u64,
}

/// Emitted when a hop is added to a channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcAddedEvent {
    #[topic]
    pub channel_id: u64,
    #[topic]
    pub htlc_id: u64,
    pub hash_lock: BytesN<32>,
    pub amount: i128,
    pub timeout_ledger: u32,
    /// Upstream hop channel id, or `0` when this is a route's first hop.
    pub parent_channel_id: u64,
    /// Upstream hop id, or `0` when this is a route's first hop.
    pub parent_htlc_id: u64,
}

/// Emitted when a hop is resolved with its preimage.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcResolvedEvent {
    #[topic]
    pub channel_id: u64,
    #[topic]
    pub htlc_id: u64,
    pub amount: i128,
}

/// Emitted when a timed-out hop is refunded to the sender's free escrow.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcRefundedEvent {
    #[topic]
    pub channel_id: u64,
    #[topic]
    pub htlc_id: u64,
    pub amount: i128,
}

fn htlc_key(channel_id: u64, htlc_id: u64) -> DataKey {
    DataKey::Htlc(channel_id, htlc_id)
}

fn count_key(channel_id: u64) -> DataKey {
    DataKey::HtlcCount(channel_id)
}

fn reserved_key(channel_id: u64) -> DataKey {
    DataKey::HtlcReserved(channel_id)
}

fn bump(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, crate::TTL_THRESHOLD, crate::TTL_EXTEND);
}

/// Total escrow currently reserved by pending HTLCs on `channel_id`.
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

fn store(env: &Env, htlc: &Htlc) {
    let key = htlc_key(htlc.channel_id, htlc.htlc_id);
    env.storage().persistent().set(&key, htlc);
    bump(env, &key);
}

fn load(env: &Env, channel_id: u64, htlc_id: u64) -> Result<Htlc, Error> {
    env.storage()
        .persistent()
        .get(&htlc_key(channel_id, htlc_id))
        .ok_or(Error::HtlcNotFound)
}

fn store_channel(env: &Env, channel_id: u64, channel: &crate::Channel) {
    env.storage()
        .instance()
        .set(&DataKey::Channel(channel_id), channel);
}

/// Add a hop reserving `amount` of `channel_id`'s escrow. Returns the new
/// `htlc_id`.
///
/// Requires the channel sender's authorization (it is their free escrow being
/// committed). Fails with [`Error::HtlcTimeoutOutOfOrder`] when `parent` is
/// supplied and `timeout_ledger` is not strictly smaller than the parent's.
pub(crate) fn add(
    env: &Env,
    channel_id: u64,
    hash_lock: BytesN<32>,
    amount: i128,
    timeout_ledger: u32,
    parent: Option<HtlcRef>,
) -> Result<u64, Error> {
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }
    channel.sender.require_auth();

    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    let current_ledger = env.ledger().sequence();
    if timeout_ledger <= current_ledger {
        return Err(Error::HtlcTimeoutElapsed);
    }

    // The hop must fit inside the sender's still-free remainder:
    // committed balance + reserved + this hop <= escrow.
    let already_reserved = reserved(env, channel_id);
    let committed = channel
        .balance
        .checked_add(already_reserved)
        .ok_or(Error::MathOverflow)?;
    if committed
        .checked_add(amount)
        .is_none_or(|t| t > channel.amount)
    {
        return Err(Error::HtlcInsufficientEscrow);
    }

    let (parent_channel_id, parent_htlc_id) = match &parent {
        Some(p) => (p.channel_id, p.htlc_id),
        None => (0, 0),
    };
    // A linked hop must expire strictly before its upstream parent, so a
    // downstream holder can always pull the upstream hop through first.
    if parent_channel_id != 0 {
        let parent_htlc = load(env, parent_channel_id, parent_htlc_id)?;
        if parent_htlc.state != HtlcState::Pending {
            return Err(Error::HtlcNotPending);
        }
        if timeout_ledger >= parent_htlc.timeout_ledger {
            return Err(Error::HtlcTimeoutOutOfOrder);
        }
    }

    let htlc_id: u64 = env
        .storage()
        .persistent()
        .get(&count_key(channel_id))
        .unwrap_or(0)
        + 1;
    let count_key = count_key(channel_id);
    env.storage().persistent().set(&count_key, &htlc_id);
    bump(env, &count_key);

    let htlc = Htlc {
        channel_id,
        htlc_id,
        hash_lock: hash_lock.clone(),
        amount,
        timeout_ledger,
        state: HtlcState::Pending,
        parent_channel_id,
        parent_htlc_id,
    };
    store(env, &htlc);
    set_reserved(env, channel_id, already_reserved + amount);

    HtlcAddedEvent {
        channel_id,
        htlc_id,
        hash_lock,
        amount,
        timeout_ledger,
        parent_channel_id,
        parent_htlc_id,
    }
    .publish(env);

    Ok(htlc_id)
}

/// Resolve a pending hop with `preimage`, crediting the receiver's balance.
/// Permissionless: anyone holding the preimage may settle the hop.
pub(crate) fn resolve(
    env: &Env,
    channel_id: u64,
    htlc_id: u64,
    preimage: Bytes,
) -> Result<(), Error> {
    let mut htlc = load(env, channel_id, htlc_id)?;
    if htlc.state != HtlcState::Pending {
        return Err(Error::HtlcNotPending);
    }
    let digest: BytesN<32> = env.crypto().sha256(&preimage).into();
    if digest != htlc.hash_lock {
        return Err(Error::InvalidPreimage);
    }

    let mut channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }

    // Release the reservation, then add the same amount to the receiver's
    // entitlement: the total locked against the escrow is unchanged.
    set_reserved(env, channel_id, reserved(env, channel_id) - htlc.amount);
    channel.balance = channel
        .balance
        .checked_add(htlc.amount)
        .ok_or(Error::MathOverflow)?;
    htlc.state = HtlcState::Resolved;

    store(env, &htlc);
    store_channel(env, channel_id, &channel);

    HtlcResolvedEvent {
        channel_id,
        htlc_id,
        amount: htlc.amount,
    }
    .publish(env);

    Ok(())
}

/// Refund a pending hop after its timeout has passed, releasing the reserved
/// amount back to the sender's free escrow. Permissionless.
pub(crate) fn refund(env: &Env, channel_id: u64, htlc_id: u64) -> Result<(), Error> {
    let mut htlc = load(env, channel_id, htlc_id)?;
    if htlc.state != HtlcState::Pending {
        return Err(Error::HtlcNotPending);
    }
    if env.ledger().sequence() <= htlc.timeout_ledger {
        return Err(Error::HtlcNotExpired);
    }
    // Ensure the owning channel still exists before touching its accounting.
    let _ = crate::StateChannel::get_channel(env.clone(), channel_id)?;

    set_reserved(env, channel_id, reserved(env, channel_id) - htlc.amount);
    htlc.state = HtlcState::Refunded;
    store(env, &htlc);

    HtlcRefundedEvent {
        channel_id,
        htlc_id,
        amount: htlc.amount,
    }
    .publish(env);

    Ok(())
}

/// Read a single hop.
pub(crate) fn get(env: &Env, channel_id: u64, htlc_id: u64) -> Result<Htlc, Error> {
    load(env, channel_id, htlc_id)
}
