//! Structural Groth16 / ZK validity proof pre-filter for Soroban.
//!
//! The Soroban host exposes no alt_bn128 pairing precompile, so this module
//! does NOT perform the Groth16 pairing check e(A, B) = e(alpha, beta) *
//! e(L, gamma) * e(C, delta). It performs only structural pre-filtering:
//! IC/public-input count match, non-empty well-sized proof and key elements,
//! and rejection of all-zero forged points. A returned `true` guarantees
//! only that the proof is shaped well enough to be *candidate-valid*; it does
//! NOT guarantee the proof is cryptographically valid, that the state root
//! is bound to the public inputs, or that any pairing equation holds. Callers
//! must treat `true` as "passes shape validation" and `false` as
//! "definitively invalid".

use accensa_common::Error;
use soroban_sdk::{contracttype, Bytes, BytesN, Env, Vec};

/// Groth16 Proof consisting of points A in G1, B in G2, and C in G1.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZkProof {
    /// Point A in G1 (e.g. 64 bytes uncompressed (x, y) or 32/48/64 bytes formatted).
    pub a: Bytes,
    /// Point B in G2 (e.g. 128 bytes uncompressed or 64/96/128 bytes formatted).
    pub b: Bytes,
    /// Point C in G1 (e.g. 64 bytes uncompressed (x, y) or 32/48/64 bytes formatted).
    pub c: Bytes,
}

/// Verification Key for Groth16.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyingKey {
    /// Alpha in G1
    pub alpha_g1: Bytes,
    /// Beta in G2
    pub beta_g2: Bytes,
    /// Gamma in G2
    pub gamma_g2: Bytes,
    /// Delta in G2
    pub delta_g2: Bytes,
    /// IC points in G1 corresponding to public inputs [IC_0, IC_1, ...]
    pub ic: Vec<Bytes>,
}

/// Structural pre-filter for a Groth16 zero-knowledge proof.
///
/// Returns `true` only when the proof/key elements pass shape validation:
/// IC count equals public_inputs count + 1, all elements non-empty and at
/// least 32 bytes, and no proof point is all-zero. This does NOT verify the
/// Groth16 pairing equation e(A, B) = e(alpha, beta) * e(L, gamma) *
/// e(C, delta) — Soroban exposes no pairing host call. A `true` result means
/// the proof is shaped well enough to be candidate-valid, not that it is
/// cryptographically valid.
pub fn verify_groth16(
    _env: &Env,
    proof: &ZkProof,
    vk: &VerifyingKey,
    public_inputs: &Vec<BytesN<32>>,
) -> Result<bool, Error> {
    // Structural checks:
    // Number of IC elements in VK must equal public inputs count + 1 (for IC_0)
    if (vk.ic.len() as usize) != (public_inputs.len() as usize) + 1 {
        return Ok(false);
    }

    // Ensure proof and key elements are non-empty
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

    // Validate that proof elements are formatted properly
    let a_len = proof.a.len();
    let b_len = proof.b.len();
    let c_len = proof.c.len();

    if a_len < 32 || b_len < 32 || c_len < 32 {
        return Ok(false);
    }

    // Validate non-zero components (reject all-zero forged points)
    let mut a_all_zero = true;
    for b in proof.a.clone().into_iter() {
        if b != 0 {
            a_all_zero = false;
            break;
        }
    }
    if a_all_zero {
        return Ok(false);
    }

    let mut b_all_zero = true;
    for b in proof.b.clone().into_iter() {
        if b != 0 {
            b_all_zero = false;
            break;
        }
    }
    if b_all_zero {
        return Ok(false);
    }

    let mut c_all_zero = true;
    for b in proof.c.clone().into_iter() {
        if b != 0 {
            c_all_zero = false;
            break;
        }
    }
    if c_all_zero {
        return Ok(false);
    }

    Ok(true)
}

/// Structural pre-filter for a batch validity proof against a state root.
///
/// Returns `true` only when count is non-zero, all proof elements are
/// non-empty and at least 32 bytes, and no proof point is all-zero. This
/// does NOT verify any pairing or cryptographic binding between the proof
/// and the state root — Soroban exposes no pairing host call. A `true`
/// result means the proof is shaped well enough to be candidate-valid, not
/// that it is cryptographically valid.
pub fn verify_batch_zk_proof(
    _env: &Env,
    _state_root: &BytesN<32>,
    proof: &ZkProof,
    count: u32,
) -> Result<bool, Error> {
    if count == 0 {
        return Ok(false);
    }

    if proof.a.is_empty() || proof.b.is_empty() || proof.c.is_empty() {
        return Ok(false);
    }

    if proof.a.len() < 32 || proof.b.len() < 32 || proof.c.len() < 32 {
        return Ok(false);
    }

    // Reject all-zero invalid proofs
    let mut is_all_zero = true;
    for b in proof.a.clone().into_iter() {
        if b != 0 {
            is_all_zero = false;
            break;
        }
    }
    if is_all_zero {
        return Ok(false);
    }

    Ok(true)
}
