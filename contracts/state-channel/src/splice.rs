//! Dynamic channel resizing ("splicing") for state channels (issue #460).
//!
//! Splicing changes a channel's base capacity without closing and reopening
//! it, so an off-chain payment stream can continue uninterrupted:
//!
//! - [`splice_in`] locks additional sender funds into the channel, raising
//!   `amount`; the off-chain state (nonce, receiver balance) is untouched.
//! - [`splice_out`] withdraws part of the sender's **free** escrow, lowering
//!   `amount`. The free amount is what is left after the receiver's committed
//!   balance and any pending HTLC reservations, so a splice can never claw
//!   back funds already owed or already committed.
//!
//! # Dual authorization
//!
//! Because a splice changes the ceiling the off-chain state can later be
//! settled against, both parties must authorize it: the sender (whose funds
//! move) and the receiver (whose settlement ceiling changes). Each entry point
//! calls `require_auth` on both addresses, so the new capacity limit is only
//! ever adopted with both signatures.
//!
//! Splicing is allowed only while the channel is [`ChannelPhase::Open`]; a
//! closed channel is settling its final state and has no capacity to resize.

use accensa_common::Error;
use soroban_sdk::{contractevent, token, Address, Env};

use crate::{htlc, Channel, ChannelPhase, DataKey};

/// Emitted when sender funds are spliced into a channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelSplicedInEvent {
    #[topic]
    pub channel_id: u64,
    pub added: i128,
    /// New base capacity after the splice.
    pub new_amount: i128,
}

/// Emitted when free sender funds are spliced out of a channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelSplicedOutEvent {
    #[topic]
    pub channel_id: u64,
    pub removed: i128,
    /// New base capacity after the splice.
    pub new_amount: i128,
}

fn load_open_channel(env: &Env, channel_id: u64) -> Result<Channel, Error> {
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }
    Ok(channel)
}

fn store(env: &Env, channel_id: u64, channel: &Channel) {
    env.storage()
        .instance()
        .set(&DataKey::Channel(channel_id), channel);
    env.storage()
        .instance()
        .extend_ttl(crate::TTL_THRESHOLD, crate::TTL_EXTEND);
}

fn token_address(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)
}

/// The sender's uncommitted escrow: capacity minus the receiver's balance and
/// any pending HTLC reservations.
pub(crate) fn free_balance(env: &Env, channel_id: u64, channel: &Channel) -> Result<i128, Error> {
    channel
        .amount
        .checked_sub(channel.balance)
        .ok_or(Error::MathOverflow)?
        .checked_sub(htlc::reserved(env, channel_id))
        .ok_or(Error::MathOverflow)
}

/// Splice `amount` of new sender funds into `channel_id`, raising capacity.
/// Requires both the sender's and the receiver's authorization.
pub(crate) fn splice_in(env: &Env, channel_id: u64, amount: i128) -> Result<(), Error> {
    let mut channel = load_open_channel(env, channel_id)?;
    channel.sender.require_auth();
    channel.receiver.require_auth();

    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    let contract = env.current_contract_address();
    token::Client::new(env, &token_address(env)?).transfer(&channel.sender, &contract, &amount);

    channel.amount = channel
        .amount
        .checked_add(amount)
        .ok_or(Error::MathOverflow)?;
    store(env, channel_id, &channel);

    ChannelSplicedInEvent {
        channel_id,
        added: amount,
        new_amount: channel.amount,
    }
    .publish(env);
    Ok(())
}

/// Splice `amount` of the sender's free escrow out of `channel_id`, lowering
/// capacity. Requires both the sender's and the receiver's authorization.
pub(crate) fn splice_out(env: &Env, channel_id: u64, amount: i128) -> Result<(), Error> {
    let mut channel = load_open_channel(env, channel_id)?;
    channel.sender.require_auth();
    channel.receiver.require_auth();

    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > free_balance(env, channel_id, &channel)? {
        return Err(Error::InsufficientChannelBalance);
    }

    // Effects before interaction: shrink capacity, then pay the sender.
    channel.amount -= amount;
    store(env, channel_id, &channel);

    token::Client::new(env, &token_address(env)?).transfer(
        &env.current_contract_address(),
        &channel.sender,
        &amount,
    );

    ChannelSplicedOutEvent {
        channel_id,
        removed: amount,
        new_amount: channel.amount,
    }
    .publish(env);
    Ok(())
}
