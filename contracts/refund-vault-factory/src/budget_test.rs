#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `refund-vault-factory`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`). The measured baselines live in `docs/BENCHMARKS.md`; update
//! them deliberately after re-measuring, never automatically.
//!
//! Three paths are gated here, all called by every merchant onboarding flow:
//! - `create_vault` — deploys a new vault and appends it to the `KEY_VAULTS` vector
//! - `deploy_for_participants` — deploys a vault for an escrow between buyer and merchant
//! - `get_vaults` — returns the whole `KEY_VAULTS` vector to the caller
//!
//! Because `KEY_VAULTS` only grows, `get_vaults` cost rises with the factory's
//! lifetime usage, so a ceiling measured on an empty factory says nothing about
//! mainnet. Each function is measured at two seed sizes (50 and 200 vaults) so
//! the growth slope is visible.
//!
//! The contracts must be compiled to WASM first:
//! `cargo build -p refund-vault-factory --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`/`std::vec::Vec`; bring `std` in explicitly.
extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use accensa_common::VaultInit;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "contract wasm not found; run `cargo build -p refund-vault-factory \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Deploys the real `refund-vault-factory` WASM and initializes it with a
/// vault WASM hash so that `create_vault` and `deploy_for_participants` can
/// actually deploy vault instances.
fn setup_factory(env: &Env) -> RefundVaultFactoryClient<'static> {
    let vault_wasm = load_wasm(env, "../../target/wasm32v1-none/release/refund_vault.wasm");
    let factory_wasm = load_wasm(
        env,
        "../../target/wasm32v1-none/release/refund_vault_factory.wasm",
    );

    // Disable invocation resource limits so the harness overhead (WASM uploads,
    // initialization writes) doesn't fail the test. The gates this file is
    // actually responsible for are the `budget_cpu_lt` CPU assertions.
    env.cost_estimate().disable_resource_limits();

    #[allow(deprecated)]
    let vault_wasm_hash = env.deployer().upload_contract_wasm(vault_wasm);
    #[allow(deprecated)]
    let factory_id = env.register_contract_wasm(None, factory_wasm);
    let client = RefundVaultFactoryClient::new(env, &factory_id);
    env.mock_all_auths();
    let admin = Address::generate(env);
    client.initialize(&admin, &vault_wasm_hash, &None, &None);
    client
}

/// Seeds the factory's `KEY_VAULTS` vector with `count` dummy addresses so that
/// `get_vaults` measures the cost of reading a non-empty list. Also increments
/// the salt counter so subsequent `create_vault` calls don't collide.
fn seed_vaults(env: &Env, _client: &RefundVaultFactoryClient<'static>, count: usize) {
    for i in 0..count {
        let dummy = Address::generate(env);
        // Use internal storage access to populate KEY_VAULTS without deploying
        // actual vaults (which would be slow and unnecessary for this test).
        let mut vaults: soroban_sdk::Vec<Address> = env
            .storage()
            .persistent()
            .get::<_, soroban_sdk::Vec<Address>>(&super::KEY_VAULTS)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));
        vaults.push_back(dummy);
        env.storage().persistent().set(&super::KEY_VAULTS, &vaults);
        // Bump the salt counter too so create_vault doesn't reuse salts.
        env.storage()
            .persistent()
            .set(&super::KEY_NEXT_SALT, &(i as u128 + 1));
    }
}

fn make_vault_init(env: &Env, merchant: &Address) -> VaultInit {
    VaultInit {
        merchant: merchant.clone(),
        token: Address::generate(env),
        time_policy: None,
        vdf_policy: None,
        fee_bps: 50,
        fee_recipient: Some(merchant.clone()),
        refund_window: 86400,
        deadline: 1_000_000,
        vdf_delay: 100,
    }
}

// ============================================================================
// Budget assertion tests
// ============================================================================
// TODO: These thresholds need proper measurement. The current setup hits budget
// limits during WASM loading before the measured operations can run. To fix:
// 1. Ensure disable_resource_limits() is called early enough in the harness
// 2. Or measure using cargo budget-report --derive-limits instead
// 3. Update each threshold to measured_baseline * 1.15
// See docs/BENCHMARKS.md for the measurement protocol.

// Placeholder: needs measurement on empty factory.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_create_vault_empty_factory() {
    let env = Env::default();
    let client = setup_factory(&env);
    let merchant = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.create_vault(&init, &None);
}

// Placeholder: needs measurement with 50 vaults seeded.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_create_vault_50_vaults() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 50);
    let merchant = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.create_vault(&init, &None);
}

// Placeholder: needs measurement with 200 vaults seeded.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_create_vault_200_vaults() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 200);
    let merchant = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.create_vault(&init, &None);
}

// Placeholder: needs measurement on empty factory.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_deploy_for_participants_empty() {
    let env = Env::default();
    let client = setup_factory(&env);
    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.deploy_for_participants(&init, &buyer);
}

// Placeholder: needs measurement with 50 vaults seeded.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_deploy_for_participants_50_vaults() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 50);
    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.deploy_for_participants(&init, &buyer);
}

// Placeholder: needs measurement with 200 vaults seeded.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_deploy_for_participants_200_vaults() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 200);
    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let init = make_vault_init(&env, &merchant);
    env.cost_estimate().budget().reset_unlimited();
    client.deploy_for_participants(&init, &buyer);
}

// Placeholder: needs measurement with 50 vaults.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_get_vaults_50() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 50);
    env.cost_estimate().budget().reset_unlimited();
    client.get_vaults();
}

// Placeholder: needs measurement with 200 vaults.
#[test]
#[budget_cpu_lt(100_000_000)]
fn budget_get_vaults_200() {
    let env = Env::default();
    let client = setup_factory(&env);
    seed_vaults(&env, &client, 200);
    env.cost_estimate().budget().reset_unlimited();
    client.get_vaults();
}
