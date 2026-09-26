//! Batched Ed25519 signature verification (issue #471).
//!
//! Multi-party channels collect one approving signature per participant over
//! one canonical close envelope. Verifying them at each call site spreads the
//! pairing and the length checks across the codebase, and every new call site
//! is another chance to pair a public key with the wrong signature or skip a
//! participant entirely. [`verify_signatures`] centralises that: callers hand
//! over one flat `signers` array and one flat `signatures` array, which must
//! line up 1:1, and the helper checks every entry against the same payload in
//! a single pass.
//!
//! # Why there is no single "batch" host call
//!
//! This runtime exposes per-signature `ed25519_verify`; it has no batched
//! Ed25519 host function (see `soroban_sdk::crypto::Crypto` for the complete
//! surface — `sha256`, `ed25519_verify`, the secp recover/verify pairs, and
//! the BLS/BN curve helpers). "One host call for the whole batch" is therefore
//! not expressible here, and this module does not pretend otherwise. What a
//! batch *can* pin down is the aggregation: the arrays are length-checked
//! against each other before the host is touched, and the payload is fixed
//! once for the batch, so a caller cannot mis-pair keys with signatures or
//! verify a participant against a different message than the others.
//!
//! # Rejection semantics
//!
//! `ed25519_verify` **traps** on an invalid signature — it returns no boolean.
//! A forged or malformed signature therefore aborts the whole invocation
//! before any state is committed, which is the correct rejection behaviour for
//! a settlement envelope: the bad state is never recorded. The only
//! recoverable failure is a length mismatch, which is a caller bug rather than
//! a forged proof, and is reported as [`Error::InvalidSignature`] without
//! touching the host.

use accensa_common::Error;
use soroban_sdk::{Bytes, BytesN, Env};

/// Verify that every `signatures[i]` is a valid Ed25519 signature of `payload`
/// by `signers[i]`, in a single pass.
///
/// # Errors
///
/// - [`Error::InvalidSignature`] when `signers` and `signatures` differ in
///   length. Nothing is verified in that case.
///
/// An individual invalid signature traps inside `ed25519_verify` (see the
/// module docs) and aborts the invocation rather than returning `Err`.
pub(crate) fn verify_signatures(
    env: &Env,
    payload: &Bytes,
    signers: &[BytesN<32>],
    signatures: &[BytesN<64>],
) -> Result<(), Error> {
    if signers.len() != signatures.len() {
        return Err(Error::InvalidSignature);
    }

    for (signer, signature) in signers.iter().zip(signatures.iter()) {
        env.crypto().ed25519_verify(signer, payload, signature);
    }

    Ok(())
}
