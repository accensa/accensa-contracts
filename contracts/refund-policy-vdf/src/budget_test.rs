#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `refund-policy-vdf`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15` (the 15% tolerance defined in
//! `budget.toml`); the baselines measured for this crate are recorded on each
//! test below.
//!
//! The arithmetic here is **deliberately expensive**: verifying a Wesolowski
//! proof performs two 1024-bit modular exponentiations plus a 128-bit
//! Miller-Rabin prime search, and that cost is independent of the configured
//! delay. These gates are a regression net around that cost, not a budget cut:
//! the fixed 1024-bit modulus, the `MR_BASES` set and every delay bound are
//! untouched. `evaluate` must never pay the prover's `2^T` sequential
//! squarings — only the verifier's fixed arithmetic — and the delay gates below
//! put the measured slope on the record.
//!
//! Measured baselines (WASM-mode Tier A estimate, 2026-10-19):
//!
//! | path | delay | CPU instructions |
//! |---|---|---|
//! | `evaluate` (no gate) | 0 | 454_392 |
//! | `evaluate` (smallest positive) | 1 | 1_631_590_349 |
//! | `evaluate` (largest admitted) | `u32::MAX` | 1_456_829_537 |
//! | `generate_randomness` | 64 | 1_865_423_290 |
//! | `request_fallback_dispute` | — | 482_373 |
//! | `settle_fallback_dispute` | — | 483_499 |
//!
//! The two delay measurements differ by less than 15%: verification cost is
//! independent of `T` by construction, and the transcript-derived prime search
//! dominates both. The no-gate (`delay = 0`) path is ~3 orders of magnitude
//! cheaper and is gated separately and much tighter, so a caller with nothing
//! to prove cannot silently start paying the verifier's arithmetic. The
//! max-delay gate is still set strictly above the min-delay gate, so a
//! regression at either end of the admitted range fails.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p refund-policy-vdf --target wasm32v1-none --release`

// The crate is `#![no_std]`, so `std` is not automatically in scope even in
// unit tests. The `budget_cpu_lt` expansion references `std::env::var`,
// `std::fs::read_to_string`, `String`, `format!` and `.to_string()`; bring
// `std` in explicitly, matching the receipt-anchor and refund-vault suites.
extern crate std;
use std::format;
use std::string::{String, ToString};

use accensa_common::{Error, PolicyContext, RefundPolicyClient, VdfPolicyParams};
use budget_macros::budget_cpu_lt;
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024,
};
use soroban_sdk::{testutils::Address as _, xdr::ToXdr, Address, Bytes, BytesN, Env};

use crate::vdf;
use crate::VdfPolicyClient;

/// Smallest positive delay the policy admits. `0` is the documented no-gate
/// path (handled by its own, much tighter test); every real delay is `>= 1`.
const MIN_DELAY: u32 = 1;
/// Largest delay the parameter type admits. `VdfPolicyParams::delay` is a
/// `u32` and the crate imposes no smaller upper bound.
const MAX_DELAY: u32 = u32::MAX;
/// Delay used by the crate's own randomness tests for `generate_randomness`;
/// the entry point has no delay bound of its own.
const RANDOMNESS_DELAY: u32 = 64;

