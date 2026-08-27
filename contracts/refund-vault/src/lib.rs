#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, BytesN, Env,
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
    InvalidRecipient = 12,
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
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecord {
    pub amount: i128,
    pub recipient: Address,
    pub ledger: u32,
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

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
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

        let token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let client = token::Client::new(&env, &token);
        client.transfer(&from, env.current_contract_address(), &amount);

        DepositEvent {
            from: from.clone(),
            amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn refund(
        env: Env,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
        paid_at_ledger: u32,
    ) -> Result<(), Error> {
        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if recipient == env.current_contract_address() {
            return Err(Error::InvalidRecipient);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::AlreadyRefunded);
        }

        let window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap();
        if window > 0 {
            let current_ledger = env.ledger().sequence();
            if current_ledger > paid_at_ledger + window {
                return Err(Error::WindowExpired);
            }
        }

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < amount {
            return Err(Error::InsufficientFloat);
        }

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        let record = RefundRecord {
            amount,
            recipient: recipient.clone(),
            ledger: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Refund(payment_ref.clone()), &record);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        env.storage().persistent().extend_ttl(
            &DataKey::Refund(payment_ref.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND,
        );

        RefundEvent {
            payment_ref,
            amount: record.amount,
            recipient: record.recipient,
            ledger: record.ledger,
        }
        .publish(&env);

        Ok(())
    }

    pub fn withdraw(env: Env, amount: i128, to: Address) -> Result<(), Error> {
        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < amount {
            return Err(Error::InsufficientFloat);
        }

        token_client.transfer(&env.current_contract_address(), &to, &amount);

        WithdrawEvent {
            to: to.clone(),
            amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn set_refund_window(env: Env, ledgers: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &ledgers);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Option<RefundRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Refund(payment_ref))
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

    pub fn extend_refund_ttl(env: Env, payment_ref: BytesN<32>) -> Result<(), Error> {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::RefundNotFound);
        }
        env.storage().persistent().extend_ttl(
            &DataKey::Refund(payment_ref),
            TTL_THRESHOLD,
            TTL_EXTEND,
        );
        Ok(())
    }

    pub fn set_token(env: Env, new_token: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage().instance().set(&DataKey::Token, &new_token);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }
}

mod fuzz_test;
mod test;
