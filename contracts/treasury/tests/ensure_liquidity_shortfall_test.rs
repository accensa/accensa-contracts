//! Integration tests asserting ensure_liquidity error reporting on liquidity shortfalls (issue #1017).

#![cfg(test)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env,
};
use treasury::strategies::AllocationConfig;
use treasury::{Error, Treasury, TreasuryClient};

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
    PartialPercent,
}

#[contract]
pub struct MockPartialDeliveryStrategy;

#[contractimpl]
impl MockPartialDeliveryStrategy {
    pub fn __constructor(env: Env, token: Address, treasury: Address, partial_percent: u32) {
        env.storage()
            .instance()
            .set(&StrategyDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&StrategyDataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &0i128);
        env.storage()
            .instance()
            .set(&StrategyDataKey::PartialPercent, &partial_percent);
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

        let percent: u32 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::PartialPercent)
            .unwrap_or(100);

        // Deliver only `percent`% of the requested principal
        let delivered_principal = (principal * percent as i128) / 100;

        env.storage().instance().set(
            &StrategyDataKey::TotalDeposited,
            &(total - delivered_principal),
        );

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
        client.transfer(
            &env.current_contract_address(),
            &treasury,
            &delivered_principal,
        );

        Ok((delivered_principal, 0i128))
    }
}

#[test]
fn ensure_liquidity_returns_insufficient_balance_on_shortfall() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_000);

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    let initial_balance: i128 = 100_000;
    StellarAssetClient::new(&env, &token).mint(&treasury, &initial_balance);

    // Strategy returns only 50% of requested principal on withdrawal
    let strategy = env.register(
        MockPartialDeliveryStrategy,
        (token.clone(), treasury.clone(), 50u32),
    );
    client.whitelist_strategy(&strategy);

    // 0% reserve, all 100,000 tokens deployed into strategy
    client.set_reserve_bps(&0);
    let alloc = AllocationConfig {
        strategy: strategy.clone(),
        weight_bps: 10_000,
    };
    client.set_allocations(&vec![&env, alloc]);
    client.rebalance_portfolio();

    // Verify treasury liquid balance is 0 and 100,000 deployed
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(client.get_deployed_total(), 100_000);

    // Create schedule for beneficiary of 100,000 tokens fully vested immediately
    client.add_schedule(&beneficiary, &100_000, &1_000, &0, &100);
    env.ledger().with_mut(|li| li.timestamp = 1_200);

    // Claim attempts to pull 100,000 tokens through ensure_liquidity.
    // Strategy delivers only 50,000, resulting in shortfall.
    // ensure_liquidity must return Error::InsufficientBalance.
    let result = client.try_claim_vested(&beneficiary);
    assert_eq!(result, Err(Ok(Error::InsufficientBalance)));

    // On revert, state changes roll back and treasury liquid balance remains 0
    assert_eq!(token_client.balance(&treasury), 0);
}

#[test]
fn ensure_liquidity_succeeds_when_liquidity_is_fully_met() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_000);

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin.clone(), token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    let initial_balance: i128 = 100_000;
    StellarAssetClient::new(&env, &token).mint(&treasury, &initial_balance);

    // Strategy returns 100% of requested principal on withdrawal
    let strategy = env.register(
        MockPartialDeliveryStrategy,
        (token.clone(), treasury.clone(), 100u32),
    );
    client.whitelist_strategy(&strategy);

    client.set_reserve_bps(&0);
    let alloc = AllocationConfig {
        strategy: strategy.clone(),
        weight_bps: 10_000,
    };
    client.set_allocations(&vec![&env, alloc]);
    client.rebalance_portfolio();

    client.add_schedule(&beneficiary, &100_000, &1_000, &0, &100);
    env.ledger().with_mut(|li| li.timestamp = 1_200);

    // Full delivery satisfies liquidity requirement
    let claimed = client.claim_vested(&beneficiary);
    assert_eq!(claimed, 100_000);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&beneficiary), 100_000);
}
