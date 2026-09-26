//! A small threshold multisig custom account for Soroban.
//!
//! soroban-sdk's `Address` is not required to be a keypair — it can be any
//! contract whose address implements `__check_auth`. This contract is such an
//! account: it requires that a call carry **at least `threshold`** of its
//! registered signers (as delegated signers on the authorization), so it can be
//! used as the `merchant`/admin of `ReceiptAnchor` or `RefundVault` and make
//! those contracts require multiple signatures without any change to them.
//!
//! Operation:
//! - `__constructor(signers, threshold)` records the initial signer set.
//! - When a privileged app contract calls `merchant.require_auth()`, the host
//!   invokes this account's [`__check_auth`](CustomAccountInterface::__check_auth).
//! - `__check_auth` requires every attached delegated signer to be a registered
//!   signer, and the count of distinct delegates to be at least `threshold`.
//!
//! - Governance may set a per-token daily allowance ([`limits`]); a routine
//!   token transfer authorized by fewer than `threshold` signers is then
//!   accepted while it fits in each signer's remaining daily quota.
//!
//! This is the piece referenced by `docs/SECURITY_MODEL.md` and
//! `DEPLOYMENTS.md`: initialize an app contract with the multisig account's
//! address, and privileged calls now need `threshold` approved signers.

#![no_std]

mod admin;
pub mod crypto;
mod errors;
pub mod limits;
mod signers;
pub mod timelock;
pub mod weights;

pub use admin::{GuardianSetEvent, PausedEvent, UnpausedEvent};
pub use errors::Error;

// The helpers are only needed by tests; gate them so the contract itself stays
// minimal. Unit tests within this crate (`#[cfg(test)]`) and downstream
// integration tests (which enable the `testutils` feature through their
// dev-dependency) both get the module.
#[cfg(any(test, feature = "testutils"))]
pub mod testutils;

#[cfg(test)]
mod test;

use soroban_sdk::{
    auth::CustomAccountInterface, contract, contractimpl, contracttype, Address, Bytes, BytesN,
    Env, Vec,
};

#[contracttype]
pub enum DataKey {
    /// Instance storage: the number of signatures required (`u32`).
    Threshold,
    /// Persistent storage per registered signer: marks it as authorized.
    Signer(Address),
    /// Persistent: a registered signer's voting weight (issue #434).
    SignerWeight(Address),
    /// Persistent: the registered signer set, in registration order
    /// (issue #434). Soroban storage cannot be iterated, so the aggregate
    /// signer weight needs this materialized list.
    SignerList,
    /// Temporary storage per approval: marks a signer has approved a queued transaction.
    TimelockApproval(u64, Address),
    /// Instance: the next available queue ID counter.
    QueueCount,
    /// Guardian address for timelock cancellation.
    TimelockGuardian,
    /// Persistent: a queued transaction identified by its queue ID.
    QueuedTransaction(u64),
    /// Instance: daily allowance for sub-threshold spends of a token (`i128`).
    DailyLimit(Address),
    /// Instance: a signer's spending of a token in the current window
    /// ([`limits::SpendingLimit`]).
    Spending(Address, Address),
    /// Instance: `true` while the emergency pause is engaged.
    Paused,
    /// Instance: security guardian allowed to pause/unpause on its own.
    PauseGuardian,
}

/// A threshold account enforcing that `threshold` distinct registered signers
/// approve every authorization.
#[contract]
pub struct MultisigAccount;

