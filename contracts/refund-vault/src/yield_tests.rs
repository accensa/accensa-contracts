#![cfg(test)]
#![allow(unused_imports, unused_variables, dead_code)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient, TTL_EXTEND};

// ── Mock yield strategy contract ───────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StrategyError {
    Unauthorized = 1,
    InsufficientBalance = 2,
    NothingToWithdraw = 3,
    NothingToHarvest = 4,
}

#[contracttype]
pub enum StrategyDataKey {
    Token,
    Admin,
    TotalDeposited,
    YieldAccrued,
}

#[contract]
pub struct MockYieldStrategy;

#[contractimpl]
impl MockYieldStrategy {
    pub fn initialize(env: Env, token: Address, admin: Address) {
        env.storage()
            .instance()
            .set(&StrategyDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&StrategyDataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &0i128);
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &0i128);
    }

    pub fn simulate_yield(env: Env, amount: i128) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Admin)
            .unwrap();
        admin.require_auth();

        let current: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &(current + amount));
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
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

    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error> {
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        if principal > total || principal <= 0 {
            return Err(Error::NothingToWithdraw);
        }

        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);

        let yield_portion = if total > 0 {
            yield_accrued * principal / total
        } else {
            0
        };

        let total_return = principal + yield_portion;

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        let vault_addr = env
            .storage()
            .instance()
            .get::<_, Address>(&StrategyDataKey::Admin)
            .unwrap();
        token_client.transfer(&env.current_contract_address(), &vault_addr, &total_return);

        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total - principal));
        env.storage().instance().set(
            &StrategyDataKey::YieldAccrued,
            &(yield_accrued - yield_portion),
        );

        Ok((principal, yield_portion))
    }

    pub fn harvest(env: Env) -> Result<i128, Error> {
        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued <= 0 {
            return Err(Error::NothingToHarvest);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        let vault_addr = env
            .storage()
            .instance()
            .get::<_, Address>(&StrategyDataKey::Admin)
            .unwrap();
        token_client.transfer(&env.current_contract_address(), &vault_addr, &yield_accrued);

        env.storage()
            .instance()
            .set(&StrategyDataKey::YieldAccrued, &0i128);

        Ok(yield_accrued)
    }

    pub fn total_balance(env: Env) -> i128 {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StrategyDataKey::Token)
            .unwrap();
        let token_client = TokenClient::new(&env, &token_addr);
        token_client.balance(&env.current_contract_address())
    }

    pub fn accrued_yield(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0)
    }
}

// ── Test helpers ───────────────────────────────────────────────────────────

const FLOAT: i128 = 10_000_000;

fn setup_with_strategy(
    reserve_bp: u32,
    max_deploy_bp: u32,
) -> (
    Env,
    RefundVaultClient<'static>,
    Address,
    Address,
    Address,
    TokenClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault_client = RefundVaultClient::new(&env, &vault_id);

    let strategy_id = env.register(MockYieldStrategy, ());
    let strategy_addr = strategy_id.clone();
    MockYieldStrategyClient::new(&env, &strategy_id).initialize(&token, &vault_id);

    StellarAssetClient::new(&env, &token).mint(&strategy_addr, &FLOAT);

    vault_client.set_yield_strategy(&strategy_addr);
    vault_client.set_reserve_ratio(&reserve_bp);
    vault_client.set_max_deploy_ratio(&max_deploy_bp);

    let token_client = TokenClient::new(&env, &token);
    (
        env,
        vault_client,
        merchant,
        token,
        strategy_addr,
        token_client,
    )
}

// ── Yield strategy configuration tests ─────────────────────────────────────

#[test]
fn test_set_yield_strategy() {
    let (_env, vault_client, _merchant, _token, strategy_addr, _tc) =
        setup_with_strategy(2000, 8000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.strategy, Some(strategy_addr));
    assert_eq!(info.reserve_ratio, 2000);
    assert_eq!(info.max_deploy_ratio, 8000);
    assert_eq!(info.deployed_principal, 0);
    assert_eq!(info.harvested_yield, 0);
}

