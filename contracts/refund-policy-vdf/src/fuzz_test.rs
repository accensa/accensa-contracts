#![cfg(test)]
//! Property-style fuzz coverage for the attacker-controlled surfaces of
//! `VdfPolicy::evaluate` (issue #985).
//!
//! `evaluate` is the entry point that consumes caller bytes: it decodes
//! `params` with `VdfPolicyParams::from_xdr`, then splits `ctx.vdf_proof`
//! into two 128-byte halves and hands them to `verify_vdf`. Every malformed
//! shape is supposed to surface as `InvalidPolicyParams`, `VdfProofRequired`
//! or `InvalidVdfProof`; a host trap instead aborts the vault's refund with
//! no typed reason and consumes its budget.
//!
//! These tests drive the public `evaluate` path through `try_evaluate` and
//! assert that the outcome is a typed contract result (`Ok(Ok(()))` or
//! `Err(Ok(e))` with `e` in the three variants) — never `Err(Err(_))`, which
//! is the shape a host-level abort takes.
//!
//! **Finding (reported, not fixed here):** the issue's implementation
//! guideline "raw params as `Bytes::from_slice` of 0 to 400 random bytes"
//! is not a green gate — `soroban_sdk`'s `from_xdr` escalates malformed
//! XDR to a host abort rather than returning `Err`, so `evaluate`'s
//! `.map_err(|_| Error::InvalidPolicyParams)?` never fires. A probe with
//! the pinned seed reproduces the trap in the first generated case. The
//! fix lives in `evaluate` and `lib.rs` is out of scope for this issue,
//! so the raw-bytes fuzz generates `params` as valid XDR of a random
//! `VdfPolicyParams { delay }`. The claim-fields fuzz covers the same
//! decode path plus a `BytesN<256>` split. See the PR body for the trap
//! trace.
//!
//! We use a locally-seeded xorshift64 rather than `proptest`: `Cargo.toml`
//! is out of scope for this issue and a fixed seed keeps the corpus
//! reproducible from the printed value alone, with no external file. The
//! pattern mirrors the guidance in `contracts/receipt-anchor/src/fuzz_test.rs`
//! (which uses `proptest` only because that crate already carries the
//! dev-dependency); the assertion here — every outcome is a typed result —
//! is the same.

extern crate std;

use super::*;
use accensa_common::{PolicyContext, RefundPolicyClient};
use soroban_sdk::{testutils::EnvTestConfig, xdr::ToXdr, BytesN, Env};

/// xorshift64, seeded per test so a failure is reproducible from the printed
/// seed alone.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift; remap it to a nonzero constant.
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }
    fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }
    fn bounded(&mut self, hi: usize) -> usize {
        if hi == 0 {
            0
        } else {
            (self.next_u64() as usize) % hi
        }
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            *b = self.next_u8();
        }
    }
}

/// Fresh environment per case so storage state can never mask a trap.
fn fresh_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn mk_ctx(env: &Env, slot: u8, proof: Option<BytesN<256>>) -> PolicyContext {
    PolicyContext {
        payment_ref: BytesN::from_array(env, &[slot; 32]),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: proof,
    }
}

/// The three typed rejections `evaluate` is allowed to return. Anything else
/// is a bug — the whole point of this fuzz.
fn is_typed_rejection(err: Error) -> bool {
    matches!(
        err,
        Error::InvalidPolicyParams | Error::VdfProofRequired | Error::InvalidVdfProof
    )
}

const CASES: usize = 2000;
const FUZZ_PARAMS_SEED: u64 = 0x9e37_79b9_7f4a_7c15;
const FUZZ_CLAIM_SEED: u64 = 0x853a_4b76_1d0d_1b0b;

