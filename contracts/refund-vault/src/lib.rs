#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, BytesN, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecord {
    pub recipient: Address,
    pub amount: i128,
    pub refunded_at_ledger: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Merchant,
    Token,
    RefundWindowLedgers,
    Refund(BytesN<32>),
}

#[contract]
pub struct RefundVault;

#[contractimpl]
impl RefundVault {
    pub fn initialize(
        env: Env,
        admin: Address,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
    ) {
        assert!(!env.storage().instance().has(&DataKey::Admin), "Already initialized");
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Merchant, &merchant);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::RefundWindowLedgers, &refund_window_ledgers);
    }

    pub fn deposit(env: Env, amount: i128) {
        let merchant: Address = env.storage().instance().get(&DataKey::Merchant).expect("Not initialized");
        merchant.require_auth();

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);

        token_client.transfer(&merchant, &env.current_contract_address(), &amount);
    }

    pub fn refund(env: Env, payment_ref: BytesN<32>, recipient: Address, amount: i128, paid_at_ledger: u32) {
        let merchant: Address = env.storage().instance().get(&DataKey::Merchant).expect("Not initialized");
        merchant.require_auth();

        let refund_key = DataKey::Refund(payment_ref.clone());
        assert!(!env.storage().persistent().has(&refund_key), "Already refunded");

        let window: u32 = env.storage().instance().get(&DataKey::RefundWindowLedgers).unwrap();
        if window > 0 {
            let current_ledger = env.ledger().sequence();
            assert!(
                paid_at_ledger + window >= current_ledger,
                "Refund window expired"
            );
        }

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        let record = RefundRecord {
            recipient: recipient.clone(),
            amount,
            refunded_at_ledger: env.ledger().sequence(),
        };

        env.storage().persistent().set(&refund_key, &record);

        env.events().publish(("refund", payment_ref), (recipient, amount, paid_at_ledger));
    }

    pub fn withdraw(env: Env, amount: i128, to: Address) {
        let merchant: Address = env.storage().instance().get(&DataKey::Merchant).expect("Not initialized");
        merchant.require_auth();

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);

        token_client.transfer(&env.current_contract_address(), &to, &amount);
    }

    pub fn set_refund_window(env: Env, window: u32) {
        let merchant: Address = env.storage().instance().get(&DataKey::Merchant).expect("Not initialized");
        merchant.require_auth();
        env.storage().instance().set(&DataKey::RefundWindowLedgers, &window);
    }

    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Option<RefundRecord> {
        env.storage().persistent().get(&DataKey::Refund(payment_ref))
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }
}
