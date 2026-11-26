#![cfg(test)]

//! Test that read_policy_cache returns typed Error::NotInitialized when DataKey::Token is absent.

use accensa_common::{Error, VaultInit};
use refund_vault::{DataKey, RefundVault, RefundVaultClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, BytesN, Env};

#[test]
fn read_policy_cache_missing_token_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);

    // Remove DataKey::Token to simulate uninitialized/missing Token state in policy cache
    env.as_contract(&contract, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    let res = client.try_refund(
        &merchant,
        &BytesN::from_array(&env, &[1u8; 32]),
        &Address::generate(&env),
        &100,
        &10,
        &100,
        &None,
        &0,
    );
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}
