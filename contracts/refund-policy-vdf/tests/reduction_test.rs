//! Integration coverage for VDF output / witness reduction modulo `N`
//! (issue #984).
//!
//! `verify_vdf` reduces both the claimed output and the witness mod `N`
//! before using them, so a 128-byte envelope holding `y + N` or `pi + N` is
//! accepted as if it held the canonical value — provided the bytes still fit
//! in 1024 bits. Off-chain provers working in a wider type can land on a
//! non-canonical representative, and the contract accepts it by construction.
//!
//! With `delay = 64` and a 128-bit transcript-derived prime `l`, `2^64 < l`,
//! so the honest witness exponent `q = floor(2^64 / l) = 0` and `pi = 1`
//! regardless of the specific prime chosen. That keeps the reduction cases
//! independent of `derive_challenge` (which is `pub(crate)` and unreachable
//! from `tests/`) and lets us drive the contract through the public
//! `evaluate` path.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, VdfPolicyParams};
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024,
};
use refund_policy_vdf::VdfPolicy;
use soroban_sdk::{testutils::EnvTestConfig, xdr::ToXdr, Bytes, BytesN, Env};

// Copy of `crate::vdf::MODULUS` — the module is private, so the integration
// test carries the constant locally, mirroring the fingerprint pin in
// `modulus_getter_test.rs`. Recompute only if the modulus rotates.
const MODULUS_BYTES: [u8; 128] = [
    0xb5, 0x6f, 0xda, 0xb2, 0xf4, 0xee, 0x59, 0x57, 0xcd, 0x1b, 0x66, 0x38, 0xb3, 0x6b, 0xd2, 0x72,
    0x70, 0x7a, 0xde, 0xbe, 0x94, 0x84, 0x99, 0x6f, 0xdd, 0x9b, 0xe6, 0xce, 0xdb, 0x8d, 0xbf, 0x99,
    0xe1, 0x18, 0xe6, 0x79, 0xb2, 0x74, 0xb6, 0x35, 0xc6, 0x5e, 0xd3, 0x58, 0xfd, 0x8a, 0x66, 0xd1,
    0xb5, 0x6c, 0xce, 0x5f, 0x96, 0xdb, 0x59, 0x2e, 0x3c, 0xae, 0xab, 0xc3, 0x6c, 0xf9, 0xfd, 0xac,
    0x5b, 0x73, 0x57, 0x99, 0x4e, 0xfe, 0xfb, 0x6e, 0x03, 0x13, 0x65, 0x32, 0x4d, 0x1b, 0x20, 0xb3,
    0xb3, 0xc3, 0xf3, 0x4d, 0x59, 0xf8, 0x46, 0xcd, 0xaf, 0xed, 0x1f, 0x92, 0x1e, 0x05, 0x20, 0xaf,
    0xf3, 0x42, 0xd8, 0xcb, 0x18, 0xa8, 0x94, 0x23, 0x6c, 0x9a, 0x8e, 0x6f, 0x15, 0xdf, 0xcc, 0xc0,
    0xe7, 0x48, 0x6d, 0x57, 0x9f, 0x79, 0xc0, 0xfa, 0x1a, 0xcd, 0x56, 0xdd, 0x2a, 0x09, 0xc7, 0xf5,
];

const DELAY: u32 = 64;

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn params(env: &Env, delay: u32) -> Bytes {
    VdfPolicyParams { delay }.to_xdr(env)
}

fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
    BytesN::from_array(env, &[slot; 32])
}

fn challenge_for(env: &Env, payment_ref: &BytesN<32>) -> [u8; 128] {
    let hash = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &payment_ref.to_array()));
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&hash.to_array());
    challenge
}

/// Honest output `y = x^(2^DELAY) mod N`, computed exactly the way an
/// off-chain prover would (repeated squaring over `DynResidue`).
fn honest_output(challenge: &[u8; 128]) -> [u8; 128] {
    let n = U1024::from_be_slice(&MODULUS_BYTES);
    let x = U1024::from_be_slice(challenge).rem(&NonZero::new(n).unwrap());
    let params = DynResidueParams::new(&n);
    let mut acc = DynResidue::new(&x, params);
    for _ in 0..DELAY {
        acc = acc.square();
    }
    acc.retrieve().to_be_bytes()
}

fn pack(env: &Env, output: &[u8; 128], witness: &[u8; 128]) -> BytesN<256> {
    let mut buf = [0u8; 256];
    buf[..128].copy_from_slice(output);
    buf[128..].copy_from_slice(witness);
    BytesN::from_array(env, &buf)
}