#[contractimpl]
impl MultisigAccount {
    /// Create the account with an initial signer set.
    ///
    /// `threshold` becomes the account's required aggregate signer weight and
    /// defaults to `signers.len()` (all signers required) when `0` is passed,
    /// so a single-signer account still needs that signer. Every signer is
    /// registered with [`weights::DEFAULT_SIGNER_WEIGHT`] (`1`), so an account
    /// that never calls the weighted entry points behaves exactly as a plain
    /// `threshold`-of-`signers` account (issue #434).
    ///
    /// Duplicate addresses in `signers` are registered once, so they cannot
    /// contribute their weight twice.
    ///
    /// # Errors
    /// - [`Error::TotalWeightBelowThreshold`] when `threshold` exceeds the
    ///   aggregate signer weight (which would make the account unable to ever
    ///   authorize anything) — including a `0`-weight, empty signer set.
    pub fn __constructor(env: Env, signers: Vec<Address>, threshold: u32) -> Result<(), Error> {
        let mut unique: Vec<Address> = Vec::new(&env);
        for signer in signers.iter() {
            if unique.contains(&signer) {
                continue;
            }
            env.storage()
                .persistent()
                .set(&DataKey::Signer(signer.clone()), &());
            weights::store_signer_weight(&env, &signer, weights::DEFAULT_SIGNER_WEIGHT);
            unique.push_back(signer);
        }
        weights::store_signer_list(&env, &unique);

        let effective = if threshold == 0 {
            unique.len()
        } else {
            threshold
        };

        // Invariant: `total_signers_weight >= required_threshold`. Without it
        // the account would be permanently unable to authorize.
        if effective == 0 || effective > weights::total_weight(&env) {
            return Err(Error::TotalWeightBelowThreshold);
        }

        env.storage()
            .instance()
            .set(&DataKey::Threshold, &effective);
        Ok(())
    }

