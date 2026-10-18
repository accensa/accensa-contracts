//! #630 — Assert refund-vault yield deployment and harvest events.
//!
//! Asserts the exact XDR shape of all five yield-related events:
//! - `StrategyApprovalEvent` (strategy whitelist approval)
//! - `YieldDeployedEvent` (deployment to yield strategy)
//! - `YieldWithdrawnEvent` (withdrawal from yield strategy)
//! - `YieldHarvestedEvent` (harvest accrued yield from strategy)
//! - `YieldDistributedEvent` (distribution of harvested yield)
//!
//! Also asserts the negative cases:
//! - Withdrawing with nothing deployed / nothing to withdraw (NothingToWithdraw = 16)
//!   emits no YieldWithdrawnEvent.
//! - A deployment that would breach the reserve (InsufficientReserve = 14) emits
//!   no YieldDeployedEvent.

use accensa_common::VaultInit;
use refund_vault::{
    strategy::{StrategyApprovalEvent, YieldDistributedEvent},
    RefundVault, RefundVaultClient, YieldDeployedEvent, YieldHarvestedEvent, YieldWithdrawnEvent,
};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    events::Event as _,
    testutils::{Address as _, Events},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, IntoVal, Map, Symbol, Val,
};

// ── Mock yield strategy contract fixture ───────────────────────────────────

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

    pub fn deposit(env: Env, amount: i128) -> Result<(), accensa_common::Error> {
        if amount <= 0 {
            return Err(accensa_common::Error::InvalidAmount);
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

    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), accensa_common::Error> {
        let total: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::TotalDeposited)
            .unwrap_or(0);
        if principal > total || principal <= 0 {
            return Err(accensa_common::Error::NothingToWithdraw);
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

    pub fn harvest(env: Env) -> Result<i128, accensa_common::Error> {
        let yield_accrued: i128 = env
            .storage()
            .instance()
            .get(&StrategyDataKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued <= 0 {
            return Err(accensa_common::Error::NothingToHarvest);
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

// ── Test Setup ─────────────────────────────────────────────────────────────

const FLOAT: i128 = 10_000_000;

fn setup(
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
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 17_280,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let vault_client = RefundVaultClient::new(&env, &vault_id);

    let strategy_id = env.register(MockYieldStrategy, ());
    let strategy_addr = strategy_id.clone();
    MockYieldStrategyClient::new(&env, &strategy_id).initialize(&token, &vault_id);

    // Mint tokens to the strategy to back yield rewards
    StellarAssetClient::new(&env, &token).mint(&strategy_addr, &FLOAT);

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

// ── Event Assertion Tests ──────────────────────────────────────────────────

#[test]
fn test_strategy_approval_event_exact_xdr_shape() {
    let (env, vault_client, _merchant, _token, strategy_addr, _tc) = setup(2000, 8000);

    // Whitelisting strategy emits StrategyApprovalEvent { strategy, approved: true }
    vault_client.approve_yield_strategy(&strategy_addr);

    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "approved").into_val(&env),
        true.into_val(&env),
    );

    let expected = vec![
        &env,
        (
            vault_client.address.clone(),
            (
                Symbol::new(&env, "strategy_approval_event"),
                strategy_addr.clone(),
            )
                .into_val(&env),
            data.into_val(&env),
        ),
    ];
    assert_eq!(
        env.events().all().filter_by_contract(&vault_client.address),
        expected
    );

    let expected_event = StrategyApprovalEvent {
        strategy: strategy_addr.clone(),
        approved: true,
    };
    assert_eq!(
        env.events()
            .all()
            .filter_by_contract(&vault_client.address)
            .events()[0],
        expected_event.to_xdr(&env, &vault_client.address)
    );
}

#[test]
fn test_yield_deployed_event_exact_xdr_shape_and_balance_accounting() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup(2000, 8000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    let deposit_amount = 5_000_000i128;
    vault_client.deposit(&merchant, &deposit_amount, &None);

    let vault_bal_before = tc.balance(&vault_client.address);
    let strat_bal_before = tc.balance(&strategy_addr);

    let deploy_amount = 2_000_000i128;
    vault_client.deploy_to_yield(&deploy_amount);

    let events = env.events().all().filter_by_contract(&vault_client.address);

    let vault_bal_after = tc.balance(&vault_client.address);
    let strat_bal_after = tc.balance(&strategy_addr);

    // Assert state alignment: balance delta and strategy position match deployed amount
    assert_eq!(vault_bal_before - vault_bal_after, deploy_amount);
    assert_eq!(strat_bal_after - strat_bal_before, deploy_amount);

    let yield_info = vault_client.get_yield_info();
    assert_eq!(yield_info.deployed_principal, deploy_amount);

    // Assert exact XDR shape of YieldDeployedEvent
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "amount").into_val(&env),
        deploy_amount.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        1u64.into_val(&env),
    );

    let expected = vec![
        &env,
        (
            vault_client.address.clone(),
            (
                Symbol::new(&env, "yield_deployed_event"),
                strategy_addr.clone(),
            )
                .into_val(&env),
            data.into_val(&env),
        ),
    ];
    assert_eq!(events, expected);

    let expected_event = YieldDeployedEvent {
        strategy: strategy_addr.clone(),
        amount: deploy_amount,
        nonce: 1,
    };
    assert_eq!(
        events.events()[0],
        expected_event.to_xdr(&env, &vault_client.address)
    );
}

#[test]
fn test_yield_withdrawn_event_exact_xdr_shape_and_balance_accounting() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup(2000, 8000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    let deposit_amount = 5_000_000i128;
    vault_client.deposit(&merchant, &deposit_amount, &None);

    let deploy_amount = 2_000_000i128;
    vault_client.deploy_to_yield(&deploy_amount);

    // Simulate accrued yield in strategy
    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&100_000);

    let vault_bal_before = tc.balance(&vault_client.address);
    let strat_bal_before = tc.balance(&strategy_addr);

    let withdraw_principal = 1_000_000i128;
    vault_client.withdraw_from_yield(&withdraw_principal);

    let events = env.events().all().filter_by_contract(&vault_client.address);

    let vault_bal_after = tc.balance(&vault_client.address);
    let strat_bal_after = tc.balance(&strategy_addr);

    // With half the principal withdrawn (1_000_000 of 2_000_000), proportional yield is 50_000
    let expected_yield = 50_000i128;
    let total_reclaimed = withdraw_principal + expected_yield;

    // Verify token transfers
    assert_eq!(vault_bal_after - vault_bal_before, total_reclaimed);
    assert_eq!(strat_bal_before - strat_bal_after, total_reclaimed);

    // Verify vault and strategy state
    let yield_info = vault_client.get_yield_info();
    assert_eq!(
        yield_info.deployed_principal,
        deploy_amount - withdraw_principal
    );
    assert_eq!(yield_info.harvested_yield, expected_yield);

    // Assert exact XDR shape of YieldWithdrawnEvent
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        2u64.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "principal").into_val(&env),
        withdraw_principal.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "yield_amount").into_val(&env),
        expected_yield.into_val(&env),
    );

    let expected = vec![
        &env,
        (
            vault_client.address.clone(),
            (
                Symbol::new(&env, "yield_withdrawn_event"),
                strategy_addr.clone(),
            )
                .into_val(&env),
            data.into_val(&env),
        ),
    ];
    assert_eq!(events, expected);

    let expected_event = YieldWithdrawnEvent {
        strategy: strategy_addr.clone(),
        principal: withdraw_principal,
        yield_amount: expected_yield,
        nonce: 2,
    };
    assert_eq!(
        events.events()[0],
        expected_event.to_xdr(&env, &vault_client.address)
    );
}

