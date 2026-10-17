//! Multi-asset collateral pooling for state channels (issue #423).
//!
//! A multi-asset channel escrows several tokens (e.g. USDC, EURC, XLM) in
//! one channel session. Each token is tracked by its own [`BalanceRecord`]
//! and every signed state commits to one receiver balance *per asset*, so:
//!
//! - **Per-asset conservation.** For every asset, the receiver balance stays
//!   within `0..=deposit`; at settlement the receiver is paid that balance
//!   and the sender is refunded `deposit - balance` of the *same* token.
//!   Value never moves between assets.
//! - **No commingling or substitution.** A state must name exactly the
//!   channel's asset set — an unknown token, or a missing one, is rejected
//!   with [`Error::UnsupportedAsset`] — so a signature can never be
//!   reinterpreted against a different currency.
//! - **Atomic settlement.** One `settle` call pays out every asset; if any
//!   transfer fails the whole invocation (and every other payout) reverts.
//!
//! Lifecycle: `open` → any number of `update`s → `close` (starts the
//! challenge window, during which a newer signed state may still be
//! submitted with `update`) → `settle` once the window elapses. An open
//! channel past the maximum lifetime may also be settled directly per its
//! latest state.
//!
//! The signed payload binds the contract address and channel id, so a state
//! cannot be replayed on another channel or deployment:
//! `"accensa-ma-channel-v1" || contract (XDR) || channel_id (BE u64) ||
//! nonce (BE u64) || balances (XDR Map<Address, i128>, sorted by address)`.

use accensa_common::Error;
use soroban_sdk::{
    contractevent, contracttype, token, xdr::ToXdr, Address, Bytes, BytesN, Env, Map,
};

use crate::{dispute, ChannelPhase, DataKey, DEFAULT_MAX_CHANNEL_LIFETIME};

/// Maximum number of distinct assets pooled in one channel.
pub const MAX_ASSETS: u32 = 10;

const PAYLOAD_DOMAIN: &[u8] = b"accensa-ma-channel-v1";

/// Per-asset escrow and entitlement.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalanceRecord {
    /// Amount of this token escrowed by the sender.
    pub deposit: i128,
    /// Amount of this token the receiver is entitled to as of the latest
    /// state.
    pub receiver_balance: i128,
}

/// A channel escrowing several tokens.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetChannel {
    pub sender: Address,
    pub receiver: Address,
    /// Ed25519 public key used to verify off-chain state signatures.
    pub sender_pubkey: BytesN<32>,
    /// Token address → escrow/entitlement for that token.
    pub assets: Map<Address, BalanceRecord>,
    /// Latest accepted state nonce.
    pub nonce: u64,
    pub phase: ChannelPhase,
    pub opened_at: u32,
    /// Ledger at which the channel was closed (or last challenged); `0` while
    /// open.
    pub closed_at: u32,
    pub challenge_period: u32,
}

/// A signed multi-asset state: the receiver's balance in every asset.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetState {
    pub nonce: u64,
    pub balances: Map<Address, i128>,
}

/// Emitted when `open` funds a multi-asset channel: the `sender` and
/// `receiver`, the per-token deposits escrowed and the effective challenge
/// period, with `channel_id` as the event topic.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetChannelOpenedEvent {
    #[topic]
    pub channel_id: u64,
    pub sender: Address,
    pub receiver: Address,
    pub deposits: Map<Address, i128>,
    pub challenge_period: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetStateUpdatedEvent {
    #[topic]
    pub channel_id: u64,
    pub nonce: u64,
    pub balances: Map<Address, i128>,
    pub phase: ChannelPhase,
}

/// Emitted when `settle` finalizes a multi-asset channel: the per-asset
/// payouts made to the receiver and refunds returned to the sender, with
/// `channel_id` as the event topic.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetSettledEvent {
    #[topic]
    pub channel_id: u64,
    /// Per-asset amount paid to the receiver.
    pub receiver_payouts: Map<Address, i128>,
    /// Per-asset remainder refunded to the sender.
    pub sender_refunds: Map<Address, i128>,
}

/// Canonical bytes the sender signs for `state` on `channel_id`.
pub fn state_payload(
    env: &Env,
    contract: &Address,
    channel_id: u64,
    state: &MultiAssetState,
) -> Bytes {
    let mut buf = Bytes::from_slice(env, PAYLOAD_DOMAIN);
    buf.append(&contract.clone().to_xdr(env));
    buf.extend_from_slice(&channel_id.to_be_bytes());
    buf.extend_from_slice(&state.nonce.to_be_bytes());
    buf.append(&state.balances.clone().to_xdr(env));
    buf
}