    /// Read the current threshold.
    pub fn get_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1)
    }

    /// True if `signer` is registered on this account.
    pub fn is_signer(env: Env, signer: Address) -> bool {
        env.storage().persistent().has(&DataKey::Signer(signer))
    }

    /// Read-only: a registered signer's voting weight (issue #434).
    ///
    /// Defaults to [`weights::DEFAULT_SIGNER_WEIGHT`] for a signer that has
    /// never been given an explicit weight.
    pub fn get_signer_weight(env: Env, signer: Address) -> u32 {
        weights::signer_weight(&env, &signer)
    }

    /// Read-only: the aggregate weight of every registered signer
    /// (issue #434). Always `>= get_threshold()` once the account exists.
    pub fn get_total_weight(env: Env) -> u32 {
        weights::total_weight(&env)
    }

    /// Governance-only: set a registered signer's voting weight (issue #434).
    ///
    /// Requires this account's own authorization, i.e. the current weighted
    /// threshold must approve the change. Rejects a zero weight and any change
    /// that would leave the aggregate signer weight below the threshold.
    ///
    /// # Events emitted on success
    /// - [`weights::SignerWeightSet`]
    pub fn set_signer_weight(env: Env, signer: Address, weight: u32) -> Result<(), Error> {
        weights::set_signer_weight(&env, signer, weight)
    }

    /// Governance-only: register `signer` with an explicit voting weight
    /// (issue #434).
    ///
    /// # Events emitted on success
    /// - [`weights::SignerAdded`]
    pub fn add_signer(env: Env, signer: Address, weight: u32) -> Result<(), Error> {
        weights::add_signer(&env, signer, weight)
    }

    /// Governance-only: drop `signer` from the account (issue #434).
    ///
    /// Refused while removing the signer would push the aggregate weight below
    /// the threshold; lower the threshold first.
    ///
    /// # Events emitted on success
    /// - [`weights::SignerRemoved`]
    pub fn remove_signer(env: Env, signer: Address) -> Result<(), Error> {
        weights::remove_signer(&env, signer)
    }

    /// Rotate signers and threshold atomically in a single call.
    ///
    /// # Parameters
    /// - `to_add`: new signers to add (must not already be signers, must not be zero address)
    /// - `to_remove`: signers to remove (must be existing signers)
    /// - `new_threshold`: new threshold (must satisfy 1 <= threshold <= total_active_signers)
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err` if validation fails.
    ///
    /// # Events emitted on success
    /// - [`SignersRotated`](crate::signers::SignersRotated)
    pub fn rotate_signers_and_threshold(
        env: Env,
        to_add: Vec<Address>,
        to_remove: Vec<Address>,
        new_threshold: u32,
    ) -> Result<(), Error> {
        signers::rotate_signers_and_threshold(&env, to_add, to_remove, new_threshold)
    }

    /// Set the daily allowance for sub-threshold transfers of `token`
    /// (`0` disables it). Requires the full threshold.
    pub fn set_daily_limit(env: Env, token: Address, limit: i128) -> Result<(), Error> {
        limits::set_daily_limit(&env, token, limit)
    }

    /// The daily allowance configured for `token` (`0` = none).
    pub fn get_daily_limit(env: Env, token: Address) -> i128 {
        limits::daily_limit(&env, &token)
    }

    /// What `signer` has spent of `token` in the current 24-hour window.
    pub fn get_spent_today(env: Env, signer: Address, token: Address) -> i128 {
        limits::spent_today(&env, &signer, &token)
    }

    /// Verify an Ed25519 `signature` by `public_key` over `message`,
    /// rejecting malleable encodings.
    ///
    /// Returns [`Error::NonCanonicalSignature`] if the signature's `s` scalar
    /// is not reduced modulo the group order; otherwise defers to the host's
    /// `ed25519_verify`, which traps on an invalid signature.
    pub fn verify_ed25519(
        env: Env,
        public_key: BytesN<32>,
        message: Bytes,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        crypto::verify_ed25519_canonical(&env, &public_key, &message, &signature)
    }

    /// Engage the emergency pause. While paused, `__check_auth` refuses every
    /// authorization except this account's own `pause`, `unpause`,
    /// `set_guardian` and `rotate_signers_and_threshold`.
    ///
    /// `caller` must be this account's own address (authorized by `threshold`
    /// signers) or the security guardian; anyone else gets
    /// [`Error::Unauthorized`].
    ///
    /// # Events emitted on success
    /// - [`PausedEvent`]
    pub fn pause(env: Env, caller: Address) -> Result<(), Error> {
        admin::pause(&env, caller)
    }

    /// Lift the emergency pause. Same authorization rules as [`Self::pause`].
    ///
    /// # Events emitted on success
    /// - [`UnpausedEvent`]
    pub fn unpause(env: Env, caller: Address) -> Result<(), Error> {
        admin::unpause(&env, caller)
    }

    /// True while the emergency pause is engaged.
    pub fn is_paused(env: Env) -> bool {
        admin::is_paused(&env)
    }

    /// Set (`Some`) or clear (`None`) the security guardian. Requires this
    /// account's own threshold authorization.
    ///
    /// # Events emitted on success
    /// - [`GuardianSetEvent`]
    pub fn set_guardian(env: Env, guardian: Option<Address>) {
        admin::set_guardian(&env, guardian)
    }

    /// The current security guardian, if any.
    pub fn get_guardian(env: Env) -> Option<Address> {
        admin::get_guardian(&env)
    }
}

#[contractimpl]
impl CustomAccountInterface for MultisigAccount {
    // The account verifies no cryptographic signature of its own; authorisation
    // is inferred from the attached delegated signers the host supplies.
    type Signature = ();
    type Error = Error;

    fn __check_auth(
        env: Env,
        _signature_payload: soroban_sdk::crypto::Hash<32>,
        _signatures: (),
        auth_contexts: Vec<soroban_sdk::auth::Context>,
    ) -> Result<(), Error> {
        // Circuit breaker: while paused, only this account's own recovery
        // calls may be authorized — never an outbound call.
        if admin::is_paused(&env)
            && !auth_contexts
                .iter()
                .all(|ctx| admin::is_allowed_while_paused(&env, &ctx))
        {
            return Err(Error::Paused);
        }

        let threshold = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);

        let delegates = env.custom_account().get_delegated_signers();

        // Sum the weights of the attached approvers, rejecting any delegate
        // that is not a registered signer (issue #434). With the default
        // weight of 1 per signer this is exactly the old distinct-signer
        // count.
        let approving_weight = weights::tally_weight(&env, &delegates)?;

        if approving_weight < threshold {
            // Below the weighted threshold, only routine spends within the
            // daily allowance are admitted (issue #413).
            return limits::authorize_within_limits(&env, &delegates, &auth_contexts);
        }

        Ok(())
    }
}
