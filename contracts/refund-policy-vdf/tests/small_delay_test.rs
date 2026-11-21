//! The `pi = 1` branch of the verifier, at the smallest delays (issue #983).
//!
//! `verify_vdf` deliberately does not range-check the witness. The reason is in
//! its own comment: when `2^T < l` the Wesolowski exponent is
//! `q = floor(2^T / l) = 0`, so an honest prover's witness *is* `x^0 = 1`, and
//! the equation `y == pi^l * x^r (mod N)` still binds the claim because it
//! collapses to `y == x^r`. Since `l` is a 128-bit prime drawn from SHA-256,
//! that inequality holds for every delay below 128.
//!
//! The end-to-end suite never isolates that shape. Its loop starts at delay 16,
//! so `pi = 1` is exercised but never named — and nothing anywhere states which
//! way a future "optimisation" would have to fall. `pi = 1` is also the cheapest
//! forgery to write down by hand (a witness of one plus a fabricated output), so
//! the acceptance of the honest one and the rejection of a wrong one are both
//! worth pinning.
//!
//! Acceptance of `pi = 1` here is itself the evidence that `q == 0`: had the
//! floor exponent been non-zero, the verifier's `x^r` term would not equal
//! `x^(2^T)` and the honest-looking `pi = 1` would have been rejected.
//!
//! # The modulus
//!
//! `MODULUS` is copied below because `mod vdf;` is private, so the group is not
//! reachable from an integration test. The copy is not taken on trust: the first
//! test compares `sha256` of these bytes against the contract's own
//! `get_modulus_fingerprint`, so rotating the modulus fails that assertion
//! rather than quietly turning this file into a proof about a different group.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, VdfPolicyParams};
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024,
};
use refund_policy_vdf::{VdfPolicy, VdfPolicyClient};
use soroban_sdk::{xdr::ToXdr, Bytes, BytesN, Env};

/// `vdf::MODULUS`, pinned by `pinned_modulus_is_the_contract_s_group`.
const MODULUS: [u8; 128] = [
    0xb5, 0x6f, 0xda, 0xb2, 0xf4, 0xee, 0x59, 0x57, 0xcd, 0x1b, 0x66, 0x38, 0xb3, 0x6b, 0xd2, 0x72,
    0x70, 0x7a, 0xde, 0xbe, 0x94, 0x84, 0x99, 0x6f, 0xdd, 0x9b, 0xe6, 0xce, 0xdb, 0x8d, 0xbf, 0x99,
    0xe1, 0x18, 0xe6, 0x79, 0xb2, 0x74, 0xb6, 0x35, 0xc6, 0x5e, 0xd3, 0x58, 0xfd, 0x8a, 0x66, 0xd1,
    0xb5, 0x6c, 0xce, 0x5f, 0x96, 0xdb, 0x59, 0x2e, 0x3c, 0xae, 0xab, 0xc3, 0x6c, 0xf9, 0xfd, 0xac,
    0x5b, 0x73, 0x57, 0x99, 0x4e, 0xfe, 0xfb, 0x6e, 0x03, 0x13, 0x65, 0x32, 0x4d, 0x1b, 0x20, 0xb3,
    0xb3, 0xc3, 0xf3, 0x4d, 0x59, 0xf8, 0x46, 0xcd, 0xaf, 0xed, 0x1f, 0x92, 0x1e, 0x05, 0x20, 0xaf,
    0xf3, 0x42, 0xd8, 0xcb, 0x18, 0xa8, 0x94, 0x23, 0x6c, 0x9a, 0x8e, 0x6f, 0x15, 0xdf, 0xcc, 0xc0,
    0xe7, 0x48, 0x6d, 0x57, 0x9f, 0x79, 0xc0, 0xfa, 0x1a, 0xcd, 0x56, 0xdd, 0x2a, 0x09, 0xc7, 0xf5,
];

/// `sha256(vdf::MODULUS)`, as the contract reports it.
const MODULUS_FINGERPRINT: [u8; 32] = [
    0xb3, 0x96, 0x11, 0x14, 0x07, 0x16, 0xa7, 0xa4, 0x06, 0xb9, 0xb3, 0xd1, 0x21, 0xe6, 0xef, 0x7c,
    0xbd, 0xc0, 0x84, 0xdd, 0x5c, 0xc7, 0x08, 0x08, 0xc2, 0x99, 0x79, 0x9e, 0x11, 0x56, 0xfe, 0x6d,
];

/// A fixed payment reference; the challenge is derived from it, so any change
/// here changes every expectation below.
const PAYMENT_REF: u8 = 0x51;

fn env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

fn client(env: &Env) -> soroban_sdk::Address {
    env.register(VdfPolicy, ())
}

/// The challenge `evaluate` derives for [`PAYMENT_REF`]: `sha256(payment_ref)`
/// zero-extended so the digest occupies the last 32 bytes of a 128-byte
/// big-endian value. Reproduced rather than reached into, exactly as the crate's
/// own unit tests do — the transcript has to match the deployed path or the test
/// proves nothing.
fn challenge(env: &Env) -> [u8; 128] {
    let hash = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &[PAYMENT_REF; 32]));
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&hash.to_array());
    challenge
}

