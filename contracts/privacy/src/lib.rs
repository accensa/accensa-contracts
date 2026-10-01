//! Privacy contract: Groth16 verification for concealed escrow amounts
//! (issue #440).
//!
//! A user creates an escrow against a Pedersen commitment to a concealed
//! amount. Verification submits a Groth16 proof (structural mock, see
//! [`groth16`]) whose public inputs carry the concealed amount and the
//! commitment's value/blinding. A successful proof marks the escrow
//! `Verified`; a failed proof or an amount below the escrow floor marks it
//! `Rejected`.
//!
//! # Model
//!
//! - **Commitment.** `sha256(value || blinding)` — a hash-based stand-in for
//!   a real Pedersen commitment, since Soroban has no curve ops.
//! - **Groth16.** Structural verification only (IC count, non-empty
//!   well-sized points, no forged zeros), mirroring
//!   `receipt-anchor/src/zk_verifier.rs`.
//! - **Amount floor.** `public_inputs[0]` decodes as the proven amount and
//!   must be `>= escrow.min_amount`.
//! - **One-shot.** An escrow can be verified at most once; a second call
//!   fails with [`Error::AlreadyVerified`].

#![no_std]

use accensa_common::storage::extend_instance_ttl_default;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, Vec,
};

pub mod groth16;
#[cfg(test)]
mod test;

pub use groth16::{
    commit, i128_from_public_input, public_input_from_i128, verify_concealed_amount,
    verify_groth16, Groth16Proof, PedersenCommitment, VerifyingKey,
};

contractmeta!(key = "name", val = "Privacy");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Errors are local to this contract rather than added to
/// `accensa_common::Error`: every contract that exposes the shared enum
/// embeds all of its variants in its WASM spec, so growing it would enlarge
/// unrelated contracts.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The caller is not the contract admin.
    NotAdmin = 1,
    /// The supplied Groth16 proof failed structural verification.
    InvalidProof = 2,
    /// The commitment is empty, all-zero, or does not match the public
    /// inputs.
    InvalidCommitment = 3,
    /// The proven amount is strictly below the escrow's minimum.
    AmountBelowMinimum = 4,
    /// The escrow has already been verified (or otherwise left `Pending`).
    AlreadyVerified = 5,
}

#[contracttype]
pub enum DataKey {
    /// Admin address.
    Admin,
    /// Installed Groth16 verifying key.
    VerifyingKey,
    /// Next escrow id to assign.
    EscrowCount,
    /// A concealed escrow, keyed by id.
    Escrow(u64),
}

/// Lifecycle of a concealed escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    /// Created, awaiting a proof.
    Pending,
    /// Proof accepted.
    Verified,
    /// Proof rejected (invalid proof or commitment mismatch).
    Rejected,
}

/// A concealed escrow: commitment to the amount plus the verification floor.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    /// Pedersen-style commitment to the concealed amount.
    pub commitment: BytesN<32>,
    /// Minimum proven amount required for acceptance.
    pub min_amount: i128,
    /// Current lifecycle status.
    pub status: EscrowStatus,
}

/// Emitted when a concealed escrow is created.
///
/// Topics: `("escrow_created_event", escrow_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCreatedEvent {
    #[topic]
    pub escrow_id: u64,
    pub commitment: BytesN<32>,
    pub min_amount: i128,
}

/// Emitted when an escrow's proof is accepted.
///
/// Topics: `("escrow_verified_event", escrow_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowVerifiedEvent {
    #[topic]
    pub escrow_id: u64,
}

/// Emitted when an escrow's proof is rejected.
///
/// Topics: `("escrow_rejected_event", escrow_id)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRejectedEvent {
    #[topic]
    pub escrow_id: u64,
}

fn admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("initialized in the constructor")
}

fn load_vk(env: &Env) -> Result<VerifyingKey, Error> {
    env.storage()
        .instance()
        .get(&DataKey::VerifyingKey)
        .ok_or(Error::InvalidProof)
}

fn load_escrow(env: &Env, escrow_id: u64) -> Option<Escrow> {
    env.storage().persistent().get(&DataKey::Escrow(escrow_id))
}

fn save_escrow(env: &Env, escrow_id: u64, escrow: &Escrow) {
    let key = DataKey::Escrow(escrow_id);
    env.storage().persistent().set(&key, escrow);
    let extend_to = accensa_common::storage::DEFAULT_TTL_BUMP.min(env.storage().max_ttl());
    env.storage()
        .persistent()
        .extend_ttl(&key, extend_to, extend_to);
}

