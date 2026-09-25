//! Tests for the pluggable yield-bearing escrow strategy hook (issue #415):
//! whitelist, instant principal redemption on refund/withdraw, emergency exit,
//! yield routing to the treasury / rebate pool, and untrusted-strategy checks.

use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, token::StellarAssetClient,
    token::TokenClient, Address, BytesN, Env, Vec,
};

use crate::test_helpers::vault_init;
use crate::yield_tests::{setup_with_strategy, MockYieldStrategy, MockYieldStrategyClient, FLOAT};
use crate::{Error, RefundParam, RefundVault, RefundVaultClient};

// ── Mock: a strategy that reports a withdrawal without paying it ──────────

#[contracttype]
enum ShortKey {
    Deposited,
}

/// Accepts deposits, then on `withdraw` claims to have returned the principal
/// without transferring anything back.
#[contract]
pub struct ShortchangingStrategy;

#[contractimpl]
impl ShortchangingStrategy {
    pub fn deposit(env: Env, amount: i128) -> Result<(), Error> {
        let d: i128 = env
            .storage()
            .instance()
            .get(&ShortKey::Deposited)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&ShortKey::Deposited, &(d + amount));
        Ok(())
    }

    pub fn withdraw(_env: Env, principal: i128) -> Result<(i128, i128), Error> {
        Ok((principal, 0))
    }

    pub fn harvest(_env: Env) -> Result<i128, Error> {
        Ok(0)
    }

    pub fn total_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&ShortKey::Deposited)
            .unwrap_or(0)
    }

    pub fn accrued_yield(_env: Env) -> i128 {
        0
    }
}

fn register_mock_strategy(env: &Env, vault: &RefundVaultClient, token: &Address) -> Address {
    let id = env.register(MockYieldStrategy, ());
    MockYieldStrategyClient::new(env, &id).initialize(token, &vault.address);
    StellarAssetClient::new(env, token).mint(&id, &FLOAT);
    id
}

// ── Whitelist ─────────────────────────────────────────────────────────────

#[test]
fn test_set_unapproved_strategy_fails() {
    let (env, vault, _merchant, token, _strategy, _tc) = setup_with_strategy(0, 10_000);
    let other = register_mock_strategy(&env, &vault, &token);

    assert!(!vault.is_strategy_approved(&other));
    assert_eq!(
        vault.try_set_yield_strategy(&other),
        Err(Ok(Error::StrategyNotApproved))
    );
}

#[test]
fn test_approve_and_revoke_strategy() {
    let (env, vault, _merchant, token, strategy, _tc) = setup_with_strategy(0, 10_000);
    assert!(vault.is_strategy_approved(&strategy));

    let other = register_mock_strategy(&env, &vault, &token);
    vault.approve_yield_strategy(&other);
    assert!(vault.is_strategy_approved(&other));
    vault.revoke_yield_strategy(&other);
    assert!(!vault.is_strategy_approved(&other));
    // Revoking a non-active strategy leaves the active one registered.
    assert_eq!(vault.get_yield_info().strategy, Some(strategy));
}

#[test]
#[should_panic]
fn test_approve_strategy_requires_auth() {
    let (env, vault, _merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);
    env.set_auths(&[]);
    vault.approve_yield_strategy(&Address::generate(&env));
}

#[test]
fn test_revoke_active_strategy_with_principal_fails() {
    let (_env, vault, merchant, _token, strategy, _tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&500_000);

    assert_eq!(
        vault.try_revoke_yield_strategy(&strategy),
        Err(Ok(Error::StrategyHasPrincipal))
    );

    // Once principal is recalled the strategy can be revoked and unregistered.
    vault.withdraw_from_yield(&500_000);
    vault.revoke_yield_strategy(&strategy);
    assert_eq!(vault.get_yield_info().strategy, None);
    assert_eq!(
        vault.try_deploy_to_yield(&100_000),
        Err(Ok(Error::StrategyNotSet))
    );
}