fn key(channel_id: u64) -> DataKey {
    DataKey::MultiAssetChannel(channel_id)
}

fn load(env: &Env, channel_id: u64) -> Result<MultiAssetChannel, Error> {
    env.storage()
        .persistent()
        .get(&key(channel_id))
        .ok_or(Error::ChannelNotFound)
}

fn store(env: &Env, channel_id: u64, channel: &MultiAssetChannel) {
    env.storage().persistent().set(&key(channel_id), channel);
    env.storage()
        .persistent()
        .extend_ttl(&key(channel_id), crate::TTL_EXTEND, crate::TTL_EXTEND);
}

fn max_lifetime(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::MaxChannelLifetime)
        .unwrap_or(DEFAULT_MAX_CHANNEL_LIFETIME)
}

fn is_expired(env: &Env, channel: &MultiAssetChannel) -> bool {
    env.ledger().sequence() > channel.opened_at.saturating_add(max_lifetime(env))
}

/// Verify `state` against `channel`: signature, freshness, exact asset set
/// and per-asset bounds. Returns the channel with the new balances applied.
fn apply_state(
    env: &Env,
    channel_id: u64,
    mut channel: MultiAssetChannel,
    state: &MultiAssetState,
    signature: &BytesN<64>,
    require_newer: bool,
) -> Result<MultiAssetChannel, Error> {
    let payload = state_payload(env, &env.current_contract_address(), channel_id, state);
    env.crypto()
        .ed25519_verify(&channel.sender_pubkey, &payload, signature);

    if require_newer && state.nonce <= channel.nonce {
        return Err(Error::StaleState);
    }

    // Exact asset-set match: same size, and every asset in the state is
    // one of the channel's. Together these rule out substitution and
    // omission.
    if state.balances.len() != channel.assets.len() {
        return Err(Error::UnsupportedAsset);
    }
    for (asset, balance) in state.balances.iter() {
        let mut record = channel
            .assets
            .get(asset.clone())
            .ok_or(Error::UnsupportedAsset)?;
        if balance < 0 || balance > record.deposit {
            return Err(Error::ExceedsPayment);
        }
        record.receiver_balance = balance;
        channel.assets.set(asset, record);
    }

    channel.nonce = state.nonce;
    Ok(channel)
}

/// Open a channel escrowing every token in `deposits`: pulls each
/// `(token, amount)` from `sender` (whose authorization is required) into
/// the contract, stores a fresh [`MultiAssetChannel`] at nonce 0 in the
/// `Open` phase, emits [`MultiAssetChannelOpenedEvent`] and returns the new
/// channel id.
///
/// Pre-conditions: the contract is initialized, `deposits` is non-empty and
/// holds at most [`MAX_ASSETS`] tokens, and every amount is strictly
/// positive. Fails with `Error::NotInitialized` when the contract has no
/// token set, `Error::InvalidAmount` for an empty map or a non-positive
/// amount, and `Error::BatchTooLarge` beyond [`MAX_ASSETS`] assets. The
/// stored `challenge_period` is the requested one normalized: `0` selects
/// the default, larger values are capped at the crate maximum.
pub(crate) fn open(
    env: &Env,
    sender: Address,
    receiver: Address,
    sender_pubkey: BytesN<32>,
    deposits: Map<Address, i128>,
    challenge_period: u32,
) -> Result<u64, Error> {
    if !env.storage().instance().has(&DataKey::Token) {
        return Err(Error::NotInitialized);
    }
    if deposits.is_empty() {
        return Err(Error::InvalidAmount);
    }
    if deposits.len() > MAX_ASSETS {
        return Err(Error::BatchTooLarge);
    }
    sender.require_auth();

    let contract = env.current_contract_address();
    let mut assets = Map::new(env);
    for (asset, amount) in deposits.iter() {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        token::Client::new(env, &asset).transfer(&sender, &contract, &amount);
        assets.set(
            asset,
            BalanceRecord {
                deposit: amount,
                receiver_balance: 0,
            },
        );
    }

    let channel_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::ChannelCount)
        .unwrap_or(0)
        + 1;
    env.storage()
        .instance()
        .set(&DataKey::ChannelCount, &channel_id);
    env.storage()
        .instance()
        .extend_ttl(crate::TTL_THRESHOLD, crate::TTL_EXTEND);

    let challenge_period = crate::effective_challenge_period(challenge_period);
    store(
        env,
        channel_id,
        &MultiAssetChannel {
            sender: sender.clone(),
            receiver: receiver.clone(),
            sender_pubkey,
            assets,
            nonce: 0,
            phase: ChannelPhase::Open,
            opened_at: env.ledger().sequence(),
            closed_at: 0,
            challenge_period,
        },
    );

    MultiAssetChannelOpenedEvent {
        channel_id,
        sender,
        receiver,
        deposits,
        challenge_period,
    }
    .publish(env);

    Ok(channel_id)
}

