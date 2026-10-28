#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `MultisigAccount`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro (issue #598).
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`), rounded up to the nearest thousand.
//!
//! The gates pin the two scaling entry points named in the issue:
//! - `execute_batch` at batch sizes 1, 5 and 20 — the loop over the
//!   caller-supplied `Vec<Call>` grows linearly with batch length, so each
//!   size gets its own ceiling and the 20-call ceiling is deliberately *not*
//!   20x the 1-call ceiling (see the marginal-cost note on
//!   `execute_batch_20_calls_cpu_budget`).
//! - `rotate_signers_and_threshold` for a 10-signer rotation — the signer-list
//!   scans scale with the signer set.
//!
//! Re-measure the baselines with `measure_execute_batch_and_rotate_cpu` (it
//! prints `BENCHMARK` lines) whenever one of the gates trips after a
//! legitimate change; never widen a ceiling without a fresh measurement.
//!
//! The wasm artifact is resolved at test runtime, in this order:
//! `MULTISIG_ACCOUNT_WASM` (full path to the artifact, explicit override) →
//! `CARGO_TARGET_DIR` (absolute as given, or relative to the workspace root)
//! joined with `wasm32v1-none/release/multisig_account.wasm` → the first
//! ancestor of the test binary that contains that relative path (honours
//! `--target-dir`, since test binaries live under `<target-dir>/debug/deps`) →
//! the default workspace `target/` path.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p multisig-account --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`/`std::vec::Vec`; bring `std` in explicitly.
extern crate std;
use std::format;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};

use budget_macros::budget_cpu_lt;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig},
    vec, Address, Env, Symbol, Vec,
};

use crate::{Call, MultisigAccountClient};

/// Location of the artifact inside any cargo target directory.
const WASM_REL: &str = "wasm32v1-none/release/multisig_account.wasm";

/// Default path (relative to this crate's root, where `cargo test` runs) of
/// the real artifact under measurement.
const WASM_PATH: &str = "../../target/wasm32v1-none/release/multisig_account.wasm";

/// Resolves `multisig_account.wasm` without reading any environment: every
/// input is passed in, so the resolution order is unit-testable.
///
/// First match wins:
/// 1. the explicit `MULTISIG_ACCOUNT_WASM` override, verbatim;
/// 2. `CARGO_TARGET_DIR` joined with `wasm32v1-none/release/…` — absolute as
///    given, relative resolved against the workspace root (two levels above
///    this crate, where `cargo` resolves a relative `CARGO_TARGET_DIR`);
/// 3. the first ancestor of the test binary's path that actually contains
///    the artifact (honours `--target-dir` with no env var, because the test
///    binary lives under `<target-dir>/debug/deps`);
/// 4. the workspace-default `default_path`.
fn resolve_wasm_path(
    override_path: Option<&str>,
    cargo_target_dir: Option<&str>,
    exe_ancestors: &[PathBuf],
    default_path: &Path,
) -> PathBuf {
    if let Some(over) = override_path {
        return PathBuf::from(over);
    }
    if let Some(dir) = cargo_target_dir {
        let dir = Path::new(dir);
        return if dir.is_absolute() {
            dir.join(WASM_REL)
        } else {
            Path::new("../..").join(dir).join(WASM_REL)
        };
    }
    for ancestor in exe_ancestors {
        let candidate = ancestor.join(WASM_REL);
        if candidate.exists() {
            return candidate;
        }
    }
    default_path.to_path_buf()
}

/// Reads the real `multisig_account.wasm` built for `wasm32v1-none --release`,
/// resolved at runtime (not `include_bytes!`) so the same test binary can
/// measure an artifact built into any cargo target directory or at an
/// explicit override path.
///
/// The gates deliberately load the compiled WASM instead of registering
/// `MultisigAccount` natively: the CPU slope of `execute_batch`'s loop and of
/// `rotate_signers_and_threshold`'s signer scans only exists in the on-chain
/// binary.
fn load_wasm() -> std::vec::Vec<u8> {
    let override_path = std::env::var("MULTISIG_ACCOUNT_WASM").ok();
    let cargo_target_dir = std::env::var("CARGO_TARGET_DIR").ok();
    let exe_ancestors: std::vec::Vec<PathBuf> = std::env::current_exe()
        .map(|exe| exe.ancestors().map(Path::to_path_buf).collect())
        .unwrap_or_default();
    let chosen = resolve_wasm_path(
        override_path.as_deref(),
        cargo_target_dir.as_deref(),
        &exe_ancestors,
        Path::new(WASM_PATH),
    );
    std::fs::read(&chosen).unwrap_or_else(|e| {
        panic!(
            "multisig-account wasm not found at `{}` ({e}); build it with \
             `cargo build -p multisig-account --target wasm32v1-none --release` \
             before the budget tests, or set MULTISIG_ACCOUNT_WASM to the full \
             path of the artifact. Consulted, in order: MULTISIG_ACCOUNT_WASM={}, \
             CARGO_TARGET_DIR={} (then {} ancestors of the test binary, then \
             the default `{}`)",
            chosen.display(),
            override_path.as_deref().unwrap_or("<unset>"),
            cargo_target_dir.as_deref().unwrap_or("<unset>"),
            exe_ancestors.len(),
            WASM_PATH,
        )
    })
}

