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
//! This is the piece referenced by `docs/SECURITY_MODEL.md` and
//! `DEPLOYMENTS.md`: initialize an app contract with the multisig account's
//! address, and privileged calls now need `threshold` approved signers.

#![no_std]

mod timelock;
mod signers;

// The helpers are only needed by tests; gate them so the contract itself stays
// minimal. Unit tests within this crate (`#[cfg(test)]`) and downstream
// integration tests (which enable the `testutils` feature through their
// dev-dependency) both get the module.
#[cfg(any(test, feature = "testutils"))]
pub mod testutils;

use soroban_sdk::{
    auth::CustomAccountInterface, contract, contracterror, contractimpl, contracttype, Address,
    Env, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// A delegated signer is not a registered signer of this account.
    UnknownSigner = 1,
    /// Fewer than `threshold` distinct signers authorized the call.
    InsufficientSignatures = 2,
    /// The caller is not authorized to perform this action.
    Unauthorized = 3,
    /// The timelock period has not yet elapsed.
    TimelockNotExpired = 4,
    /// The requested proposal or queue entry was not found.
    ProposalNotFound = 5,
    /// The signer has already approved this transaction.
    AlreadyVoted = 6,
}

#[contracttype]
pub enum DataKey {
    /// Instance storage: the number of signatures required (`u32`).
    Threshold,
    /// Persistent storage per registered signer: marks it as authorized.
    Signer(Address),
    /// Temporary storage per approval: marks a signer has approved a queued transaction.
    TimelockApproval(u64, Address),
    /// Instance: the next available queue ID counter.
    QueueCount,
    /// Guardian address for timelock cancellation.
    TimelockGuardian,
    /// Persistent: a queued transaction identified by its queue ID.
    QueuedTransaction(u64),
}

/// A threshold account enforcing that `threshold` distinct registered signers
/// approve every authorization.
#[contract]
pub struct MultisigAccount;

#[contractimpl]
impl MultisigAccount {
    /// Create the account with an initial signer set.
    ///
    /// `threshold` defaults to `signers.len()` (all signers required) when `0`
    /// is passed, so a single-signer account still needs that signer.
    pub fn __constructor(env: Env, signers: Vec<Address>, threshold: u32) {
        let effective = if threshold == 0 {
            signers.len()
        } else {
            threshold
        };
        for signer in signers.iter() {
            env.storage()
                .persistent()
                .set(&DataKey::Signer(signer), &());
        }
        env.storage()
            .instance()
            .set(&DataKey::Threshold, &effective);
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
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Call {
    pub contract: Address,
    pub function: soroban_sdk::Symbol,
    pub args: Vec<soroban_sdk::Val>,
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
        _auth_contexts: Vec<soroban_sdk::auth::Context>,
    ) -> Result<(), Error> {
        let threshold = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);

        let delegates = env.custom_account().get_delegated_signers();

        for delegate in delegates.iter() {
            if !env.storage().persistent().has(&DataKey::Signer(delegate)) {
                return Err(Error::UnknownSigner);
            }
        }

        if delegates.len() < threshold {
            return Err(Error::InsufficientSignatures);
        }

        Ok(())
    }
}
// audit implementation
