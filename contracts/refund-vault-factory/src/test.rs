#![cfg(test)]

use crate::{Error, RefundVaultFactory, RefundVaultFactoryClient};
use refund_window_policy::RefundWindowPolicy;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, IntoVal, Symbol,
};

const FLOAT: i128 = 1_000_000;

/// The `RefundVault` wasm, built by `cargo build -p refund-vault --target
/// wasm32v1-none --release` before these tests run (CI does this in the same
/// step that installs the wasm32v1-none target; see `.github/workflows/ci.yml`
/// and the README's "Build and test" section for the local equivalent).
/// `RefundVaultFactory::deploy` factory-deploys vaults from a real installed
/// wasm hash, so these tests need the same wasm the unit tests do.
mod vault_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/refund_vault.wasm");
}

struct Setup {
    env: Env,
    factory: RefundVaultFactoryClient<'static>,
    merchant: Address,
    token: Address,
    policy: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let factory_id = env.register(RefundVaultFactory, ());
    let factory = RefundVaultFactoryClient::new(&env, &factory_id);

    let policy_id = env.register(RefundWindowPolicy, ());

    Setup {
        env,
        factory,
        merchant,
        token,
        policy: policy_id,
    }
}

/// Upload the real vault wasm and point the factory at it.
fn init(s: &Setup) {
    let vault_wasm_hash = s.env.deployer().upload_contract_wasm(vault_wasm::WASM);
    s.factory.initialize(&s.merchant, &vault_wasm_hash, &s.policy);
}

#[test]
fn test_initialize_and_getters() {
    let s = setup();
    let vault_wasm_hash = s.env.deployer().upload_contract_wasm(vault_wasm::WASM);
    s.factory.initialize(&s.merchant, &vault_wasm_hash, &s.policy);

    assert_eq!(s.factory.get_vault_wasm_hash(), Some(vault_wasm_hash));
    assert_eq!(s.factory.get_default_policy(), Some(s.policy.clone()));
    assert_eq!(s.factory.get_vault_count(), 0);
}

#[test]
fn test_double_initialize_fails() {
    let s = setup();
    init(&s);
    let vault_wasm_hash = s.env.deployer().upload_contract_wasm(vault_wasm::WASM);
    assert_eq!(
        s.factory.try_initialize(&s.merchant, &vault_wasm_hash, &s.policy),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_deploy_before_initialize_fails() {
    let s = setup();
    assert_eq!(
        s.factory.try_deploy(&s.merchant, &s.token, &100),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_deploy_creates_initialized_vault() {
    let s = setup();
    init(&s);

    // The factory-deployed vault is a real, already-initialised contract: a
    // deposit then a refund flow works against it end-to-end.
    let vault_addr = s.factory.deploy(&s.merchant, &s.token, &100);
    assert_eq!(s.factory.get_vault(&0), Some(vault_addr.clone()));
    assert_eq!(s.factory.get_vault(&1), None);
    assert_eq!(s.factory.get_vault_count(), 1);

    let vault = vault_wasm::Client::new(&s.env, &vault_addr);
    vault.deposit(&s.merchant, &600_000);
    let token_client = TokenClient::new(&s.env, &s.token);
    assert_eq!(token_client.balance(&vault_addr), 600_000);

    let payment_ref = BytesN::from_array(&s.env, &[7u8; 32]);
    let buyer = Address::generate(&s.env);
    vault.refund(&payment_ref, &buyer, &100, &0, &100);
    let record = vault.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 100);
}

#[test]
fn test_deployed_vault_calls_into_bound_policy() {
    let s = setup();
    init(&s);

    let vault_addr = s.factory.deploy(&s.merchant, &s.token, &100);
    let vault = vault_wasm::Client::new(&s.env, &vault_addr);
    assert_eq!(vault.get_refund_policy(), s.policy);

    // Outside the 100-ledger window the bound policy rejects the refund with
    // WindowExpired — proving the vault routes its window check through the
    // policy contract rather than evaluating it inline. (The exact code is
    // asserted via accensa_common::Error in refund-vault's own unit tests;
    // the imported-wasm client decodes the raw value generically.)
    vault.deposit(&s.merchant, &600_000);
    let payment_ref = BytesN::from_array(&s.env, &[8u8; 32]);
    let buyer = Address::generate(&s.env);
    s.env.ledger().with_mut(|li| li.sequence_number = 201);
    assert!(
        vault.try_refund(&payment_ref, &buyer, &100, &100, &100).is_err(),
        "refund past the payment's window must be rejected by the bound policy"
    );
}

#[test]
fn test_deploy_with_policy_binds_custom_policy() {
    let s = setup();
    init(&s);

    // A different (here equal-behaviour) policy contract, deployed separately.
    let custom_policy = s.env.register(RefundWindowPolicy, ());
    let vault_addr = s
        .factory
        .deploy_with_policy(&s.merchant, &s.token, &100, &custom_policy);
    assert_eq!(s.factory.get_vault_count(), 1);

    let vault = vault_wasm::Client::new(&s.env, &vault_addr);
    assert_eq!(
        vault.get_refund_policy(),
        custom_policy,
        "vault must be bound to the explicitly supplied policy, not the default"
    );
}

#[test]
fn test_deploy_emits_vault_created_event() {
    let s = setup();
    init(&s);

    let vault_addr = s.factory.deploy(&s.merchant, &s.token, &100);

    let mut data = soroban_sdk::Map::<soroban_sdk::Val, soroban_sdk::Val>::new(&s.env);
    data.set(
        Symbol::new(&s.env, "vault_address").into_val(&s.env),
        vault_addr.into_val(&s.env),
    );
    data.set(
        Symbol::new(&s.env, "merchant").into_val(&s.env),
        s.merchant.clone().into_val(&s.env),
    );
    data.set(
        Symbol::new(&s.env, "token").into_val(&s.env),
        s.token.clone().into_val(&s.env),
    );
    data.set(
        Symbol::new(&s.env, "refund_window_ledgers").into_val(&s.env),
        100u32.into_val(&s.env),
    );
    data.set(
        Symbol::new(&s.env, "policy").into_val(&s.env),
        s.policy.clone().into_val(&s.env),
    );

    let events = s
        .env
        .events()
        .all()
        .filter_by_contract(&s.factory.address);
    assert_eq!(
        events,
        vec![
            &s.env,
            (
                s.factory.address.clone(),
                vec![
                    &s.env,
                    Symbol::new(&s.env, "vault_created_event").into_val(&s.env),
                    0u64.into_val(&s.env),
                ],
                data.into_val(&s.env),
            )
        ]
    );
}

#[test]
fn test_registry_tracks_multiple_vaults() {
    let s = setup();
    init(&s);

    let v0 = s.factory.deploy(&s.merchant, &s.token, &100);
    let v1 = s.factory.deploy(&s.merchant, &s.token, &200);
    let v2 = s.factory.deploy(&s.merchant, &s.token, &300);

    assert_eq!(s.factory.get_vault_count(), 3);
    assert_ne!(v0, v1);
    assert_ne!(v1, v2);
    assert_eq!(s.factory.get_vault(&0), Some(v0));
    assert_eq!(s.factory.get_vault(&1), Some(v1));
    assert_eq!(s.factory.get_vault(&2), Some(v2));
    assert_eq!(s.factory.get_vault(&3), None);
}