#[contract]
pub struct Privacy;

#[contractimpl]
impl Privacy {
    /// Bind this instance to `admin`.
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::EscrowCount, &0u64);
        extend_instance_ttl_default(&env);
    }

    /// Admin-only: install the Groth16 verifying key used by
    /// [`verify_escrow`](Self::verify_escrow).
    pub fn set_verifying_key(env: Env, vk: VerifyingKey) -> Result<(), Error> {
        admin(&env).require_auth();
        if vk.ic.is_empty()
            || vk.alpha_g1.is_empty()
            || vk.beta_g2.is_empty()
            || vk.gamma_g2.is_empty()
            || vk.delta_g2.is_empty()
        {
            return Err(Error::InvalidProof);
        }
        env.storage().instance().set(&DataKey::VerifyingKey, &vk);
        extend_instance_ttl_default(&env);
        Ok(())
    }

    /// Create a concealed escrow against `commitment` with floor `min_amount`.
    /// Returns the new escrow id.
    ///
    /// # Errors
    /// - `InvalidCommitment`: the commitment is all-zero.
    /// - `AmountBelowMinimum`: `min_amount` is negative.
    pub fn create_escrow(env: Env, commitment: BytesN<32>, min_amount: i128) -> Result<u64, Error> {
        if commitment.to_array().iter().all(|&b| b == 0) {
            return Err(Error::InvalidCommitment);
        }
        if min_amount < 0 {
            return Err(Error::AmountBelowMinimum);
        }
        let escrow_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EscrowCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::EscrowCount, &(escrow_id + 1));
        let escrow = Escrow {
            commitment,
            min_amount,
            status: EscrowStatus::Pending,
        };
        save_escrow(&env, escrow_id, &escrow);
        extend_instance_ttl_default(&env);

        EscrowCreatedEvent {
            escrow_id,
            commitment: escrow.commitment,
            min_amount,
        }
        .publish(&env);
        Ok(escrow_id)
    }

    /// Verify `proof` against `escrow_id` using the installed verifying key.
    /// Returns `true` when the escrow is marked `Verified`.
    ///
    /// # Errors
    /// - `AlreadyVerified`: the escrow is no longer `Pending`.
    /// - `AmountBelowMinimum`: the proven amount is below the escrow floor.
    /// - `InvalidProof`: no verifying key is installed.
    pub fn verify_escrow(
        env: Env,
        escrow_id: u64,
        proof: Groth16Proof,
        public_inputs: Vec<BytesN<32>>,
    ) -> Result<bool, Error> {
        let mut escrow = match load_escrow(&env, escrow_id) {
            Some(e) => e,
            None => return Ok(false),
        };
        if escrow.status != EscrowStatus::Pending {
            return Err(Error::AlreadyVerified);
        }
        let vk = load_vk(&env)?;

        // Amount floor is a hard error at the contract boundary so callers
        // can distinguish "wrong proof" from "honest but too small".
        if !public_inputs.is_empty() {
            let amount = i128_from_public_input(&public_inputs.get_unchecked(0));
            if amount < escrow.min_amount {
                return Err(Error::AmountBelowMinimum);
            }
        }

        let ok = verify_concealed_amount(
            &env,
            &escrow.commitment,
            &proof,
            &vk,
            &public_inputs,
            escrow.min_amount,
        )?;

        if !ok {
            escrow.status = EscrowStatus::Rejected;
            save_escrow(&env, escrow_id, &escrow);
            EscrowRejectedEvent { escrow_id }.publish(&env);
            extend_instance_ttl_default(&env);
            return Ok(false);
        }

        escrow.status = EscrowStatus::Verified;
        save_escrow(&env, escrow_id, &escrow);
        EscrowVerifiedEvent { escrow_id }.publish(&env);
        extend_instance_ttl_default(&env);
        Ok(true)
    }

    /// The escrow record, if it exists.
    pub fn get_escrow(env: Env, escrow_id: u64) -> Option<Escrow> {
        load_escrow(&env, escrow_id)
    }

    /// The installed verifying key.
    ///
    /// # Errors
    /// - `InvalidProof`: no key has been set.
    pub fn get_verifying_key(env: Env) -> Result<VerifyingKey, Error> {
        load_vk(&env)
    }

    /// Read-only: the contract admin.
    pub fn get_admin(env: Env) -> Address {
        admin(&env)
    }
}