/// Registers the real `multisig_account` WASM as an account with `n_signers`
/// signers (threshold = `n_signers`), constructor included, and returns its
/// address plus the generated signer addresses.
///
/// The entry point under measurement shares this test "transaction" with the
/// WASM upload below (~47 KB of writes against a 66 KB Mainnet per-transaction
/// write budget, plus the constructor's own CPU). A real deployment uploads
/// contract code in its own transaction and never shares one with the call
/// under test, so with Mainnet limits enforced it is that harness overhead,
/// not the contract, that would trip the gate. Disable invocation resource
/// limits here, as the SDK recommends for tests whose resource usage is
/// intentional: the gates this file is actually responsible for are the
/// `budget_cpu_lt` CPU assertions, none of which rely on host enforcement.
/// Every test also points the CPU budget at just the measured call via
/// `budget().reset_unlimited()`.
fn register_account(env: &Env, n_signers: u32) -> (Address, Vec<Address>) {
    env.cost_estimate().disable_resource_limits();
    let wasm = load_wasm();
    let mut signers: Vec<Address> = Vec::new(env);
    for _ in 0..n_signers {
        signers.push_back(Address::generate(env));
    }
    let id = env.register(wasm.as_slice(), (signers.clone(), n_signers));
    (id, signers)
}

/// A batch of `n` identical read-only calls: each one is a real wasm → wasm
/// cross-contract invocation of `get_threshold` on `target`, so the measured
/// slope is exactly the per-call cost the `execute_batch` loop adds.
fn batch_calls(env: &Env, target: &Address, n: u32) -> Vec<Call> {
    let mut calls: Vec<Call> = Vec::new(env);
    for _ in 0..n {
        calls.push_back(Call {
            contract: target.clone(),
            function: Symbol::new(env, "get_threshold"),
            args: vec![env],
        });
    }
    calls
}

/// Builds an account of `n_signers` signers plus a second instance of the same
/// WASM used as the callee, with auth mocked for the measured call.
fn setup(env: &Env, n_signers: u32) -> (MultisigAccountClient<'static>, Address) {
    let (account, _) = register_account(env, n_signers);
    let (target, _) = register_account(env, 1);
    env.mock_all_auths();
    (MultisigAccountClient::new(env, &account), target)
}

// ── execute_batch scaling gates (#598) ──────────────────────────────────────
//
// Baselines measured with `measure_execute_batch_and_rotate_cpu` against
// `multisig_account.wasm` (WASM-mode estimate), threshold = baseline * 1.15
// rounded up to the nearest thousand.

// Baseline 1_142_118 (MEASURED via measure_execute_batch_and_rotate_cpu,
// WASM-mode) * 1.15 = 1_313_436 -> 1_314_000.
#[test]
#[budget_cpu_lt(1_314_000)]
fn execute_batch_1_call_cpu_budget() {
    let env = Env::default();
    let (client, target) = setup(&env, 3);
    let calls = batch_calls(&env, &target, 1);
    env.cost_estimate().budget().reset_unlimited();
    let results = client.execute_batch(&calls);
    assert_eq!(results.len(), 1);
}

// Baseline 3_455_938 (MEASURED) * 1.15 = 3_974_329 -> 3_975_000.
// Cross-check of the slope: (cpu5 - cpu1) / 4 = (3_455_938 - 1_142_118) / 4
// = 578_455 per call, matching the (cpu20 - cpu1) / 19 derivation below.
#[test]
#[budget_cpu_lt(3_975_000)]
fn execute_batch_5_calls_cpu_budget() {
    let env = Env::default();
    let (client, target) = setup(&env, 3);
    let calls = batch_calls(&env, &target, 5);
    env.cost_estimate().budget().reset_unlimited();
    let results = client.execute_batch(&calls);
    assert_eq!(results.len(), 5);
}

// Baseline 12_133_048 (MEASURED) * 1.15 = 13_953_005 -> 13_954_000.
//
// This ceiling is deliberately NOT 20x the 1-call ceiling (20 * 1_314_000 =
// 26_280_000): the scaling term is only the loop's per-call marginal cost,
// measured from the 1/5/20-call runs:
//
//   marginal = (cpu20 - cpu1) / 19
//            = (12_133_048 - 1_142_118) / 19
//            = 10_990_930 / 19 = 578_470 CPU instructions per additional call
//   cross-check: (cpu20 - cpu5) / 15 = (12_133_048 - 3_455_938) / 15
//              = 578_474 per call (agrees within 4 instructions),
//                (cpu5 - cpu1) / 4 = 578_455 per call
//
// Fixed entry-point overhead ~= cpu1 - 1 marginal call = 563_648 CPU
// instructions (require_auth, argument decoding, result `Vec` allocation);
// each extra element in the batch then costs ~578_470. The headroom here is
// one modest 15% on top of the total, so a future per-call regression (an
// extra host hop or storage read inside the loop) increases the slope: cpu20
// would exceed cpu1 + 19 * 578_474 and trip THIS gate while the 1-call gate
// stays green — attributable to the per-call path, not the entry point.
#[test]
#[budget_cpu_lt(13_954_000)]
fn execute_batch_20_calls_cpu_budget() {
    let env = Env::default();
    let (client, target) = setup(&env, 3);
    let calls = batch_calls(&env, &target, 20);
    env.cost_estimate().budget().reset_unlimited();
    let results = client.execute_batch(&calls);
    assert_eq!(results.len(), 20);
}