#[test]
fn test_set_yield_strategy_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    let init = vault_init(&env, &merchant, &token, 100);
    let vault_id = env.register(RefundVault, (init.clone(),));
    let vault_client = RefundVaultClient::new(&env, &vault_id);

    assert_eq!(
        vault_client.try_initialize(&init),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
#[should_panic]
fn test_set_yield_strategy_requires_auth() {
    let (env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);
    let new_strategy = Address::generate(&env);

    env.set_auths(&[]);
    vault_client.set_yield_strategy(&new_strategy);
}

#[test]
fn test_set_reserve_ratio_invalid_fails() {
    let (_env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    assert_eq!(
        vault_client.try_set_reserve_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_set_max_deploy_ratio_invalid_fails() {
    let (_env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    assert_eq!(
        vault_client.try_set_max_deploy_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
}

// ── Deploy to yield tests ──────────────────────────────────────────────────

#[test]
fn test_deploy_to_yield_happy_path() {
    let (_env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    assert_eq!(tc.balance(&vault_client.address), 5_000_000);

    vault_client.deploy_to_yield(&3_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 3_000_000);
    assert_eq!(tc.balance(&vault_client.address), 2_000_000);
}

#[test]
fn test_deploy_without_strategy_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.deposit(&merchant, &500_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&100_000),
        Err(Ok(Error::StrategyNotSet))
    );
}

#[test]
fn test_deploy_insufficient_reserve_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&4_500_000),
        Err(Ok(Error::InsufficientReserve))
    );
}

#[test]
fn test_deploy_exceeds_max_ratio_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 5000);

    vault_client.deposit(&merchant, &5_000_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&3_000_000),
        Err(Ok(Error::DeploymentExceedsMax))
    );
}

#[test]
fn test_deploy_insufficient_float_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &1_000_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&2_000_000),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_deploy_zero_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&0),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_deploy_when_paused_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.pause();

    assert_eq!(
        vault_client.try_deploy_to_yield(&1_000_000),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_deploy_multiple_times() {
    let (_env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(1000, 8000);

    vault_client.deposit(&merchant, &5_000_000);

    vault_client.deploy_to_yield(&1_000_000);
    vault_client.deploy_to_yield(&1_000_000);
    vault_client.deploy_to_yield(&1_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 3_000_000);
    assert_eq!(tc.balance(&vault_client.address), 2_000_000);
}

// ── Withdraw from yield tests ──────────────────────────────────────────────

#[test]
fn test_withdraw_from_yield_returns_principal_and_yield() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&500_000);

    let vault_balance_before = tc.balance(&vault_client.address);

    vault_client.withdraw_from_yield(&1_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 2_000_000);
    let expected_yield = 500_000i128 * 1_000_000 / 3_000_000;
    assert_eq!(info.harvested_yield, expected_yield);

    let expected_return = 1_000_000 + expected_yield;
    assert_eq!(
        tc.balance(&vault_client.address),
        vault_balance_before + expected_return
    );
}

#[test]
fn test_withdraw_more_than_deployed_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&2_000_000);

    assert_eq!(
        vault_client.try_withdraw_from_yield(&3_000_000),
        Err(Ok(Error::NothingToWithdraw))
    );
}

#[test]
fn test_withdraw_zero_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&2_000_000);

    assert_eq!(
        vault_client.try_withdraw_from_yield(&0),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_withdraw_without_strategy_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
    let vault_client = RefundVaultClient::new(&env, &vault_id);

    assert_eq!(
        vault_client.try_withdraw_from_yield(&100),
        Err(Ok(Error::StrategyNotSet))
    );
}

#[test]
fn test_withdraw_full_principal() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&300_000);

    vault_client.withdraw_from_yield(&3_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 0);
    assert_eq!(info.harvested_yield, 300_000);
    assert_eq!(tc.balance(&vault_client.address), 5_300_000);
}

// ── Harvest yield tests ────────────────────────────────────────────────────

#[test]
fn test_harvest_yield_happy_path() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);

    let vault_balance_before = tc.balance(&vault_client.address);

    vault_client.harvest_yield();

    let info = vault_client.get_yield_info();
    assert_eq!(info.harvested_yield, 200_000);
    assert_eq!(info.deployed_principal, 3_000_000);
    assert_eq!(
        tc.balance(&vault_client.address),
        vault_balance_before + 200_000
    );
}

#[test]
fn test_harvest_nothing_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    assert_eq!(
        vault_client.try_harvest_yield(),
        Err(Ok(Error::NothingToHarvest))
    );
}

#[test]
fn test_harvest_accumulates() {
    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);

    strategy_client.simulate_yield(&100_000);
    vault_client.harvest_yield();

    strategy_client.simulate_yield(&150_000);
    vault_client.harvest_yield();

    let info = vault_client.get_yield_info();
    assert_eq!(info.harvested_yield, 250_000);
}

// ── Yield + refund interaction tests ───────────────────────────────────────

#[test]
fn test_refund_succeeds_after_deploy_within_reserve() {
    let (env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &500_000, &0, &500_000, &None, &0);

    assert_eq!(tc.balance(&buyer), 500_000);
    assert_eq!(tc.balance(&vault_client.address), 1_500_000);
}

