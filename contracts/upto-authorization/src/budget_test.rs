#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `UptoAuthorization`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15`.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p upto-authorization --target wasm32v1-none --release`

extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    testutils::Address as _, token::StellarAssetClient, Address, Bytes, BytesN, Env,
};

const TOKEN_SUPPLY: i128 = 10_000_000;

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "upto-authorization wasm not found; run `cargo build -p upto-authorization \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

struct Setup<'a> {
    client: UptoAuthorizationClient<'a>,
    buyer: Address,
    seller: Address,
}

fn setup(env: &Env) -> Setup<'_> {
    env.mock_all_auths();
    env.cost_estimate().disable_resource_limits();

    let admin = Address::generate(env);
    let buyer = Address::generate(env);
    let seller = Address::generate(env);
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();

    StellarAssetClient::new(env, &token).mint(&buyer, &TOKEN_SUPPLY);

    let wasm = load_wasm(
        env,
        "../../target/wasm32v1-none/release/upto_authorization.wasm",
    );
    #[allow(deprecated)]
    let contract = env.register_contract_wasm(None, wasm);
    let client = UptoAuthorizationClient::new(env, &contract);
    client.initialize(&admin, &token);

    Setup {
        client,
        buyer,
        seller,
    }
}

fn pid(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

// Baseline: 713_619 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 820_662 (rounded to 825_000).
#[test]
#[budget_cpu_lt(825_000)]
fn budget_authorize() {
    let env = Env::default();
    let s = setup(&env);
    let p = pid(&env, 1);
    env.cost_estimate().budget().reset_unlimited();
    s.client.authorize(&p, &s.buyer, &s.seller, &1000, &1000);
}

// Baseline: 985_389 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 1_133_197 (rounded to 1_135_000).
#[test]
#[budget_cpu_lt(1_135_000)]
fn budget_settle_at_cap() {
    let env = Env::default();
    let s = setup(&env);
    let p = pid(&env, 2);
    s.client.authorize(&p, &s.buyer, &s.seller, &1000, &1000);
    env.cost_estimate().budget().reset_unlimited();
    s.client.settle(&p, &1000);
}

// Baseline: 985_389 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 1_133_197 (rounded to 1_135_000).
#[test]
#[budget_cpu_lt(1_135_000)]
fn budget_settle_at_min_boundary() {
    let env = Env::default();
    let s = setup(&env);
    let p = pid(&env, 3);
    s.client.authorize(&p, &s.buyer, &s.seller, &1000, &1000);
    env.cost_estimate().budget().reset_unlimited();
    s.client.settle(&p, &1);
}
