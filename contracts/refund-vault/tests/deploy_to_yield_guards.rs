//! #966 — `deploy_to_yield`'s guards, including one no public call can produce.
//!
//! The deploy path checks, in order: not paused, amount positive, a strategy is
//! registered, that strategy is still whitelisted, the vault holds the float,
//! then the reserve floor and the deployment ceiling.
//!
//! Only three of those are reachable through the public API, because
//! `revoke_yield_strategy` unregisters the active strategy in the same call that
//! de-whitelists it (`strategy.rs:139-147`), so a vault can never hold an
//! active-but-unapproved strategy by itself. This file reaches that state by
//! deleting the approval key from inside the contract — the only way to exercise
//! the `StrategyNotApproved` arm inside `deploy_to_yield` — and then pins where the
//! reserve floor and the ceiling each take over.

use accensa_common::{Error, VaultInit};
use refund_vault::{DataKey, RefundVault, RefundVaultClient};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, token::StellarAssetClient,
    Address, Env,
};

// ── Strategy fixture ───────────────────────────────────────────────────────

#[contracttype]
pub enum Store {
    Deployed,
}

#[contract]
pub struct MockStrategy;

#[contractimpl]
impl MockStrategy {
    /// The vault transfers the tokens before calling this, so what the strategy
    /// records is the amount it was actually credited with — the pair with
    /// `get_yield_info().deployed_principal` proves both sides agree.
    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total: i128 = env
            .storage()
            .persistent()
            .get(&Store::Deployed)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&Store::Deployed, &(total + amount));
        Ok(())
    }

    pub fn deployed_total(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&Store::Deployed)
            .unwrap_or(0)
    }
}

// ── Setup ──────────────────────────────────────────────────────────────────

const FLOAT: i128 = 10_000_000;
const DEPOSIT: i128 = 1_000_000;
const RESERVE_BP: u32 = 2_000;
const MAX_BP: u32 = 5_000;
/// Half of the deposited total value: the exact ceiling the ratio admits.
const AT_MAX: i128 = 500_000;

/// A vault with an approved, active strategy, a 20 % reserve floor and a 50 %
/// ceiling, holding 1,000,000 units of liquid float.
///
/// Those two ratios bind on different amounts — 500,001 is stopped by the
/// ceiling, 900,000 by the reserve — which is what makes both limits and their
/// precedence testable against one setup.
fn setup() -> (
    Env,
    RefundVaultClient<'static>,
    Address,
    Address,
    Address,
    MockStrategyClient<'static>,
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
    let strategy = MockStrategyClient::new(&env, &strategy_id);

    client.approve_yield_strategy(&strategy_id);
    client.set_yield_strategy(&strategy_id);
    client.set_reserve_ratio(&RESERVE_BP);
    client.set_max_deploy_ratio(&MAX_BP);
    client.deposit(&merchant, &DEPOSIT, &None);

    (env, client, merchant, token, strategy_id, strategy)
}

/// Delete the whitelist entry while leaving the strategy registered.
fn unapprove(env: &Env, vault_id: &Address, strategy: &Address) {
    env.as_contract(vault_id, || {
        env.storage()
            .persistent()
            .remove(&DataKey::ApprovedStrategy(strategy.clone()));
    });
}

/// Active strategy, approval key gone: the whitelist check rejects before any
/// amount, float, reserve or ceiling arithmetic runs.
#[test]
fn test_active_but_unapproved_strategy_is_rejected() {
    let (env, client, _merchant, _token, strategy_id, strategy) = setup();
    let vault_id = client.address.clone();

    assert!(client.is_strategy_approved(&strategy_id));
    unapprove(&env, &vault_id, &strategy_id);
    assert!(!client.is_strategy_approved(&strategy_id));

    // Still the registered strategy, which is the whole point: the deploy path
    // resolves the address and only then consults the whitelist.
    assert_eq!(client.get_yield_info().strategy, Some(strategy_id));
    assert_eq!(
        client.try_deploy_to_yield(&AT_MAX),
        Err(Ok(Error::StrategyNotApproved))
    );
    assert_eq!(client.get_yield_info().deployed_principal, 0);
    assert_eq!(strategy.deployed_total(), 0);
}