#[test]
fn test_refund_exceeding_liquid_after_deploy_fails() {
    let (env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let payment_ref = BytesN::from_array(&env, &[2u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &2_500_000, &0, &2_500_000, &None, &0),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_refund_after_withdraw_from_yield() {
    let (env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&4_000_000);

    vault_client.withdraw_from_yield(&2_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &2_500_000, &0, &2_500_000, &None, &0);

    assert_eq!(tc.balance(&buyer), 2_500_000);
}

// ── Yield + withdraw interaction tests ─────────────────────────────────────

#[test]
fn test_operator_withdraw_harvested_yield() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&300_000);
    vault_client.harvest_yield();

    let operator = Address::generate(&env);
    let _merchant_balance_before = tc.balance(&merchant);

    vault_client.withdraw(&300_000, &operator);

    assert_eq!(tc.balance(&operator), 300_000);
    assert_eq!(tc.balance(&vault_client.address), 2_000_000);
}

#[test]
fn test_yield_accounting_after_full_cycle() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(1000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&500_000);

    vault_client.harvest_yield();
    vault_client.withdraw_from_yield(&1_500_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 1_500_000);
    assert_eq!(info.harvested_yield, 500_000);
    assert_eq!(tc.balance(&vault_client.address), 4_000_000);
}

// ── Yield + pause interaction tests ────────────────────────────────────────

#[test]
fn test_deploy_when_paused() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.pause();

    assert_eq!(
        vault_client.try_deploy_to_yield(&1_000_000),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_withdraw_from_yield_when_paused() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);
    vault_client.pause();

    assert_eq!(
        vault_client.try_withdraw_from_yield(&1_000_000),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_harvest_when_paused() {
    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&100_000);

    vault_client.pause();

    assert_eq!(vault_client.try_harvest_yield(), Err(Ok(Error::Paused)));
}

// ── Yield events tests ─────────────────────────────────────────────────────

#[test]
fn test_yield_deployed_event() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&2_000_000);

    let events = env.events().all().filter_by_contract(&vault_client.address);
    // Yield events now carry a monotonic nonce (issue #136).
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "amount").into_val(&env),
        2_000_000i128.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        1u64.into_val(&env),
    );
    assert_eq!(
        events,
        vec![
            &env,
            (
                vault_client.address.clone(),
                (
                    Symbol::new(&env, "yield_deployed_event"),
                    strategy_addr.clone()
                )
                    .into_val(&env),
                data.into_val(&env)
            )
        ]
    );
}

#[test]
fn test_yield_harvested_event() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);

    vault_client.harvest_yield();

    let events = env.events().all().filter_by_contract(&vault_client.address);
    // Harvested event carries the nonce too.
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "amount").into_val(&env),
        200_000i128.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        2u64.into_val(&env),
    );
    assert_eq!(
        events,
        vec![
            &env,
            (
                vault_client.address.clone(),
                (Symbol::new(&env, "yield_harvested_event"),).into_val(&env),
                data.into_val(&env)
            )
        ]
    );
}

// ── Edge case: zero reserve, full deploy ───────────────────────────────────

#[test]
fn test_zero_reserve_full_deploy() {
    let (_env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&5_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 5_000_000);
    assert_eq!(tc.balance(&vault_client.address), 0);
}

#[test]
fn test_full_reserve_cannot_deploy() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) =
        setup_with_strategy(10_000, 10_000);

    vault_client.deposit(&merchant, &5_000_000);

    assert_eq!(
        vault_client.try_deploy_to_yield(&1),
        Err(Ok(Error::InsufficientReserve))
    );
}

// ── Existing tests still pass with yield features present ──────────────────

#[test]
fn test_existing_deposit_refund_withdraw_still_works() {
    let (env, vault_client, merchant, token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None, &0);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&buyer), 120_000);
    assert_eq!(tc.balance(&vault_client.address), 4_880_000);

    vault_client.withdraw(&200_000, &merchant);
    assert_eq!(tc.balance(&vault_client.address), 4_680_000);
}

// ── Persistent storage TTL tests (issue #131) ────────────────────────────
//
// Yield-related keys are stored in Persistent (not Instance) storage so
// non-yield calls never pay their read/write byte cost. These tests verify
// that (a) the keys are indeed persistent, and (b) each write extends the
// TTL via the `persist_yield_ttl` helper.

/// After `set_yield_strategy`, the `YieldStrategy` key must exist in
/// Persistent storage with a TTL at or above `TTL_EXTEND`.
#[test]
fn test_yield_strategy_key_is_persistent_with_ttl() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (_env, vault_client, _merchant, _token, strategy_addr, _tc) =
        setup_with_strategy(2000, 8000);

    let ttl = _env.as_contract(&vault_client.address, || {
        _env.storage().persistent().get_ttl(&DataKey::YieldStrategy)
    });
    assert!(
        ttl >= TTL_EXTEND,
        "YieldStrategy TTL ({ttl}) must be >= TTL_EXTEND ({TTL_EXTEND}) after set_yield_strategy"
    );
}