/// Submit a newer signed state. Valid while the channel is open, or while
/// it is closed and the challenge window is still running (which re-arms
/// the window).
pub(crate) fn update(
    env: &Env,
    channel_id: u64,
    state: MultiAssetState,
    signature: BytesN<64>,
) -> Result<(), Error> {
    let channel = load(env, channel_id)?;
    match channel.phase {
        ChannelPhase::Open => {}
        ChannelPhase::Closed => {
            dispute::ensure_window_open(env, channel.closed_at, channel.challenge_period)?
        }
        _ => return Err(Error::ChannelNotOpen),
    }

    let mut channel = apply_state(env, channel_id, channel, &state, &signature, true)?;
    if channel.phase == ChannelPhase::Closed {
        channel.closed_at = env.ledger().sequence();
    }
    store(env, channel_id, &channel);

    MultiAssetStateUpdatedEvent {
        channel_id,
        nonce: state.nonce,
        balances: state.balances,
        phase: channel.phase,
    }
    .publish(env);
    Ok(())
}

/// Close an open channel with a signed state, starting the challenge window.
pub(crate) fn close(
    env: &Env,
    channel_id: u64,
    state: MultiAssetState,
    signature: BytesN<64>,
) -> Result<(), Error> {
    let channel = load(env, channel_id)?;
    if channel.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }
    if is_expired(env, &channel) {
        return Err(Error::ChannelExpired);
    }

    // The closing state may repeat the latest nonce but never go back.
    if state.nonce < channel.nonce {
        return Err(Error::StaleState);
    }
    let mut channel = apply_state(env, channel_id, channel, &state, &signature, false)?;
    channel.phase = ChannelPhase::Closed;
    channel.closed_at = env.ledger().sequence();
    store(env, channel_id, &channel);

    MultiAssetStateUpdatedEvent {
        channel_id,
        nonce: state.nonce,
        balances: state.balances,
        phase: channel.phase,
    }
    .publish(env);
    Ok(())
}

/// Settle every asset in one atomic transaction. Callable by anyone once a
/// closed channel's challenge window has elapsed, or once an open channel
/// has outlived the maximum lifetime.
pub(crate) fn settle(env: &Env, channel_id: u64) -> Result<(), Error> {
    let mut channel = load(env, channel_id)?;
    match channel.phase {
        ChannelPhase::Closed => {
            dispute::ensure_window_elapsed(env, channel.closed_at, channel.challenge_period)?
        }
        ChannelPhase::Open if is_expired(env, &channel) => {}
        ChannelPhase::Open => return Err(Error::ChallengeActive),
        _ => return Err(Error::ChannelAlreadyClosed),
    }

    // Mark finalized before any external call.
    channel.phase = ChannelPhase::Finalized;
    store(env, channel_id, &channel);

    let contract = env.current_contract_address();
    let mut receiver_payouts = Map::new(env);
    let mut sender_refunds = Map::new(env);
    for (asset, record) in channel.assets.iter() {
        let refund = record
            .deposit
            .checked_sub(record.receiver_balance)
            .ok_or(Error::MathOverflow)?;
        let client = token::Client::new(env, &asset);
        if record.receiver_balance > 0 {
            client.transfer(&contract, &channel.receiver, &record.receiver_balance);
        }
        if refund > 0 {
            client.transfer(&contract, &channel.sender, &refund);
        }
        receiver_payouts.set(asset.clone(), record.receiver_balance);
        sender_refunds.set(asset, refund);
    }

    MultiAssetSettledEvent {
        channel_id,
        receiver_payouts,
        sender_refunds,
    }
    .publish(env);
    Ok(())
}

/// Read the [`MultiAssetChannel`] stored for `channel_id`; fails with
/// `Error::ChannelNotFound` when no channel with that id exists.
pub(crate) fn get(env: &Env, channel_id: u64) -> Result<MultiAssetChannel, Error> {
    load(env, channel_id)
}
