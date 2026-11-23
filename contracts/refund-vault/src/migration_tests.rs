//! Tests for the RefundVault state migration runbook (`docs/REFUND_VAULT_MIGRATION.md`).
//!
//! Validates the version advancement and validation rules of `migrate_state`,
//! the post-upgrade verification through `get_storage_version`, and handling of
//! legacy and uninitialized states.

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::test_helpers::vault_init;
use crate::{DataKey, Error, RefundVault, RefundVaultClient};

fn setup_vault(env: &Env) -> (Address, RefundVaultClient<'_>) {
    let merchant = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();

    let vault_id = env.register(RefundVault, (vault_init(env, &merchant, &token, 17_280),));
    let vault = RefundVaultClient::new(env, &vault_id);
    (vault_id, vault)
}

#[test]
fn initial_storage_version_returns_one() {
    let env = Env::default();
    env.mock_all_auths();
    let (_vault_id, vault) = setup_vault(&env);

    assert_eq!(vault.get_storage_version(), 1);
}

#[test]
fn migrate_state_advances_version_and_updates_get_storage_version() {
    let env = Env::default();
    env.mock_all_auths();
    let (_vault_id, vault) = setup_vault(&env);

    // Initial version is 1.
    assert_eq!(vault.get_storage_version(), 1);

    // Runbook Step 3: Call migrate_state(target_version)
    vault.migrate_state(&2);

    // Runbook Step 6: Verify get_storage_version returns the updated target version
    assert_eq!(vault.get_storage_version(), 2);

    // Advance to version 3
    vault.migrate_state(&3);
    assert_eq!(vault.get_storage_version(), 3);
}

#[test]
fn migrate_state_rejects_target_equal_or_lower_than_current() {
    let env = Env::default();
    env.mock_all_auths();
    let (_vault_id, vault) = setup_vault(&env);

    // Current version is 1; target equal to current must fail
    assert_eq!(
        vault.try_migrate_state(&1),
        Err(Ok(Error::InvalidMigrationVersion))
    );

    // Target lower than current must fail
    assert_eq!(
        vault.try_migrate_state(&0),
        Err(Ok(Error::InvalidMigrationVersion))
    );

    // Advance to 3
    vault.migrate_state(&3);

    // Target 2 (lower) and 3 (equal) must fail
    assert_eq!(
        vault.try_migrate_state(&2),
        Err(Ok(Error::InvalidMigrationVersion))
    );
    assert_eq!(
        vault.try_migrate_state(&3),
        Err(Ok(Error::InvalidMigrationVersion))
    );
}

#[test]
fn migrate_state_run_twice_with_same_target_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (_vault_id, vault) = setup_vault(&env);

    // First migration to target version 2 succeeds
    vault.migrate_state(&2);
    assert_eq!(vault.get_storage_version(), 2);

    // Repeating the same target version is idempotency-safe: it rejects the second call
    // with InvalidMigrationVersion rather than silently claiming another migration.
    assert_eq!(
        vault.try_migrate_state(&2),
        Err(Ok(Error::InvalidMigrationVersion))
    );
    assert_eq!(vault.get_storage_version(), 2);
}

#[test]
fn legacy_deployment_without_marker_defaults_to_version_one() {
    let env = Env::default();
    env.mock_all_auths();
    let (vault_id, vault) = setup_vault(&env);

    // Simulate a legacy deployment where StorageVersion was not recorded
    env.as_contract(&vault_id, || {
        env.storage().instance().remove(&DataKey::StorageVersion);
    });

    // Runbook rule: legacy deployments without a marker are treated as version 1
    assert_eq!(vault.get_storage_version(), 1);

    // Attempting migration to 1 fails because current defaults to 1
    assert_eq!(
        vault.try_migrate_state(&1),
        Err(Ok(Error::InvalidMigrationVersion))
    );

    // Migrating to 2 succeeds and stores version 2
    vault.migrate_state(&2);
    assert_eq!(vault.get_storage_version(), 2);
}

#[test]
fn uninitialized_vault_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let (vault_id, vault) = setup_vault(&env);

    // Simulate missing admin / uninitialized instance storage
    env.as_contract(&vault_id, || {
        env.storage().instance().remove(&DataKey::Admin);
    });

    assert_eq!(
        vault.try_get_storage_version(),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(vault.try_migrate_state(&2), Err(Ok(Error::NotInitialized)));
}
