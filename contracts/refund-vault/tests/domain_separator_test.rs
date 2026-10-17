use accensa_common::{Error, VaultInit};
use refund_vault::{DataKey, RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn test_get_domain_separator_before_initialization_returns_not_initialized() {
    let env = Env::default();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);

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
        env.storage().instance().remove(&DataKey::DomainSeparator);
    });

    assert_eq!(
        client.try_get_domain_separator(),
        Err(Ok(Error::NotInitialized)),
        "absent DomainSeparator must return NotInitialized error arm, not trap"
    );
}

#[test]
fn test_get_domain_separator_after_initialization_succeeds() {
    let env = Env::default();
    let merchant = Address::generate(&env);
    let token = Address::generate(&env);

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

    let res = client.try_get_domain_separator();
    assert!(
        res.is_ok(),
        "initialized vault must return domain separator"
    );
    assert_eq!(res.unwrap().unwrap(), client.get_domain_separator());
}