// ── rotate_signers_and_threshold scaling gate (#598) ────────────────────────

// Baseline 5_187_764 (MEASURED) * 1.15 = 5_965_929 -> 5_966_000.
//
// A full 10-signer rotation: all 10 original signers are removed and 10 new
// ones added in one call. This is the worst case for the signer-set scans in
// `signers::rotate_signers_and_threshold` (the `remaining` list is rebuilt
// once per removal: 10 + 9 + ... + 1 comparisons) plus 20 persistent
// storage writes, the `SignerList` rewrite and the `SignersRotated` event
// carrying both 10-element vectors.
#[test]
#[budget_cpu_lt(5_966_000)]
fn rotate_10_signers_cpu_budget() {
    let env = Env::default();
    let (account, original) = register_account(&env, 10);
    let client = MultisigAccountClient::new(&env, &account);
    let mut to_add: Vec<Address> = Vec::new(&env);
    for _ in 0..10 {
        to_add.push_back(Address::generate(&env));
    }
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    // The generated client panics (test fails) if the contract returns Err,
    // so a successful return is the post-condition here.
    client.rotate_signers_and_threshold(&to_add, &original, &10);
}

/// Prints the measured CPU for every gate in this file so baselines (and the
/// per-call marginal cost of `execute_batch`) can be re-derived deliberately
/// after a legitimate change, never guessed. See the module header.
#[test]
fn measure_execute_batch_and_rotate_cpu() {
    for n in [1u32, 5, 20] {
        // One Env per size: the budget counters are cumulative per Env, so a
        // fresh Env (and no snapshot files from the loop) keeps each reading
        // independent.
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        let (client, target) = setup(&env, 3);
        let calls = batch_calls(&env, &target, n);
        env.cost_estimate().budget().reset_unlimited();
        client.execute_batch(&calls);
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        std::println!("BENCHMARK execute_batch n={n} cpu={cpu}");
    }

    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let (account, original) = register_account(&env, 10);
    let client = MultisigAccountClient::new(&env, &account);
    let mut to_add: Vec<Address> = Vec::new(&env);
    for _ in 0..10 {
        to_add.push_back(Address::generate(&env));
    }
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    client.rotate_signers_and_threshold(&to_add, &original, &10);
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    std::println!("BENCHMARK rotate_signers_and_threshold signers=10 cpu={cpu}");
}

// ── wasm artifact path resolver (#598) ─────────────────────────────────────
//
// Pure-function tests of `resolve_wasm_path`: the inputs are passed in, no
// environment variable is read.

#[test]
fn wasm_path_explicit_override_wins() {
    let got = resolve_wasm_path(
        Some("/tmp/ms-override/multisig_account.wasm"),
        Some("/custom/target"),
        &[PathBuf::from("/exe/deps")],
        Path::new(WASM_PATH),
    );
    assert_eq!(got, PathBuf::from("/tmp/ms-override/multisig_account.wasm"));
}

#[test]
fn wasm_path_cargo_target_dir_next() {
    // Absolute CARGO_TARGET_DIR is used as given…
    let got = resolve_wasm_path(
        None,
        Some("/custom/target"),
        &[PathBuf::from("/exe/deps")],
        Path::new(WASM_PATH),
    );
    assert_eq!(got, Path::new("/custom/target").join(WASM_REL));
    // …and a relative one resolves against the workspace root (`../..` from
    // the crate root where `cargo test` runs).
    let got = resolve_wasm_path(None, Some("my-target"), &[], Path::new(WASM_PATH));
    assert_eq!(got, Path::new("../../my-target").join(WASM_REL));
}

#[test]
fn wasm_path_ancestor_walk_then_default() {
    // An ancestor that does not hold the artifact is skipped…
    let base = std::env::temp_dir().join("multisig_budget_path_resolver_test");
    let artifact = base.join(WASM_REL);
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, b"stub").unwrap();
    let got = resolve_wasm_path(
        None,
        None,
        &[PathBuf::from("/nonexistent-exe-deps"), base.clone()],
        Path::new(WASM_PATH),
    );
    assert_eq!(got, artifact);
    let _ = std::fs::remove_dir_all(&base);

    // …and with no ancestor holding it, the workspace default is returned.
    let got = resolve_wasm_path(
        None,
        None,
        &[PathBuf::from("/nonexistent-exe-deps")],
        Path::new(WASM_PATH),
    );
    assert_eq!(got, Path::new(WASM_PATH));
}
