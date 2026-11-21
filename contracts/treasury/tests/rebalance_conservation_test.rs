//! Property-based testing of rebalance_portfolio asset conservation (issue #1023).

#![cfg(test)]

extern crate std;

use proptest::prelude::*;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{Address as _, Ledger as _},
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
    YieldAccrued,
}

#[contract]
pub struct MockYieldStrategy;

#[contractimpl]
impl MockYieldStrategy {
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
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &0i128);
    }

    pub fn simulate_yield(env: Env, amount: i128) {
        let current: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &(current + amount));
    }

    pub fn deposited(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0)
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
        let accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        let yield_portion = if total > 0 {
            accrued * principal / total
        } else {
            0
        };

        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total - principal));
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &(accrued - yield_portion));

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
            &(principal + yield_portion),
        );

        Ok((principal, yield_portion))
    }

    pub fn total_balance(env: Env) -> i128 {
        let dep: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        let yld: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        dep + yld
    }

    pub fn accrued_yield(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0)
    }
}

fn accrue(env: &Env, token: &Address, strategy: &Address, amount: i128) {
    if amount > 0 {
        StellarAssetClient::new(env, token).mint(strategy, &amount);
        MockYieldStrategyClient::new(env, strategy).simulate_yield(&amount);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn prop_rebalance_portfolio_strictly_conserves_assets(
        w1 in 1_000u32..=4_000u32,
        w2 in 1_000u32..=4_000u32,
        reserve_bps in 500u32..=3_000u32,
        yield1 in 0i128..=50_000i128,
        yield2 in 0i128..=50_000i128,
        yield3 in 0i128..=50_000i128,
    ) {
        let w3 = 10_000u32.saturating_sub(w1 + w2);
        prop_assume!(w3 >= 1_000 && w3 <= 8_000);

        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| li.timestamp = 1_700_000_000);

        let admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let treasury = env.register(Treasury, (admin.clone(), token.clone()));
        let client = TreasuryClient::new(&env, &treasury);

        let initial_supply: i128 = 1_000_000;
        StellarAssetClient::new(&env, &token).mint(&treasury, &initial_supply);

        let s1 = env.register(MockYieldStrategy, (token.clone(), treasury.clone()));
        let s2 = env.register(MockYieldStrategy, (token.clone(), treasury.clone()));
        let s3 = env.register(MockYieldStrategy, (token.clone(), treasury.clone()));

        client.whitelist_strategy(&s1);
        client.whitelist_strategy(&s2);
        client.whitelist_strategy(&s3);

        client.set_reserve_bps(&reserve_bps);

        let allocs = vec![
            &env,
            AllocationConfig {
                strategy: s1.clone(),
                weight_bps: w1,
            },
            AllocationConfig {
                strategy: s2.clone(),
                weight_bps: w2,
            },
            AllocationConfig {
                strategy: s3.clone(),
                weight_bps: w3,
            },
        ];
        client.set_allocations(&allocs);

        // First rebalance: deploys deployable funds (100% - reserve_bps)
        client.rebalance_portfolio();

        // Check conservation after initial deployment
        let bal1 = TokenClient::new(&env, &token).balance(&treasury);
        let dep1 = client.get_deployed_total();
        prop_assert_eq!(bal1 + dep1, initial_supply);

        // Emergency liquidity check: balance never falls below target reserve
        let expected_reserve = (initial_supply * reserve_bps as i128) / 10_000;
        prop_assert!(bal1 >= expected_reserve);

        // Accrue variable yields across all three strategies
        accrue(&env, &token, &s1, yield1);
        accrue(&env, &token, &s2, yield2);
        accrue(&env, &token, &s3, yield3);
        let total_yield = yield1 + yield2 + yield3;

        // Second rebalance: recalls yield + balances across new rotated weights
        let rot_w1 = w2;
        let rot_w2 = w3;
        let rot_w3 = w1;
        let rot_allocs = vec![
            &env,
            AllocationConfig {
                strategy: s1.clone(),
                weight_bps: rot_w1,
            },
            AllocationConfig {
                strategy: s2.clone(),
                weight_bps: rot_w2,
            },
            AllocationConfig {
                strategy: s3.clone(),
                weight_bps: rot_w3,
            },
        ];
        client.set_allocations(&rot_allocs);
        client.rebalance_portfolio();

        let bal2 = TokenClient::new(&env, &token).balance(&treasury);
        let dep2 = client.get_deployed_total();
        let total_assets = bal2 + dep2;

        // Asset conservation invariant:
        // Total treasury assets must equal original deposits + total accrued yield
        prop_assert_eq!(total_assets, initial_supply + total_yield);
    }
}
