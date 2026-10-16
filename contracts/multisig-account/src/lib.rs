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
//! - Approvals of a queued timelock transaction expire after 14 days
//!   ([`ttl`]); stale ones are pruned before the quorum is evaluated.
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
pub mod ttl;
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
    /// Persistent storage per approval: the signer's approval of a queued
    /// transaction, stamped with `created_at` (`ttl::ApprovalRecord`), so it
    /// can be expired after the 14-day TTL (issue #449).
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

    /// Execute a batch of transactions
    pub fn execute_batch(env: Env, calls: Vec<Call>) -> Vec<soroban_sdk::Val> {
        env.current_contract_address().require_auth();

        let mut results = Vec::new(&env);
        for call in calls.iter() {
            let res = env.invoke_contract(&call.contract, &call.function, call.args);
            results.push_back(res);
        }
        results
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

        // Fail closed (issue #644): `Threshold` is written exactly once, in
        // `__constructor`, and no entry point in this crate extends the
        // instance TTL it lives on, so the entry can lapse. Reading a missing
        // entry as `1` would let a single attached signer authorize a 2-of-3
        // account; without the stored threshold the quorum cannot be
        // evaluated, so authorization is refused instead.
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .ok_or(Error::InsufficientSignatures)?;

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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Call {
    pub contract: Address,
    pub function: soroban_sdk::Symbol,
    pub args: Vec<soroban_sdk::Val>,
}

#[cfg(test)]
mod tests {
    //! Issue #644: a missing `DataKey::Threshold` must fail closed.
    //!
    //! `Threshold` is written exactly once, in `__constructor`, and no entry
    //! point in this crate extends the instance TTL it lives on, so the entry
    //! can lapse. These tests drive the account's real `__check_auth` with
    //! hand-built delegated-signer auth entries (never `mock_all_auths`), and
    //! assert that an authorization the old `unwrap_or(1)` default would have
    //! granted is refused once the entry is gone.

    use crate::testutils::make_auth_entry_with_nonce;
    use crate::{DataKey, Error, MultisigAccount, MultisigAccountClient};
    use soroban_sdk::{
        auth::{Context, ContractContext},
        testutils::Address as _,
        vec, Address, BytesN, Env, IntoVal, Symbol, Val,
    };

    /// A real 2-of-3 account, driven through hand-built auth entries so the
    /// host runs the account's actual `__check_auth`.
    struct Fx {
        env: Env,
        account: Address,
        s1: Address,
        s2: Address,
        s3: Address,
        nonce: core::cell::Cell<i64>,
    }

    fn setup() -> Fx {
        let env = Env::default();
        let s1 = Address::generate(&env);
        let s2 = Address::generate(&env);
        let s3 = Address::generate(&env);
        let account = env.register(
            MultisigAccount,
            (vec![&env, s1.clone(), s2.clone(), s3.clone()], 2u32),
        );
        Fx {
            env,
            account,
            s1,
            s2,
            s3,
            nonce: core::cell::Cell::new(1),
        }
    }

    impl Fx {
        fn client(&self) -> MultisigAccountClient<'_> {
            MultisigAccountClient::new(&self.env, &self.account)
        }

        /// Try to set the guardian through the account's real `__check_auth`
        /// with `delegates` attached. Returns whether the host accepted it.
        fn try_set_guardian(&self, guardian: &Address, delegates: &[Address]) -> bool {
            let args: [Val; 1] = [Some(guardian.clone()).into_val(&self.env)];
            let nonce = self.nonce.get();
            self.nonce.set(nonce + 1);
            let entry = make_auth_entry_with_nonce(
                &self.env,
                &self.account,
                &self.account,
                "set_guardian",
                &args,
                delegates,
                nonce,
            );
            self.env.set_auths(&[entry]);
            self.client()
                .try_set_guardian(&Some(guardian.clone()))
                .is_ok()
        }

        /// Drop `DataKey::Threshold` from instance storage: the state a
        /// lapsed TTL leaves behind (issue #644).
        fn remove_threshold(&self) {
            self.env.as_contract(&self.account, || {
                self.env.storage().instance().remove(&DataKey::Threshold);
                assert!(!self.env.storage().instance().has(&DataKey::Threshold));
            });
        }

        /// Invoke the account's `__check_auth` directly so its own error code
        /// is observable.
        fn check_auth(&self) -> Result<(), Error> {
            self.env.set_auths(&[]);
            let context = Context::Contract(ContractContext {
                contract: self.account.clone(),
                fn_name: Symbol::new(&self.env, "set_guardian"),
                args: vec![&self.env],
            });
            self.env
                .try_invoke_contract_check_auth::<Error>(
                    &self.account,
                    &BytesN::from_array(&self.env, &[0; 32]),
                    ().into_val(&self.env),
                    &vec![&self.env, context],
                )
                .map_err(|e| e.expect("__check_auth returned a non-contract error"))
        }
    }

    #[test]
    fn a_lapsed_threshold_entry_rejects_authorization() {
        let fx = setup();
        assert_eq!(fx.client().get_threshold(), 2);

        // Baseline, threshold present: one registered delegate is not enough
        // for a 2-of-3 account, two are.
        let first = Address::generate(&fx.env);
        assert!(!fx.try_set_guardian(&first, core::slice::from_ref(&fx.s1)));
        assert_eq!(fx.client().get_guardian(), None);
        let second = Address::generate(&fx.env);
        assert!(fx.try_set_guardian(&second, &[fx.s1.clone(), fx.s2.clone()]));
        assert_eq!(fx.client().get_guardian(), Some(second.clone()));

        // The threshold entry lapses: nothing in this crate extends the
        // instance TTL it lives on.
        fx.remove_threshold();

        // The exact bug: a single registered delegate on a 2-of-3 account.
        // The old `unwrap_or(1)` default authorized this; it must be refused.
        let third = Address::generate(&fx.env);
        assert!(!fx.try_set_guardian(&third, core::slice::from_ref(&fx.s3)));
        assert_eq!(fx.client().get_guardian(), Some(second.clone()));

        // Not even the previously valid 2-of-3 quorum can be evaluated
        // without the stored threshold — authority never widens.
        let fourth = Address::generate(&fx.env);
        assert!(!fx.try_set_guardian(&fourth, &[fx.s1.clone(), fx.s2.clone()]));
        assert_eq!(fx.client().get_guardian(), Some(second.clone()));

        // And the account's own error code says why.
        assert_eq!(fx.check_auth(), Err(Error::InsufficientSignatures));
    }
}
