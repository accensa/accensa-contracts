//! Insurance pool with Compound-style interest accrual (issue #445).
//!
//! Suppliers deposit capital that earns a utilization-scaled supply rate;
//! borrowers draw against their supplied collateral at an algorithmic borrow
//! rate. Global supply/borrow indexes grow every time anyone interacts, and
//! each user's position is settled against those indexes on their next call.
//!
//! # Model
//!
//! - **Indexes.** [`BorrowIndex`](DataKey::BorrowIndex) and
//!   [`SupplyIndex`](DataKey::SupplyIndex) start at
//!   [`interest::PRECISION`] (1.0) and compound continuously via
//!   [`interest::accrue_interest`] / [`interest::accrue_supply`].
//! - **Per-user checkpoints.** [`UserInfo`] stores the indexes at the user's
//!   last interaction; accrued interest is folded into `supplied` /
//!   `borrowed` on the next deposit, withdraw, borrow or repay.
//! - **Utilization.** `total_borrowed / total_supplied`, clamped to 10 000
//!   bps, drives both rates through the algorithmic model in [`interest`].
//! - **Liquidity.** A withdraw or borrow is rejected when the requested
//!   amount exceeds `total_supplied - total_borrowed` (tokens actually
//!   available to move).
//! - **Collateral.** A borrower may not owe more than their settled
//!   `supplied` balance.
//!
//! Every state-changing interaction runs [`InsurancePool::accrue`] first so
//! indexes are current before balances are read or written.

#![no_std]

use accensa_common::storage::extend_instance_ttl_default;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, Env,
};

pub mod interest;
#[cfg(test)]
mod test;

pub use interest::{
    accrue_interest, accrue_supply, borrow_rate, supply_rate, utilization_ratio, BASE_RATE_BPS,
    MAX_UTILIZATION_BPS, PRECISION, RESERVE_FACTOR_BPS, SECONDS_PER_YEAR, SLOPE_BPS,
};

contractmeta!(key = "name", val = "InsurancePool");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Errors are local to this contract rather than added to
/// `accensa_common::Error`: every contract that exposes the shared enum
/// embeds all of its variants in its WASM spec, so growing it would enlarge
/// unrelated contracts.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The caller is not the contract admin.
    NotAdmin = 1,
    /// The pool does not hold enough liquid tokens for a withdraw or borrow.
    InsufficientLiquidity = 2,
    /// A checked fixed-point operation overflowed.
    ArithmeticOverflow = 3,
    /// An amount was non-positive, or a withdraw exceeded the user's balance.
    InvalidAmount = 4,
    /// A borrow would push the user's debt above their supplied collateral.
    ExceedsCollateral = 5,
}

#[contracttype]
pub enum DataKey {
    /// Admin address.
    Admin,
    /// SEP-41 token the pool is denominated in.
    Token,
    /// Total supplied principal, in token units, at the current supply index.
    TotalSupplied,
    /// Total borrowed principal, in token units, at the current borrow index.
    TotalBorrowed,
    /// Global borrow index, scaled by [`PRECISION`].
    BorrowIndex,
    /// Global supply index, scaled by [`PRECISION`].
    SupplyIndex,
    /// Unix timestamp of the last interest accrual.
    LastAccrual,
    /// Per-user position and index checkpoints.
    UserInfo(Address),
}

/// Per-user insurance-pool position.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserInfo {
    /// Supplied principal (plus settled supply interest), in token units.
    pub supplied: i128,
    /// Outstanding borrow (plus settled borrow interest), in token units.
    pub borrowed: i128,
    /// Supply index at the user's last interaction.
    pub supply_checkpoint: i128,
    /// Borrow index at the user's last interaction.
    pub borrow_checkpoint: i128,
}

/// Emitted when a user supplies capital.
///
/// Topics: `("deposit_event", user)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositEvent {
    #[topic]
    pub user: Address,
    pub amount: i128,
    pub total_supplied: i128,
}

/// Emitted when a user withdraws supplied capital.
///
/// Topics: `("withdraw_event", user)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawEvent {
    #[topic]
    pub user: Address,
    pub amount: i128,
    pub total_supplied: i128,
}

/// Emitted when a user borrows against collateral.
///
/// Topics: `("borrow_event", user)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BorrowEvent {
    #[topic]
    pub user: Address,
    pub amount: i128,
    pub total_borrowed: i128,
}

/// Emitted when a user repays outstanding debt.
///
/// Topics: `("repay_event", user)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepayEvent {
    #[topic]
    pub user: Address,
    pub amount: i128,
    pub total_borrowed: i128,
}

fn admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("initialized in the constructor")
}

fn token_client(env: &Env) -> token::Client<'_> {
    let token: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .expect("initialized in the constructor");
    token::Client::new(env, &token)
}

fn total_supplied(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalSupplied)
        .unwrap_or(0)
}

fn total_borrowed(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalBorrowed)
        .unwrap_or(0)
}

fn borrow_index(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::BorrowIndex)
        .unwrap_or(PRECISION)
}

