#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `StreamVault`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`), rounded up to the nearest thousand.
//!
//! ### Measured CPU Baselines (WASM-mode)
//! - `create_stream`: 787,668 (threshold: 906,000)
//! - `claim_stream` (partially-elapsed): 798,521 (threshold: 919,000)
//! - `claim_stream` (fully-elapsed): 742,117 (threshold: 854,000)
//! - `pause_stream`: 562,273 (threshold: 647,000)
//! - `resume_stream`: 566,683 (threshold: 652,000)
//! - `cancel_stream`: 947,760 (threshold: 1,090,000)
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p stream-vault --target wasm32v1-none --release`

extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Bytes, BytesN, Env,
};

const BUYER_FUNDS: i128 = 10_000_000;
const START: u32 = 100;
const STOP: u32 = 200;
const RATE: i128 = 10;
const DEPOSIT: i128 = 1_000; // (STOP - START) * RATE

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "stream-vault wasm not found; run `cargo build -p stream-vault \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Minimal contract that wraps `deploy_v2`, so tests can deploy the stream vault
/// wasm from inside a real contract context.
#[contract]
pub struct TestDeployer;

#[contractimpl]
impl TestDeployer {
    pub fn deploy(
        env: Env,
        wasm_hash: BytesN<32>,
        salt: BytesN<32>,
        merchant: Address,
        token: Address,
    ) -> Address {
        env.deployer()
            .with_current_contract(salt)
            .deploy_v2(wasm_hash, (merchant, token))
    }
}

/// Deploys the real `stream_vault` WASM, mints a test token, and initializes the
/// vault so entry points behave exactly as they do on-chain.
fn setup(env: &Env) -> (StreamVaultClient<'static>, Address, Address, Address) {
    let wasm = load_wasm(env, "../../target/wasm32v1-none/release/stream_vault.wasm");
    env.cost_estimate().disable_resource_limits();
    let wasm_hash = env.deployer().upload_contract_wasm(wasm);
    env.mock_all_auths();
    let merchant = Address::generate(env);
    let buyer = Address::generate(env);
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(env, &token).mint(&buyer, &BUYER_FUNDS);

    let salt = BytesN::from_array(env, &[0u8; 32]);
    let deployer = env.register(TestDeployer, ());
    let id = TestDeployerClient::new(env, &deployer).deploy(&wasm_hash, &salt, &merchant, &token);
    let client = StreamVaultClient::new(env, &id);
    (client, merchant, buyer, token)
}

// Baseline 787_668 * 1.15.
#[test]
#[budget_cpu_lt(906_000)]
fn budget_create_stream() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    env.cost_estimate().budget().reset_unlimited();
    client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
}

// Baseline 798_521 * 1.15.
#[test]
#[budget_cpu_lt(919_000)]
fn budget_claim_stream_partially_elapsed() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
    // Advance to mid-stream (50 ledgers elapsed => 500 streamed of 1000 deposit).
    env.ledger().set_sequence_number(START + 50);
    env.cost_estimate().budget().reset_unlimited();
    client.claim_stream(&stream_id);
}

// Baseline 742_117 * 1.15.
#[test]
#[budget_cpu_lt(854_000)]
fn budget_claim_stream_fully_elapsed() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
    // Advance to past stop ledger (fully elapsed => full deposit claimed and stream closed).
    env.ledger().set_sequence_number(STOP + 10);
    env.cost_estimate().budget().reset_unlimited();
    client.claim_stream(&stream_id);
}

// Baseline 562_273 * 1.15.
#[test]
#[budget_cpu_lt(647_000)]
fn budget_pause_stream() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
    env.ledger().set_sequence_number(START + 50);
    env.cost_estimate().budget().reset_unlimited();
    client.pause_stream(&stream_id);
}

// Baseline 566_683 * 1.15.
#[test]
#[budget_cpu_lt(652_000)]
fn budget_resume_stream() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
    env.ledger().set_sequence_number(START + 50);
    client.pause_stream(&stream_id);
    // Advance 10 ledgers while paused.
    env.ledger().set_sequence_number(START + 60);
    env.cost_estimate().budget().reset_unlimited();
    client.resume_stream(&stream_id);
}

// Baseline 947_760 * 1.15.
#[test]
#[budget_cpu_lt(1_090_000)]
fn budget_cancel_stream() {
    let env = Env::default();
    let (client, _, buyer, _) = setup(&env);
    env.ledger().set_sequence_number(START - 10);
    let stream_id = client.create_stream(&buyer, &START, &STOP, &RATE, &DEPOSIT);
    // Cancel mid-stream so both merchant_amount and buyer_amount are > 0.
    env.ledger().set_sequence_number(START + 50);
    env.cost_estimate().budget().reset_unlimited();
    client.cancel_stream(&stream_id);
}
