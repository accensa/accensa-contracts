//! Stateless **VDF** refund policy (issue #129).
//!
//! Verifies a Wesolowski VDF proof that `delay` sequential squarings have
//! elapsed on a challenge derived from the claim's `payment_ref` before the
//! vault honors the refund. This was historically embedded in `RefundVault`
//! (issue #138); moving it into a separate, stateless contract keeps the rate
//! block out of every vault instance and lets the verification logic be
//! upgraded (or the modulus rotated) by pointing vaults at a new policy
//! contract instead of redeploying them.
//!
//! See `vdf.rs` for the scheme, the fixed 1024-bit modulus, and the
//! trust/cert ceremony notes.
//!
//! Statelessness: configuration arrives as the `params` blob of a
//! [`accensa_common::PolicyEntry`] (an [`accensa_common::VdfPolicyParams`]
//! XDR blob) and the claim facts as [`accensa_common::PolicyContext`]. It
//! keeps no storage and must not call back into the vault.

#![no_std]

use accensa_common::{Error, PolicyContext, RefundPolicy, VdfPolicyParams};
use soroban_sdk::{contract, contractimpl, xdr::FromXdr, Bytes, Env};

mod vdf;
use vdf::verify_vdf;

#[contract]
pub struct VdfPolicy;

#[cfg(test)]
mod test;

#[contractimpl]
impl RefundPolicy for VdfPolicy {
    /// Rejects a claim when the configured delay is unproven.
    ///
    /// - `delay == 0` (no gate): always `Ok`. The vault only emits a VDF
    ///   entry for a positive delay, so this is defensive only.
    /// - positive delay, missing proof: [`Error::VdfProofRequired`].
    /// - positive delay, malformed or wrong proof: [`Error::InvalidVdfProof`].
    ///
    /// The challenge is `sha256(payment_ref)` zero-extended to 128 bytes,
    /// exactly the transcript the clerk's off-chain prover used, so proofs
    /// cannot be replayed across payments or across policy changes.
    fn evaluate(env: Env, params: Bytes, ctx: PolicyContext) -> Result<(), Error> {
        let p = VdfPolicyParams::from_xdr(&env, &params).map_err(|_| Error::InvalidPolicyParams)?;
        if p.delay == 0 {
            return Ok(());
        }

        let proof = match ctx.vdf_proof {
            None => return Err(Error::VdfProofRequired),
            Some(p) => p,
        };

        let payment_hash = env
            .crypto()
            .sha256(&Bytes::from_slice(&env, &ctx.payment_ref.to_array()));
        let mut challenge = [0u8; 128];
        challenge[96..].copy_from_slice(&payment_hash.to_array());

        let packed = proof.to_array();
        let mut output = [0u8; 128];
        let mut witness = [0u8; 128];
        output.copy_from_slice(&packed[..128]);
        witness.copy_from_slice(&packed[128..]);

        verify_vdf(&env, &challenge, p.delay, &output, &witness)
    }
}
