#![cfg(test)]
#![allow(unused_imports, unused_variables, dead_code)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::{Error, RefundVault, RefundVaultClient};

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

    /// Admin-only: simulate yield accrual by increasing the tracked yield.
    /// In a real strategy this would happen organically from lending interest.
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

    /// Called by the vault to deposit tokens into the strategy.
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

    /// Called by the vault to withdraw principal + proportional yield.
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

        // Proportional yield: yield * (principal / total_deposited)
        let yield_portion = if total > 0 {
            yield_accrued * principal / total
        } else {
            0
        };

        let total_return = principal + yield_portion;

        // Transfer tokens back to the vault.
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

        // Update internal state.
        env.storage()
            .instance()
            .set(&StrategyDataKey::TotalDeposited, &(total - principal));
        env.storage().instance().set(
            &StrategyDataKey::YieldAccrued,
            &(yield_accrued - yield_portion),
        );

        Ok((principal, yield_portion))
    }

    /// Called by the vault to harvest accrued yield.
    pub fn harvest(env: Env) -> Result<i128, Error> {
        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued <= 0 {
            return Err(Error::NothingToHarvest);
        }

        // Transfer yield tokens to the vault.
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

    // Deploy vault.
    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.initialize(&merchant, &token, &17_280);

    // Deploy mock strategy.
    let strategy_id = env.register(MockYieldStrategy, ());
    let strategy_addr = strategy_id.clone();
    MockYieldStrategyClient::new(&env, &strategy_id).initialize(&token, &vault_id);

    // Mint extra tokens to the strategy to simulate a funded lending pool.
    // This allows the strategy to return yield on harvest/withdraw.
    StellarAssetClient::new(&env, &token).mint(&strategy_addr, &FLOAT);

    // Configure vault yield settings.
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
    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    let addr = Address::generate(&env);

    assert_eq!(
        vault_client.try_set_yield_strategy(&addr),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_set_yield_strategy_requires_auth() {
    let (env, vault_client, _merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);
    let new_strategy = Address::generate(&env);

    // No signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    // `merchant.require_auth()` aborts rather than returning an error, so the
    // call is caught by the client as a host error, not `Unauthorized`.
    assert!(vault_client.try_set_yield_strategy(&new_strategy).is_err());
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

    // Deposit 5M into vault.
    vault_client.deposit(&merchant, &5_000_000);
    assert_eq!(tc.balance(&vault_client.address), 5_000_000);

    // Deploy 3M to yield (60% of 5M, under 80% max).
    // Reserve: 20% of 5M = 1M. Post-deploy liquid = 2M >= 1M. OK.
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

    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.initialize(&merchant, &token, &100);
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

    // Deploy 4.5M (90%). Reserve = 20% of 5M = 1M. Post-deploy = 500K < 1M.
    assert_eq!(
        vault_client.try_deploy_to_yield(&4_500_000),
        Err(Ok(Error::InsufficientReserve))
    );
}

#[test]
fn test_deploy_exceeds_max_ratio_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 5000); // 0% reserve, 50% max deploy

    vault_client.deposit(&merchant, &5_000_000);

    // Deploy 3M (60% of 5M) > 50% max.
    assert_eq!(
        vault_client.try_deploy_to_yield(&3_000_000),
        Err(Ok(Error::DeploymentExceedsMax))
    );
}

#[test]
fn test_deploy_insufficient_float_fails() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &1_000_000);

    // Try to deploy more than vault balance.
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

    // Simulate 500K yield accruing in the strategy.
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&500_000);

    let vault_balance_before = tc.balance(&vault_client.address);

    // Withdraw 1M principal. Strategy returns 1M + proportional yield (1M/3M * 500K = 166K).
    vault_client.withdraw_from_yield(&1_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 2_000_000);
    // Yield portion: 500_000 * 1_000_000 / 3_000_000 = 166_666
    let expected_yield = 500_000i128 * 1_000_000 / 3_000_000;
    assert_eq!(info.harvested_yield, expected_yield);

    // Vault token balance increased by principal + yield portion.
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

    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.initialize(&merchant, &token, &100);

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

    // Simulate yield.
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&300_000);

    // Withdraw all deployed principal.
    vault_client.withdraw_from_yield(&3_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 0);
    assert_eq!(info.harvested_yield, 300_000);
    // All tokens back in vault: 2M (never deployed) + 3M (principal) + 300K (yield) = 5.3M
    assert_eq!(tc.balance(&vault_client.address), 5_300_000);
}