#[test]
fn test_replace_strategy_with_principal_fails() {
    let (env, vault, merchant, token, _strategy, _tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&500_000);

    let other = register_mock_strategy(&env, &vault, &token);
    vault.approve_yield_strategy(&other);
    assert_eq!(
        vault.try_set_yield_strategy(&other),
        Err(Ok(Error::StrategyHasPrincipal))
    );

    vault.emergency_exit_yield();
    vault.set_yield_strategy(&other);
    assert_eq!(vault.get_yield_info().strategy, Some(other));
}

// ── Instant principal redemption ──────────────────────────────────────────

#[test]
fn test_merchant_withdraw_recalls_principal() {
    let (env, vault, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&800_000);

    let to = Address::generate(&env);
    vault.withdraw(&900_000, &to);

    assert_eq!(tc.balance(&to), 900_000);
    assert_eq!(tc.balance(&vault.address), 0);
    assert_eq!(vault.get_yield_info().deployed_principal, 100_000);
}

/// A customer refund against a vault whose float is almost fully deployed
/// recalls principal *and* the proportional yield; the yield is earmarked for
/// the yield recipient rather than silently absorbed into the float.
#[test]
fn test_refund_recall_books_proportional_yield() {
    let (env, vault, merchant, _token, strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&1_000_000);
    MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&100_000);

    let buyer = Address::generate(&env);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    vault.refund(&payment_ref, &buyer, &400_000, &0, &400_000, &None, &0);

    let info = vault.get_yield_info();
    assert_eq!(tc.balance(&buyer), 400_000);
    assert_eq!(info.deployed_principal, 600_000);
    assert_eq!(info.harvested_yield, 40_000);
    assert_eq!(tc.balance(&vault.address), 40_000);
}

/// `process_batch` runs without the reentrancy lock by design; the recall
/// path still works there.
#[test]
fn test_process_batch_recalls_principal() {
    let (env, vault, merchant, _token, _strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&900_000);

    let buyer = Address::generate(&env);
    let mut items = Vec::new(&env);
    for i in 0..3u8 {
        items.push_back(RefundParam {
            payment_ref: BytesN::from_array(&env, &[0x40 + i; 32]),
            recipient: buyer.clone(),
            amount: 300_000,
            paid_at_ledger: 0,
            payment_amount: 300_000,
            vdf_proof: None,
        });
    }
    let results = vault.process_batch(&items, &0);

    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|ok| ok));
    assert_eq!(tc.balance(&buyer), 900_000);
    // 100_000 was liquid, so only the 800_000 shortfall was recalled.
    assert_eq!(tc.balance(&vault.address), 0);
    assert_eq!(vault.get_yield_info().deployed_principal, 100_000);
}

#[test]
fn test_shortchanging_strategy_recall_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);
    let strategy = env.register(ShortchangingStrategy, ());
    vault.approve_yield_strategy(&strategy);
    vault.set_yield_strategy(&strategy);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&1_000_000);

    // The strategy claims to return principal but pays nothing: the vault
    // measures its own balance and refuses to book the phantom principal.
    assert_eq!(
        vault.try_withdraw_from_yield(&500_000),
        Err(Ok(Error::InsufficientFloat))
    );
    let buyer = Address::generate(&env);
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    assert_eq!(
        vault.try_refund(&payment_ref, &buyer, &500_000, &0, &500_000, &None, &0),
        Err(Ok(Error::InsufficientFloat))
    );
    assert_eq!(vault.get_yield_info().deployed_principal, 1_000_000);
    assert_eq!(TokenClient::new(&env, &token).balance(&buyer), 0);
}

// ── Emergency exit ────────────────────────────────────────────────────────

#[test]
fn test_emergency_exit_works_while_paused() {
    let (env, vault, merchant, _token, strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &2_000_000);
    vault.deploy_to_yield(&1_500_000);
    MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&150_000);

    vault.pause();
    assert_eq!(vault.emergency_exit_yield(), 1_500_000);

    let info = vault.get_yield_info();
    assert_eq!(info.deployed_principal, 0);
    assert_eq!(info.harvested_yield, 150_000);
    assert_eq!(tc.balance(&vault.address), 2_150_000);
}