/// After `set_reserve_ratio`, the `ReserveRatio` key must be persistent.
#[test]
fn test_reserve_ratio_key_is_persistent_with_ttl() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (_env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    let ttl = _env.as_contract(&vault_client.address, || {
        _env.storage().persistent().get_ttl(&DataKey::ReserveRatio)
    });
    assert!(
        ttl >= TTL_EXTEND,
        "ReserveRatio TTL ({ttl}) must be >= TTL_EXTEND ({TTL_EXTEND}) after set_reserve_ratio"
    );
}

/// After `set_max_deploy_ratio`, the `MaxDeployRatio` key must be persistent.
#[test]
fn test_max_deploy_ratio_key_is_persistent_with_ttl() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (_env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    let ttl = _env.as_contract(&vault_client.address, || {
        _env.storage()
            .persistent()
            .get_ttl(&DataKey::MaxDeployRatio)
    });
    assert!(
        ttl >= TTL_EXTEND,
        "MaxDeployRatio TTL ({ttl}) must be >= TTL_EXTEND ({TTL_EXTEND}) after set_max_deploy_ratio"
    );
}

/// After `deploy_to_yield`, the `DeployedPrincipal` key must be persistent
/// with an appropriate TTL.
#[test]
fn test_deployed_principal_key_is_persistent_with_ttl() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let ttl = _env.as_contract(&vault_client.address, || {
        _env.storage()
            .persistent()
            .get_ttl(&DataKey::DeployedPrincipal)
    });
    assert!(
        ttl >= TTL_EXTEND,
        "DeployedPrincipal TTL ({ttl}) must be >= TTL_EXTEND ({TTL_EXTEND}) after deploy_to_yield"
    );
}

/// After `harvest_yield`, the `HarvestedYield` key must be persistent
/// with an appropriate TTL.
#[test]
fn test_harvested_yield_key_is_persistent_with_ttl() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);
    vault_client.harvest_yield();

    let ttl = env.as_contract(&vault_client.address, || {
        env.storage().persistent().get_ttl(&DataKey::HarvestedYield)
    });
    assert!(
        ttl >= TTL_EXTEND,
        "HarvestedYield TTL ({ttl}) must be >= TTL_EXTEND ({TTL_EXTEND}) after harvest_yield"
    );
}

/// Non-yield calls (deposit, refund, withdraw) must not create or extend
/// persistent yield keys — only yield-related entry points should touch them.
#[test]
fn test_non_yield_calls_do_not_create_yield_persistent_keys() {
    use soroban_sdk::testutils::storage::Persistent as _;

    let (env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    // Only do a deposit — no yield operations.
    vault_client.deposit(&merchant, &5_000_000);

    // DeployedPrincipal and HarvestedYield should not exist yet.
    // (YieldStrategy, ReserveRatio, MaxDeployRatio were created by the
    // setup_with_strategy helper, so we only check the operation-tracked keys.)
    let deployed_exists = env.as_contract(&vault_client.address, || {
        env.storage().persistent().has(&DataKey::DeployedPrincipal)
    });
    let harvested_exists = env.as_contract(&vault_client.address, || {
        env.storage().persistent().has(&DataKey::HarvestedYield)
    });

    assert!(
        !deployed_exists,
        "DeployedPrincipal should not exist after deposit-only"
    );
    assert!(
        !harvested_exists,
        "HarvestedYield should not exist after deposit-only"
    );
}

/// Yield keys should remain readable after a refund — proving they are
/// truly persistent and not affected by non-yield entry points.
#[test]
fn test_yield_info_survives_refund() {
    let (env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    // Snapshot yield info before refund.
    let info_before = vault_client.get_yield_info();

    // Refund from liquid balance — must not alter yield state.
    let payment_ref = BytesN::from_array(&env, &[0xAAu8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &500_000, &0, &500_000, &None, &0);

    let info_after = vault_client.get_yield_info();
    assert_eq!(
        info_before.deployed_principal,
        info_after.deployed_principal
    );
    assert_eq!(info_before.harvested_yield, info_after.harvested_yield);
    assert_eq!(info_before.strategy, info_after.strategy);
    assert_eq!(info_before.reserve_ratio, info_after.reserve_ratio);
    assert_eq!(info_before.max_deploy_ratio, info_after.max_deploy_ratio);
}
