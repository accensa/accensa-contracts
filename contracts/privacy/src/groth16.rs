//! Groth16 proof verification for concealed escrow amounts (issue #440).
//!
//! Structural Groth16 verification in the spirit of
//! `receipt-anchor/src/zk_verifier.rs`. The Soroban host does not expose
//! alt_bn128 pairing, so this module performs the structural checks the
//! issue sanctions as the mock equivalent: IC/public-input count match,
//! non-empty well-sized proof and key elements, and rejection of all-zero
//! forged points. A Pedersen-style commitment is `sha256(value || blinding)`
//! via the host hash — again a stand-in for real curve operations.

use soroban_sdk::{contracttype, Bytes, BytesN, Env, Vec};

use crate::Error;

/// Groth16 proof: A in G1, B in G2, C in G1.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Groth16Proof {
    /// Point A in G1 (64 bytes uncompressed).
    pub a: Bytes,
    /// Point B in G2 (128 bytes uncompressed).
    pub b: Bytes,
    /// Point C in G1 (64 bytes uncompressed).
    pub c: Bytes,
}

/// Groth16 verification key.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyingKey {
    /// Alpha in G1.
    pub alpha_g1: Bytes,
    /// Beta in G2.
    pub beta_g2: Bytes,
    /// Gamma in G2.
    pub gamma_g2: Bytes,
    /// Delta in G2.
    pub delta_g2: Bytes,
    /// Public-input commitments IC_0 .. IC_n (G1).
    pub ic: Vec<Bytes>,
}

/// Pedersen-style commitment pair (value + blinding).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PedersenCommitment {
    /// Committed value.
    pub value: BytesN<32>,
    /// Blinding factor.
    pub blinding: BytesN<32>,
}

fn all_zero(bytes: &Bytes) -> bool {
    let mut zero = true;
    for b in bytes.clone().into_iter() {
        if b != 0 {
            zero = false;
            break;
        }
    }
    zero
}

fn all_zero_n32(b: &BytesN<32>) -> bool {
    b.to_array().iter().all(|&x| x == 0)
}

/// Hash-based Pedersen commitment: `sha256(value || blinding)`.
///
/// Real curve commitments are unavailable via the Soroban host, so this is
/// the sanctioned mock equivalent — deterministic and binding to both
/// inputs.
pub fn commit(env: &Env, value: &BytesN<32>, blinding: &BytesN<32>) -> BytesN<32> {
    let mut buf = Bytes::from_slice(env, &value.to_array());
    buf.append(&Bytes::from_slice(env, &blinding.to_array()));
    env.crypto().sha256(&buf).into()
}

/// Structural Groth16 verification (mock equivalent for the missing
/// alt_bn128 pairing).
///
/// Checks, following `zk_verifier.rs`:
/// - `vk.ic.len() == public_inputs.len() + 1`
/// - every proof and VK element is non-empty
/// - each of a/b/c is at least 32 bytes
/// - no all-zero (forged) proof point
pub fn verify_groth16(
    _env: &Env,
    proof: &Groth16Proof,
    vk: &VerifyingKey,
    public_inputs: &Vec<BytesN<32>>,
) -> Result<bool, Error> {
    if (vk.ic.len() as usize) != (public_inputs.len() as usize) + 1 {
        return Ok(false);
    }
    if proof.a.is_empty() || proof.b.is_empty() || proof.c.is_empty() {
        return Ok(false);
    }
    if vk.alpha_g1.is_empty()
        || vk.beta_g2.is_empty()
        || vk.gamma_g2.is_empty()
        || vk.delta_g2.is_empty()
    {
        return Ok(false);
    }
    if proof.a.len() < 32 || proof.b.len() < 32 || proof.c.len() < 32 {
        return Ok(false);
    }
    if all_zero(&proof.a) || all_zero(&proof.b) || all_zero(&proof.c) {
        return Ok(false);
    }
    Ok(true)
}

/// Verify a concealed-amount claim: Groth16 proof + commitment binding +
/// public-input amount floor.
///
/// When `public_inputs` carries at least three elements the Pedersen
/// commitment is re-derived from `(value, blinding) = (inputs[1], inputs[2])`
/// and checked against `commitment`. The proven amount is read from
/// `public_inputs[0]` (big-endian i128 in the low 16 bytes) and must be
/// `>= min_amount`.
pub fn verify_concealed_amount(
    env: &Env,
    commitment: &BytesN<32>,
    proof: &Groth16Proof,
    vk: &VerifyingKey,
    public_inputs: &Vec<BytesN<32>>,
    min_amount: i128,
) -> Result<bool, Error> {
    if all_zero_n32(commitment) {
        return Ok(false);
    }
    if !verify_groth16(env, proof, vk, public_inputs)? {
        return Ok(false);
    }
    if public_inputs.is_empty() {
        return Ok(false);
    }
    if public_inputs.len() >= 3 {
        let value = public_inputs.get_unchecked(1);
        let blinding = public_inputs.get_unchecked(2);
        let expected = commit(env, &value, &blinding);
        if expected != *commitment {
            return Ok(false);
        }
    }
    let amount = i128_from_public_input(&public_inputs.get_unchecked(0));
    if amount < min_amount {
        return Ok(false);
    }
    Ok(true)
}

/// Decode a concealed amount from a public input (big-endian i128 in the
/// low 16 bytes of a 32-byte field element).
pub fn i128_from_public_input(input: &BytesN<32>) -> i128 {
    let arr = input.to_array();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&arr[16..32]);
    i128::from_be_bytes(bytes)
}

/// Encode an amount as a 32-byte public input (big-endian, low 16 bytes).
pub fn public_input_from_i128(env: &Env, amount: i128) -> BytesN<32> {
    let mut arr = [0u8; 32];
    arr[16..32].copy_from_slice(&amount.to_be_bytes());
    BytesN::from_array(env, &arr)
}
