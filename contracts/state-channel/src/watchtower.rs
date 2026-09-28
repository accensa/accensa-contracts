//! Watchtower reward bounties for channel defense (issue #459).
//!
//! A participant who may be forced offline can attach a token bounty to their
//! channel: if a **watchtower** successfully files a newer signed state on
//! their behalf (counter-evidence against a stale close), a fraction of the
//! balance that defense recovers is paid to that watchtower when the dispute
//! settles.
//!
//! # Who posts the bounty
//!
//! The bounty is configured by the channel's **receiver** — the party whose
//! balance a successful counter-proof protects. `set_watchtower_bounty`
//! requires the receiver's authorization and caps the reward at
//! [`MAX_REWARD_BPS`] (20%) of the recovered balance, so a bounty can never
//! consume the entire payout.
//!
//! # Who earns it
//!
//! The watchtower is the address that authorized the accepted counter-evidence
//! (`watchtower_counter_evidence`). Recording it is one-shot: once a dispute
//! settles, [`take_reward`] clears the stored watchtower, so a later dispute on
//! the same channel must name a new defender. The reward is carved out of the
//! receiver's payout, never out of the sender's refund, so the escrow still
//! balances exactly.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Address, Env};

use crate::{ChannelPhase, DataKey};

/// Maximum bounty, in basis points of the recovered balance (20%).
pub const MAX_REWARD_BPS: u32 = 2_000;

/// A channel's bounty configuration and the watchtower currently entitled to
/// it (if any).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchtowerBounty {
    /// Reward as basis points of the receiver payout at settlement.
    pub reward_bps: u32,
    /// The watchtower that filed the accepted counter-evidence, if any.
    pub watchtower: Option<Address>,
}

/// Emitted when a channel's bounty is configured.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchtowerBountySetEvent {
    #[topic]
    pub channel_id: u64,
    pub reward_bps: u32,
}

/// Emitted when a watchtower is paid for a successful counter-proof.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchtowerRewardEvent {
    #[topic]
    pub channel_id: u64,
    pub watchtower: Address,
    pub reward: i128,
}

fn key(channel_id: u64) -> DataKey {
    DataKey::Bounty(channel_id)
}

fn load(env: &Env, channel_id: u64) -> Option<WatchtowerBounty> {
    env.storage().instance().get(&key(channel_id))
}

/// The configured reward for `channel_id`, in basis points (`0` if unset).
pub(crate) fn reward_bps(env: &Env, channel_id: u64) -> u32 {
    load(env, channel_id).map(|b| b.reward_bps).unwrap_or(0)
}

fn store(env: &Env, channel_id: u64, bounty: &WatchtowerBounty) {
    env.storage().instance().set(&key(channel_id), bounty);
}

/// Configure (or update) the bounty on `channel_id`. Receiver-authorized.
pub(crate) fn set_bounty(env: &Env, channel_id: u64, reward_bps: u32) -> Result<(), Error> {
    let channel = crate::StateChannel::get_channel(env.clone(), channel_id)?;
    if channel.phase == ChannelPhase::Finalized {
        return Err(Error::ChannelAlreadyClosed);
    }
    channel.receiver.require_auth();
    if reward_bps > MAX_REWARD_BPS {
        return Err(Error::InvalidRatio);
    }

    let existing = load(env, channel_id);
    store(
        env,
        channel_id,
        &WatchtowerBounty {
            reward_bps,
            watchtower: existing.and_then(|b| b.watchtower),
        },
    );

    WatchtowerBountySetEvent {
        channel_id,
        reward_bps,
    }
    .publish(env);
    Ok(())
}

/// Record `watchtower` as the defender of `channel_id`. Called only after its
/// counter-evidence has been accepted.
pub(crate) fn record(env: &Env, channel_id: u64, watchtower: Address) {
    let existing = load(env, channel_id);
    store(
        env,
        channel_id,
        &WatchtowerBounty {
            reward_bps: existing.as_ref().map(|b| b.reward_bps).unwrap_or(0),
            watchtower: Some(watchtower),
        },
    );
}

/// Split `receiver_payout` into `(watchtower_reward, remaining_receiver_payout,
/// watchtower)` and clear the stored watchtower (one-shot). Returns a zero
/// reward and `None` when no bounty or no watchtower is set.
pub(crate) fn take_reward(
    env: &Env,
    channel_id: u64,
    receiver_payout: i128,
) -> Result<(i128, i128, Option<Address>), Error> {
    let Some(bounty) = load(env, channel_id) else {
        return Ok((0, receiver_payout, None));
    };
    let Some(watchtower) = bounty.watchtower else {
        return Ok((0, receiver_payout, None));
    };
    if bounty.reward_bps == 0 || receiver_payout <= 0 {
        // Nothing to split; clear the stale watchtower so it cannot be reused.
        store(
            env,
            channel_id,
            &WatchtowerBounty {
                reward_bps: bounty.reward_bps,
                watchtower: None,
            },
        );
        return Ok((0, receiver_payout, None));
    }

    let reward = receiver_payout
        .checked_mul(bounty.reward_bps as i128)
        .ok_or(Error::MathOverflow)?
        / 10_000;

    store(
        env,
        channel_id,
        &WatchtowerBounty {
            reward_bps: bounty.reward_bps,
            watchtower: None,
        },
    );

    WatchtowerRewardEvent {
        channel_id,
        watchtower: watchtower.clone(),
        reward,
    }
    .publish(env);

    Ok((reward, receiver_payout - reward, Some(watchtower)))
}