#[test]
fn test_yield_harvested_event_exact_xdr_shape_and_balance_accounting() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup(0, 10_000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    let deposit_amount = 5_000_000i128;
    vault_client.deposit(&merchant, &deposit_amount, &None);

    let deploy_amount = 3_000_000i128;
    vault_client.deploy_to_yield(&deploy_amount);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    let harvest_amount = 200_000i128;
    strategy_client.simulate_yield(&harvest_amount);

    let vault_bal_before = tc.balance(&vault_client.address);
    let strat_bal_before = tc.balance(&strategy_addr);

    vault_client.harvest_yield();

    let events = env.events().all().filter_by_contract(&vault_client.address);

    let vault_bal_after = tc.balance(&vault_client.address);
    let strat_bal_after = tc.balance(&strategy_addr);

    // Assert balance moves
    assert_eq!(vault_bal_after - vault_bal_before, harvest_amount);
    assert_eq!(strat_bal_before - strat_bal_after, harvest_amount);

    let yield_info = vault_client.get_yield_info();
    assert_eq!(yield_info.harvested_yield, harvest_amount);

    // Assert exact XDR shape of YieldHarvestedEvent
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "amount").into_val(&env),
        harvest_amount.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        2u64.into_val(&env),
    );

    let expected = vec![
        &env,
        (
            vault_client.address.clone(),
            (Symbol::new(&env, "yield_harvested_event"),).into_val(&env),
            data.into_val(&env),
        ),
    ];
    assert_eq!(events, expected);

    let expected_event = YieldHarvestedEvent {
        amount: harvest_amount,
        nonce: 2,
    };
    assert_eq!(
        events.events()[0],
        expected_event.to_xdr(&env, &vault_client.address)
    );
}

