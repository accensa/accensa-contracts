//! Chainlink data-feed consumer adapter for dynamic fee scaling.
//!
//! See [`chainlink`] for the round model. One contract instance serves one
//! fiat feed; `feed_id` args exist only for compatibility with the standard
//! `Oracle` interface (`get_price` + `get_last_update_ledger`) and are
//! otherwise ignored.

#![no_std]

pub mod chainlink;

#[cfg(test)]
mod test;

use chainlink::{latest_round, RoundData};
use soroban_sdk::{contract, contracterror, contractimpl, contractmeta, Address, BytesN, Env};

contractmeta!(key = "name", val = "AccensaChainlinkConsumer");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Consumer errors. Discriminants avoid the `RefundVault` oracle range.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    /// Round failed completeness checks (`answered_in_round < round_id`,
    /// `updated_at == 0`, or non-positive answer).
    IncompleteRound = 4,
    /// Valid round older than `max_staleness_ledgers`.
    StalePrice = 5,
    /// No round stored yet.
    NoData = 6,
    /// `round_id` is not newer than the latest stored round.
    StaleRoundId = 7,
}

#[contract]
pub struct ChainlinkConsumer;

#[contractimpl]
impl ChainlinkConsumer {
    /// Bind this instance to `admin`, the relayer allowed to push rounds.
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&chainlink::DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&chainlink::DataKey::Admin, &admin);
        Ok(())
    }

    /// Push a new feed round (admin only). Rejects incomplete rounds and
    /// non-monotonic `round_id`s.
    pub fn set_round_data(env: Env, round: RoundData) -> Result<(), Error> {
        chainlink::set_round_data(&env, &round)
    }

    /// Raw round by id, without freshness checks.
    pub fn get_round_data(env: Env, round_id: u64) -> Result<RoundData, Error> {
        chainlink::get_round_data(&env, round_id)
    }

    /// Latest round with completeness + freshness checks.
    pub fn latest_round_data(env: Env, max_staleness_ledgers: u32) -> Result<RoundData, Error> {
        let round = latest_round(&env)?;
        chainlink::validate_round(&round, env.ledger().sequence(), max_staleness_ledgers)?;
        Ok(round)
    }

    /// Latest validated price. Fails closed on incomplete or stale rounds.
    pub fn latest_answer(env: Env, max_staleness_ledgers: u32) -> Result<i128, Error> {
        let round = latest_round(&env)?;
        chainlink::validate_round(&round, env.ledger().sequence(), max_staleness_ledgers)
    }

    // ── Standard Oracle interface (RefundVault-compatible) ──────────────

    /// Latest price; `feed_id` ignored (single-feed instance). Never applies
    /// a staleness bound itself — the aggregator filters via
    /// [`get_last_update_ledger`](Self::get_last_update_ledger).
    pub fn get_price(env: Env, _feed_id: BytesN<32>) -> Result<i128, Error> {
        let round = latest_round(&env)?;
        chainlink::validate_round(&round, env.ledger().sequence(), 0)
    }

    /// Ledger at which the latest round was answered; `0` when empty.
    pub fn get_last_update_ledger(env: Env, _feed_id: BytesN<32>) -> u32 {
        latest_round(&env).map(|r| r.updated_at).unwrap_or(0)
    }

    /// Bound authority, if initialized. Read-only.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&chainlink::DataKey::Admin)
    }
}
