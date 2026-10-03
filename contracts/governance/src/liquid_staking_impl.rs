#![no_internal_source]

use soroban_sdk {
    contracterror, contractevent, contracttype, Env, Address, Symbol, Val, Vec,
};

use soroban_sdk::address::Address;

const LOCK_EPOCH_LEDGERS: u32 = 86400; // 1 day in ledgers (adjustable)

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
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
}

/// The current exchange rate: `u64` represents 1e6 precision of stACC per
/// underlying token. An exchange rate of 1_000_000 means 1 stACC = 1.0 underlying
/// token. A rate > 1_000_000 means stACC has accrued value (yield).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeRate(pub u64);

/// Emitted when stACC is minted upon locking underlying tokens.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mint {
    #[topic]
    pub caller: Address,
    pub amount: u64,
    pub stacc_minted: u64,
}

/// Emitted when stACC is burned and underlying tokens are redeemed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Burn {
    #[topic]
    pub caller: Address,
    pub stacc_burned: u64,
    pub underlying_redeemed: u64,
    pub ledger: u32,
}

/// Per-address stACC balance and locked amount tracking.
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

/// Global liquid-staking state.
#[contracttype]
pub enum DataKey {
    /// Exchange rate: 1e6 precision of stACC per underlying token.
    ExchangeRate,
    /// Total underlying tokens locked across all users.
    TotalLocked,
    /// Total stACC supply across all users.
    TotalSupply,
    /// Per-user stACC balance and lock state.
    User(Address),
}

impl Env {
    fn require_auth_from_auth(&self) {}
}

/// Initialize the liquid staking contract with an initial exchange rate.
///
/// The exchange rate starts at 1_000_000 (1:1 stACC-to-underlying at
/// origination). It may be updated by governance to reflect accumulated
/// yield.
pub fn __initialize(env: Env, initial_exchange_rate: u64) -> Result<(), Error> {
    if initial_exchange_rate == 0 {
        return Err(Error::ZeroAmount);
    }

    env.storage()
        .instance()
        .set(&DataKey::ExchangeRate, &ExchangeRate(initial_exchange_rate));
    env.storage()
        .instance()
        .set(&DataKey::TotalLocked, &0u64);
    env.storage()
        .instance()
        .set(&DataKey::TotalSupply, &0u64);

    Ok(())
}

/// Mint stACC 1:1 upon locking underlying tokens.
///
/// `sender` locks `amount` underlying tokens and receives an equal amount
/// of stACC. The caller must be the address performing the lock.
///
/// Emits [`Mint`] event.
pub fn mint(env: Env, sender: Address, amount: u64) -> Result<(), Error> {
    if amount == 0 {
        return Err(Error::ZeroAmount);
    }
    sender.require_auth();

    let mut total_supply: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalSupply)
        .unwrap_or(0);
    let mut total_locked: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalLocked)
        .unwrap_or(0);

    // 1:1 mint - lock amount underlying, mint amount stACC
    let stacc_minted = amount;

    total_supply = total_supply
        .checked_add(stacc_minted)
        .ok_or(Error::MathOverflow)?;
    total_locked = total_locked
        .checked_add(amount)
        .ok_or(Error::MathOverflow)?;

    env.storage()
        .instance()
        .set(&DataKey::TotalSupply, &total_supply);
    env.storage()
        .instance()
        .set(&DataKey::TotalLocked, &total_locked);

    // Per-user: add to stACC balance and record lock
    let user_key = DataKey::User(sender.clone());
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
        .ok_or(Error::MathOverflow)?;
    user_data.locked_underlying = user_data
        .locked_underlying
        .checked_add(amount)
        .ok_or(Error::MathOverflow)?;
    user_data.lock_start_ledger = env.ledger().sequence();
    env.storage()
        .persistent()
        .set(&user_key, &user_data);

    Mint {
        caller: sender.clone(),
        amount,
        stacc_minted,
    }
    .publish(&env);

    Ok(())
}

/// Burn stACC to redeem underlying tokens after the lock epoch expires.
///
/// `sender` burns `stacc_amount` stACC and receives
/// `stacc_amount * exchange_rate` underlying tokens, computed at the
/// current exchange rate. The lock epoch must have elapsed:
/// `current_ledger >= lock_start_ledger + LOCK_EPOCH_LEDGERS`.
///
/// Emits [`Burn`] event.
pub fn burn(env: Env, sender: Address, stacc_amount: u64) -> Result<(), Error> {
    if stacc_amount == 0 {
        return Err(Error::ZeroAmount);
    }
    sender.require_auth();

    // Check lock epoch has expired
    let user_key = DataKey::User(sender.clone());
    let mut user_data: UserData = env
        .storage()
        .persistent()
        .get::<_, UserData>(&user_key)
        .ok_or(Error::InsufficientBalance)?;

    let current_ledger = env.ledger().sequence();
    let lock_expiry = user_data
        .lock_start_ledger
        .saturating_add(LOCK_EPOCH_LEDGERS);
    if current_ledger < lock_expiry {
        return Err(Error::LockNotExpired);
    }

    // Get current exchange rate
    let exchange_rate: ExchangeRate = env
        .storage()
        .instance()
        .get(&DataKey::ExchangeRate)
        .unwrap_or(ExchangeRate(1_000_000));

    // Calculate underlying tokens: stACC * exchange_rate / 1e6
    // Using u128 intermediate to avoid overflow
    let underlying_redeemed: u64 = {
        let tmp: u128 = (stacc_amount as u128)
            .saturating_mul(exchange_rate.0 as u128);
        let div: u128 = 1_000_000u128;
        (tmp / div) as u64
    };

    if underlying_redeemed == 0 && stacc_amount > 0 {
        return Err(Error::InsufficientBalance);
    }

    // Update user state: remove stACC and underlying
    user_data.stacc_balance = user_data
        .stacc_balance
        .checked_sub(stacc_amount)
        .ok_or(Error::MathOverflow)?;
    user_data.locked_underlying = user_data
        .locked_underlying
        .checked_sub(stacc_amount)
        .ok_or(Error::MathOverflow)?; // 1:1 burn of underlying that was locked

    env.storage()
        .persistent()
        .set(&user_key, &user_data);

    // Update global totals
    let mut total_supply: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalSupply)
        .unwrap_or(0);
    let mut total_locked: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TotalLocked)
        .unwrap_or(0);

    total_supply = total_supply
        .checked_sub(stacc_amount)
        .ok_or(Error::MathOverflow)?;
    // Only reduce locked if this user had locked underlying; the burn
    // redeems from the locked amount.
    total_locked = total_locked
        .checked_sub(stacc_amount)
        .ok_or(Error::MathOverflow)?;

    env.storage()
        .instance()
        .set(&DataKey::TotalSupply, &total_supply);
    env.storage()
        .instance()
        .set(&DataKey::TotalLocked, &total_locked);

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