#[test]
fn test_emergency_exit_nothing_deployed_fails() {
    let (_env, vault, _merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);
    assert_eq!(
        vault.try_emergency_exit_yield(),
        Err(Ok(Error::NothingToWithdraw))
    );
}

// ── Yield routing ─────────────────────────────────────────────────────────

#[test]
fn test_distribute_yield_to_treasury() {
    let (env, vault, merchant, _token, strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&1_000_000);
    MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&70_000);
    vault.harvest_yield();

    let treasury = Address::generate(&env);
    vault.set_yield_recipient(&treasury);
    assert_eq!(vault.get_yield_recipient(), treasury);
    assert_eq!(vault.distribute_yield(), 70_000);

    assert_eq!(tc.balance(&treasury), 70_000);
    assert_eq!(vault.get_yield_info().harvested_yield, 0);
    // Principal is untouched.
    assert_eq!(vault.get_yield_info().deployed_principal, 1_000_000);
    assert_eq!(
        vault.try_distribute_yield(),
        Err(Ok(Error::NothingToHarvest))
    );
}

#[test]
fn test_distribute_yield_defaults_to_merchant() {
    let (env, vault, merchant, _token, strategy, tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&500_000);
    MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&25_000);
    vault.harvest_yield();

    let before = tc.balance(&merchant);
    assert_eq!(vault.get_yield_recipient(), merchant);
    vault.distribute_yield();
    assert_eq!(tc.balance(&merchant), before + 25_000);
}

#[test]
fn test_set_yield_recipient_to_vault_fails() {
    let (_env, vault, _merchant, _token, _strategy, _tc) = setup_with_strategy(0, 10_000);
    assert_eq!(
        vault.try_set_yield_recipient(&vault.address),
        Err(Ok(Error::SelfTransfer))
    );
}

#[test]
fn test_distribute_yield_when_paused_fails() {
    let (env, vault, merchant, _token, strategy, _tc) = setup_with_strategy(0, 10_000);
    vault.deposit(&merchant, &1_000_000);
    vault.deploy_to_yield(&500_000);
    MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&25_000);
    vault.harvest_yield();
    vault.pause();
    assert_eq!(vault.try_distribute_yield(), Err(Ok(Error::Paused)));
}

// ── Property: deployed principal is always fully redeemable ───────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// For any deposit, deployment and accrued yield, a refund of up to the
    /// full deposit succeeds, and principal is conserved:
    /// `liquid principal + deployed principal == deposit - refunded`.
    #[test]
    fn prop_principal_always_redeemable(
        deposit in 1_000i128..5_000_000,
        deploy_pct in 0u32..=100,
        yield_amt in 0i128..500_000,
        refund_pct in 1u32..=100,
    ) {
        let (env, vault, merchant, _token, strategy, tc) = setup_with_strategy(0, 10_000);
        vault.deposit(&merchant, &deposit);
        let deploy = deposit * deploy_pct as i128 / 100;
        if deploy > 0 {
            vault.deploy_to_yield(&deploy);
        }
        if yield_amt > 0 {
            MockYieldStrategyClient::new(&env, &strategy).simulate_yield(&yield_amt);
        }

        let refund = (deposit * refund_pct as i128 / 100).max(1);
        let buyer = Address::generate(&env);
        let payment_ref = BytesN::from_array(&env, &[0xAB; 32]);
        vault.refund(&payment_ref, &buyer, &refund, &0, &refund, &None, &0);

        let info = vault.get_yield_info();
        prop_assert_eq!(tc.balance(&buyer), refund);
        let liquid_principal = tc.balance(&vault.address) - info.harvested_yield;
        prop_assert_eq!(liquid_principal + info.deployed_principal, deposit - refund);
        prop_assert!(info.deployed_principal >= 0 && info.harvested_yield >= 0);
    }
}
