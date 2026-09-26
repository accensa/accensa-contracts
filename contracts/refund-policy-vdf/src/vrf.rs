//! Verifiable randomness (VRF) seed derivation from verified VDF output
//! (issue #429).
//!
//! A completed Verifiable Delay Function evaluation is a natural randomness
//! beacon: its output `y = x^(2^T) mod N` cannot be known until all `T`
//! sequential squarings have been performed, and no participant can choose it
//! after the fact without breaking the adaptive-root assumption (see
//! [`crate::vdf`]). This module turns a *verified* Wesolowski proof into a
//! 256-bit seed that consumer contracts — lottery draws, validator selection,
//! tie-breaking — can read deterministically.
//!
//! # Why the seed is unbiasable
//!
//! - The challenge is bound to the proposal identifier:
//!   `x = sha256(vdf_id)` zero-extended to 128 bytes. A prover cannot select a
//!   different challenge to steer the output; changing `vdf_id` changes `x`.
//! - [`crate::vdf::verify_vdf`] refuses any proof whose output did not come
//!   from `T` genuine squarings of that challenge, so only a *verified* output
//!   is ever hashed.
//! - The seed is `sha256(output || vdf_id || T)`: a one-way function of the
//!   transcript. Nobody — not the prover, not the proposal author — can
//!   predict or bias it before the delay elapses, because doing so would
//!   require predicting `y` (or inverting SHA-256).
//! - Derivation is **deterministic**: the same verified transcript always
//!   yields the same seed, so independent observers can reproduce it and
//!   [`get_verified_randomness`](VdfPolicy::get_verified_randomness) is a
//!   stable read rather than a mutable lottery ticket.
//!
//! The ledger number is deliberately *not* part of the seed, so a consumer can
//! recompute the expected value off-chain; it is only recorded alongside the
//! seed for auditability.

use accensa_common::{
    storage::{extend_instance_ttl_default, DEFAULT_TTL_BUMP},
    Error,
};
use soroban_sdk::{contractevent, contractimpl, contracttype, Bytes, BytesN, Env};

use crate::VdfPolicy;

/// Storage keys for the randomness registry.
///
/// The registry is keyed by the VDF/proposal identifier, so re-generating a
/// seed for an identifier that already has one is idempotent (first verified
/// proof wins).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Randomness(BytesN<32>),
}

/// A verified randomness seed and the transcript metadata that produced it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomnessRecord {
    /// The 256-bit derived seed.
    pub seed: BytesN<32>,
    /// The proposal/VDF identifier the seed is bound to.
    pub vdf_id: BytesN<32>,
    /// The proven delay (squarings) that was verified.
    pub delay: u32,
    /// Ledger the seed was first recorded at (audit only; not part of the seed).
    pub generated_at_ledger: u32,
}

/// Emitted when a seed is derived from a verified VDF proof.
///
/// Topics: `("randomness_generated", vdf_id)`. Consumer contracts subscribe
/// to this to pick up new verifiable randomness without polling.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomnessGenerated {
    #[topic]
    pub vdf_id: BytesN<32>,
    pub seed: BytesN<32>,
    pub delay: u32,
    pub ledger: u32,
}

/// The challenge `x = sha256(vdf_id)` zero-extended to the low 32 bytes of a
/// 128-byte big-endian value — the same domain convention the VDF *policy*
/// uses for `sha256(payment_ref)`, so off-chain provers share one transcript
/// builder.
fn derive_challenge(env: &Env, vdf_id: &BytesN<32>) -> [u8; 128] {
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &vdf_id.to_array()))
        .to_array();
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&digest);
    challenge
}

/// Derive the 256-bit seed from a verified transcript:
/// `sha256(output || vdf_id || delay_be)`.
///
/// Deterministic and one-way; the output is only available after the delay, so
/// the seed cannot be known or biased beforehand.
fn derive_seed(env: &Env, vdf_id: &BytesN<32>, output: &[u8; 128], delay: u32) -> BytesN<32> {
    let mut buf = [0u8; 164];
    buf[..128].copy_from_slice(output);
    buf[128..160].copy_from_slice(&vdf_id.to_array());
    buf[160..164].copy_from_slice(&delay.to_be_bytes());
    env.crypto().sha256(&Bytes::from_slice(env, &buf))
}

#[contractimpl]
impl VdfPolicy {
    /// Verify a Wesolowski VDF proof for `vdf_id` and derive its randomness
    /// seed. `proof` is the 256-byte `output || witness` blob (the same layout
    /// the refund policy consumes).
    ///
    /// Idempotent: if a seed already exists for `vdf_id`, it is returned
    /// unchanged (the first verified proof wins), so consumers cannot be
    /// griefed into recomputing a different value.
    ///
    /// The record is written to **persistent** storage and its TTL is extended
    /// to the shared target ([`DEFAULT_TTL_BUMP`]) as part of the same call, so
    /// a seed cannot archive while consumers still expect to read it. The
    /// extension passes the target as the threshold too
    /// (`extend_ttl(target, target)`), because a freshly written persistent
    /// entry already carries the network floor — a low threshold would make the
    /// bump a no-op. See `contracts/common/src/storage.rs`.
    ///
    /// Errors:
    /// - [`Error::InvalidVdfProof`] if the proof does not verify against the
    ///   `vdf_id`-derived challenge.
    pub fn generate_randomness(
        env: Env,
        vdf_id: BytesN<32>,
        delay: u32,
        proof: BytesN<256>,
    ) -> Result<BytesN<32>, Error> {
        let key = DataKey::Randomness(vdf_id.clone());
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<_, RandomnessRecord>(&key)
        {
            return Ok(existing.seed);
        }

        let challenge = derive_challenge(&env, &vdf_id);
        let packed = proof.to_array();
        let mut output = [0u8; 128];
        let mut witness = [0u8; 128];
        output.copy_from_slice(&packed[..128]);
        witness.copy_from_slice(&packed[128..]);

        crate::vdf::verify_vdf(&env, &challenge, delay, &output, &witness)?;

        let seed = derive_seed(&env, &vdf_id, &output, delay);
        let ledger = env.ledger().sequence();
        let record = RandomnessRecord {
            seed: seed.clone(),
            vdf_id: vdf_id.clone(),
            delay,
            generated_at_ledger: ledger,
        };
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .extend_ttl(&key, DEFAULT_TTL_BUMP, DEFAULT_TTL_BUMP);
        extend_instance_ttl_default(&env);

        RandomnessGenerated {
            vdf_id,
            seed: seed.clone(),
            delay,
            ledger,
        }
        .publish(&env);

        Ok(seed)
    }

    /// Read-only: the verified seed recorded for `vdf_id` (issue #429).
    ///
    /// Returns [`Error::RandomnessNotFound`] until
    /// [`generate_randomness`](Self::generate_randomness) has verified and
    /// recorded one.
    pub fn get_verified_randomness(env: Env, vdf_id: BytesN<32>) -> Result<BytesN<32>, Error> {
        env.storage()
            .persistent()
            .get::<_, RandomnessRecord>(&DataKey::Randomness(vdf_id))
            .map(|record| record.seed)
            .ok_or(Error::RandomnessNotFound)
    }

    /// Read-only: whether a verified seed exists for `vdf_id`.
    pub fn has_randomness(env: Env, vdf_id: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Randomness(vdf_id))
    }

    /// Read-only: the full record (seed + transcript metadata) for `vdf_id`.
    pub fn get_randomness_record(env: Env, vdf_id: BytesN<32>) -> Option<RandomnessRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Randomness(vdf_id))
    }
}