/// The public revoke path does *not* reach that state — it unregisters too, so
/// the deploy fails one check earlier. This is the assertion that keeps the
/// storage poke above from looking like an unnecessary detour.
#[test]
fn test_public_revoke_unregisters_so_deploy_reports_strategy_not_set() {
    let (_env, client, _merchant, _token, strategy_id, _strategy) = setup();

    client.revoke_yield_strategy(&strategy_id);

    assert!(!client.is_strategy_approved(&strategy_id));
    assert_eq!(client.get_yield_info().strategy, None);
    assert_eq!(
        client.try_deploy_to_yield(&AT_MAX),
        Err(Ok(Error::StrategyNotSet))
    );
}

/// The ceiling admits exactly `max_deploy_ratio` of total value.
#[test]
fn test_ceiling_admits_exactly_max_deploy_ratio() {
    let (_env, client, _merchant, _token, _strategy_id, strategy) = setup();

    client.deploy_to_yield(&AT_MAX);

    assert_eq!(client.get_yield_info().deployed_principal, AT_MAX);
    assert_eq!(strategy.deployed_total(), AT_MAX);
}

/// One unit past the ceiling is refused by the ceiling, not the reserve: 500,001
/// still leaves 499,999 liquid against a 200,000 floor.
#[test]
fn test_ceiling_rejects_one_unit_over() {
    let (_env, client, _merchant, _token, _strategy_id, strategy) = setup();

    assert_eq!(
        client.try_deploy_to_yield(&(AT_MAX + 1)),
        Err(Ok(Error::DeploymentExceedsMax))
    );
    assert_eq!(client.get_yield_info().deployed_principal, 0);
    assert_eq!(strategy.deployed_total(), 0);
}

/// The reserve floor binds before the ceiling on a large deployment, and it is
/// read first — so an amount that breaks both limits reports
/// `InsufficientReserve`. That precedence is a behavior worth pinning: nothing
/// in `deploy_to_yield` says which of the two an over-max, under-reserve call
/// should name.
#[test]
fn test_reserve_floor_is_reported_before_the_ceiling() {
    let (_env, client, _merchant, _token, _strategy_id, strategy) = setup();

    // 900,000 leaves 100,000 liquid against a 200,000 floor and is 400,000
    // past the 500,000 ceiling.
    assert_eq!(
        client.try_deploy_to_yield(&900_000),
        Err(Ok(Error::InsufficientReserve))
    );
    assert_eq!(client.get_yield_info().deployed_principal, 0);
    assert_eq!(strategy.deployed_total(), 0);
}

/// A deployment larger than the liquid float is caught by the float check
/// before either ratio is consulted.
#[test]
fn test_amount_above_liquid_float_reports_insufficient_float() {
    let (_env, client, _merchant, _token, _strategy_id, strategy) = setup();

    assert_eq!(
        client.try_deploy_to_yield(&(DEPOSIT + 1)),
        Err(Ok(Error::InsufficientFloat))
    );
    assert_eq!(client.get_yield_info().deployed_principal, 0);
    assert_eq!(strategy.deployed_total(), 0);
}

/// Rejections leave the deploy nonce alone, so the call that finally succeeds
/// is not a replay of a consumed number — and a success consumes exactly one.
#[test]
fn test_rejected_deploys_do_not_bump_the_nonce() {
    let (_env, client, _merchant, _token, _strategy_id, _strategy) = setup();

    let before = client.get_nonce();
    assert_eq!(
        client.try_deploy_to_yield(&(AT_MAX + 1)),
        Err(Ok(Error::DeploymentExceedsMax))
    );
    assert_eq!(
        client.try_deploy_to_yield(&900_000),
        Err(Ok(Error::InsufficientReserve))
    );
    assert_eq!(client.get_nonce(), before);

    client.deploy_to_yield(&AT_MAX);
    assert_eq!(client.get_nonce(), before + 1);
}
