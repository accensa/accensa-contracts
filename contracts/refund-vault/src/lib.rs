#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, BytesN, Env,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AlreadyRefunded = 4,
    WindowExpired = 5,
    InsufficientFloat = 6,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    RefundWindow,
    Refund(BytesN<32>),
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
/// Topics: `("refunded", payment_ref)`. The data map mirrors [`RefundRecord`],
/// so indexers can decode it with the same shape stored under the payment ref.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refunded {
    #[topic]
    pub payment_ref: BytesN<32>,
    pub amount: i128,
    pub recipient: Address,
    pub ledger: u32,
}

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
        env.storage().instance().extend_ttl(100, 100000);
        Ok(())
    }

    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if from != merchant {
            return Err(Error::Unauthorized);
        }

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let vault = env.current_contract_address();
        token_client.transfer(&merchant, &vault, &amount);

        env.storage().instance().extend_ttl(100, 100000);
        Ok(())
    }

    pub fn refund(
        env: Env,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
        paid_at_ledger: u32,
    ) -> Result<(), Error> {
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

        env.storage().instance().extend_ttl(100, 100000);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Refund(payment_ref.clone()), 100, 100000);

        Refunded {
            payment_ref,
            amount: record.amount,
            recipient: record.recipient,
            ledger: record.ledger,
        }
        .publish(&env);

        Ok(())
    }

    pub fn withdraw(env: Env, amount: i128, to: Address) -> Result<(), Error> {
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

        env.storage().instance().extend_ttl(100, 100000);
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
        env.storage().instance().extend_ttl(100, 100000);
        Ok(())
    }

    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Option<RefundRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Refund(payment_ref))
    }
}

mod test;