fn supply_index(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::SupplyIndex)
        .unwrap_or(PRECISION)
}

fn load_user(env: &Env, user: &Address) -> UserInfo {
    env.storage()
        .persistent()
        .get(&DataKey::UserInfo(user.clone()))
        .unwrap_or_else(|| UserInfo {
            supplied: 0,
            borrowed: 0,
            supply_checkpoint: supply_index(env),
            borrow_checkpoint: borrow_index(env),
        })
}

fn save_user(env: &Env, user: &Address, info: &UserInfo) {
    let key = DataKey::UserInfo(user.clone());
    env.storage().persistent().set(&key, info);
    let extend_to = accensa_common::storage::DEFAULT_TTL_BUMP.min(env.storage().max_ttl());
    env.storage()
        .persistent()
        .extend_ttl(&key, extend_to, extend_to);
}

/// Fold accrued interest into the user's balances and re-anchor checkpoints.
fn settle_view(env: &Env, info: &mut UserInfo) -> Result<(), Error> {
    let s_idx = supply_index(env);
    let b_idx = borrow_index(env);

    if s_idx > info.supply_checkpoint {
        let accrued = info
            .supplied
            .checked_mul(s_idx - info.supply_checkpoint)
            .ok_or(Error::ArithmeticOverflow)?
            / PRECISION;
        info.supplied = info
            .supplied
            .checked_add(accrued)
            .ok_or(Error::ArithmeticOverflow)?;
    }
    info.supply_checkpoint = s_idx;

    if b_idx > info.borrow_checkpoint {
        let accrued = info
            .borrowed
            .checked_mul(b_idx - info.borrow_checkpoint)
            .ok_or(Error::ArithmeticOverflow)?
            / PRECISION;
        info.borrowed = info
            .borrowed
            .checked_add(accrued)
            .ok_or(Error::ArithmeticOverflow)?;
    }
    info.borrow_checkpoint = b_idx;
    Ok(())
}

fn settle_user(env: &Env, user: &Address) -> Result<UserInfo, Error> {
    let mut info = load_user(env, user);
    settle_view(env, &mut info)?;
    Ok(info)
}

fn mul_div(a: i128, b: i128, c: i128) -> Result<i128, Error> {
    if c == 0 {
        return Ok(a);
    }
    a.checked_mul(b)
        .ok_or(Error::ArithmeticOverflow)
        .map(|v| v / c)
}

/// Update global indexes and scale totals to the current time.
fn accrue_internal(env: &Env) -> Result<(), Error> {
    let now = env.ledger().timestamp();
    let last: u64 = env
        .storage()
        .instance()
        .get(&DataKey::LastAccrual)
        .unwrap_or(0);
    let dt = now.saturating_sub(last) as i128;

    let t_supplied = total_supplied(env);
    let t_borrowed = total_borrowed(env);
    let b_idx = borrow_index(env);
    let s_idx = supply_index(env);

    if dt > 0 && (t_supplied > 0 || t_borrowed > 0) {
        let util = utilization_ratio(t_borrowed, t_supplied);
        let br = borrow_rate(util);
        let sr = supply_rate(br, util);
        let new_b_idx = accrue_interest(b_idx, br, dt)?;
        let new_s_idx = accrue_supply(s_idx, sr, dt)?;

        let new_t_borrowed = mul_div(t_borrowed, new_b_idx, b_idx)?;
        let new_t_supplied = mul_div(t_supplied, new_s_idx, s_idx)?;

        env.storage()
            .instance()
            .set(&DataKey::BorrowIndex, &new_b_idx);
        env.storage()
            .instance()
            .set(&DataKey::SupplyIndex, &new_s_idx);
        env.storage()
            .instance()
            .set(&DataKey::TotalBorrowed, &new_t_borrowed);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupplied, &new_t_supplied);
    }

    env.storage().instance().set(&DataKey::LastAccrual, &now);
    extend_instance_ttl_default(env);
    Ok(())
}

#[contract]
pub struct InsurancePool;