/// `x^(2^T) mod N`, by genuinely squaring `T` times.
fn square_repeatedly(challenge: &[u8; 128], t: usize) -> [u8; 128] {
    let n = U1024::from_be_slice(&MODULUS);
    let x = U1024::from_be_slice(challenge).rem(&NonZero::new(n).unwrap());
    let params = DynResidueParams::new(&n);
    let mut acc = DynResidue::new(&x, params);
    for _ in 0..t {
        acc = acc.square();
    }
    acc.retrieve().to_be_bytes()
}

/// A 1024-bit big-endian constant witness.
fn witness(value: u8) -> [u8; 128] {
    let mut pi = [0u8; 128];
    pi[127] = value;
    pi
}

/// The 256-byte `output || witness` blob the vault hands the policy.
fn pack(env: &Env, output: &[u8; 128], pi: &[u8; 128]) -> BytesN<256> {
    let mut buf = [0u8; 256];
    buf[..128].copy_from_slice(output);
    buf[128..].copy_from_slice(pi);
    BytesN::from_array(env, &buf)
}

fn evaluate(
    env: &Env,
    contract: &soroban_sdk::Address,
    delay: u32,
    proof: Option<BytesN<256>>,
) -> Result<(), Error> {
    let ctx = PolicyContext {
        payment_ref: BytesN::from_array(env, &[PAYMENT_REF; 32]),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: proof,
    };
    let params = VdfPolicyParams { delay }.to_xdr(env);
    match RefundPolicyClient::new(env, contract).try_evaluate(&params, &ctx) {
        Ok(Ok(())) => Ok(()),
        Ok(other) => panic!("unexpected success payload {other:?}"),
        Err(Ok(error)) => Err(error),
        Err(other) => panic!("host failure: {other:?}"),
    }
}

#[test]
fn pinned_modulus_is_the_contract_s_group() {
    let env = env();
    let digest = env.crypto().sha256(&Bytes::from_array(&env, &MODULUS));
    assert_eq!(
        digest.to_array(),
        MODULUS_FINGERPRINT,
        "the local copy of the modulus no longer hashes to the pinned fingerprint"
    );
    let contract = client(&env);
    let got = VdfPolicyClient::new(&env, &contract).get_modulus_fingerprint();
    assert_eq!(
        got.to_array(),
        MODULUS_FINGERPRINT,
        "`vdf::MODULUS` was rotated; this file's copy is stale and its proofs are          about a different group"
    );
    assert_ne!(MODULUS[0], 0, "the copy is not a 1024-bit value");
}

#[test]
fn delay_one_accepts_the_honest_unit_witness() {
    let env = env();
    let contract = client(&env);
    let output = square_repeatedly(&challenge(&env), 1);
    assert_eq!(
        evaluate(&env, &contract, 1, Some(pack(&env, &output, &witness(1)))),
        Ok(()),
        "delay 1 has q = 0, so pi = 1 is the honest witness and must verify"
    );
}

#[test]
fn delay_two_accepts_the_honest_unit_witness() {
    let env = env();
    let contract = client(&env);
    let output = square_repeatedly(&challenge(&env), 2);
    assert_eq!(
        evaluate(&env, &contract, 2, Some(pack(&env, &output, &witness(1)))),
        Ok(())
    );
}

#[test]
fn delay_one_rejects_a_witness_of_two() {
    let env = env();
    let contract = client(&env);
    let output = square_repeatedly(&challenge(&env), 1);
    assert_eq!(
        evaluate(&env, &contract, 1, Some(pack(&env, &output, &witness(2)))),
        Err(Error::InvalidVdfProof),
        "2^l is not 1 mod N: a witness other than the honest one must not verify"
    );
}

#[test]
fn delay_two_rejects_a_witness_of_two() {
    let env = env();
    let contract = client(&env);
    let output = square_repeatedly(&challenge(&env), 2);
    assert_eq!(
        evaluate(&env, &contract, 2, Some(pack(&env, &output, &witness(2)))),
        Err(Error::InvalidVdfProof)
    );
}

/// A unit witness does not generalise: it verifies only where the floor exponent
/// is actually zero. At delay 128 the exponent `floor(2^128 / l)` is at least 1
/// for this transcript's `l`, so `pi = 1` stops being the honest witness.
#[test]
fn a_unit_witness_is_not_a_blanket_pass() {
    let env = env();
    let contract = client(&env);
    let output = square_repeatedly(&challenge(&env), 300);
    assert_eq!(
        evaluate(&env, &contract, 300, Some(pack(&env, &output, &witness(1)))),
        Err(Error::InvalidVdfProof),
        "`pi = 1` accepted at delay 300 would mean the witness is not bound at all"
    );
}

#[test]
fn delay_zero_needs_no_proof_at_all() {
    let env = env();
    let contract = client(&env);
    assert_eq!(evaluate(&env, &contract, 0, None), Ok(()));
    // And a proof does not change that: `delay == 0` returns before the
    // challenge is derived (`src/lib.rs:59-61`).
    assert_eq!(
        evaluate(
            &env,
            &contract,
            0,
            Some(pack(&env, &[0u8; 128], &[0u8; 128]))
        ),
        Ok(())
    );
}

#[test]
fn a_positive_delay_still_demands_a_proof() {
    let env = env();
    let contract = client(&env);
    assert_eq!(
        evaluate(&env, &contract, 1, None),
        Err(Error::VdfProofRequired)
    );
}