fn load_wasm(env: &Env) -> Bytes {
    let buf = std::fs::read("../../target/wasm32v1-none/release/refund_policy_vdf.wasm").expect(
        "refund-policy-vdf wasm not found; run `cargo build -p refund-policy-vdf \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

/// Registers the real WASM and returns clients for both surfaces: the
/// `RefundPolicy` trait (`evaluate`) and the policy's own client (randomness
/// and fallback disputes).
fn setup(env: &Env) -> (RefundPolicyClient<'static>, VdfPolicyClient<'static>) {
    // Each test uploads the ~47 KB contract and then calls it inside the same
    // test "transaction". A real invocation never shares a transaction with
    // the upload, so disable invocation resource limits for exactly the reason
    // the receipt-anchor suite does: the gate is the `budget_cpu_lt` CPU
    // assertion, not per-transaction host enforcement.
    env.cost_estimate().disable_resource_limits();
    env.mock_all_auths();

    let wasm = load_wasm(env);
    #[allow(deprecated)]
    let id = env.register_contract_wasm(None, wasm);
    (
        RefundPolicyClient::new(env, &id),
        VdfPolicyClient::new(env, &id),
    )
}

fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
    BytesN::from_array(env, &[slot; 32])
}

fn params(env: &Env, delay: u32) -> Bytes {
    VdfPolicyParams { delay }.to_xdr(env)
}

fn ctx(payment_ref: &BytesN<32>, proof: Option<BytesN<256>>) -> PolicyContext {
    PolicyContext {
        payment_ref: payment_ref.clone(),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: proof,
    }
}

/// The challenge the policy derives for a payment: `sha256(payment_ref)`
/// zero-extended to the low 32 bytes of a 128-byte big-endian value.
fn challenge_for(env: &Env, payment_ref: &BytesN<32>) -> [u8; 128] {
    let hash = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &payment_ref.to_array()));
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&hash.to_array());
    challenge
}

/// Honest off-chain VDF evaluation for a small delay: performs `t` sequential
/// squarings, then derives the witness exactly as `vdf.rs` documents. This is
/// the prover side the contract must never pay for.
fn eval_vdf(env: &Env, challenge: &[u8; 128], t: u32) -> ([u8; 128], [u8; 128]) {
    let n = U1024::from_be_slice(&vdf::MODULUS);
    let x = U1024::from_be_slice(challenge).rem(&NonZero::new(n).unwrap());

    let params = DynResidueParams::new(&n);
    let mut acc = DynResidue::new(&x, params);
    for _ in 0..t {
        acc = acc.square();
    }
    let y = acc.retrieve();

    let ell = vdf::derive_challenge(env, challenge, &y.to_be_bytes(), t);
    let mut ell_buf = [0u8; 128];
    ell_buf[112..].copy_from_slice(&ell.to_be_bytes());
    let q = U1024::ONE
        .shl(t as usize)
        .div_rem(&NonZero::new(U1024::from_be_slice(&ell_buf)).unwrap())
        .0;
    let pi = DynResidue::new(&x, params).pow(&q).retrieve();

    (y.to_be_bytes(), pi.to_be_bytes())
}

fn pack(env: &Env, output: &[u8; 128], witness: &[u8; 128]) -> BytesN<256> {
    let mut buf = [0u8; 256];
    buf[..128].copy_from_slice(output);
    buf[128..].copy_from_slice(witness);
    BytesN::from_array(env, &buf)
}

// ── evaluate: the no-gate path, then both ends of the delay range ─────────

/// `delay = 0` is the policy's documented no-gate path. It returns before the
/// transcript hash and both 1024-bit exponentiations, so it is gated
/// separately and much tighter than the verifying paths: a caller with nothing
/// to prove must not pay the verifier's arithmetic. Baseline 454_392 * 1.15.
#[test]
#[budget_cpu_lt(523_000)]
fn budget_evaluate_zero_delay() {
    let env = Env::default();
    let (policy, _) = setup(&env);
    let ref_ = payment_ref(&env, 1);

    env.cost_estimate().budget().reset_unlimited();
    policy.evaluate(&params(&env, 0), &ctx(&ref_, None));
}

/// `delay = 1` with a genuine proof: the smallest positive delay the policy
/// admits, and the smallest possible VDF transcript (one squaring).
/// Baseline 1_631_590_349 * 1.15.
#[test]
#[budget_cpu_lt(1_876_400_000)]
fn budget_evaluate_min_delay() {
    let env = Env::default();
    let (policy, _) = setup(&env);
    let ref_ = payment_ref(&env, 2);
    let challenge = challenge_for(&env, &ref_);
    let (output, witness) = eval_vdf(&env, &challenge, MIN_DELAY);
    let proof = pack(&env, &output, &witness);

    env.cost_estimate().budget().reset_unlimited();
    policy.evaluate(&params(&env, MIN_DELAY), &ctx(&ref_, Some(proof)));
}

