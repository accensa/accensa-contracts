//! Chainlink `AggregatorV3Interface`-style consumer adapter.
//!
//! Ingests an off-chain fiat price feed for dynamic fee scaling. The admin
//! (relayer) pushes [`RoundData`] via [`set_round_data`]; readers use
//! [`latest_round_data`] / [`get_round_data`] with round-completeness and
//! freshness checks. [`get_price`] / [`get_last_update_ledger`] expose the
//! standard `Oracle` interface consumed by `RefundVault`'s median aggregator.
//!
//! # MVP cuts
//!
//! Deliberately out of scope: multi-feed routing (one feed per contract
//! instance; `feed_id` is accepted for interface compatibility and ignored),
//! decimals introspection, historical round windows, and access-controlled
//! heartbeat automation.

use soroban_sdk::{contracttype, Address, Env};

/// A single Chainlink-style round, mirroring `AggregatorV3Interface`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoundData {
    /// Monotonic round identifier.
    pub round_id: u64,
    /// Price answer in the feed's fixed-point scale. Must be positive.
    pub answer: i128,
    /// Ledger sequence when the round started.
    pub started_at: u32,
    /// Ledger sequence when the round was answered.
    pub updated_at: u32,
    /// Round in which the answer was computed; must equal `round_id`.
    pub answered_in_round: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    LatestRound,
    Round(u64),
}

/// Validate a round: completeness first, then freshness.
///
/// - `answered_in_round < round_id` → incomplete round.
/// - `updated_at == 0` or `answer <= 0` → incomplete round.
/// - `max_staleness_ledgers > 0` and the round is older than that many
///   ledgers → stale.
pub fn validate_round(
    round: &RoundData,
    current_ledger: u32,
    max_staleness_ledgers: u32,
) -> Result<i128, crate::Error> {
    if round.answered_in_round < round.round_id || round.updated_at == 0 || round.answer <= 0 {
        return Err(crate::Error::IncompleteRound);
    }
    if max_staleness_ledgers > 0
        && round.updated_at.saturating_add(max_staleness_ledgers) < current_ledger
    {
        return Err(crate::Error::StalePrice);
    }
    Ok(round.answer)
}

/// Persist a new round. Caller must have authenticated as admin.
pub fn set_round_data(env: &Env, round: &RoundData) -> Result<(), crate::Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(crate::Error::NotInitialized)?;
    admin.require_auth();
    if round.answered_in_round < round.round_id || round.updated_at == 0 || round.answer <= 0 {
        return Err(crate::Error::IncompleteRound);
    }
    // Monotonic rounds only; replaying an old round is rejected.
    let latest: u64 = env
        .storage()
        .instance()
        .get(&DataKey::LatestRound)
        .unwrap_or(0);
    if round.round_id <= latest && latest != 0 {
        return Err(crate::Error::StaleRoundId);
    }
    env.storage()
        .instance()
        .set(&DataKey::LatestRound, &round.round_id);
    env.storage()
        .persistent()
        .set(&DataKey::Round(round.round_id), round);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Round(round.round_id), 100, 518_400);
    Ok(())
}

/// Fetch a stored round by id.
pub fn get_round_data(env: &Env, round_id: u64) -> Result<RoundData, crate::Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Round(round_id))
        .ok_or(crate::Error::NoData)
}

/// Fetch the latest stored round.
pub fn latest_round(env: &Env) -> Result<RoundData, crate::Error> {
    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::LatestRound)
        .ok_or(crate::Error::NoData)?;
    get_round_data(env, id)
}
