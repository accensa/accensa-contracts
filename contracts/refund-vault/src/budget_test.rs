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

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;

fn load_wasm(path: &str) -> std::vec::Vec<u8> {
    std::fs::read(path).expect(
        "refund-vault wasm not found; run `cargo build -p refund-vault \
         --target wasm32v1-none --release` before the budget tests",
    )
}

/// Deploys the real `refund_vault` WASM, mints a test token, and initializes the
/// vault so `deposit` / `refund` behave exactly as they do on-chain.
fn setup(env: &Env, window: u32) -> (RefundVaultClient<'static>, Address, Address) {
    let wasm = load_wasm("../../target/wasm32v1-none/release/refund_vault.wasm");
    let id = env.register_contract_wasm(None, &wasm);
    let client = RefundVaultClient::new(env, &id);
    env.mock_all_auths();
    let merchant = Address::generate(env);
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(env, &token).mint(&merchant, &FLOAT);
    client.initialize(&merchant, &token, &window);
    (client, merchant, token)
}

#[test]
#[budget_cpu_lt(1_500_000)]
fn budget_deposit() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    env.cost_estimate().budget().reset_unlimited();
    client.deposit(&merchant, &600_000);
}

#[test]
#[budget_cpu_lt(2_000_000)]
fn budget_refund() {
    let env = Env::default();
    let (client, merchant, _token) = setup(&env, 100);
    client.deposit(&merchant, &500_000);
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    env.cost_estimate().budget().reset_unlimited();
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);
}