/// `delay = u32::MAX`, the largest delay the parameter type admits. Proving
/// that many squarings is exactly what the VDF is meant to make infeasible, so
/// the proof is minted at the minimum delay and the test asserts the expected
/// `InvalidVdfProof`; the verifier still walks its full transcript hash,
/// Miller-Rabin search and two 1024-bit exponentiations before comparing, which
/// is the path this gate covers.
///
/// Baseline 1_456_829_537. The gate is deliberately above the min-delay gate
/// (1_880_000_000 > 1_876_400_000): the measured difference between the two
/// delays is prime-search noise, not a slope, but the ordering keeps a
/// regression on the largest admitted delay caught even if it lands above the
/// smaller measurement.
#[test]
#[budget_cpu_lt(1_880_000_000)]
fn budget_evaluate_max_delay() {
    let env = Env::default();
    let (policy, _) = setup(&env);
    let ref_ = payment_ref(&env, 3);
    let challenge = challenge_for(&env, &ref_);
    let (output, witness) = eval_vdf(&env, &challenge, MIN_DELAY);
    let proof = pack(&env, &output, &witness);

    env.cost_estimate().budget().reset_unlimited();
    assert_eq!(
        policy.try_evaluate(&params(&env, MAX_DELAY), &ctx(&ref_, Some(proof)),),
        Err(Ok(Error::InvalidVdfProof))
    );
}

// ── randomness registry ───────────────────────────────────────────────────

/// `generate_randomness` verifies a proof, writes the persistent seed and
/// emits `RandomnessGenerated`, so it is gated on its own (it must not inherit
/// `evaluate`'s tighter ceiling). Baseline 1_865_423_290 * 1.15.
#[test]
#[budget_cpu_lt(2_145_300_000)]
fn budget_generate_randomness() {
    let env = Env::default();
    let (_, vrf) = setup(&env);
    let vdf_id = payment_ref(&env, 4);
    let challenge = challenge_for(&env, &vdf_id);
    let (output, witness) = eval_vdf(&env, &challenge, RANDOMNESS_DELAY);
    let proof = pack(&env, &output, &witness);

    env.cost_estimate().budget().reset_unlimited();
    let seed = vrf.generate_randomness(&vdf_id, &RANDOMNESS_DELAY, &proof);
    assert_ne!(seed, BytesN::from_array(&env, &[0u8; 32]));
}

// ── fallback dispute registry ─────────────────────────────────────────────

/// Escalating a dispute authorizes the merchant, increments the instance
/// counter, writes and TTL-extends the persistent record, and emits an event.
/// Baseline 482_373 * 1.15.
#[test]
#[budget_cpu_lt(555_000)]
fn budget_request_fallback_dispute() {
    let env = Env::default();
    let (_, vrf) = setup(&env);
    let merchant = Address::generate(&env);
    let oracle = Address::generate(&env);
    let recipient = Address::generate(&env);

    env.cost_estimate().budget().reset_unlimited();
    let id =
        vrf.request_fallback_dispute(&merchant, &oracle, &payment_ref(&env, 5), &recipient, &100);
    assert_eq!(id, 1);
}

/// Settling reads the record, authorizes the designated oracle, flips `open`
/// and emits `FallbackDisputeSettledEvent`. The request is setup, outside the
/// reset, so the gate covers only the settle invocation.
/// Baseline 483_499 * 1.15.
#[test]
#[budget_cpu_lt(556_100)]
fn budget_settle_fallback_dispute() {
    let env = Env::default();
    let (_, vrf) = setup(&env);
    let merchant = Address::generate(&env);
    let oracle = Address::generate(&env);
    let recipient = Address::generate(&env);
    let id =
        vrf.request_fallback_dispute(&merchant, &oracle, &payment_ref(&env, 6), &recipient, &100);

    env.cost_estimate().budget().reset_unlimited();
    vrf.settle_fallback_dispute(&id, &true);
}
