//! Integration tests for fresh treasury accounting defaults and zero-yield recall (issue #1016).

#![cfg(test)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env,
};
use treasury::strategies::AllocationConfig;
use treasury::{Treasury, TreasuryClient};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StrategyError {
    Unauthorized = 1,
    InsufficientBalance = 2,
    NothingToWithdraw = 3,
}

#[contracttype]
#[derive(Clone)]
pub enum StrategyDataKey {
    Token,
    Treasury,
    TotalDeposited,
}

#[contract]
pub struct MockZeroYieldStrategy;

#[contractimpl]
impl MockZeroYieldStrategy {
    pub fn __constructor(env: Env, token: Address, treasury: Address) {
        env.storage()
            .instance()
            .set(&StrategyDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&StrategyDataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &0i128);
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::InsufficientBalance);
        }
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total + amount));
        Ok(())
    }

    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), StrategyError> {
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        if principal <= 0 || principal > total {
            return Err(StrategyError::NothingToWithdraw);
        }
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total - principal));

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let treasury: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Treasury)
            .unwrap();

        let client = TokenClient::new(&env, &token_addr);
        client.transfer(&env.current_contract_address(), &treasury, &principal);

        Ok((principal, 0i128))
    }
}

#[test]
fn fresh_treasury_accounting_defaults_are_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    // Initialized contract with empty allocations has 0 deployed and 0 yield earned
    let deployed: i128 = client.get_deployed_total();
    let yield_earned: i128 = client.get_yield_earned();

    assert_eq!(deployed, 0);
    assert_eq!(yield_earned, 0);
    assert_eq!(client.get_allocations().len(), 0);
}

#[test]
fn accounting_remains_zero_after_zero_yield_allocation_and_recall() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    let initial_balance: i128 = 1_000_000;
    StellarAssetClient::new(&env, &token).mint(&treasury, &initial_balance);

    let strategy = env.register(MockZeroYieldStrategy, (token.clone(), treasury.clone()));
    client.whitelist_strategy(&strategy);

    // Configure 20% reserve, 80% deployable to the single strategy
    client.set_reserve_bps(&2_000);
    let alloc = AllocationConfig {
        strategy: strategy.clone(),
        weight_bps: 10_000,
    };
    client.set_allocations(&vec![&env, alloc]);

    client.rebalance_portfolio();

    let deployed_after_rebalance = client.get_deployed_total();
    assert_eq!(deployed_after_rebalance, 800_000);
    assert_eq!(client.get_yield_earned(), 0);

    // Recall full deployed principal yielding zero interest
    let (recalled_principal, recalled_yield) =
        client.recall_strategy(&strategy, &deployed_after_rebalance);
    assert_eq!(recalled_principal, 800_000);
    assert_eq!(recalled_yield, 0);

    // Deployed total is back to 0, yield earned remains 0
    assert_eq!(client.get_deployed_total(), 0);
    assert_eq!(client.get_yield_earned(), 0);
}
