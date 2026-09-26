//! Token vesting treasury (issue #467).
//!
//! Deploys a token allocation contract that releases core team and investor
//! allocations linearly over a four-year period after a one-year cliff. The
//! admin funds the contract once and registers one [`VestingSchedule`] per
//! beneficiary; from then on a beneficiary may call [`Treasury::claim_vested`]
//! at any time to pull out whatever has unlocked but not yet been paid.
//!
//! # Model
//!
//! - **Time is wall-clock.** Vesting is measured against
//!   `env.ledger().timestamp()` (Unix seconds), so a schedule means the same
//!   length of time whatever the network's ledger rate turns out to be. See
//!   [`vesting`] for the release curve.
//! - **Unlocked, not schedule-driven.** There is no keeper and no per-claim
//!   schedule: [`Treasury::claim_vested`] pays out everything unlocked at the
//!   current ledger, so being late costs nothing and being early is
//!   impossible.
//! - **Never over-pays.** The contract only ever transfers
//!   `vested(now) - claimed`, and the ceiling is the schedule's `total`. A
//!   second claim in the same ledger is rejected with
//!   [`Error::NothingToClaim`].
//! - **Per-beneficiary auth.** A claim requires the beneficiary's own
//!   authorization, so an allocation can never be pulled by anyone else.
//!
//! Administered by a single admin address that creates schedules. There is no
//! revoke path in this MVP: changing or clawing back a live allocation is a
//! governance decision that this contract deliberately does not encode.

#![no_std]

use accensa_common::storage::{
    extend_instance_ttl_default, DEFAULT_TTL_BUMP, DEFAULT_TTL_LOW_WATER,
};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, Env,
};

#[cfg(test)]
mod test;
pub mod vesting;

use vesting::{VestingSchedule, FOUR_YEARS_SECS, ONE_YEAR_SECS};

contractmeta!(key = "name", val = "Treasury");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Storage keys. The schedule is keyed by beneficiary, so an allocation is
/// addressed by the same value that authorizes claiming it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// The admin that may create schedules.
    Admin,
    /// The SEP-41 token being vested.
    Token,
    /// A beneficiary's [`VestingSchedule`].
    Schedule(Address),
}

/// Errors returned by [`Treasury`].
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `initialize` / `__constructor` ran on an already-initialized contract.
    AlreadyInitialized = 1,
    /// A state-changing call ran before initialization.
    NotInitialized = 2,
    /// The caller is not the admin.
    NotAuthorized = 3,
    /// No vesting schedule exists for the beneficiary.
    ScheduleNotFound = 4,
    /// The beneficiary already has a schedule.
    ScheduleAlreadyExists = 5,
    /// The schedule parameters were invalid (zero/negative total, zero
    /// duration, or a cliff longer than the release window).
    InvalidSchedule = 6,
    /// Nothing has unlocked since the last claim (or ever).
    NothingToClaim = 7,
    /// Checked vesting arithmetic over- or under-flowed.
    MathOverflow = 8,
}

/// Emitted when the admin registers a beneficiary's allocation.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleCreatedEvent {
    #[topic]
    pub beneficiary: Address,
    pub total: i128,
    pub start: u64,
    pub cliff: u64,
    pub duration: u64,
}

/// Emitted when a beneficiary claims unlocked tokens.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedEvent {
    #[topic]
    pub beneficiary: Address,
    /// Tokens paid out by this call.
    pub amount: i128,
    /// Cumulative amount paid out for this beneficiary after the call.
    pub total_claimed: i128,
}

#[contract]
pub struct Treasury;

#[contractimpl]
impl Treasury {
    /// Constructor-wired initialization: records the admin and the token the
    /// treasury vests.
    pub fn __constructor(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        init(&env, admin, token)
    }

