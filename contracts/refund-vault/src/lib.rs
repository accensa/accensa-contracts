#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contractmeta,
    contracttype, token, Address, BytesN, Env,
};

contractmeta!(key = "name", val = "RefundVault");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AlreadyRefunded = 4,
    WindowExpired = 5,
    InsufficientFloat = 6,
    InvalidAmount = 7,
    Paused = 8,
    RefundNotFound = 9,
    MetadataTooLong = 10,
    AmountExceedsMax = 11,
    NoPendingTransfer = 12,
    StrategyNotSet = 13,
    InsufficientReserve = 14,
    DeploymentExceedsMax = 15,
    NothingToWithdraw = 16,
    NothingToHarvest = 17,
    InvalidRatio = 18,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    RefundWindow,
    Refund(BytesN<32>),
    IsPaused,
    Metadata,
    RefundMax,
    Admins,
    Threshold,
    PendingAdmin,
    YieldStrategy,
    DeployedPrincipal,
    HarvestedYield,
    ReserveRatio,
    MaxDeployRatio,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecord {
    pub amount: i128,
    pub recipient: Address,
    pub ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldInfo {
    pub deployed_principal: i128,
    pub harvested_yield: i128,
    pub strategy: Option<Address>,
    pub reserve_ratio: u32,
    pub max_deploy_ratio: u32,
}

/// Emitted when a payment is refunded from the vault float.
///
/// Topics: `("refund_event", payment_ref)`. The data map mirrors [`RefundRecord`],
/// so indexers can decode it with the same shape stored under the payment ref.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundEvent {
    #[topic]
    pub payment_ref: BytesN<32>,
    pub amount: i128,
    pub recipient: Address,
    pub ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositEvent {
    #[topic]
    pub from: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawEvent {
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferInitiatedEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferAcceptedEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldDeployedEvent {
    #[topic]
    pub strategy: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldWithdrawnEvent {
    #[topic]
    pub strategy: Address,
    pub principal: i128,
    pub yield_amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldHarvestedEvent {
    pub amount: i128,
}

/// Interface for external yield-generating strategies (e.g., Soroban lending protocols).
///
/// Any contract that implements these methods can be registered as the vault's yield
/// strategy. The vault calls these to deploy idle funds and harvest accrued yield.
#[contractclient(name = "YieldStrategyClient")]
pub trait YieldStrategy {
    /// Deploy `amount` tokens into the strategy. The vault transfers tokens to the
    /// strategy contract before calling this.
    fn deposit(env: Env, amount: i128) -> Result<(), Error>;

    /// Withdraw `principal` worth of tokens plus any proportional accrued yield.
    /// Returns `(principal_returned, yield_returned)`. The strategy transfers tokens
    /// back to the vault before returning.
    fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error>;

    /// Harvest all accrued yield without touching deployed principal.
    /// Returns the yield amount. The strategy transfers yield tokens to the vault.
    fn harvest(env: Env) -> Result<i128, Error>;

    /// Read-only: total tokens held by this strategy (principal + accrued yield).
    fn total_balance(env: Env) -> i128;

    /// Read-only: accrued yield only (total_balance - total principal deployed).
    fn accrued_yield(env: Env) -> i128;
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
/// This ensures refund records survive long-term audit use before requiring a TTL bump or restoration.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;

#[contract]
pub struct RefundVault;

#[contractimpl]
impl RefundVault {
    pub fn initialize(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &merchant);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &refund_window_ledgers);
        env.storage().instance().set(&DataKey::IsPaused, &false);
        env.storage().instance().set(&DataKey::RefundMax, &0i128);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        if Self::is_paused(env.clone()) {
            return Err(Error::Paused);
        }
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        if from != merchant {
            return Err(Error::Unauthorized);
        }
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::TokenClient::new(&env, &token_address);
        token_client.transfer(&merchant, &env.current_contract_address(), &amount);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        DepositEvent { from, amount }.publish(&env);

        Ok(())
    }

    pub fn refund(
        env: Env,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
        paid_at_ledger: u32,
    ) -> Result<(), Error> {
        if Self::is_paused(env.clone()) {
            return Err(Error::Paused);
        }
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let max_refund: i128 = env
            .storage()
            .instance()
            .get(&DataKey::RefundMax)
            .unwrap_or(0);
        if max_refund > 0 && amount > max_refund {
            return Err(Error::AmountExceedsMax);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::AlreadyRefunded);
        }

        let refund_window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .ok_or(Error::NotInitialized)?;

        if refund_window > 0 {
            let current_ledger = env.ledger().sequence();
            if current_ledger > paid_at_ledger.saturating_add(refund_window) {
                return Err(Error::WindowExpired);
            }
        }

        let token_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::TokenClient::new(&env, &token_address);
        let vault_balance = token_client.balance(&env.current_contract_address());

        // Account for deployed yield if a strategy is active
        let available_float = if let Some(strategy) = Self::get_yield_strategy(env.clone()) {
            let strategy_client = YieldStrategyClient::new(&env, &strategy);
            let strategy_balance = strategy_client.total_balance();
            vault_balance.saturating_add(strategy_balance)
        } else {
            vault_balance
        };

        if amount > available_float {
            return Err(Error::InsufficientFloat);
        }

        // If vault_balance is insufficient due to deployment, withdraw from strategy first
        if amount > vault_balance {
            let needed = amount - vault_balance;
            Self::withdraw_from_yield_internal(env.clone(), needed)?;
        }

        let record = RefundRecord {
            amount,
            recipient: recipient.clone(),
            ledger: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Refund(payment_ref.clone()), &record);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Refund(payment_ref.clone()), TTL_THRESHOLD, TTL_EXTEND);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        RefundEvent {
            payment_ref,
            amount,
            recipient,
            ledger: record.ledger,
        }
        .publish(&env);

        Ok(())
    }

    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Result<RefundRecord, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Refund(payment_ref))
            .ok_or(Error::RefundNotFound)
    }

    pub fn extend_refund_ttl(env: Env, payment_ref: BytesN<32>) -> Result<(), Error> {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::RefundNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Refund(payment_ref), TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn withdraw(env: Env, amount: i128, recipient: Address) -> Result<(), Error> {
        if Self::is_paused(env.clone()) {
            return Err(Error::Paused);
        }
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::TokenClient::new(&env, &token_address);
        let vault_balance = token_client.balance(&env.current_contract_address());

        if amount > vault_balance {
            let needed = amount - vault_balance;
            Self::withdraw_from_yield_internal(env.clone(), needed)?;
        }

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        WithdrawEvent { to: recipient, amount }.publish(&env);

        Ok(())
    }

    pub fn pause(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        env.storage().instance().set(&DataKey::IsPaused, &true);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        env.storage().instance().set(&DataKey::IsPaused, &false);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
    }

    pub fn set_refund_window(env: Env, refund_window_ledgers: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &refund_window_ledgers);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn get_refund_window(env: Env) -> Result<u32, Error> {
        env.storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .ok_or(Error::NotInitialized)
    }

    pub fn set_refund_max(env: Env, max_amount: i128) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        if max_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::RefundMax, &max_amount);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn get_refund_max(env: Env) -> Result<i128, Error> {
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::RefundMax)
            .unwrap_or(0))
    }

    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        AdminTransferInitiatedEvent {
            from: merchant,
            to: new_admin,
        }
        .publish(&env);

        Ok(())
    }

    pub fn accept_admin(env: Env) -> Result<(), Error> {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingTransfer)?;
        pending.require_auth();

        let old_merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        env.storage()
            .instance()
            .set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        AdminTransferAcceptedEvent {
            from: old_merchant,
            to: pending,
        }
        .publish(&env);

        Ok(())
    }

    pub fn cancel_admin_transfer(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if !env.storage().instance().has(&DataKey::PendingAdmin) {
            return Err(Error::NoPendingTransfer);
        }

        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn set_yield_strategy(
        env: Env,
        strategy: Address,
        reserve_ratio: u32,
        max_deploy_ratio: u32,
    ) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if reserve_ratio > 100 || max_deploy_ratio > 100 || reserve_ratio + max_deploy_ratio > 100 {
            return Err(Error::InvalidRatio);
        }

        let info = YieldInfo {
            deployed_principal: 0,
            harvested_yield: 0,
            strategy: Some(strategy),
            reserve_ratio,
            max_deploy_ratio,
        };

        env.storage().instance().set(&DataKey::YieldStrategy, &info);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn get_yield_info(env: Env) -> Result<YieldInfo, Error> {
        env.storage()
            .instance()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)
    }

    fn get_yield_strategy(env: Env) -> Option<Address> {
        env.storage()
            .instance()
            .get::<_, YieldInfo>(&DataKey::YieldStrategy)
            .and_then(|info| info.strategy)
    }

    pub fn deploy_to_yield(env: Env, amount: i128) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let mut info = Self::get_yield_info(env.clone())?;
        let strategy = info.strategy.ok_or(Error::StrategyNotSet)?;

        let token_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;
        let token_client = token::TokenClient::new(&env, &token_address);
        let vault_balance = token_client.balance(&env.current_contract_address());

        if amount > vault_balance {
            return Err(Error::InsufficientFloat);
        }

        let reserve_required = vault_balance * (info.reserve_ratio as i128) / 100;
        let max_deployable = vault_balance - reserve_required;
        if amount > max_deployable {
            return Err(Error::DeploymentExceedsMax);
        }

        token_client.transfer(&env.current_contract_address(), &strategy, &amount);

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        strategy_client.deposit(&amount)?;

        info.deployed_principal = info.deployed_principal.saturating_add(amount);
        env.storage().instance().set(&DataKey::YieldStrategy, &info);

        YieldDeployedEvent { strategy, amount }.publish(&env);

        Ok(())
    }

    fn withdraw_from_yield_internal(env: Env, amount: i128) -> Result<(), Error> {
        let mut info = Self::get_yield_info(env.clone())?;
        let strategy = info.strategy.ok_or(Error::StrategyNotSet)?;

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let (principal_returned, yield_returned) = strategy_client.withdraw(&amount)?;

        info.deployed_principal = info
            .deployed_principal
            .saturating_sub(principal_returned);
        info.harvested_yield = info.harvested_yield.saturating_add(yield_returned);
        env.storage().instance().set(&DataKey::YieldStrategy, &info);

        YieldWithdrawnEvent {
            strategy,
            principal: principal_returned,
            yield_amount: yield_returned,
        }
        .publish(&env);

        Ok(())
    }

    pub fn harvest_yield(env: Env) -> Result<i128, Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let mut info = Self::get_yield_info(env.clone())?;
        let strategy = info.strategy.ok_or(Error::StrategyNotSet)?;

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let harvested = strategy_client.harvest()?;

        info.harvested_yield = info.harvested_yield.saturating_add(harvested);
        env.storage().instance().set(&DataKey::YieldStrategy, &info);

        YieldHarvestedEvent { amount: harvested }.publish(&env);

        Ok(harvested)
    }
}
