#![cfg(test)]

//! Tests for `RefundVaultFactory` (issue #129).
//!
//! `deploy_vault` deploys real `refund_vault` wasm instances, so these tests
//! need the vault wasm built first — see the README's "Build and test"
//! section and `.github/workflows/ci.yml`, exactly like
//! `contracts/refund-vault/tests/integration_test.rs`. The vault wasm hash is
//! uploaded to the ledger once, then the factory rows deployments through
//! `deploy_v2`.

use accensa_common::{Error as CommonError, VaultInit};
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use refund_vault::RefundVaultClient;
use refund_vault_factory::{Error, RefundVaultFactory, RefundVaultFactoryClient};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val, Vec,
};

mod vault_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/refund_vault.wasm");
}

const FLOAT: i128 = 1_000_000;

struct Ctx {
    env: Env,
    factory: RefundVaultFactoryClient<'static>,
    factory_id: Address,
    admin: Address,
    merchant: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let wasm_hash = env.deployer().upload_contract_wasm(vault_wasm::WASM);
    // Constructor-wired: the factory is fully initialized at deploy time.
    let factory_id = env.register(
        RefundVaultFactory,
        (admin.clone(), wasm_hash, None::<Address>, None::<Address>),
    );
    let factory = RefundVaultFactoryClient::new(&env, &factory_id);

    Ctx {
        env,
        factory,
        factory_id,
        admin,
        merchant,
    }
}

fn token_for(env: &Env, admin: &Address) -> Address {
    env.register_stellar_asset_contract_v2(admin.clone())
        .address()
}

fn vault_init(_env: &Env, merchant: &Address, token: &Address, window: u32) -> VaultInit {
    VaultInit {
        merchant: merchant.clone(),
        token: token.clone(),
        time_policy: None,
        vdf_policy: None,
        fee_bps: 0,
        fee_recipient: None,
        refund_window: window,
        deadline: 0,
        vdf_delay: 0,
    }
}

fn zero_wasm_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0u8; 32])
}

#[test]
fn deploy_vault_lands_on_deterministic_unique_addresses() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let token = token_for(&env, &merchant);

    let a = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let b = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));

    assert_ne!(a, b, "each deployment must advance the salt counter");
    assert_eq!(factory.get_next_salt(), 2);
    assert_eq!(
        factory.get_vaults(),
        vec![&env, a.clone(), b.clone()],
        "deployed vaults are tracked for operator inspection"
    );
}

#[test]
fn two_merchants_get_distinct_salt_families() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let merchant_b = Address::generate(&env);
    let token = token_for(&env, &merchant);

    let a1 = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let a2 = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let b1 = factory.deploy_vault(&vault_init(&env, &merchant_b, &token, 100));

    assert_ne!(a1, b1);
    assert_ne!(a2, b1);
    assert_ne!(a1, a2);
}

#[test]
fn factory_defaults_wire_policies_when_merchant_leaves_them_unset() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let time_id = env.register(TimePolicy, ());
    let vdf_id = env.register(VdfPolicy, ());
    factory.set_time_policy_contract(&Some(time_id.clone()));
    factory.set_vdf_policy_contract(&Some(vdf_id.clone()));

    let token = token_for(&env, &merchant);
    let vault = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let client = RefundVaultClient::new(&env, &vault);

    // The factory's global policies are shared with the new vault.
    assert_eq!(client.get_time_policy_contract(), Some(time_id));
    assert_eq!(client.get_vdf_policy_contract(), Some(vdf_id));

    // Factory admin and vault merchant are distinct roles.
    assert_eq!(client.get_admin(), merchant.clone());
    assert_eq!(client.get_refund_window(), 100);
}

