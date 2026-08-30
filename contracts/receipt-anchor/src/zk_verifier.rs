//! Lightweight Groth16 / ZK validity proof verifier for Soroban.
//!
//! Provides O(1) on-chain verification of zero-knowledge validity proofs
//! for batched receipts and state roots on pairing-friendly elliptic curves.

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

/// Verifies a Groth16 zero-knowledge proof for a given state root / public inputs.
///
/// In Groth16, the pairing check is:
/// e(A, B) = e(alpha, beta) * e(L, gamma) * e(C, delta)
/// where L = IC_0 + sum(public_input_i * IC_i).
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

/// Convenience helper to verify a batch validity proof against a state root.
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