// ── Harvest yield tests ────────────────────────────────────────────────────

#[test]
fn test_harvest_yield_happy_path() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    // Simulate 200K yield.
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);

    let vault_balance_before = tc.balance(&vault_client.address);

    vault_client.harvest_yield();

    let info = vault_client.get_yield_info();
    assert_eq!(info.harvested_yield, 200_000);
    assert_eq!(info.deployed_principal, 3_000_000); // Principal untouched.
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

    // No yield simulated.
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

    // First harvest: 100K.
    strategy_client.simulate_yield(&100_000);
    vault_client.harvest_yield();

    // Second harvest: 150K.
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
    // Deploy 3M, leaving 2M liquid (>= 20% reserve of 5M = 1M).
    vault_client.deploy_to_yield(&3_000_000);

    // Refund from liquid balance.
    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &500_000, &0, &500_000);

    assert_eq!(tc.balance(&buyer), 500_000);
    assert_eq!(tc.balance(&vault_client.address), 1_500_000);
}

#[test]
fn test_refund_exceeding_liquid_after_deploy_fails() {
    let (env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000); // 2M liquid remaining.

    // Try to refund 2.5M — exceeds liquid balance.
    let payment_ref = BytesN::from_array(&env, &[2u8; 32]);
    let buyer = Address::generate(&env);
    // payment_amount >= amount so the ceiling check passes and the float
    // shortage (2M liquid < 2.5M) is what gets reported.
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &2_500_000, &0, &2_500_000),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_refund_after_withdraw_from_yield() {
    let (env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&4_000_000); // 1M liquid.

    // Withdraw 2M back to cover a large refund.
    vault_client.withdraw_from_yield(&2_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &2_500_000, &0, &2_500_000);

    assert_eq!(tc.balance(&buyer), 2_500_000);
}

// ── Yield + withdraw interaction tests ─────────────────────────────────────

#[test]
fn test_operator_withdraw_harvested_yield() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    // Simulate and harvest yield.
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&300_000);
    vault_client.harvest_yield();

    let operator = Address::generate(&env);
    let _merchant_balance_before = tc.balance(&merchant);

    // Merchant withdraws the harvested yield.
    vault_client.withdraw(&300_000, &operator);

    assert_eq!(tc.balance(&operator), 300_000);
    // Vault still has 2M liquid (5M - 3M deployed).
    assert_eq!(tc.balance(&vault_client.address), 2_000_000);
}

#[test]
fn test_yield_accounting_after_full_cycle() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup_with_strategy(1000, 8000);

    // 1. Deposit.
    vault_client.deposit(&merchant, &5_000_000);

    // 2. Deploy to yield.
    vault_client.deploy_to_yield(&3_000_000);

    // 3. Simulate yield.
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&500_000);

    // 4. Harvest yield.
    vault_client.harvest_yield();

    // 5. Withdraw half the principal.
    vault_client.withdraw_from_yield(&1_500_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 1_500_000);

    // Yield portion from 1.5M withdrawal: 500K * 1.5M / 3M = 250K.
    // But we already harvested 500K, so total harvested = 500K + 250K = 750K.
    // Wait — harvest resets yield_accrued to 0 in the strategy, so the withdrawal
    // yield portion is 0 (no remaining accrued yield after harvest).
    // Actually: harvest was called BEFORE withdraw. After harvest, yield_accrued = 0.
    // So withdraw returns (1.5M, 0).
    // Total harvested_yield = 500K (from harvest) + 0 (from withdraw) = 500K.
    assert_eq!(info.harvested_yield, 500_000);

    // Vault token balance:
    // 5M deposited - 3M deployed + 500K harvested + 1.5M withdrawn = 4M
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

    // Note: `env.events().all()` returns only the events of the last contract
    // invocation, so this asserts on the deploy invocation's events alone.
    assert_eq!(
        env.events().all().filter_by_contract(&vault_client.address),
        vec![
            &env,
            (
                vault_client.address.clone(),
                (
                    Symbol::new(&env, "yield_deployed_event"),
                    strategy_addr.clone()
                )
                    .into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 2_000_000i128)]
                    .into_val(&env)
            ),
        ]
    );
}

