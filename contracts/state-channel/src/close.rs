//! Cooperative mutual close (issue #412).
//!
//! A unilateral `close_channel` has to sit out the challenge window because
//! only the sender signed the closing state. When *both* parties sign the
//! final balance distribution there is nothing left to challenge, so
//! [`mutual_close`] settles immediately:
//!
//! 1. The receiver registers an Ed25519 key for the channel once with
//!    `register_receiver_key` (authorized by the receiver's address). The
//!    sender's key is already fixed at `open_channel`.
//! 2. Both parties sign the same [`MutualCloseState`] envelope:
//!    `"accensa-mutual-close-v1" || contract (XDR) || channel_id (BE u64) ||
//!    receiver_balance (BE i128) || sender_balance (BE i128)`. Binding the
//!    contract address and channel id stops a signed envelope from being
//!    replayed on another channel or deployment.
//! 3. `mutual_close` checks both signatures, requires the two balances to add
//!    up to exactly the escrow, pays both sides, deletes the channel's
//!    storage entries and emits [`ChannelClosedCooperative`].
//!
//! It works from `Open`, `Closed` or `Disputed`, skipping any running
//! challenge window. Since the channel record is deleted, the same envelope
//! cannot be used twice.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, token, xdr::ToXdr, Address, Bytes, BytesN, Env};

use crate::{crypto, ChannelPhase, DataKey};

const PAYLOAD_DOMAIN: &[u8] = b"accensa-mutual-close-v1";

/// The final balance distribution both parties sign.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutualCloseState {
    pub channel_id: u64,
    /// Amount paid to the receiver.
    pub receiver_balance: i128,
    /// Amount returned to the sender.
    pub sender_balance: i128,
}

/// Emitted when a channel is settled by mutual close.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelClosedCooperative {
    #[topic]
    pub channel_id: u64,
    pub receiver_balance: i128,
    pub sender_balance: i128,
}

/// Canonical bytes both parties sign for `state`.
pub fn close_payload(env: &Env, contract: &Address, state: &MutualCloseState) -> Bytes {
    let mut buf = Bytes::from_slice(env, PAYLOAD_DOMAIN);
    buf.append(&contract.clone().to_xdr(env));
    buf.extend_from_slice(&state.channel_id.to_be_bytes());
    buf.extend_from_slice(&state.receiver_balance.to_be_bytes());
    buf.extend_from_slice(&state.sender_balance.to_be_bytes());
    buf
}

/// Record the receiver's Ed25519 key for `channel_id`. The receiver may
/// replace it while the channel exists.
pub(crate) fn register_receiver_key(
    env: &Env,
    channel_id: u64,
    pubkey: BytesN<32>,
) -> Result<(), Error> {
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    channel.receiver.require_auth();
    if channel.phase == ChannelPhase::Finalized {
        return Err(Error::ChannelAlreadyClosed);
    }
    env.storage()
        .instance()
        .set(&DataKey::ReceiverPubkey(channel_id), &pubkey);
    Ok(())
}

pub(crate) fn receiver_key(env: &Env, channel_id: u64) -> Option<BytesN<32>> {
    env.storage()
        .instance()
        .get(&DataKey::ReceiverPubkey(channel_id))
}

pub(crate) fn mutual_close(
    env: &Env,
    state: MutualCloseState,
    sender_sig: BytesN<64>,
    receiver_sig: BytesN<64>,
) -> Result<(), Error> {
    let channel_id = state.channel_id;
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase == ChannelPhase::Finalized {
        return Err(Error::ChannelAlreadyClosed);
    }
    let receiver_pubkey = receiver_key(env, channel_id).ok_or(Error::InvalidSignature)?;

    if state.receiver_balance < 0
        || state.sender_balance < 0
        || state.receiver_balance.checked_add(state.sender_balance) != Some(channel.amount)
    {
        return Err(Error::ExceedsPayment);
    }

    // Both signatures cover the same canonical envelope and are verified in
    // one batch call. `ed25519_verify` traps on a bad signature, so a forged
    // half aborts before any transfer; the length pairing is checked first.
    let payload = close_payload(env, &env.current_contract_address(), &state);
    crypto::verify_signatures(
        env,
        &payload,
        &[channel.sender_pubkey.clone(), receiver_pubkey],
        &[sender_sig, receiver_sig],
    )?;

    // Delete storage before paying out so a re-entrant token cannot close
    // the same channel twice.
    env.storage()
        .instance()
        .remove(&DataKey::Channel(channel_id));
    env.storage()
        .instance()
        .remove(&DataKey::ReceiverPubkey(channel_id));

    let token_id: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    let tok = token::Client::new(env, &token_id);
    let contract = env.current_contract_address();
    if state.receiver_balance > 0 {
        tok.transfer(&contract, &channel.receiver, &state.receiver_balance);
    }
    if state.sender_balance > 0 {
        tok.transfer(&contract, &channel.sender, &state.sender_balance);
    }

    ChannelClosedCooperative {
        channel_id,
        receiver_balance: state.receiver_balance,
        sender_balance: state.sender_balance,
    }
    .publish(env);

    Ok(())
}