#[test]
fn test_yield_distributed_event_exact_xdr_shape_and_balance_accounting() {
    let (env, vault_client, merchant, _token, strategy_addr, tc) = setup(0, 10_000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    let recipient = Address::generate(&env);
    vault_client.set_yield_recipient(&recipient);

    vault_client.deposit(&merchant, &5_000_000, &None);
    vault_client.deploy_to_yield(&3_000_000);

    let strategy_client = MockYieldStrategyClient::new(&env, &strategy_addr);
    strategy_client.simulate_yield(&200_000);

    vault_client.harvest_yield();

    let recipient_bal_before = tc.balance(&recipient);
    let vault_bal_before = tc.balance(&vault_client.address);

    let distributed = vault_client.distribute_yield();
    assert_eq!(distributed, 200_000);

    let events = env.events().all().filter_by_contract(&vault_client.address);

    let recipient_bal_after = tc.balance(&recipient);
    let vault_bal_after = tc.balance(&vault_client.address);

    // Assert token transfers
    assert_eq!(recipient_bal_after - recipient_bal_before, 200_000);
    assert_eq!(vault_bal_before - vault_bal_after, 200_000);

    let yield_info = vault_client.get_yield_info();
    assert_eq!(yield_info.harvested_yield, 0);

    // Assert exact XDR shape of YieldDistributedEvent
    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "amount").into_val(&env),
        200_000i128.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "nonce").into_val(&env),
        3u64.into_val(&env),
    );

    let expected = vec![
        &env,
        (
            vault_client.address.clone(),
            (
                Symbol::new(&env, "yield_distributed_event"),
                recipient.clone(),
            )
                .into_val(&env),
            data.into_val(&env),
        ),
    ];
    assert_eq!(events, expected);

    let expected_event = YieldDistributedEvent {
        recipient: recipient.clone(),
        amount: 200_000,
        nonce: 3,
    };
    assert_eq!(
        events.events()[0],
        expected_event.to_xdr(&env, &vault_client.address)
    );
}

// ── Negative Cases ─────────────────────────────────────────────────────────

#[test]
fn test_negative_withdraw_with_nothing_deployed_emits_no_events() {
    let (env, vault_client, _merchant, _token, strategy_addr, _tc) = setup(2000, 8000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    // Nothing deployed yet: withdraw_from_yield must fail with NothingToWithdraw (code 16)
    let res = vault_client.try_withdraw_from_yield(&500_000);
    assert_eq!(res, Err(Ok(accensa_common::Error::NothingToWithdraw)));

    // Assert no events emitted by vault
    let events = env.events().all().filter_by_contract(&vault_client.address);
    assert_eq!(events.events().len(), 0);
}

#[test]
fn test_negative_deployment_breaching_reserve_emits_no_events() {
    // 50% reserve ratio: must keep 50% liquid
    let (env, vault_client, merchant, _token, strategy_addr, _tc) = setup(5000, 10_000);

    vault_client.approve_yield_strategy(&strategy_addr);
    vault_client.set_yield_strategy(&strategy_addr);

    vault_client.deposit(&merchant, &1_000_000, &None);

    // Trying to deploy 600_000 would leave 400_000 liquid, less than 500_000 required reserve.
    // Must fail with InsufficientReserve (code 14)
    let res = vault_client.try_deploy_to_yield(&600_000);
    assert_eq!(res, Err(Ok(accensa_common::Error::InsufficientReserve)));

    // Assert no events emitted by vault
    let events = env.events().all().filter_by_contract(&vault_client.address);
    assert_eq!(events.events().len(), 0);
}
