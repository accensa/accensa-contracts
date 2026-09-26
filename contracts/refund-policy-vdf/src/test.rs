#![cfg(test)]

extern crate std;

use super::*;
use crate::vdf;
use accensa_common::{PolicyContext, RefundPolicyClient, VdfPolicyParams};
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024,
};
use crate::vrf::RandomnessRecord;
use crate::VdfPolicyClient;
use soroban_sdk::{
    testutils::{EnvTestConfig, Events},
    xdr::ToXdr,
    Bytes, BytesN, Env,
};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn register(env: &Env) -> RefundPolicyClient<'_> {
    let id = env.register(VdfPolicy, ());
    RefundPolicyClient::new(env, &id)
}

fn params(env: &Env, delay: u32) -> Bytes {
    VdfPolicyParams { delay }.to_xdr(env)
}

fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
    BytesN::from_array(env, &[slot; 32])
}

/// The challenge the contract derives for a payment: `sha256(payment_ref)`
/// zero-extended to the low 32 bytes of a 128-byte big-endian value.
fn challenge_for(env: &Env, payment_ref: &BytesN<32>) -> [u8; 128] {
    let hash = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &payment_ref.to_array()));
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&hash.to_array());
    challenge
}

/// Honest VDF evaluation: `(y, pi) = (x^(2^t) mod N, x^(floor(2^t / l)) mod N)`
/// with `l = derive_challenge(x, y, t)`, by genuinely performing `t` sequential
/// squarings. Shares `derive_challenge` with the contract, so the transcript
/// binding is identical by construction.
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

/// Packs `(output, witness)` into the 256-byte `output || witness` blob the
/// vault hands the policy as `vdf_proof`.
fn pack(env: &Env, output: &[u8; 128], witness: &[u8; 128]) -> BytesN<256> {
    let mut buf = [0u8; 256];
    buf[..128].copy_from_slice(output);
    buf[128..].copy_from_slice(witness);
    BytesN::from_array(env, &buf)
}

fn ctx(_env: &Env, payment_ref: &BytesN<32>, proof: Option<BytesN<256>>) -> PolicyContext {
    PolicyContext {
        payment_ref: payment_ref.clone(),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: proof,
    }
}

// ── Contract behavior ─────────────────────────────────────────────────────

#[test]
fn test_evaluate_accepts_correct_proofs() {
    let env = test_env();
    let client = register(&env);
    for t in [16u32, 32, 64, 300] {
        for slot in [1u8, 2, 7] {
            let ref_ = payment_ref(&env, slot);
            let challenge = challenge_for(&env, &ref_);
            let (output, witness) = eval_vdf(&env, &challenge, t);
            let result = client.try_evaluate(
                &params(&env, t),
                &ctx(&env, &ref_, Some(pack(&env, &output, &witness))),
            );
            assert_eq!(
                result,
                Ok(Ok(())),
                "valid proof for t={t}, slot={slot} rejected"
            );
        }
    }
}

#[test]
fn test_evaluate_requires_proof_for_positive_delay() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 1);
    let result = client.try_evaluate(&params(&env, 64), &ctx(&env, &ref_, None));
    assert_eq!(result, Err(Ok(Error::VdfProofRequired)));
}

#[test]
fn test_evaluate_zero_delay_is_noop() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 1);
    let result = client.try_evaluate(&params(&env, 0), &ctx(&env, &ref_, None));
    assert_eq!(result, Ok(Ok(())));
}

#[test]
fn test_evaluate_rejects_tampered_output() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 1);
    let challenge = challenge_for(&env, &ref_);
    let (mut output, witness) = eval_vdf(&env, &challenge, 64);

    output[127] ^= 0x01;
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_, Some(pack(&env, &output, &witness))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));
}

#[test]
fn test_evaluate_rejects_tampered_witness() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 3);
    let challenge = challenge_for(&env, &ref_);
    let (output, mut witness) = eval_vdf(&env, &challenge, 300);

    witness[127] ^= 0x01;
    let result = client.try_evaluate(
        &params(&env, 300),
        &ctx(&env, &ref_, Some(pack(&env, &output, &witness))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));
}

