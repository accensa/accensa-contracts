//! #967 — `withdraw` recalls deployed principal only for the shortfall.
//!
//! `withdraw` (`lib.rs:1543`) runs `strategy::ensure_liquidity` *before* its own
//! float check, so a merchant withdrawal larger than the liquid reserve pulls
//! exactly `needed - balance` (capped at what is deployed) back from the
//! strategy in the same invocation, and only reports `InsufficientFloat` if the
//! recall still cannot cover it. `src/strategy_tests.rs:146` covers the recall
//! happy path through `withdraw_from_yield`; nothing covered the three cases
//! that decide whether the recall is a *shortfall* recall or a full one, and
//! whether a reverted recall leaves its bookkeeping behind.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

// ── Strategy fixture ───────────────────────────────────────────────────────

#[contracttype]
pub enum Store {
    Token,
    Vault,
    TotalDeposited,
}

#[contract]
pub struct MockStrategy;

#[contractimpl]
impl MockStrategy {
    pub fn initialize(env: Env, token: Address, vault: Address) {
        env.storage().persistent().set(&Store::Token, &token);
        env.storage().persistent().set(&Store::Vault, &vault);
        env.storage()
            .persistent()
            .set(&Store::TotalDeposited, &0i128);
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total: i128 = env
            .storage()
            .persistent()
            .get(&Store::TotalDeposited)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&Store::TotalDeposited, &(total + amount));
        Ok(())
    }

    /// Hands the principal back before returning, which is what lets
    /// `recall_principal` verify that the vault's balance actually grew.
    /// No yield accrues in these tests, so the second element is always 0.
    pub fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error> {
        let total: i128 = env
            .storage()
            .persistent()
            .get(&Store::TotalDeposited)
            .unwrap_or(0);
        if principal <= 0 || principal > total {
            return Err(Error::NothingToWithdraw);
        }
        let token = TokenClient::new(
            &env,
            &env.storage()
                .persistent()
                .get::<_, Address>(&Store::Token)
                .unwrap(),
        );
        let vault: Address = env.storage().persistent().get(&Store::Vault).unwrap();
        token.transfer(&env.current_contract_address(), &vault, &principal);
        env.storage()
            .persistent()
            .set(&Store::TotalDeposited, &(total - principal));
        Ok((principal, 0))
    }

    pub fn principal_held(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&Store::TotalDeposited)
            .unwrap_or(0)
    }
}

// ── Setup ──────────────────────────────────────────────────────────────────

const FLOAT: i128 = 10_000_000;
const DEPOSIT: i128 = 1_000_000;
const DEPLOYED: i128 = 800_000;
/// What the vault keeps liquid once `DEPLOYED` has gone out.
const LIQUID: i128 = DEPOSIT - DEPLOYED;

/// A vault that has deployed 800,000 of a 1,000,000 deposit, so the liquid
/// float is 200,000 and any withdrawal above that has to be met from the
/// strategy.
///
/// The ratios are 0 and 10,000 — a deploy of 80 % needs no reserve floor and no
/// ceiling below the whole pool.
fn setup() -> (
    Env,
    RefundVaultClient<'static>,
    Address,
    Address,
    MockStrategyClient<'static>,
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
    let client = RefundVaultClient::new(&env, &vault_id);

    let strategy_id = env.register(MockStrategy, ());
    MockStrategyClient::new(&env, &strategy_id).initialize(&token, &vault_id);

    client.approve_yield_strategy(&strategy_id);
    client.set_yield_strategy(&strategy_id);
    client.set_reserve_ratio(&0);
    client.set_max_deploy_ratio(&10_000);

    client.deposit(&merchant, &DEPOSIT, &None);
    client.deploy_to_yield(&DEPLOYED);

    let tc = TokenClient::new(&env, &token);
    let strategy = MockStrategyClient::new(&env, &strategy_id);
    (env, client, merchant, token, strategy, tc)
}