#[contractimpl]
impl InsurancePool {
    /// Bind this instance to `admin` and the SEP-41 `token` the pool moves.
    pub fn __constructor(env: Env, admin: Address, token: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::BorrowIndex, &PRECISION);
        env.storage()
            .instance()
            .set(&DataKey::SupplyIndex, &PRECISION);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupplied, &0i128);
        env.storage()
            .instance()
            .set(&DataKey::TotalBorrowed, &0i128);
        env.storage()
            .instance()
            .set(&DataKey::LastAccrual, &env.ledger().timestamp());
        extend_instance_ttl_default(&env);
    }

    /// Update indexes based on elapsed time since the last accrual.
    pub fn accrue(env: Env) -> Result<(), Error> {
        accrue_internal(&env)
    }

    /// Supply `amount` of capital. Requires the user's authorization.
    ///
    /// Runs accrual first, settles the user's accrued supply interest, then
    /// credits `amount` against the new supply index.
    pub fn deposit(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        accrue_internal(&env)?;
        let mut info = settle_user(&env, &user)?;
        info.supplied = info
            .supplied
            .checked_add(amount)
            .ok_or(Error::ArithmeticOverflow)?;

        let new_total = total_supplied(&env)
            .checked_add(amount)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalSupplied, &new_total);
        save_user(&env, &user, &info);

        token_client(&env).transfer(&user, env.current_contract_address(), &amount);

        DepositEvent {
            user,
            amount,
            total_supplied: new_total,
        }
        .publish(&env);
        Ok(())
    }

    /// Withdraw `amount` of supplied capital. Requires the user's
    /// authorization.
    ///
    /// Rejects amounts above the user's settled balance (`InvalidAmount`),
    /// withdrawals that would leave outstanding debt above the remaining
    /// collateral (`ExceedsCollateral`), or amounts above the pool's liquid
    /// float (`InsufficientLiquidity`).
    pub fn withdraw(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        accrue_internal(&env)?;
        let mut info = settle_user(&env, &user)?;
        if amount > info.supplied {
            return Err(Error::InvalidAmount);
        }
        let remaining = info
            .supplied
            .checked_sub(amount)
            .ok_or(Error::InvalidAmount)?;
        if info.borrowed > remaining {
            return Err(Error::ExceedsCollateral);
        }
        let liquid = total_supplied(&env).saturating_sub(total_borrowed(&env));
        if amount > liquid {
            return Err(Error::InsufficientLiquidity);
        }

        info.supplied = info
            .supplied
            .checked_sub(amount)
            .ok_or(Error::ArithmeticOverflow)?;
        let new_total = total_supplied(&env)
            .checked_sub(amount)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalSupplied, &new_total);
        save_user(&env, &user, &info);

        token_client(&env).transfer(&env.current_contract_address(), &user, &amount);

        WithdrawEvent {
            user,
            amount,
            total_supplied: new_total,
        }
        .publish(&env);
        Ok(())
    }

    /// Borrow `amount` against the user's supplied collateral. Requires the
    /// user's authorization.
    ///
    /// Rejects debt that would exceed the user's settled `supplied` balance
    /// (`ExceedsCollateral`) or a draw above the liquid float
    /// (`InsufficientLiquidity`).
    pub fn borrow(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        accrue_internal(&env)?;
        let mut info = settle_user(&env, &user)?;
        let new_borrowed = info
            .borrowed
            .checked_add(amount)
            .ok_or(Error::ArithmeticOverflow)?;
        if new_borrowed > info.supplied {
            return Err(Error::ExceedsCollateral);
        }
        let liquid = total_supplied(&env).saturating_sub(total_borrowed(&env));
        if amount > liquid {
            return Err(Error::InsufficientLiquidity);
        }

        info.borrowed = new_borrowed;
        let new_total_borrowed = total_borrowed(&env)
            .checked_add(amount)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalBorrowed, &new_total_borrowed);
        save_user(&env, &user, &info);

        token_client(&env).transfer(&env.current_contract_address(), &user, &amount);

        BorrowEvent {
            user,
            amount,
            total_borrowed: new_total_borrowed,
        }
        .publish(&env);
        Ok(())
    }

    /// Repay up to `amount` of outstanding debt. Requires the user's
    /// authorization. Repays only what is owed if `amount` exceeds the
    /// balance.
    pub fn repay(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        accrue_internal(&env)?;
        let mut info = settle_user(&env, &user)?;
        if info.borrowed <= 0 {
            return Err(Error::InvalidAmount);
        }
        let actual = amount.min(info.borrowed);

        info.borrowed = info
            .borrowed
            .checked_sub(actual)
            .ok_or(Error::ArithmeticOverflow)?;
        let new_total_borrowed = total_borrowed(&env)
            .checked_sub(actual)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalBorrowed, &new_total_borrowed);
        save_user(&env, &user, &info);

        token_client(&env).transfer(&user, env.current_contract_address(), &actual);

        RepayEvent {
            user,
            amount: actual,
            total_borrowed: new_total_borrowed,
        }
        .publish(&env);
        Ok(())
    }

    /// Current utilization in basis points (0–10000).
    pub fn get_utilization(env: Env) -> i128 {
        utilization_ratio(total_borrowed(&env), total_supplied(&env))
    }

    /// Current borrow index (stored value; runs on every prior interaction).
    pub fn get_borrow_index(env: Env) -> i128 {
        borrow_index(&env)
    }

    /// Current supply index (stored value; runs on every prior interaction).
    pub fn get_supply_index(env: Env) -> i128 {
        supply_index(&env)
    }

    /// The user's position with accrued interest folded in (read-only
    /// view). Unknown users report zeros against the current indexes.
    pub fn get_user_info(env: Env, user: Address) -> UserInfo {
        let mut info = load_user(&env, &user);
        if settle_view(&env, &mut info).is_err() {
            return load_user(&env, &user);
        }
        info
    }

    /// Read-only: the pool admin.
    pub fn get_admin(env: Env) -> Address {
        admin(&env)
    }

    /// Read-only: the SEP-41 token the pool moves.
    pub fn get_token(env: Env) -> Address {
        token_client(&env).address
    }
}
