//! Issue #685: `deploy_to_yield` answers a missing `DataKey::Token` with a
//! typed error instead of a host trap.
//!
//! The vault reads its payment token from instance storage on the deployment
//! path. The read used to be an `unwrap()`, so a call that reached it without a
//! stored token aborted with an untyped `WasmRuntimeError` while every other
//! failure in `deploy_to_yield` returns a variant of `Error`.
//!
//! The test approves and activates a strategy — so the call reaches the token
//! read — then removes the stored token and asserts the returned variant.

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient};

#[test]
fn deploy_to_yield_without_a_stored_token_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    let vault_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(&env, &vault_id);

    // A whitelisted, active strategy: the pause, amount, admin and strategy
    // arms all pass, so the call reaches the token read. The address is never
    // invoked — the deployment fails before any transfer.
    let strategy = Address::generate(&env);
    vault.approve_yield_strategy(&strategy);
    vault.set_yield_strategy(&strategy);

    env.as_contract(&vault_id, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    assert_eq!(
        vault.try_deploy_to_yield(&1_000),
        Err(Ok(Error::NotInitialized))
    );
}