#[test]
fn test_evaluate_rejects_premature_proof() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 4);
    let challenge = challenge_for(&env, &ref_);

    let (output, witness) = eval_vdf(&env, &challenge, 63);
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_, Some(pack(&env, &output, &witness))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));

    let (output2, witness2) = eval_vdf(&env, &challenge, 65);
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_, Some(pack(&env, &output2, &witness2))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));
}

#[test]
fn test_evaluate_proof_is_payment_bound() {
    let env = test_env();
    let client = register(&env);
    let ref_a = payment_ref(&env, 1);
    let ref_b = payment_ref(&env, 2);
    let challenge_a = challenge_for(&env, &ref_a);
    let (output, witness) = eval_vdf(&env, &challenge_a, 64);

    // A proof minted for payment A cannot be replayed against payment B.
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_b, Some(pack(&env, &output, &witness))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));
}

#[test]
fn test_evaluate_rejects_wrong_params_type() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 1);
    let wrong_type = 7u32.to_xdr(&env);
    let result = client.try_evaluate(&wrong_type, &ctx(&env, &ref_, None));
    assert_eq!(result, Err(Ok(Error::InvalidPolicyParams)));
}

#[test]
fn test_evaluate_rejects_proof_for_degnerate_output() {
    // A witness envelope whose claimed output was computed from x = 0 (a
    // trivially forgeable transcript) must be rejected even when the witness
    // bytes "verify" arithmetically — the challenge reduction check in the
    // verifier rejects x ≤ 1. See the direct verifier test below for the
    // exhaustive x ∈ {0, 1, N-1, N} sweep.
    let env = test_env();
    let client = register(&env);

    let degenerate = [0u8; 128];
    let (output, _witness) = eval_vdf(&env, &degenerate, 64);
    let ref_ = payment_ref(&env, 9);
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_, Some(pack(&env, &output, &[0u8; 128]))),
    );
    assert_eq!(result, Err(Ok(Error::InvalidVdfProof)));
}

#[test]
fn test_verify_vdf_direct_rejects_degenerate_challenges() {
    let env = test_env();
    let n = U1024::from_be_slice(&vdf::MODULUS);

    let zero = [0u8; 128];
    let mut one = [0u8; 128];
    one[127] = 1;
    let n_minus_1 = n.wrapping_sub(&U1024::ONE).to_be_bytes();
    let n_bytes = vdf::MODULUS;

    for bad in [&zero, &one, &n_minus_1, &n_bytes] {
        let (output, witness) = eval_vdf(&env, bad, 64);
        assert_eq!(
            vdf::verify_vdf(&env, bad, 64, &output, &witness),
            Err(Error::InvalidVdfProof),
            "degenerate challenge must be rejected"
        );
    }
}

// ── Verifiable randomness (issue #429) ────────────────────────────────────

fn vrf_id(env: &Env, slot: u8) -> BytesN<32> {
    BytesN::from_array(env, &[slot; 32])
}

fn register_vrf(env: &Env) -> VdfPolicyClient<'_> {
    let id = env.register(VdfPolicy, ());
    VdfPolicyClient::new(env, &id)
}

/// Recompute the seed exactly as the contract documents it:
/// `sha256(output || vdf_id || delay_be)`.
fn expected_seed(env: &Env, vdf_id: &BytesN<32>, output: &[u8; 128], delay: u32) -> BytesN<32> {
    let mut buf = [0u8; 164];
    buf[..128].copy_from_slice(output);
    buf[128..160].copy_from_slice(&vdf_id.to_array());
    buf[160..164].copy_from_slice(&delay.to_be_bytes());
    env.crypto().sha256(&Bytes::from_slice(env, &buf))
}

#[test]
fn test_generate_randomness_is_deterministic_from_transcript() {
    let env = test_env();
    let client = register_vrf(&env);
    let vid = vrf_id(&env, 1);
    let challenge = challenge_for(&env, &vid);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    let seed = client.generate_randomness(&vid, &64, &pack(&env, &output, &witness));
    assert_eq!(seed, expected_seed(&env, &vid, &output, 64));

    // Read paths agree with the derived value and are stable.
    assert_eq!(client.get_verified_randomness(&vid), seed);
    assert!(client.has_randomness(&vid));
    let record: RandomnessRecord = client.get_randomness_record(&vid).unwrap();
    assert_eq!(record.seed, seed);
    assert_eq!(record.delay, 64);
    assert_eq!(record.vdf_id, vid);
}

