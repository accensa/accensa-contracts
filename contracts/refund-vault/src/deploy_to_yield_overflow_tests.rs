//! Boundary tests for `deploy_to_yield`'s checked arithmetic (issue #719).
//!
//! `deploy_to_yield` computed `total_value = token_balance + deployed -
//! harvested`, the reserve and max-deploy ratios, and `deployed + amount` with
//! bare operators. With `[profile.release]` enabling `overflow-checks` and
//! `panic = "abort"`, a non-representable intermediate aborted the host call
//! instead of answering with a typed error. They now run through
//! `accensa_common::math::{add_amounts, sub_amounts, mul_ratio}`, whose
//! failures map to `Error::MathOverflow`.
//!
//! Each test stores a stored-principal value that makes the named intermediate
//! unrepresentable while every earlier check passes, so the call answers with
//! the typed variant rather than trapping.

use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient};

const DEPOSIT: i128 = 10;

fn setup() -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &DEPOSIT);

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);
    vault.deposit(&merchant, &DEPOSIT, &None);

    // Approved and active so both checks before the arithmetic pass. The
    // address is never invoked: every failing path below returns before the
    // deposit call.
    let strategy = Address::generate(&env);
    vault.approve_yield_strategy(&strategy);
    vault.set_yield_strategy(&strategy);

    (env, vault, vault_id)
}

fn set_deployed(env: &Env, vault_id: &Address, deployed: i128) {
    env.as_contract(vault_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::DeployedPrincipal, &deployed);
    });
}

#[test]
fn overflowing_total_value_returns_math_overflow() {
    let (env, vault, vault_id) = setup();
    // `token_balance + deployed` overflows before `harvested` is subtracted.
    set_deployed(&env, &vault_id, i128::MAX);

    assert_eq!(vault.try_deploy_to_yield(&1), Err(Ok(Error::MathOverflow)));
}

#[test]
fn overflowing_reserve_ratio_product_returns_math_overflow() {
    let (env, vault, vault_id) = setup();
    vault.set_reserve_ratio(&10_000);
    // `total_value` itself fits, but `total_value * 10_000` does not.
    set_deployed(&env, &vault_id, i128::MAX / 10);

    assert_eq!(vault.try_deploy_to_yield(&1), Err(Ok(Error::MathOverflow)));
}

#[test]
fn overflowing_max_deploy_ratio_product_returns_math_overflow() {
    let (env, vault, vault_id) = setup();
    // A zero reserve ratio leaves the reserve term at zero, so the call
    // reaches the max-deploy product with a fitting `total_value`.
    vault.set_max_deploy_ratio(&10_000);
    set_deployed(&env, &vault_id, i128::MAX / 10);

    assert_eq!(vault.try_deploy_to_yield(&1), Err(Ok(Error::MathOverflow)));
}
