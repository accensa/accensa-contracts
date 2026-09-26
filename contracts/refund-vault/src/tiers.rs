//! Merchant fee tiers (branch `feature/vault-merchant-tier-promotion`).
//!
//! A merchant can be promoted through a ladder of **fee tiers** as the vault
//! settles refund volume for them: refund more, pay a lower fee. The ladder is
//! merchant-configured (like [`RefundVault::set_fee_bps`]) and entirely
//! optional — a vault with no ladder installed charges the flat
//! [`DataKey::FeeBps`] rate exactly as before, so this module is purely
//! additive.
//!
//! # Model
//!
//! - [`MerchantTier`] is one rung: a `min_settled` volume threshold and the
//!   `fee_bps` charged once the merchant reaches it. Rungs are strictly
//!   increasing and the first one must start at `0`, so every merchant is
//!   always on some rung.
//! - [`RefundVault::set_tier_ladder`] installs the ladder and seeds
//!   [`MerchantTierState`] from the volume already settled (rung 0 on a fresh
//!   vault). [`RefundVault::clear_tier_ladder`] removes it, restoring the flat
//!   fee.
//! - Every successful claim accrues its gross amount into
//!   `MerchantTierState::settled_volume` ([`on_settled`]). When that crosses
//!   `next_threshold` the merchant is promoted to the highest rung the volume
//!   qualifies for and [`MerchantTierPromoted`] is emitted. Demotion never
//!   happens from volume (volume is monotonic); re-installing a ladder can
//!   move the merchant to any rung.
//!
//! # Cost
//!
//! The claim hot path must not decode the ladder on every refund, so the
//! active rung's fee, the settled volume, and the next rung's threshold are
//! cached together in [`MerchantTierState`] (one instance-storage read, taken
//! once per entry point by the policy cache). The ladder itself is read only
//! when a threshold is actually crossed.
//!
//! # Promotion timing
//!
//! The fee a claim charges is resolved once at the start of the entry point
//! and shared across a batch, so a rung crossed *by* claim `N` takes effect
//! from claim `N + 1` onward — never retroactively within the same call.

use accensa_common::{storage::extend_instance_ttl, Error};
use soroban_sdk::{contractevent, contractimpl, contracttype, Address, Env, Vec};

use crate::{DataKey, RefundVault, TTL_EXTEND, TTL_THRESHOLD};

/// Maximum number of rungs in a merchant's fee ladder. Bounds both the
/// one-off ladder decode and the instance-storage footprint.
pub const MAX_TIERS: u32 = 16;

/// Basis-point denominator: `fee_bps` values may not exceed this (100%).
const BPS_DENOMINATOR: u32 = 10_000;

/// One rung of the merchant fee ladder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTier {
    /// Cumulative settled refund volume (in the settlement token's smallest
    /// unit) at which this rung becomes active. The first rung is always `0`;
    /// thresholds are strictly increasing.
    pub min_settled: i128,
    /// Fee, in basis points, applied to refunds while this rung is the
    /// merchant's active tier.
    pub fee_bps: u32,
}

/// The merchant's cached position on the fee ladder.
///
/// Kept in instance storage beside the ladder so the claim hot path can read
/// the active fee and decide whether a threshold was crossed in one read,
/// without decoding the whole ladder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierState {
    /// Cumulative gross refund volume that has driven promotion so far.
    pub settled_volume: i128,
    /// Index of the active rung in [`DataKey::TierLadder`].
    pub current_tier: u32,
    /// The active rung's fee — a mirror of `ladder[current_tier].fee_bps`.
    pub fee_bps: u32,
    /// Settled volume at which the next rung activates, or [`i128::MAX`] when
    /// the merchant is already on the top rung.
    pub next_threshold: i128,
}