fn ctx(ref_: &BytesN<32>, proof: Option<BytesN<256>>) -> PolicyContext {
    PolicyContext {
        payment_ref: ref_.clone(),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: proof,
    }
}

/// `1` encoded as a big-endian 128-byte value (the honest witness for
/// `delay = 64`, because `q = floor(2^64 / l) = 0` for a 128-bit `l > 2^64`).
fn one_bytes() -> [u8; 128] {
    let mut w = [0u8; 128];
    w[127] = 1;
    w
}

/// Find a payment ref whose honest output `y` satisfies `y + N < 2^1024`
/// so the congruent envelope actually fits in 128 bytes. We probe with
/// `wrapping_add` and reject any ref whose sum wrapped (sum < y).
fn find_noncanon_envelope(env: &Env) -> (u8, [u8; 128], [u8; 128]) {
    let n = U1024::from_be_slice(&MODULUS_BYTES);
    for slot in 1u8..=200 {
        let ref_ = payment_ref(env, slot);
        let c = challenge_for(env, &ref_);
        let y = honest_output(&c);
        let y_u = U1024::from_be_slice(&y);
        let sum = y_u.wrapping_add(&n);
        if sum >= y_u {
            return (slot, y, sum.to_be_bytes());
        }
    }
    panic!("no payment ref in 1..=200 yields y + N < 2^1024")
}

#[test]
fn congruent_output_and_witness_reduce_and_are_accepted() {
    let env = test_env();
    let id = env.register(VdfPolicy, ());
    let client = RefundPolicyClient::new(&env, &id);

    let (slot, canonical_y, noncanon_y) = find_noncanon_envelope(&env);
    let ref_ = payment_ref(&env, slot);
    let one = one_bytes();

    // Case A: output = y + N (non-canonical, congruent mod N), witness = 1.
    // The verifier reduces the envelope back to `y` before checking the
    // Wesolowski equation, so the call succeeds.
    let result_a = client.try_evaluate(
        &params(&env, DELAY),
        &ctx(&ref_, Some(pack(&env, &noncanon_y, &one))),
    );
    assert_eq!(
        result_a,
        Ok(Ok(())),
        "output ≡ y (mod N) with canonical witness must reduce and be accepted (slot={slot})"
    );

    // Case B: canonical output, witness = N + 1 (reduces to 1).
    let n = U1024::from_be_slice(&MODULUS_BYTES);
    let witness_n_plus_1 = n.wrapping_add(&U1024::ONE).to_be_bytes();
    let result_b = client.try_evaluate(
        &params(&env, DELAY),
        &ctx(&ref_, Some(pack(&env, &canonical_y, &witness_n_plus_1))),
    );
    assert_eq!(
        result_b,
        Ok(Ok(())),
        "witness ≡ 1 (mod N) must reduce and be accepted (slot={slot})"
    );
}

#[test]
fn breaking_the_congruence_or_reducing_the_witness_to_zero_is_rejected() {
    let env = test_env();
    let id = env.register(VdfPolicy, ());
    let client = RefundPolicyClient::new(&env, &id);

    let (slot, canonical_y, mut broken_y_plus_n) = find_noncanon_envelope(&env);
    let ref_ = payment_ref(&env, slot);
    let one = one_bytes();

    // Case C: flip the low byte of the congruent envelope — the value is no
    // longer `y (mod N)`, so the reduction lands on `y ± 1` and the equation
    // fails.
    broken_y_plus_n[127] ^= 0x01;
    let result_c = client.try_evaluate(
        &params(&env, DELAY),
        &ctx(&ref_, Some(pack(&env, &broken_y_plus_n, &one))),
    );
    assert_eq!(
        result_c,
        Err(Ok(Error::InvalidVdfProof)),
        "output ≢ y (mod N) must be rejected (slot={slot})"
    );

    // Case D: witness = N. Reduce to `pi = 0`, so `pi^l * x^r = 0`; the
    // honest y is not `0`, so the equation collapses and the verifier
    // rejects.
    let result_d = client.try_evaluate(
        &params(&env, DELAY),
        &ctx(&ref_, Some(pack(&env, &canonical_y, &MODULUS_BYTES))),
    );
    assert_eq!(
        result_d,
        Err(Ok(Error::InvalidVdfProof)),
        "witness ≡ 0 (mod N) must be rejected (slot={slot})"
    );
}