    /// `initialize` alias of [`__constructor`](Self::__constructor) for
    /// environments where deploy-via-constructor is unavailable.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        init(&env, admin, token)
    }

    /// Admin-only: register `beneficiary`'s allocation, unlocking linearly
    /// from `start` over `duration` seconds after a `cliff`-second delay.
    ///
    /// Use [`Self::add_team_schedule`] for the canonical one-year-cliff /
    /// four-year allocation.
    ///
    /// # Errors
    ///
    /// - [`Error::NotInitialized`] before initialization;
    /// - [`Error::InvalidSchedule`] for a non-positive total, a zero duration,
    ///   or a cliff longer than the window;
    /// - [`Error::ScheduleAlreadyExists`] if the beneficiary already has one.
    pub fn add_schedule(
        env: Env,
        beneficiary: Address,
        total: i128,
        start: u64,
        cliff: u64,
        duration: u64,
    ) -> Result<(), Error> {
        require_initialized(&env)?;
        require_admin(&env);

        let schedule = VestingSchedule {
            beneficiary: beneficiary.clone(),
            total,
            claimed: 0,
            start,
            cliff,
            duration,
        };
        if !schedule.is_valid() {
            return Err(Error::InvalidSchedule);
        }

        let key = DataKey::Schedule(beneficiary.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::ScheduleAlreadyExists);
        }
        env.storage().persistent().set(&key, &schedule);
        bump_schedule_ttl(&env, &key);

        ScheduleCreatedEvent {
            beneficiary,
            total,
            start,
            cliff,
            duration,
        }
        .publish(&env);

        extend_instance_ttl_default(&env);
        Ok(())
    }

    /// Admin-only convenience for the protocol's standard allocation: a
    /// one-year cliff followed by four years of linear release
    /// ([`ONE_YEAR_SECS`] / [`FOUR_YEARS_SECS`]).
    ///
    /// # Errors
    ///
    /// As [`Self::add_schedule`].
    pub fn add_team_schedule(
        env: Env,
        beneficiary: Address,
        total: i128,
        start: u64,
    ) -> Result<(), Error> {
        Self::add_schedule(
            env,
            beneficiary,
            total,
            start,
            ONE_YEAR_SECS,
            FOUR_YEARS_SECS,
        )
    }

    /// Read-only: `beneficiary`'s schedule, if one exists.
    pub fn get_schedule(env: Env, beneficiary: Address) -> Option<VestingSchedule> {
        env.storage()
            .persistent()
            .get(&DataKey::Schedule(beneficiary))
    }

    /// Read-only: the cumulative amount unlocked for `beneficiary` right now,
    /// regardless of what has already been claimed.
    ///
    /// # Errors
    ///
    /// [`Error::NotInitialized`] or [`Error::ScheduleNotFound`].
    pub fn vested_amount(env: Env, beneficiary: Address) -> Result<i128, Error> {
        require_initialized(&env)?;
        let schedule = load_schedule(&env, &beneficiary)?;
        schedule.vested(env.ledger().timestamp())
    }

    /// Read-only: the amount `beneficiary` could claim right now (unlocked
    /// minus already claimed), without changing any state.
    ///
    /// # Errors
    ///
    /// [`Error::NotInitialized`] or [`Error::ScheduleNotFound`].
    pub fn claimable(env: Env, beneficiary: Address) -> Result<i128, Error> {
        require_initialized(&env)?;
        let schedule = load_schedule(&env, &beneficiary)?;
        schedule.claimable(env.ledger().timestamp())
    }

    /// Claim everything unlocked for `beneficiary` and transfer it to them.
    /// Requires the beneficiary's authorization. Returns the amount paid.
    ///
    /// # Errors
    ///
    /// - [`Error::NotInitialized`] / [`Error::ScheduleNotFound`];
    /// - [`Error::NothingToClaim`] when nothing new has unlocked (including
    ///   before the cliff, and on a repeat claim in the same ledger);
    /// - [`Error::MathOverflow`] if the vesting arithmetic overflows.
    pub fn claim_vested(env: Env, beneficiary: Address) -> Result<i128, Error> {
        require_initialized(&env)?;
        beneficiary.require_auth();

        let key = DataKey::Schedule(beneficiary.clone());
        let mut schedule = load_schedule(&env, &beneficiary)?;

        let amount = schedule.claimable(env.ledger().timestamp())?;
        if amount <= 0 {
            return Err(Error::NothingToClaim);
        }

        // Checks-effects-interactions: persist the claim before the token
        // transfer, so a re-entrant token cannot claim the same unlock twice.
        schedule.claimed = schedule
            .claimed
            .checked_add(amount)
            .ok_or(Error::MathOverflow)?;
        env.storage().persistent().set(&key, &schedule);
        bump_schedule_ttl(&env, &key);

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &amount,
        );

        ClaimedEvent {
            beneficiary,
            amount,
            total_claimed: schedule.claimed,
        }
        .publish(&env);

        extend_instance_ttl_default(&env);
        Ok(amount)
    }

    /// Read-only: the admin that may create schedules.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    /// Read-only: the token this treasury vests.
    pub fn get_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Token)
    }
}

fn init(env: &Env, admin: Address, token: Address) -> Result<(), Error> {
    if env.storage().instance().has(&DataKey::Admin) {
        return Err(Error::AlreadyInitialized);
    }
    admin.require_auth();
    env.storage().instance().set(&DataKey::Admin, &admin);
    env.storage().instance().set(&DataKey::Token, &token);
    extend_instance_ttl_default(env);
    Ok(())
}

fn require_initialized(env: &Env) -> Result<(), Error> {
    if env.storage().instance().has(&DataKey::Admin) {
        Ok(())
    } else {
        Err(Error::NotInitialized)
    }
}

fn require_admin(env: &Env) {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("treasury must be initialized");
    admin.require_auth();
}

fn load_schedule(env: &Env, beneficiary: &Address) -> Result<VestingSchedule, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Schedule(beneficiary.clone()))
        .ok_or(Error::ScheduleNotFound)
}

/// Keep a schedule entry alive using the shared TTL policy. A vesting schedule
/// is long-lived, so both creation and every successful claim bump the entry
/// (see [`accensa_common::storage`] for the policy values).
fn bump_schedule_ttl(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, DEFAULT_TTL_LOW_WATER, DEFAULT_TTL_BUMP);
}