/// Emitted when a claim's settled volume crosses a rung and the merchant is
/// promoted.
///
/// Topics: `("merchant_tier_promoted", new_tier)`. The data carries the rung's
/// fee so an indexer can reconstruct the effective rate without reading state.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierPromoted {
    #[topic]
    pub new_tier: u32,
    pub previous_tier: u32,
    pub settled_volume: i128,
    pub fee_bps: u32,
}

/// Emitted when the merchant installs, replaces, or clears the fee ladder.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierLadderUpdated {
    /// Number of rungs now installed (`0` when the ladder was cleared).
    pub tier_count: u32,
    /// The rung the merchant sits on after the change.
    pub current_tier: u32,
    /// The fee that will apply to the next claim.
    pub fee_bps: u32,
}

fn admin(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)
}

/// Validate a proposed ladder: non-empty, within [`MAX_TIERS`], starting at a
/// `0` threshold, strictly increasing, and with in-range fees.
fn validate(ladder: &Vec<MerchantTier>) -> Result<(), Error> {
    if ladder.is_empty() || ladder.len() > MAX_TIERS {
        return Err(Error::InvalidTierLadder);
    }

    let mut previous: Option<i128> = None;
    for tier in ladder.iter() {
        if tier.fee_bps > BPS_DENOMINATOR {
            return Err(Error::InvalidTierLadder);
        }
        match previous {
            // The first rung must cover a fresh merchant.
            None => {
                if tier.min_settled != 0 {
                    return Err(Error::InvalidTierLadder);
                }
            }
            Some(prev) if tier.min_settled <= prev => return Err(Error::InvalidTierLadder),
            Some(_) => {}
        }
        previous = Some(tier.min_settled);
    }
    Ok(())
}

/// The highest rung `volume` qualifies for. Always returns a valid index for a
/// ladder that passed [`validate`] (rung 0 starts at `0`).
fn tier_for_volume(ladder: &Vec<MerchantTier>, volume: i128) -> u32 {
    let mut active = 0u32;
    let mut i = 0u32;
    while i < ladder.len() {
        if ladder.get(i).unwrap().min_settled <= volume {
            active = i;
        }
        i += 1;
    }
    active
}

/// The threshold of the rung after `tier`, or [`i128::MAX`] on the top rung.
fn next_threshold(ladder: &Vec<MerchantTier>, tier: u32) -> i128 {
    let next = tier + 1;
    if next < ladder.len() {
        ladder.get(next).unwrap().min_settled
    } else {
        i128::MAX
    }
}

/// The merchant's tier state, or `None` when no ladder is installed.
///
/// The single read that both the effective fee and the "is a ladder active?"
/// decision are derived from, so the claim path pays one instance load per
/// entry point rather than one per claim.
pub(crate) fn state(env: &Env) -> Option<MerchantTierState> {
    env.storage().instance().get(&DataKey::TierState)
}

/// The fee the next claim will charge: the active tier's rate when a ladder is
/// installed, and the flat [`DataKey::FeeBps`] config otherwise.
pub(crate) fn effective_fee_bps(env: &Env) -> u32 {
    match state(env) {
        Some(state) => state.fee_bps,
        None => env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0),
    }
}

/// Accrue `amount` of settled volume and promote the merchant if the next rung
/// was crossed. A no-op when no ladder is installed.
///
/// The state is re-read from storage on every call (rather than taken from the
/// entry point's cache) so a batch accrues correctly item by item. Failures
/// never reach here: `claim_single` calls this only after the transfers and
/// the record write have succeeded.
pub(crate) fn on_settled(env: &Env, amount: i128) {
    let state: Option<MerchantTierState> = env.storage().instance().get(&DataKey::TierState);
    let Some(mut state) = state else {
        return;
    };

    // Saturating: volume is monotonic and bounded by real token flows, but a
    // pathological caller must never be able to trap the vault with an
    // overflow the arithmetic cannot represent.
    state.settled_volume = state.settled_volume.saturating_add(amount);

    if state.settled_volume >= state.next_threshold {
        let ladder: Option<Vec<MerchantTier>> = env.storage().instance().get(&DataKey::TierLadder);
        if let Some(ladder) = ladder {
            let new_tier = tier_for_volume(&ladder, state.settled_volume);
            if new_tier > state.current_tier {
                let fee_bps = ladder.get(new_tier).unwrap().fee_bps;
                MerchantTierPromoted {
                    new_tier,
                    previous_tier: state.current_tier,
                    settled_volume: state.settled_volume,
                    fee_bps,
                }
                .publish(env);
                state.current_tier = new_tier;
                state.fee_bps = fee_bps;
            }
            // Refresh even when no promotion happened (e.g. a rung was skipped
            // by a single large claim) so the ladder is not re-read per claim.
            state.next_threshold = next_threshold(&ladder, state.current_tier);
        }
    }

    env.storage().instance().set(&DataKey::TierState, &state);
}