#[test]
fn test_get_verified_randomness_missing_is_not_found() {
    let env = test_env();
    let client = register_vrf(&env);
    let vid = vrf_id(&env, 2);
    assert_eq!(
        client.try_get_verified_randomness(&vid),
        Err(Ok(Error::RandomnessNotFound))
    );
    assert!(!client.has_randomness(&vid));
    assert!(client.get_randomness_record(&vid).is_none());
}

#[test]
fn test_generate_randomness_rejects_tampered_proof_and_stores_nothing() {
    let env = test_env();
    let client = register_vrf(&env);
    let vid = vrf_id(&env, 3);
    let challenge = challenge_for(&env, &vid);
    let (mut output, witness) = eval_vdf(&env, &challenge, 64);
    output[127] ^= 0x01;

    assert_eq!(
        client.try_generate_randomness(&vid, &64, &pack(&env, &output, &witness)),
        Err(Ok(Error::InvalidVdfProof))
    );
    assert!(
        !client.has_randomness(&vid),
        "a rejected proof must not record a seed"
    );
}

#[test]
fn test_randomness_is_bound_to_the_vdf_identifier() {
    let env = test_env();
    let client = register_vrf(&env);
    let id_a = vrf_id(&env, 1);
    let id_b = vrf_id(&env, 2);
    let challenge_a = challenge_for(&env, &id_a);
    let (output, witness) = eval_vdf(&env, &challenge_a, 64);

    // A proof minted for proposal A cannot seed proposal B.
    assert_eq!(
        client.try_generate_randomness(&id_b, &64, &pack(&env, &output, &witness)),
        Err(Ok(Error::InvalidVdfProof))
    );
}

#[test]
fn test_generate_randomness_is_idempotent() {
    let env = test_env();
    let client = register_vrf(&env);
    let vid = vrf_id(&env, 4);
    let challenge = challenge_for(&env, &vid);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    let first = client.generate_randomness(&vid, &64, &pack(&env, &output, &witness));

    // Even a later, tampered proof must not replace an already-recorded seed:
    // the first verified witness wins, so consumers cannot be re-targeted.
    let mut tampered = output;
    tampered[127] ^= 0x01;
    let second = client.generate_randomness(&vid, &64, &pack(&env, &tampered, &witness));
    assert_eq!(first, second);
}

#[test]
fn test_generate_randomness_emits_event() {
    let env = test_env();
    let client = register_vrf(&env);
    let vid = vrf_id(&env, 5);
    let challenge = challenge_for(&env, &vid);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    client.generate_randomness(&vid, &64, &pack(&env, &output, &witness));
    assert!(
        !env.events().all().is_empty(),
        "RandomnessGenerated must be published"
    );
}

// ── Budget ────────────────────────────────────────────────────────────────

/// How much of the default CPU / memory budget a single VDF verification may
/// consume, with headroom, before the test fails. Measured empirically on
/// 2026-08-29 against the default test host budget (100M CPU instructions /
/// 40MB): the two 1024-bit modular exponentiations dominate. The vault
/// re-baselines this further down its own cross-contract call stack.
const VDF_VERIFY_MAX_CPU: u64 = 8_000_000;
const VDF_VERIFY_MAX_MEM: u64 = 5_000_000;

#[test]
fn test_verify_resource_cost_budget() {
    let env = test_env();
    let client = register(&env);
    let ref_ = payment_ref(&env, 1);
    let challenge = challenge_for(&env, &ref_);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    env.cost_estimate().budget().reset_default();
    let result = client.try_evaluate(
        &params(&env, 64),
        &ctx(&env, &ref_, Some(pack(&env, &output, &witness))),
    );
    assert_eq!(result, Ok(Ok(())));
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().memory_bytes_cost();

    assert!(
        cpu <= VDF_VERIFY_MAX_CPU,
        "VDF verify CPU cost regression! Measured: {cpu}, Limit: {VDF_VERIFY_MAX_CPU}"
    );
    assert!(
        mem <= VDF_VERIFY_MAX_MEM,
        "VDF verify memory cost regression! Measured: {mem}, Limit: {VDF_VERIFY_MAX_MEM}"
    );
}