/// Withdrawals inside the liquid float never touch the strategy: the recall is
/// for the *shortfall*, and a zero shortfall means `ensure_liquidity` returns
/// before it reads any yield storage.
#[test]
fn test_withdrawal_within_float_recalls_nothing() {
    let (env, client, _merchant, _token, strategy, tc) = setup();
    let to = Address::generate(&env);

    client.withdraw(&150_000, &to);

    assert_eq!(client.get_yield_info().deployed_principal, DEPLOYED);
    assert_eq!(strategy.principal_held(), DEPLOYED);
    assert_eq!(tc.balance(&to), 150_000);
    assert_eq!(tc.balance(&client.address), LIQUID - 150_000);
}

/// A withdrawal larger than the float recalls exactly the difference, so the
/// strategy keeps the rest of the principal deployed and still earning.
#[test]
fn test_withdrawal_above_float_recalls_only_the_shortfall() {
    let (env, client, _merchant, _token, strategy, tc) = setup();
    let to = Address::generate(&env);

    // 500,000 wanted, 200,000 liquid: recall 300,000, not the whole 800,000.
    client.withdraw(&500_000, &to);

    assert_eq!(client.get_yield_info().deployed_principal, 500_000);
    assert_eq!(strategy.principal_held(), 500_000);
    assert_eq!(tc.balance(&to), 500_000);
    // Nothing left in the vault: 200,000 liquid + 300,000 recalled = 500,000.
    assert_eq!(tc.balance(&client.address), 0);
    // The recalled yield is zero, so no harvested-yield bookkeeping appeared.
    assert_eq!(client.get_yield_info().harvested_yield, 0);
}

/// An exact withdrawal of total vault value recalls everything and leaves the
/// deployed principal at zero.
#[test]
fn test_exact_total_withdrawal_recalls_all_principal() {
    let (env, client, _merchant, _token, strategy, tc) = setup();
    let to = Address::generate(&env);

    client.withdraw(&DEPOSIT, &to);

    assert_eq!(client.get_yield_info().deployed_principal, 0);
    assert_eq!(strategy.principal_held(), 0);
    assert_eq!(tc.balance(&to), DEPOSIT);
    assert_eq!(tc.balance(&client.address), 0);
}

/// One unit past total vault value: the recall caps at what is deployed, the
/// float check then fails, and the reverted invocation leaves both books and
/// balances exactly as they were — including the strategy's own ledger.
#[test]
fn test_withdrawal_one_over_total_is_rejected_with_state_intact() {
    let (env, client, _merchant, _token, strategy, tc) = setup();
    let to = Address::generate(&env);
    let vault_before = tc.balance(&client.address);
    let nonce_before = client.get_nonce();

    assert_eq!(
        client.try_withdraw(&(DEPOSIT + 1), &to),
        Err(Ok(Error::InsufficientFloat))
    );

    // The 800,000 recall that `ensure_liquidity` performed rolled back with the
    // error rather than half-applying.
    assert_eq!(client.get_yield_info().deployed_principal, DEPLOYED);
    assert_eq!(strategy.principal_held(), DEPLOYED);
    assert_eq!(tc.balance(&client.address), vault_before);
    assert_eq!(tc.balance(&to), 0);
    assert_eq!(client.get_nonce(), nonce_before);
}

/// With nothing deployed, a short withdrawal reports the shortfall directly and
/// `ensure_liquidity` stays a no-op.
#[test]
fn test_withdrawal_without_deployed_principal_reports_shortfall() {
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
    let client = RefundVaultClient::new(&env, &vault_id);
    client.deposit(&merchant, &DEPOSIT, &None);

    let to = Address::generate(&env);
    let tc = TokenClient::new(&env, &token);

    assert_eq!(
        client.try_withdraw(&(DEPOSIT + 1), &to),
        Err(Ok(Error::InsufficientFloat))
    );
    assert_eq!(tc.balance(&to), 0);
    assert_eq!(tc.balance(&vault_id), DEPOSIT);
}

/// The strategy still holds what the vault says it holds after every scenario
/// that succeeds, which is the invariant `ensure_liquidity` exists to keep.
#[test]
fn test_vault_and_strategy_agree_after_a_partial_recall() {
    let (env, client, _merchant, _token, strategy, _tc) = setup();
    let to = Address::generate(&env);

    client.withdraw(&300_000, &to);

    let deployed = client.get_yield_info().deployed_principal;
    assert_eq!(deployed, strategy.principal_held());
    assert_eq!(deployed, DEPLOYED - 100_000);
}