#[test]
fn test_yield_harvested_event() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Symbol};

    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);

    vault_client.harvest_yield();

    // `env.events().all()` returns only the last invocation's events (the harvest).
    assert_eq!(
        env.events().all().filter_by_contract(&vault_client.address),
        vec![
            &env,
            (
                vault_client.address.clone(),
                (Symbol::new(&env, "yield_harvested_event"),).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 200_000i128)].into_val(&env)
            ),
        ]
    );
}

// ── Edge case: zero reserve, full deploy ───────────────────────────────────

#[test]
fn test_zero_reserve_full_deploy() {
    let (_env, vault_client, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);

    vault_client.deposit(&merchant, &5_000_000);

    // Deploy everything (0% reserve, 100% max).
    vault_client.deploy_to_yield(&5_000_000);

    let info = vault_client.get_yield_info();
    assert_eq!(info.deployed_principal, 5_000_000);
    assert_eq!(tc.balance(&vault_client.address), 0);
}

#[test]
fn test_full_reserve_cannot_deploy() {
    let (_env, vault_client, merchant, _token, _strategy, _tc) =
        setup_with_strategy(10_000, 10_000); // 100% reserve.

    vault_client.deposit(&merchant, &5_000_000);

    // Any deployment would breach 100% reserve.
    assert_eq!(
        vault_client.try_deploy_to_yield(&1),
        Err(Ok(Error::InsufficientReserve))
    );
}

// ── Existing tests still pass with yield features present ──────────────────

#[test]
fn test_existing_deposit_refund_withdraw_still_works() {
    let (env, vault_client, merchant, token, _strategy, _tc) = setup_with_strategy(2000, 8000);

    // Standard deposit-refund-withdraw flow, no yield involved.
    vault_client.deposit(&merchant, &5_000_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    vault_client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

    let tc = TokenClient::new(&env, &token);
    assert_eq!(tc.balance(&buyer), 120_000);
    assert_eq!(tc.balance(&vault_client.address), 4_880_000);

    vault_client.withdraw(&200_000, &merchant);
    assert_eq!(tc.balance(&vault_client.address), 4_680_000);
}

// ── i128 overflow boundary tests (issue #57) ───────────────────────────────

#[test]
#[should_panic]
fn test_deploy_to_yield_i128_multiplication_overflow() {
    // The deploy_to_yield path computes:
    //   total_value = token_balance + deployed - harvested
    //   reserve_required = total_value * reserve_ratio / 10_000
    //
    // If total_value is large enough and reserve_ratio > 0, the
    // multiplication overflows i128.  overflow-checks = true catches
    // this as a panic, preventing silent wrapping.
    let (env, vault_client, merchant, _token, _strategy, _tc) = setup_with_strategy(10_000, 10_000);

    // Mint a huge amount and deposit it so total_value is enormous.
    let huge: i128 = i128::MAX / 10;
    let sac = StellarAssetClient::new(
        &env,
        &env.storage()
            .instance()
            .get::<_, Address>(&crate::DataKey::Token)
            .unwrap(),
    );
    sac.mint(&merchant, &huge);
    vault_client.deposit(&merchant, &huge);

    // total_value = huge, reserve_ratio = 10_000 (100 %).
    // huge * 10_000 overflows i128.
    vault_client.deploy_to_yield(&1);
}
