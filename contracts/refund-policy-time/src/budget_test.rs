#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `refund-policy-time` (issue #602), using
//! Tollcraft's `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`). The baseline each gate was derived from is recorded on the
//! test and in the table below.
//!
//! `TimePolicy::evaluate` (`contracts/refund-policy-time/src/lib.rs:52`) picks a
//! schema from the params blob and, for `TimeOraclePolicyParams`, delegates to
//! `oracle::evaluate` (`oracle.rs:158`) → `resolve` (`:110`) → an *external*
//! `get_delivery_status` call (`:61`) before `is_usable` (`:102`) and
//! `check_time_gates` (`:142`). A cross-contract invocation is the most expensive
//! thing a refund policy can do, and until now nothing in the workspace pinned
//! its ceiling. The three gates below are deliberately separate numbers so a
//! regression in the fallback path cannot hide behind a blended ceiling:
//!
//! | configuration | what `evaluate` pays for | measured CPU | gate |
//! |---|---|---|---|
//! | no oracle configured (`TimePolicyParams`) | XDR decode + two integer comparisons | 397_906 | 458_000 |
//! | reachable oracle, fresh report | decode + publish/serialize args + cross-contract call + `is_usable` + `OracleResolutionApplied` event | 485_512 | 558_000 |
//! | oracle traps → window fallback | the same cross-contract call, its failure caught, then `check_time_gates` | 435_029 | 500_000 |
//!
//! (measured 2026-10-21, soroban-sdk 27.0.6, `refund_policy_time.wasm` after
//! `scripts/optimize_wasm.sh`; each gate is that baseline × 1.15).
//!
//! The spread between the three is the cost of the oracle hop itself: +37_123
//! instructions when the call traps and is caught, +87_606 when it returns a
//! report that is decoded, checked against `max_report_age` and answered with an
//! published event. Because each configuration carries its own number, a
//! regression in the fallback path cannot hide behind the oracle-answering
//! ceiling, and vice versa.
//!
//! The oracle is the workspace's existing `MockDeliveryOracle` fixture from
//! `src/oracle_test.rs`, registered as a native test contract: the WASM→native
//! cross-contract hop is cheaper than a WASM→WASM one, so these gates are a
//! regression net around the *shape* of the path (call, decode, event publish),
//! not a prediction of mainnet cost — which is Tier B's `cargo budget-report`
//! job. The behaviours they measure are the ones `oracle_test.rs` asserts
//! functionally: `lost_parcel_is_refunded_even_outside_the_window`,
//! `trapping_oracle_falls_back_to_the_window`, and the plain-params path.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p refund-policy-time --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`, and this
// file's own helper uses `std::fs`; bring `std` in explicitly, matching the
// receipt-anchor and refund-policy-vdf suites.
extern crate std;
use std::format;
use std::string::{String, ToString};

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams};
use budget_macros::budget_cpu_lt;
use soroban_sdk::{testutils::Ledger, xdr::ToXdr, Bytes, BytesN, Env};

use crate::oracle::{DeliveryReport, DeliveryStatus, TimeOraclePolicyParams};
use crate::oracle_test::{MockDeliveryOracle, MockDeliveryOracleClient};

/// Ledger sequence and wall clock the policy evaluates against, matching the
/// `oracle_test.rs` fixture so both suites describe the same claim.
const LEDGER: u32 = 1_000;
const NOW: u64 = 50_000;
/// Refund window in ledgers, and the oracle report's accepted maximum age in
/// seconds — same values as the functional fixture.
const WINDOW: u32 = 100;
const MAX_AGE: u64 = 3_600;
/// A report 60 seconds old: inside `MAX_AGE` and not in the future, so
/// `is_usable` accepts it.
const REPORTED_AT: u64 = NOW - 60;

