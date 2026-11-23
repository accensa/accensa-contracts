#![cfg(test)]

//! Test coverage for `RefundVault::set_token` when DataKey::Token is absent (issue #684).

use accensa_common::{Error, VaultInit};
use refund_vault::{DataKey, RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn test_set_token_when_token_uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let new_token = Address::generate(&env);

    let vault_id = env.register(
        RefundVault,
        (VaultInit {
            merchant,
            token,
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &vault_id);

    env.as_contract(&vault_id, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    assert_eq!(
        client.try_set_token(&new_token),
        Err(Ok(Error::NotInitialized)),
        "absent Token in set_token must return NotInitialized error, not trap"
    );
}
