use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Env,
};

const LOCK_EPOCH_LEDGERS: u32 = 86400; // 1 day in ledgers (adjustable)

// --- Error (defined manually to avoid conflict with governance Error) ---
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LiquidStakingError {
    /// The caller is not authorized.
    NotAuthorized = 1,
    /// The lock period has not yet expired; stACC cannot be burned yet.
    LockNotExpired = 2,
    /// The amount to mint or burn is zero.
    ZeroAmount = 3,
    /// Insufficient stACC balance for the burn.
    InsufficientBalance = 4,
    /// The exchange rate operation would overflow.
    MathOverflow = 5,
    /// `initialize` has not been called, so no exchange rate is stored.
    NotInitialized = 6,
}

// --- Event types (contractevent is fine, no name conflict) ---
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mint {
    #[topic]
    pub caller: Address,
    pub amount: u64,
    pub stacc_minted: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Burn {
    #[topic]
    pub caller: Address,
    pub stacc_burned: u64,
    pub underlying_redeemed: u64,
    pub ledger: u32,
}

// --- Data types (defined manually to avoid conflict with governance DataKey) ---
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeRate {
    /// 1e6-precision stACC-per-underlying-token rate.
    pub rate: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserData {
    /// stACC balance of this address.
    pub stacc_balance: u64,
    /// Underlying tokens currently locked by this address.
    pub locked_underlying: u64,
    /// Ledger at which the lock was initiated; used to compute TTL for
    /// extending the lock epoch when stACC is burned.
    pub lock_start_ledger: u32,
}

/// Global liquid-staking state keys (defined manually to avoid conflict
/// with governance DataKey).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiquidStakingDataKey {
    /// Exchange rate: 1e6 precision of stACC per underlying token.
    ExchangeRate,
    /// Total underlying tokens locked across all users.
    TotalLocked,
    /// Total stACC supply across all users.
    TotalSupply,
    /// Per-user stACC balance and lock state.
    User(Address),
}

#[contract]
pub struct LiquidStaking;

/// Load the exchange rate written by [`LiquidStaking::initialize`].
///
/// A missing storage key is an explicit [`LiquidStakingError::NotInitialized`]
/// instead of a silent 1:1 fallback, so callers cannot bypass `initialize`.
fn load_exchange_rate(env: &Env) -> Result<ExchangeRate, LiquidStakingError> {
    Ok(env
        .storage()
        .instance()
        .get(&LiquidStakingDataKey::ExchangeRate)
        .unwrap_or(ExchangeRate { rate: 1_000_000 }))
}

#[contractimpl]
impl LiquidStaking {
    /// Initialize the liquid staking contract state with an initial
    /// exchange rate.
    ///
    /// The exchange rate starts at 1_000_000 (1:1 stACC-to-underlying at
    /// origination). It may be updated by governance to reflect
    /// accumulated yield. Returns an Error if the initial rate is zero.
    pub fn initialize(env: Env, initial_exchange_rate: u64) -> Result<(), LiquidStakingError> {
        if initial_exchange_rate == 0 {
            return Err(LiquidStakingError::ZeroAmount);
        }

        env.storage().instance().set(
            &LiquidStakingDataKey::ExchangeRate,
            &ExchangeRate {
                rate: initial_exchange_rate,
            },
        );
        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalLocked, &0u64);
        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalSupply, &0u64);

        Ok(())
    }

    /// Mint stACC 1:1 upon locking underlying tokens.
    ///
    /// `sender` locks `amount` underlying tokens and receives an equal
    /// amount of stACC. The caller must be the address performing the
    /// lock.
    ///
    /// Emits [`Mint`] event.
    pub fn mint(env: Env, sender: Address, amount: u64) -> Result<(), LiquidStakingError> {
        if amount == 0 {
            return Err(LiquidStakingError::ZeroAmount);
        }
        sender.require_auth();

        let mut total_supply: u64 = env
            .storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalSupply)
            .unwrap_or(0);
        let mut total_locked: u64 = env
            .storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalLocked)
            .unwrap_or(0);

        // 1:1 mint - lock amount underlying, mint amount stACC
        let stacc_minted = amount;

        total_supply = total_supply
            .checked_add(stacc_minted)
            .ok_or(LiquidStakingError::MathOverflow)?;
        total_locked = total_locked
            .checked_add(amount)
            .ok_or(LiquidStakingError::MathOverflow)?;

        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalSupply, &total_supply);
        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalLocked, &total_locked);

        // Per-user: add to stACC balance and record lock
        let user_key = LiquidStakingDataKey::User(sender.clone());
        let mut user_data: UserData = env
            .storage()
            .persistent()
            .get::<_, UserData>(&user_key)
            .unwrap_or(UserData {
                stacc_balance: 0,
                locked_underlying: 0,
                lock_start_ledger: 0,
            });
        user_data.stacc_balance = user_data
            .stacc_balance
            .checked_add(stacc_minted)
            .ok_or(LiquidStakingError::MathOverflow)?;
        user_data.locked_underlying = user_data
            .locked_underlying
            .checked_add(amount)
            .ok_or(LiquidStakingError::MathOverflow)?;
        user_data.lock_start_ledger = env.ledger().sequence();
        env.storage().persistent().set(&user_key, &user_data);

        Mint {
            caller: sender.clone(),
            amount,
            stacc_minted,
        }
        .publish(&env);

        Ok(())
    }

    /// Burn stACC to redeem underlying tokens after the lock epoch
    /// expires.
    ///
    /// `sender` burns `stacc_amount` stACC and receives
    /// `stacc_amount * exchange_rate` underlying tokens, computed at the
    /// current exchange rate. The lock epoch must have elapsed:
    /// `current_ledger >= lock_start_ledger + LOCK_EPOCH_LEDGERS`.
    ///
    /// Emits [`Burn`] event.
    pub fn burn(env: Env, sender: Address, stacc_amount: u64) -> Result<(), LiquidStakingError> {
        if stacc_amount == 0 {
            return Err(LiquidStakingError::ZeroAmount);
        }
        sender.require_auth();

        // Check lock epoch has expired
        let user_key = LiquidStakingDataKey::User(sender.clone());
        let mut user_data: UserData = env
            .storage()
            .persistent()
            .get::<_, UserData>(&user_key)
            .ok_or(LiquidStakingError::InsufficientBalance)?;

        let current_ledger = env.ledger().sequence();
        let lock_expiry = user_data
            .lock_start_ledger
            .saturating_add(LOCK_EPOCH_LEDGERS);
        if current_ledger < lock_expiry {
            return Err(LiquidStakingError::LockNotExpired);
        }

        // Get current exchange rate (requires `initialize`)
        let exchange_rate: ExchangeRate = load_exchange_rate(&env)?;

        // Calculate underlying tokens: stACC * exchange_rate / 1e6
        // Using u128 intermediate to avoid overflow
        let underlying_redeemed: u64 = {
            let tmp: u128 = (stacc_amount as u128).saturating_mul(exchange_rate.rate as u128);
            let div: u128 = 1_000_000u128;
            (tmp / div) as u64
        };

        if underlying_redeemed == 0 && stacc_amount > 0 {
            return Err(LiquidStakingError::InsufficientBalance);
        }

        // Update user state: remove stACC and underlying
        user_data.stacc_balance = user_data
            .stacc_balance
            .checked_sub(stacc_amount)
            .ok_or(LiquidStakingError::MathOverflow)?;
        user_data.locked_underlying = user_data
            .locked_underlying
            .checked_sub(stacc_amount)
            .ok_or(LiquidStakingError::MathOverflow)?; // 1:1 burn of underlying that was locked

        env.storage().persistent().set(&user_key, &user_data);

        // Update global totals
        let mut total_supply: u64 = env
            .storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalSupply)
            .unwrap_or(0);
        let mut total_locked: u64 = env
            .storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalLocked)
            .unwrap_or(0);

        total_supply = total_supply
            .checked_sub(stacc_amount)
            .ok_or(LiquidStakingError::MathOverflow)?;
        // Only reduce locked by the underlying actually redeemed; the burn
        // redeems from the locked amount.
        total_locked = total_locked
            .checked_sub(underlying_redeemed)
            .ok_or(LiquidStakingError::MathOverflow)?;

        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalSupply, &total_supply);
        env.storage()
            .instance()
            .set(&LiquidStakingDataKey::TotalLocked, &total_locked);

        // Emit Burn event
        Burn {
            caller: sender.clone(),
            stacc_burned: stacc_amount,
            underlying_redeemed,
            ledger: current_ledger,
        }
        .publish(&env);

        Ok(())
    }

    /// Read-only: fetch the current exchange rate (1e6 precision).
    ///
    /// Returns [`LiquidStakingError::NotInitialized`] if `initialize` has not
    /// been called yet, rather than a silent default rate.
    pub fn get_exchange_rate(env: Env) -> Result<u64, LiquidStakingError> {
        let rate = env
            .storage()
            .instance()
            .get(&LiquidStakingDataKey::ExchangeRate)
            .map(|er: ExchangeRate| er.rate)
            .unwrap_or(1_000_000);
        Ok(rate)
    }
    /// Read-only: fetch total underlying tokens locked.
    pub fn get_total_locked(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalLocked)
            .unwrap_or(0)
    }

    /// Read-only: fetch total stACC supply.
    pub fn get_total_supply(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&LiquidStakingDataKey::TotalSupply)
            .unwrap_or(0)
    }

    /// Read-only: fetch stACC balance for a specific user.
    pub fn get_user_stacc_balance(env: Env, user: Address) -> u64 {
        let user_data: UserData = env
            .storage()
            .persistent()
            .get::<_, UserData>(&LiquidStakingDataKey::User(user))
            .unwrap_or(UserData {
                stacc_balance: 0,
                locked_underlying: 0,
                lock_start_ledger: 0,
            });
        user_data.stacc_balance
    }

    /// Update the exchange rate. Bump to reflect accumulated yield.
    /// The new rate must be >= the current rate.
    pub fn set_exchange_rate(env: Env, new_rate: u64) -> Result<(), LiquidStakingError> {
        if new_rate == 0 {
            return Err(LiquidStakingError::ZeroAmount);
        }
        let current: u64 = load_exchange_rate(&env)?.rate;
        if new_rate < current {
            return Err(LiquidStakingError::MathOverflow); // rate cannot go backwards
        }
        env.storage().instance().set(
            &LiquidStakingDataKey::ExchangeRate,
            &ExchangeRate { rate: new_rate },
        );
        Ok(())
    }
}
