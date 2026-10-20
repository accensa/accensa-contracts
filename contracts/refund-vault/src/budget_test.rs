#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `RefundVault`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`). The measured baselines live in `docs/BENCHMARKS.md`; update
//! them deliberately after re-measuring, never automatically.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p refund-vault --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`/`std::vec::Vec`; bring `std` in explicitly.
extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use crate::test_helpers::vault_init;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    contract, contractimpl, testutils::Address as _, token::StellarAssetClient, Address, Bytes,
    BytesN, Env,
};

const FLOAT: i128 = 1_000_000;

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "refund-vault wasm not found; run `cargo build -p refund-vault \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Minimal contract that wraps `deploy_v2`, so tests can deploy the vault
/// wasm from inside a real contract context.
#[contract]
pub struct TestDeployer;

#[contractimpl]
impl TestDeployer {
    pub fn deploy(env: Env, wasm_hash: BytesN<32>, salt: BytesN<32>, init: VaultInit) -> Address {
        env.deployer()
            .with_current_contract(salt)
            .deploy_v2(wasm_hash, (init,))
    }
}

/// Deploys the real `refund_vault` WASM, mints a test token, and initializes the
/// vault so `deposit` / `refund` behave exactly as they do on-chain.
fn setup(env: &Env, window: u32) -> (RefundVaultClient<'static>, Address, Address) {
    let wasm = load_wasm(env, "../../target/wasm32v1-none/release/refund_vault.wasm");
    let wasm_hash = env.deployer().upload_contract_wasm(wasm);
    env.mock_all_auths();
    let merchant = Address::generate(env);
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(env, &token).mint(&merchant, &FLOAT);
    let init = vault_init(env, &merchant, &token, window);
    let salt = BytesN::from_array(env, &[0u8; 32]);
    // `deploy_v2`/`with_current_contract` only run inside a contract context
    // (the test harness has no current contract ID), so the vault is deployed
    // through a tiny natively-registered deployer contract.
    let deployer = env.register(TestDeployer, ());
    let id = TestDeployerClient::new(env, &deployer).deploy(&wasm_hash, &salt, &init);
    let client = RefundVaultClient::new(env, &id);
    (client, merchant, token)
}

#[test]
#[budget_cpu_lt(1_600_000)]
fn budget_deposit() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    env.cost_estimate().budget().reset_unlimited();
    client.deposit(&merchant, &600_000, &None);
}

#[test]
#[budget_cpu_lt(2_870_000)]
fn budget_refund() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    client.deposit(&merchant, &500_000, &None);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    env.cost_estimate().budget().reset_unlimited();
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None, &0);
}

#[contract]
pub struct MockStrategy;

#[contractimpl]
impl MockStrategy {
    pub fn deposit(_env: Env, _amount: i128) -> Result<(), Error> {
        Ok(())
    }
    pub fn withdraw(_env: Env, principal: i128) -> Result<(i128, i128), Error> {
        Ok((principal, 0))
    }
    pub fn harvest(_env: Env) -> Result<i128, Error> {
        Ok(0)
    }
    pub fn total_balance(_env: Env) -> i128 {
        0
    }
}

// Baseline: 11_128_842 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 12_798_168 (rounded to 12_800_000).
#[test]
#[budget_cpu_lt(12_800_000)]
fn budget_claim_batch() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    client.deposit(&merchant, &500_000, &None);
    let mut claims = soroban_sdk::Vec::new(&env);
    for i in 0..10u8 {
        let buyer = Address::generate(&env);
        claims.push_back(RefundClaim {
            payment_ref: BytesN::from_array(&env, &[i; 32]),
            recipient: buyer,
            amount: 10_000,
            paid_at_ledger: 0,
            payment_amount: 10_000,
            vdf_proof: None,
        });
    }
    env.cost_estimate().budget().reset_unlimited();
    client.claim_batch(&claims, &0);
}

// Baseline: 1_906_686 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 2_192_689 (rounded to 2_195_000).
#[test]
#[budget_cpu_lt(2_195_000)]
fn budget_deploy_to_yield() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    client.deposit(&merchant, &1_000_000, &None);

    let strategy = env.register(MockStrategy, ());
    client.approve_yield_strategy(&strategy);
    client.set_yield_strategy(&strategy);
    client.set_reserve_ratio(&2000);
    client.set_max_deploy_ratio(&8000);

    env.cost_estimate().budget().reset_unlimited();
    client.deploy_to_yield(&500_000);
}

// Baseline: 952_850 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 1_095_778 (rounded to 1_100_000).
#[test]
#[budget_cpu_lt(1_100_000)]
fn budget_extend_refund_ttl() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    client.deposit(&merchant, &500_000, &None);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None, &0);

    env.cost_estimate().budget().reset_unlimited();
    client.extend_refund_ttl(&payment_ref);
}