#[contractimpl]
impl RefundVault {
    /// Install (or replace) the merchant fee ladder. Merchant auth required.
    ///
    /// `tiers` must be non-empty, at most [`MAX_TIERS`] long, start at a `0`
    /// threshold, be strictly increasing, and carry fees at or below `10_000`
    /// bps; otherwise [`Error::InvalidTierLadder`]. The merchant's current rung
    /// is recomputed from the volume already settled, which is preserved across
    /// a replacement (re-installing a ladder does not reset progress).
    ///
    /// While a ladder is installed it is the source of truth for the fee: the
    /// flat [`RefundVault::set_fee_bps`] value applies only when the ladder is
    /// cleared. Emits [`MerchantTierLadderUpdated`].
    pub fn set_tier_ladder(env: Env, tiers: Vec<MerchantTier>) -> Result<(), Error> {
        admin(&env)?.require_auth();
        validate(&tiers)?;

        let settled_volume = env
            .storage()
            .instance()
            .get::<_, MerchantTierState>(&DataKey::TierState)
            .map_or(0, |state| state.settled_volume);

        let current_tier = tier_for_volume(&tiers, settled_volume);
        let fee_bps = tiers.get(current_tier).unwrap().fee_bps;
        let state = MerchantTierState {
            settled_volume,
            current_tier,
            fee_bps,
            next_threshold: next_threshold(&tiers, current_tier),
        };

        env.storage().instance().set(&DataKey::TierLadder, &tiers);
        env.storage().instance().set(&DataKey::TierState, &state);

        MerchantTierLadderUpdated {
            tier_count: tiers.len(),
            current_tier,
            fee_bps,
        }
        .publish(&env);

        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Remove the merchant fee ladder and its progress, restoring the flat
    /// [`RefundVault::set_fee_bps`] rate. Merchant auth required. Emits
    /// [`MerchantTierLadderUpdated`] with `tier_count` `0`.
    pub fn clear_tier_ladder(env: Env) -> Result<(), Error> {
        admin(&env)?.require_auth();

        env.storage().instance().remove(&DataKey::TierLadder);
        env.storage().instance().remove(&DataKey::TierState);

        MerchantTierLadderUpdated {
            tier_count: 0,
            current_tier: 0,
            fee_bps: env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0),
        }
        .publish(&env);

        extend_instance_ttl(&env, TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Read-only: the installed fee ladder, if any.
    pub fn get_tier_ladder(env: Env) -> Option<Vec<MerchantTier>> {
        env.storage().instance().get(&DataKey::TierLadder)
    }

    /// Read-only: the merchant's tier bookkeeping (settled volume, active rung,
    /// and next threshold), if a ladder is installed.
    pub fn get_tier_state(env: Env) -> Option<MerchantTierState> {
        env.storage().instance().get(&DataKey::TierState)
    }

    /// Read-only: the fee the next claim will charge. Equals the flat
    /// [`RefundVault::get_fee_bps`] value when no ladder is installed.
    pub fn get_effective_fee_bps(env: Env) -> u32 {
        effective_fee_bps(&env)
    }
}
