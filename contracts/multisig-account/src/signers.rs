//! Dynamic threshold rotation for multisig-account signers.
//!
//! Supports atomic multi-signer threshold reconfiguration in a single call
//! to avoid intermediate insecure states when replacing signers.
//!
//! Validates invariant: 1 <= new_threshold <= total_active_signers.
//! Prevents duplicate public keys and zeroed addresses.
//! Emits SignersRotated audit event.
//!
//! Rotation also maintains the issue #434 weighted bookkeeping: newly added
//! signers receive [`DEFAULT_SIGNER_WEIGHT`](crate::weights::DEFAULT_SIGNER_WEIGHT)
//! and are appended to the materialized signer list, removed signers are
//! dropped from both, and the rotation is refused when the resulting aggregate
//! signer weight would fall below `new_threshold`.

use soroban_sdk::{contractevent, Address, Env, Vec};

use crate::weights;
use crate::DataKey;
use crate::Error;

/// Strkeys of the all-zero account and contract addresses.
const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const ZERO_CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

fn is_zero_address(env: &Env, addr: &Address) -> bool {
    *addr == Address::from_str(env, ZERO_ACCOUNT) || *addr == Address::from_str(env, ZERO_CONTRACT)
}

/// Rotate signers and threshold atomically in a single call.
///
/// Requires the account's own authorization, i.e. `threshold` of the
/// *current* signers must approve the rotation.
///
/// # Parameters
/// - `to_add`: new signers to add (must not already be signers)
/// - `to_remove`: signers to remove (must be existing signers)
/// - `new_threshold`: new threshold (must satisfy 1 <= threshold <= total_active_signers)
///
/// # Returns
/// `Ok(())` on success, or `Err` if validation fails.
///
/// # Events emitted on success
/// - [`SignersRotated`](crate::signers::SignersRotated)
pub fn rotate_signers_and_threshold(
    env: &Env,
    to_add: Vec<Address>,
    to_remove: Vec<Address>,
    new_threshold: u32,
) -> Result<(), Error> {
    env.current_contract_address().require_auth();

    // Validate new_threshold: 1 <= new_threshold
    if new_threshold < 1 {
        return Err(Error::InsufficientSignatures);
    }

    // Check for duplicate addresses in to_add
    let mut seen_in_add = Vec::<Address>::new(env);
    for addr in to_add.iter() {
        if seen_in_add.contains(&addr) {
            return Err(Error::InsufficientSignatures);
        }
        seen_in_add.push_back(addr);
    }

    // Can't both add and remove the same address
    for addr in to_remove.iter() {
        if seen_in_add.contains(&addr) {
            return Err(Error::InsufficientSignatures);
        }
    }

    // Reject zeroed addresses.
    for addr in to_add.iter().chain(to_remove.iter()) {
        if is_zero_address(env, &addr) {
            return Err(Error::InsufficientSignatures);
        }
    }

    let previous_threshold: u32 = env
        .storage()
        .instance()
        .get(&DataKey::Threshold)
        .unwrap_or(1);

    // Build the resulting signer set and its aggregate weight *before*
    // writing anything, so a rejected rotation leaves storage untouched
    // (returning `Err` does not roll back writes on its own).
    let mut remaining = weights::signer_list(env);
    let mut total = weights::total_weight(env);

    for addr in to_remove.iter() {
        if !remaining.contains(&addr) {
            return Err(Error::UnknownSigner);
        }
        total = total
            .checked_sub(weights::signer_weight(env, &addr))
            .ok_or(Error::TotalWeightBelowThreshold)?;
        let mut kept = Vec::new(env);
        for entry in remaining.iter() {
            if entry != addr {
                kept.push_back(entry);
            }
        }
        remaining = kept;
    }

    for addr in to_add.iter() {
        // Re-adding an existing signer would double-count its weight.
        if remaining.contains(&addr) {
            return Err(Error::SignerAlreadyRegistered);
        }
        remaining.push_back(addr.clone());
        total = total
            .checked_add(weights::DEFAULT_SIGNER_WEIGHT)
            .ok_or(Error::TotalWeightBelowThreshold)?;
    }

    // Invariant: `total_signers_weight >= required_threshold`.
    if total < new_threshold {
        return Err(Error::TotalWeightBelowThreshold);
    }

    for addr in to_remove.iter() {
        env.storage()
            .persistent()
            .remove(&DataKey::Signer(addr.clone()));
        weights::clear_signer_weight(env, &addr);
    }
    for addr in to_add.iter() {
        env.storage()
            .persistent()
            .set(&DataKey::Signer(addr.clone()), &());
        weights::store_signer_weight(env, &addr, weights::DEFAULT_SIGNER_WEIGHT);
    }
    weights::store_signer_list(env, &remaining);

    env.storage()
        .instance()
        .set(&DataKey::Threshold, &new_threshold);

    SignersRotated {
        previous_threshold,
        new_threshold,
        added: to_add,
        removed: to_remove,
    }
    .publish(env);

    Ok(())
}

/// Audit event emitted when signers and threshold are rotated.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignersRotated {
    #[topic]
    pub previous_threshold: u32,
    pub new_threshold: u32,
    pub added: Vec<Address>,
    pub removed: Vec<Address>,
}

#[cfg(test)]
mod tests {
    use crate::{Error, MultisigAccount, MultisigAccountClient};
    use soroban_sdk::{testutils::Address as _, vec, Address, Env};

    use super::{ZERO_ACCOUNT, ZERO_CONTRACT};

    fn setup() -> (Env, MultisigAccountClient<'static>, Address) {
        let env = Env::default();
        let signer = Address::generate(&env);
        let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
        (env.clone(), MultisigAccountClient::new(&env, &id), signer)
    }

    #[test]
    fn rotation_replaces_signers_and_threshold() {
        let (env, client, old) = setup();
        env.mock_all_auths();
        let a = Address::generate(&env);
        let b = Address::generate(&env);

        client.rotate_signers_and_threshold(
            &vec![&env, a.clone(), b.clone()],
            &vec![&env, old.clone()],
            &2,
        );

        assert!(client.is_signer(&a) && client.is_signer(&b));
        assert!(!client.is_signer(&old));
        assert_eq!(client.get_threshold(), 2);
    }

    #[test]
    fn rotation_rejects_zero_addresses() {
        let (env, client, _) = setup();
        env.mock_all_auths();
        for zero in [ZERO_ACCOUNT, ZERO_CONTRACT] {
            let zero = Address::from_str(&env, zero);
            assert_eq!(
                client.try_rotate_signers_and_threshold(&vec![&env, zero], &vec![&env], &1),
                Err(Ok(Error::InsufficientSignatures))
            );
        }
    }

    #[test]
    #[should_panic]
    fn rotation_requires_account_auth() {
        let (env, client, _) = setup();
        let a = Address::generate(&env);
        client.rotate_signers_and_threshold(&vec![&env, a], &vec![&env], &1);
    }
}
