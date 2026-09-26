//! Token vesting treasury (issue #467) with diversified yield (issue #466).
//!
//! Deploys a token allocation contract that releases core team and investor
//! allocations linearly over a four-year period after a one-year cliff. The
//! admin funds the contract once and registers one [`VestingSchedule`] per
//! beneficiary; from then on a beneficiary may call [`Treasury::claim_vested`]
//! at any time to pull out whatever has unlocked but not yet been paid.
//!
//! Idle reserves — the balance that is not backing a live vesting claim — can
//! additionally be parked in several whitelisted yield protocols. Governance
//! picks the split; [`Treasury::rebalance_portfolio`] rotates the portfolio
//! onto it and [`Treasury::recall_strategy`] brings any position home. See
//! [`strategies`] for the model.
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

pub mod strategies;
#[cfg(test)]
mod strategies_test;
#[cfg(test)]
mod test;
pub mod vesting;

use strategies::{AllocationConfig, StrategyAllocation};
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
    /// Whether a yield strategy is on the admin-approved whitelist (#466).
    WhitelistedStrategy(Address),
    /// A strategy's weight and deployed bookkeeping (#466).
    Allocation(Address),
    /// The allocated strategies, in governance-supplied order (#466).
    AllocationOrder,
    /// The share of the balance that must stay liquid, in basis points (#466).
    ReserveBps,
    /// Reentrancy guard held across a strategy call (#466).
    ReentrancyLock,
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
    /// The strategy has not been approved for yield deployment (issue #466).
    StrategyNotWhitelisted = 9,
    /// The strategy is already on the whitelist (issue #466).
    StrategyAlreadyWhitelisted = 10,
    /// No allocation exists for the strategy (issue #466).
    StrategyNotAllocated = 11,
    /// The strategy still holds principal or is owed yield, so it cannot be
    /// dropped from the portfolio (issue #466).
    StrategyHasFunds = 12,
    /// The weight set was empty, over-long, contained a zero weight or a
    /// duplicate, or did not sum to exactly 100% (issue #466).
    InvalidAllocations = 13,
    /// The requested recall exceeds what is deployed at the strategy
    /// (issue #466).
    RecallExceedsDeployed = 14,
    /// Nothing is deployed at the strategy to recall (issue #466).
    NothingToRecall = 15,
    /// A strategy reported returning more than the treasury actually received
    /// (issue #466).
    StrategyUnderpaid = 16,
    /// A guarded, strategy-calling entry point was re-entered (issue #466).
    ReentrancyBlocked = 17,
    /// A zero-amount deployment was requested (issue #466).
    NothingToDeploy = 18,
    /// The liquid reserve was set above 100% (issue #466).
    InvalidReserve = 19,
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
        bump_ttl(&env, &key);

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
        bump_ttl(&env, &key);

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);

        // A claim must never fail because governance parked the float in a
        // yield strategy: recall the shortfall from the portfolio first
        // (issue #466). The reserve policy is the routine buffer; this is what
        // makes the promise above hold even at a 0% reserve.
        strategies::with_lock(&env, || {
            strategies::ensure_liquidity(
                &env,
                &token_client,
                token_client.balance(&env.current_contract_address()),
                amount,
            )
        })?;

        token_client.transfer(&env.current_contract_address(), &beneficiary, &amount);

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

    // ── Diversified stablecoin yield (issue #466) ─────────────────────────

    /// Admin-only: approve `strategy` to hold treasury tokens. Nothing can be
    /// deployed to a strategy that is not whitelisted first.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`], or [`Error::StrategyAlreadyWhitelisted`].
    pub fn whitelist_strategy(env: Env, strategy: Address) -> Result<(), Error> {
        require_initialized(&env)?;
        require_admin(&env);
        strategies::whitelist(&env, strategy)?;
        extend_instance_ttl_default(&env);
        Ok(())
    }

    /// Admin-only: de-approve `strategy` and drop its allocation.
    ///
    /// Refused with [`Error::StrategyHasFunds`] while the strategy still holds
    /// principal or is owed yield — recall it first, so the treasury never
    /// loses the record of funds it can reclaim.
    pub fn revoke_strategy(env: Env, strategy: Address) -> Result<(), Error> {
        require_initialized(&env)?;
        require_admin(&env);
        strategies::revoke(&env, strategy)?;
        extend_instance_ttl_default(&env);
        Ok(())
    }

    /// Admin-only: replace the portfolio's weight set.
    ///
    /// `configs` must be non-empty, at most
    /// [`strategies::MAX_STRATEGIES`] long, free of duplicates and zero
    /// weights, and its `weight_bps` must sum to exactly
    /// [`strategies::TOTAL_WEIGHT_BPS`] (100%). Every strategy must already be
    /// whitelisted.
    /// Bookkeeping for a retained strategy is preserved; only its weight
    /// changes. A strategy dropped from the set must hold no funds.
    ///
    /// Returns the resulting allocations.
    pub fn set_allocations(
        env: Env,
        configs: soroban_sdk::Vec<AllocationConfig>,
    ) -> Result<soroban_sdk::Vec<StrategyAllocation>, Error> {
        require_initialized(&env)?;
        require_admin(&env);
        let stored = strategies::set_allocations(&env, configs)?;
        extend_instance_ttl_default(&env);
        Ok(stored)
    }

    /// Admin-only: set the share of the treasury balance that must stay liquid,
    /// in basis points. `10_000` keeps everything liquid (the default), `0`
    /// allows the whole balance to be deployed.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidReserve`] above 100%.
    pub fn set_reserve_bps(env: Env, reserve_bps: u32) -> Result<(), Error> {
        require_initialized(&env)?;
        require_admin(&env);
        strategies::set_reserve_bps(&env, reserve_bps)?;
        extend_instance_ttl_default(&env);
        Ok(())
    }

    /// Admin-only: rotate the portfolio onto the current weights.
    ///
    /// Every strategy is recalled first (bringing principal and yield home),
    /// then the treasury's balance — minus the liquid reserve — is split by
    /// weight and redeployed. Because the recall comes first, a rebalance
    /// doubles as the emergency exit: with the reserve at 100% it returns the
    /// entire portfolio to cash. Returns the total principal now deployed.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidAllocations`] if no weights are set, or any error a
    /// strategy call raises (e.g. [`Error::StrategyUnderpaid`]).
    pub fn rebalance_portfolio(env: Env) -> Result<i128, Error> {
        require_initialized(&env)?;
        require_admin(&env);
        let deployed = strategies::with_lock(&env, || strategies::rebalance(&env))?;
        extend_instance_ttl_default(&env);
        Ok(deployed)
    }

    /// Admin-only: recall `principal` from `strategy`, returning
    /// `(principal_returned, yield_returned)`. The yield stays liquid in the
    /// treasury (and is booked against the strategy) until the next rebalance.
    ///
    /// # Errors
    ///
    /// - [`Error::StrategyNotWhitelisted`] / [`Error::StrategyNotAllocated`];
    /// - [`Error::NothingToRecall`] if nothing is deployed there;
    /// - [`Error::RecallExceedsDeployed`] if `principal` is larger than the
    ///   deployed principal.
    pub fn recall_strategy(
        env: Env,
        strategy: Address,
        principal: i128,
    ) -> Result<(i128, i128), Error> {
        require_initialized(&env)?;
        require_admin(&env);
        let recalled = strategies::with_lock(&env, || {
            strategies::recall_strategy(&env, strategy, principal)
        })?;
        extend_instance_ttl_default(&env);
        Ok(recalled)
    }

    /// Read-only: every allocation, in governance-supplied order.
    pub fn get_allocations(env: Env) -> soroban_sdk::Vec<StrategyAllocation> {
        strategies::allocations(&env)
    }

    /// Read-only: one strategy's allocation, if it has one.
    pub fn get_allocation(env: Env, strategy: Address) -> Option<StrategyAllocation> {
        strategies::allocation(&env, &strategy)
    }

    /// Read-only: whether `strategy` is approved to hold treasury tokens.
    pub fn is_strategy_whitelisted(env: Env, strategy: Address) -> bool {
        strategies::is_whitelisted(&env, &strategy)
    }

    /// Read-only: the liquid-reserve policy, in basis points.
    pub fn get_reserve_bps(env: Env) -> u32 {
        strategies::reserve_bps(&env)
    }

    /// Read-only: total principal currently deployed across all strategies.
    pub fn get_deployed_total(env: Env) -> i128 {
        strategies::deployed_total(&env)
    }

    /// Read-only: total yield recalled from all strategies, cumulatively.
    pub fn get_yield_earned(env: Env) -> i128 {
        strategies::yield_earned_total(&env)
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

/// Keep a long-lived persistent entry alive using the shared TTL policy. Both
/// vesting schedules and strategy allocations are long-lived, so creation and
/// every successful state change bump the entry (see
/// [`accensa_common::storage`] for the policy values).
pub(crate) fn bump_ttl(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, DEFAULT_TTL_LOW_WATER, DEFAULT_TTL_BUMP);
}