#[test]
fn merchant_requested_policy_wins_over_factory_default() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let default_time = env.register(TimePolicy, ());
    let merchant_time = env.register(TimePolicy, ());
    factory.set_time_policy_contract(&Some(default_time));

    let token = token_for(&env, &merchant);
    let mut init = vault_init(&env, &merchant, &token, 100);
    init.time_policy = Some(merchant_time.clone());

    let vault = factory.deploy_vault(&init);
    let client = RefundVaultClient::new(&env, &vault);
    assert_eq!(client.get_time_policy_contract(), Some(merchant_time));
}

#[cfg(any())]
fn not_initialized_state_is_unreachable_with_constructor() {}

#[test]
fn double_initialization_fails() {
    let Ctx {
        env,
        factory,
        admin,
        ..
    } = setup();
    let wasm_hash = env.deployer().upload_contract_wasm(vault_wasm::WASM);

    assert_eq!(
        factory.try_initialize(&admin, &wasm_hash, &None, &None),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn rotations_require_admin_and_reject_zero_wasm() {
    let Ctx {
        env,
        factory,
        admin,
        ..
    } = setup();

    // Non-admin calls are rejected.
    env.set_auths(&[]);
    assert!(factory.try_set_time_policy_contract(&None).is_err());
    assert!(factory.try_set_vdf_policy_contract(&None).is_err());
    assert!(factory.try_set_vault_wasm(&zero_wasm_hash(&env)).is_err());
    env.mock_all_auths();

    // Admin may clear the policy defaults...
    factory.set_time_policy_contract(&None);
    factory.set_vdf_policy_contract(&None);
    assert_eq!(factory.get_time_policy_contract(), None);
    assert_eq!(factory.get_vdf_policy_contract(), None);

    // ...but never the wasm hash, and never to zeros.
    assert_eq!(
        factory.try_set_vault_wasm(&zero_wasm_hash(&env)),
        Err(Ok(Error::InvalidWasmHash))
    );

    let wasm_hash = env.deployer().upload_contract_wasm(vault_wasm::WASM);
    factory.set_vault_wasm(&wasm_hash);
    assert_eq!(factory.get_vault_wasm(), Some(wasm_hash));
    let _ = admin;
}

#[test]
fn vault_deployed_event_carries_merchant_topic() {
    let Ctx {
        env,
        factory,
        factory_id,
        merchant,
        ..
    } = setup();
    let token = token_for(&env, &merchant);
    let vault = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));

    let mut data = Map::new(&env);
    data.set(Symbol::new(&env, "vault"), vault.clone());
    let expected: Vec<(Address, Vec<Val>, Val)> = vec![
        &env,
        (
            factory_id.clone(),
            vec![
                &env,
                Symbol::new(&env, "vault_deployed_event").into_val(&env),
                merchant.clone().into_val(&env),
            ],
            data.into_val(&env),
        ),
    ];
    assert_eq!(env.events().all(), expected);
}

#[test]
fn deployed_vault_honors_window_via_factory_time_policy() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let time_id = env.register(TimePolicy, ());
    factory.set_time_policy_contract(&Some(time_id));

    let token = token_for(&env, &merchant);
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let client = RefundVaultClient::new(&env, &vault);

    client.deposit(&merchant, &FLOAT);
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);

    // Scroll inside the active window: the factory-wired time policy
    // evaluates this claim exactly as a directly-deployed vault would.
    env.ledger().with_mut(|li| {
        li.sequence_number = 50;
        li.timestamp = 50;
    });
    client.refund(&payment_ref, &buyer, &100_000, &0, &100_000, &None);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn deployed_vault_refuses_when_time_policy_unconfigured() {
    let Ctx {
        env,
        factory,
        merchant,
        ..
    } = setup();
    let token = token_for(&env, &merchant);
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    // Active window gate, factory has no global time policy either: the vault
    // accepts the deployment but fails the claim closed with #317.
    let vault = factory.deploy_vault(&vault_init(&env, &merchant, &token, 100));
    let client = RefundVaultClient::new(&env, &vault);

    client.deposit(&merchant, &FLOAT);
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000, &None),
        Err(Ok(CommonError::PolicyContractsNotConfigured))
    );
}
