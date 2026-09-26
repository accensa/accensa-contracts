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
use soroban_sdk::{contract, contractimpl, xdr::FromXdr, Address, Bytes, BytesN, Env};

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
mod slashing;
pub use slashing::*;

/// Dispute-resolution extension (issue #469): escalates a dispute whose
/// primary arbitrators failed to reach quorum to an external fallback oracle
/// (e.g. an optimistic oracle), and settles it on the oracle's ruling.
#[contractimpl]
impl VdfPolicy {
    /// Escalate a dispute to `oracle` for fallback resolution. `merchant`
    /// (the party seeking a ruling) authorizes the escalation. Returns the
    /// new dispute id.
    pub fn request_fallback_dispute(
        env: Env,
        merchant: Address,
        oracle: Address,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
    ) -> Result<u32, Error> {
        fallback::request_dispute(&env, &merchant, &oracle, &payment_ref, &recipient, amount)
    }

    /// The request payload handed to the fallback oracle for `dispute_id`:
    /// the dispute serialized as XDR.
    pub fn build_fallback_oracle_request(env: Env, dispute_id: u32) -> Result<Bytes, Error> {
        fallback::oracle_request_bytes(&env, dispute_id)
    }

    /// Settle `dispute_id` with the oracle's ruling (`refund` true = refund,
    /// false = deny). Only the dispute's designated oracle may call this.
    pub fn settle_fallback_dispute(env: Env, dispute_id: u32, refund: bool) -> Result<(), Error> {
        fallback::settle_dispute(&env, dispute_id, refund)
    }

    /// Read-only: the current state of a fallback dispute.
    pub fn get_fallback_dispute(env: Env, dispute_id: u32) -> Result<FallbackDispute, Error> {
        fallback::get_dispute(&env, dispute_id)
    }
}

mod fallback;
pub use fallback::*;