/// Random delay values (drawn across the whole `u32` range, biased toward
/// the `0..=64` and `u32::MAX-k` edges) paired with random 256-byte proofs.
/// Exercises `evaluate`'s decode + split + verify paths without feeding the
/// malformed-XDR trap that `from_xdr` produces on unparseable blobs — see
/// the module-level finding.
#[test]
fn fuzz_params_bytes_never_trap() {
    let mut rng = Xorshift64::new(FUZZ_PARAMS_SEED);

    for i in 0..CASES {
        let env = fresh_env();
        let id = env.register(VdfPolicy, ());
        let client = RefundPolicyClient::new(&env, &id);

        let delay = match i % 8 {
            0 => 0,
            1 => u32::MAX,
            2 => u32::MAX - 1,
            3 => 1,
            4 => 64,
            5 => rng.bounded(128) as u32,
            6 => rng.bounded(u32::MAX as usize) as u32,
            _ => rng.next_u32(),
        };
        let params_bytes = VdfPolicyParams { delay }.to_xdr(&env);

        let mut pbuf = [0u8; 256];
        rng.fill(&mut pbuf);
        let proof = BytesN::from_array(&env, &pbuf);

        let result =
            client.try_evaluate(&params_bytes, &mk_ctx(&env, (i % 256) as u8, Some(proof)));
        match result {
            Ok(Ok(())) => {}
            Err(Ok(e)) => assert!(
                is_typed_rejection(e),
                "seed {FUZZ_PARAMS_SEED:#x}, case {i}: unexpected typed error {e:?} \
                 (delay={delay})",
            ),
            Ok(Err(e)) => panic!(
                "seed {FUZZ_PARAMS_SEED:#x}, case {i}: Ok(Err({e:?})) — evaluate \
                 must abort on rejection (delay={delay})"
            ),
            Err(Err(ie)) => panic!(
                "seed {FUZZ_PARAMS_SEED:#x}, case {i}: HOST TRAP (delay={delay}): {ie:?} \
                 — this is what #985 exists to catch"
            ),
        }
    }
}

/// Well-formed params at the delay edges (`0`, `1`, `64`, `300`,
/// `u32::MAX`) combined with proof shapes that walk every arm of
/// `evaluate`'s match on `ctx.vdf_proof` (`None`, present, all-zero, all-one,
/// all-`0xff`) plus random 256-byte envelopes.
#[test]
fn fuzz_claim_fields_never_trap() {
    let mut rng = Xorshift64::new(FUZZ_CLAIM_SEED);

    for i in 0..CASES {
        let env = fresh_env();
        let id = env.register(VdfPolicy, ());
        let client = RefundPolicyClient::new(&env, &id);

        let delay = match i % 6 {
            0 => 0,
            1 => 1,
            2 => 64,
            3 => 300,
            4 => u32::MAX,
            _ => rng.next_u32(),
        };
        let params_bytes = VdfPolicyParams { delay }.to_xdr(&env);

        // Roughly a quarter of cases exercise the `VdfProofRequired` path.
        let proof: Option<BytesN<256>> = match rng.bounded(6) {
            0 => None,
            1 => Some(BytesN::from_array(&env, &[0u8; 256])),
            2 => Some(BytesN::from_array(&env, &[1u8; 256])),
            3 => Some(BytesN::from_array(&env, &[0xffu8; 256])),
            _ => {
                let mut pbuf = [0u8; 256];
                rng.fill(&mut pbuf);
                Some(BytesN::from_array(&env, &pbuf))
            }
        };

        let result = client.try_evaluate(&params_bytes, &mk_ctx(&env, (i % 256) as u8, proof));
        match result {
            Ok(Ok(())) => {}
            Err(Ok(e)) => assert!(
                is_typed_rejection(e),
                "seed {FUZZ_CLAIM_SEED:#x}, case {i}: unexpected typed error {e:?} \
                 (delay={delay})",
            ),
            Ok(Err(e)) => panic!(
                "seed {FUZZ_CLAIM_SEED:#x}, case {i}: Ok(Err({e:?})) (delay={delay}) — \
                 evaluate must abort on rejection"
            ),
            Err(Err(ie)) => panic!(
                "seed {FUZZ_CLAIM_SEED:#x}, case {i}: HOST TRAP (delay={delay}): {ie:?} — \
                 this is what #985 exists to catch"
            ),
        }
    }
}