fn load_wasm(env: &Env) -> Bytes {
    let buf = std::fs::read("../../target/wasm32v1-none/release/refund_policy_time.wasm").expect(
        "refund-policy-time wasm not found; run `cargo build -p refund-policy-time \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Registers the real `refund_policy_time.wasm`. The policy is stateless, so
/// there is no constructor to run and no router to wire up.
fn setup_policy(env: &Env) -> RefundPolicyClient<'static> {
    // The harness uploads the ~26 KB WASM in the same test "transaction" as the
    // call it measures, which no real invocation does; the gate is the
    // `budget_cpu_lt` CPU assertion, not per-transaction host enforcement, so
    // invocation limits are disabled here exactly as the receipt-anchor and
    // refund-policy-vdf suites do. Each test points the budget at just the
    // measured call with `budget().reset_unlimited()`.
    env.cost_estimate().disable_resource_limits();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = LEDGER;
        li.timestamp = NOW;
    });
    #[allow(deprecated)]
    let id = env.register_contract_wasm(None, load_wasm(env));
    RefundPolicyClient::new(env, &id)
}

/// A claim for `payment_ref` made `paid_at_ledger`, evaluated against the
/// ledger and clock this suite fixed above.
fn ctx(env: &Env, paid_at_ledger: u32) -> PolicyContext {
    PolicyContext {
        payment_ref: BytesN::from_array(env, &[9u8; 32]),
        amount: 100,
        paid_at_ledger,
        current_ledger: LEDGER,
        timestamp: NOW,
        vdf_proof: None,
    }
}

/// Store `status` as the oracle's answer and return the params blob that points
/// the policy at it.
fn with_oracle(env: &Env, status: DeliveryStatus) -> Bytes {
    let oracle = env.register(MockDeliveryOracle, ());
    MockDeliveryOracleClient::new(env, &oracle).set_report(&DeliveryReport {
        payment_ref: BytesN::from_array(env, &[9u8; 32]),
        status,
        timestamp: REPORTED_AT,
        proof: BytesN::from_array(env, &[0xAB; 32]),
    });
    TimeOraclePolicyParams {
        window: WINDOW,
        deadline: 0,
        oracle,
        max_report_age: MAX_AGE,
    }
    .to_xdr(env)
}

/// The params blob for an oracle that always traps.
fn with_trapping_oracle(env: &Env) -> Bytes {
    let oracle = env.register(MockDeliveryOracle, ());
    MockDeliveryOracleClient::new(env, &oracle).set_broken();
    TimeOraclePolicyParams {
        window: WINDOW,
        deadline: 0,
        oracle,
        max_report_age: MAX_AGE,
    }
    .to_xdr(env)
}

// ── evaluate, three configurations ──────────────────────────────────────────

// Baseline 397_906 (measured 2026-10-21) * 1.15.
#[test]
#[budget_cpu_lt(458_000)]
fn budget_evaluate_no_oracle() {
    let env = Env::default();
    let policy = setup_policy(&env);
    let params = TimePolicyParams {
        window: WINDOW,
        deadline: 0,
    }
    .to_xdr(&env);

    env.cost_estimate().budget().reset_unlimited();
    // Inside the window: the plain clock gates admit the claim, and no
    // cross-contract call happens at all.
    assert_eq!(
        policy.try_evaluate(&params, &ctx(&env, LEDGER - 10)),
        Ok(Ok(()))
    );
}

// Baseline 485_512 (measured 2026-10-21) * 1.15.
#[test]
#[budget_cpu_lt(558_000)]
fn budget_evaluate_oracle_fresh_report() {
    let env = Env::default();
    let policy = setup_policy(&env);
    let params = with_oracle(&env, DeliveryStatus::Lost);

    env.cost_estimate().budget().reset_unlimited();
    // Outside the window, but the oracle says Lost: the refund is admitted on
    // the oracle's answer, which also publishes `OracleResolutionApplied`.
    assert_eq!(
        policy.try_evaluate(&params, &ctx(&env, LEDGER - WINDOW - 1)),
        Ok(Ok(()))
    );
}

// Baseline 435_029 (measured 2026-10-21) * 1.15.
#[test]
#[budget_cpu_lt(500_000)]
fn budget_evaluate_oracle_traps() {
    let env = Env::default();
    let policy = setup_policy(&env);
    let params = with_trapping_oracle(&env);

    env.cost_estimate().budget().reset_unlimited();
    // The oracle call traps, `resolve` maps that to `Fallback`, and the claim is
    // decided by the window alone: 37_123 instructions more than the no-oracle
    // path for a call that produced no answer at all, which is what this gate
    // exists to keep bounded.
    assert_eq!(
        policy.try_evaluate(&params, &ctx(&env, LEDGER - WINDOW - 1)),
        Err(Ok(Error::WindowExpired))
    );
}
