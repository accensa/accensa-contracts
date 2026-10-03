//! Timelock delay queue for sensitive admin actions in the multisig account.
//!
//! High-risk operations (signer set updates, threshold decreases, code
//! upgrades) are queued with a mandatory delay (48 hours in ledger
//! sequence increments). Authorized signers or a guardian can cancel
//! malicious or erroneous queued actions during the delay window.
//! Execution is enforced after the timelock elapses and rejected before.
//!
//! Approvals themselves expire: each one is stamped with `created_at` and
//! only counts toward `required_approvals` while it is younger than
//! [`ttl::APPROVAL_TTL_SECONDS`] (14 days, issue #449). See [`crate::ttl`].

use soroban_sdk::{contracttype, Address, BytesN, Env, Vec};

use crate::ttl;
use crate::DataKey;
use crate::Error;

/// A queued transaction awaiting timelock execution.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedTransaction {
    /// Hash of the call to be executed.
    pub call_hash: BytesN<32>,
    /// Ledger sequence at which execution becomes allowed.
    pub execution_ledger: u32,
    /// Number of approvals counted so far (stale ones are pruned away).
    pub approval_count: u32,
    /// Threshold number of approvals required for execution.
    pub required_approvals: u32,
    /// Address of the guardian who can cancel this transaction.
    pub guardian: Address,
    /// Signers whose approvals are still recorded, in approval order; the
    /// materialized list Soroban's non-iterable storage needs in order to
    /// prune expired approvals (issue #449).
    pub approvers: Vec<Address>,
}

/// Default timelock delay in ledger sequences (48 hours at ~5s/ledger ≈ 345600 ledgers).
pub const DEFAULT_TIMELOCK_DELAY: u32 = 345600;

/// Queue a high-risk transaction for delayed execution.
///
/// The transaction enters a queued state and cannot be executed
/// until `execution_ledger` is reached. The `guardian` can cancel
/// the transaction during the delay window.
pub fn queue_transaction(
    env: &Env,
    call_hash: BytesN<32>,
    execution_delay: u32,
    required_approvals: u32,
    guardian: Address,
) -> u64 {
    let execution_ledger = env.ledger().sequence().saturating_add(execution_delay);

    let queue_id = env
        .storage()
        .instance()
        .get(&DataKey::QueueCount)
        .unwrap_or(0u64)
        + 1;

    let queued = QueuedTransaction {
        call_hash,
        execution_ledger,
        approval_count: 0,
        required_approvals,
        guardian,
        approvers: Vec::new(env),
    };

    store_queued(env, queue_id, &queued);
    env.storage()
        .instance()
        .set(&DataKey::QueueCount, &queue_id);

    queue_id
}

/// Persist `queued` under its queue ID.
fn store_queued(env: &Env, queue_id: u64, queued: &QueuedTransaction) {
    env.storage()
        .persistent()
        .set(&DataKey::QueuedTransaction(queue_id), queued);
}

/// Execute a queued transaction after the timelock has elapsed.
///
/// Approvals are pruned against the 14-day TTL before the quorum check
/// (issue #449), so only fresh approvals are counted.
///
/// Returns `Ok(())` if the transaction was executed.
/// Returns `Err(Error::TimelockNotExpired)` if the timelock has not yet elapsed.
/// Returns `Err(Error::StaleSignature)` if expired approvals left the fresh
/// count below `required_approvals`.
/// Returns `Err(Error::InsufficientSignatures)` if there were never enough
/// approvals.
/// Returns `Err(Error::ProposalNotFound)` if the queue ID does not exist.
pub fn execute_queued_transaction(env: &Env, queue_id: u64) -> Result<(), Error> {
    crate::admin::require_not_paused(env)?;
    let mut queued = get_queued_transaction(env, queue_id)?;

    if env.ledger().sequence() < queued.execution_ledger {
        return Err(Error::TimelockNotExpired);
    }

    // Drop approvals that aged past the TTL before counting quorum: a stale
    // partial signature must never be executed (issue #449).
    let pruned = ttl::prune_stale_approvals(env, queue_id, &mut queued);
    if pruned > 0 {
        store_queued(env, queue_id, &queued);
    }

    if queued.approval_count < queued.required_approvals {
        return if pruned > 0 {
            Err(Error::StaleSignature)
        } else {
            Err(Error::InsufficientSignatures)
        };
    }

    // The entry is consumed, so no approval may outlive it.
    ttl::clear_approvals(env, queue_id, &queued.approvers);
    env.storage()
        .persistent()
        .remove(&DataKey::QueuedTransaction(queue_id));
    Ok(())
}

/// Cancel a queued transaction during the delay window.
///
/// Only the transaction's guardian or a registered signer can cancel.
/// Returns `Err(Error::TimelockNotExpired)` if the timelock has already elapsed.
pub fn cancel_queued_transaction(env: &Env, queue_id: u64, caller: &Address) -> Result<(), Error> {
    let queued = get_queued_transaction(env, queue_id)?;

    if env.ledger().sequence() >= queued.execution_ledger {
        return Err(Error::TimelockNotExpired);
    }

    let is_signer = env
        .storage()
        .persistent()
        .has(&DataKey::Signer(caller.clone()));
    if *caller != queued.guardian && !is_signer {
        return Err(Error::Unauthorized);
    }

    ttl::clear_approvals(env, queue_id, &queued.approvers);
    env.storage()
        .persistent()
        .remove(&DataKey::QueuedTransaction(queue_id));
    Ok(())
}

/// Approve a queued transaction. Each authorized signer can approve once.
///
/// The approval is recorded with `created_at = now` and only counts toward
/// the quorum while younger than [`ttl::APPROVAL_TTL_SECONDS`]
/// (issue #449); approvals that expired in the meantime are pruned first,
/// which also means a signer whose previous approval went stale may vote
/// again.
///
/// Returns `Ok(())` if the approval was recorded.
/// Returns `Err(Error::AlreadyVoted)` if the signer still holds a fresh
/// approval.
pub fn approve_queued_transaction(env: &Env, queue_id: u64, signer: &Address) -> Result<(), Error> {
    let mut queued = get_queued_transaction(env, queue_id)?;

    // Expired approvals never count, so they must not block a fresh vote.
    let pruned = ttl::prune_stale_approvals(env, queue_id, &mut queued);
    if pruned > 0 {
        store_queued(env, queue_id, &queued);
    }

    if queued.approvers.contains(signer) {
        return Err(Error::AlreadyVoted);
    }

    ttl::record_approval(env, queue_id, signer);
    queued.approvers.push_back(signer.clone());
    queued.approval_count = queued.approvers.len();
    store_queued(env, queue_id, &queued);

    Ok(())
}

/// Read-only: fetch a queued transaction by ID.
pub fn get_queued_transaction(env: &Env, queue_id: u64) -> Result<QueuedTransaction, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::QueuedTransaction(queue_id))
        .ok_or(Error::ProposalNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MultisigAccount;
    use soroban_sdk::testutils::{Address as _, Ledger as _};
    use soroban_sdk::vec;

    /// Register a one-signer account so the helpers run inside a contract
    /// storage context. Returns `(env, contract_id, signer)`.
    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let signer = Address::generate(&env);
        let id = env.register(MultisigAccount, (vec![&env, signer.clone()], 1u32));
        (env, id, signer)
    }

    fn hash(env: &Env) -> BytesN<32> {
        BytesN::from_array(env, &[1u8; 32])
    }

    #[test]
    fn test_queue_and_execute() {
        let (env, id, _) = setup();
        let guardian = Address::generate(&env);

        env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), 10, 1, guardian.clone());
            let queued = get_queued_transaction(&env, queue_id).unwrap();

            assert_eq!(queued.call_hash, hash(&env));
            assert_eq!(queued.required_approvals, 1);
            assert_eq!(queued.execution_ledger, env.ledger().sequence() + 10);
        });
    }

    #[test]
    fn test_execute_before_timelock_fails() {
        let (env, id, _) = setup();
        let guardian = Address::generate(&env);

        env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), DEFAULT_TIMELOCK_DELAY, 1, guardian);
            assert_eq!(
                execute_queued_transaction(&env, queue_id),
                Err(Error::TimelockNotExpired)
            );
        });
    }

    #[test]
    fn test_cancel_during_delay() {
        let (env, id, _) = setup();
        let guardian = Address::generate(&env);

        env.as_contract(&id, || {
            let queue_id = queue_transaction(
                &env,
                hash(&env),
                DEFAULT_TIMELOCK_DELAY,
                1,
                guardian.clone(),
            );
            assert!(cancel_queued_transaction(&env, queue_id, &guardian).is_ok());
            assert!(get_queued_transaction(&env, queue_id).is_err());
        });
    }

    #[test]
    fn test_cancel_by_stranger_fails() {
        let (env, id, _) = setup();
        let guardian = Address::generate(&env);
        let stranger = Address::generate(&env);

        env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), DEFAULT_TIMELOCK_DELAY, 1, guardian);
            assert_eq!(
                cancel_queued_transaction(&env, queue_id, &stranger),
                Err(Error::Unauthorized)
            );
        });
    }

    #[test]
    fn test_cancel_after_timelock_fails() {
        let (env, id, _) = setup();
        let guardian = Address::generate(&env);

        let queue_id = env.as_contract(&id, || {
            queue_transaction(&env, hash(&env), 0, 1, guardian.clone())
        });
        env.ledger()
            .with_mut(|l| l.sequence_number += DEFAULT_TIMELOCK_DELAY + 1);

        env.as_contract(&id, || {
            assert!(cancel_queued_transaction(&env, queue_id, &guardian).is_err());
        });
    }

    #[test]
    fn test_approve_and_execute() {
        let (env, id, signer) = setup();
        let guardian = Address::generate(&env);

        let queue_id = env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), 10, 1, guardian);
            approve_queued_transaction(&env, queue_id, &signer).unwrap();
            assert_eq!(
                approve_queued_transaction(&env, queue_id, &signer),
                Err(Error::AlreadyVoted)
            );
            queue_id
        });

        env.ledger().with_mut(|l| l.sequence_number += 11);
        env.as_contract(&id, || {
            assert!(execute_queued_transaction(&env, queue_id).is_ok());
        });
    }

    #[test]
    fn test_execute_while_paused_fails() {
        let (env, id, signer) = setup();
        let guardian = Address::generate(&env);

        let queue_id = env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), 10, 1, guardian);
            approve_queued_transaction(&env, queue_id, &signer).unwrap();
            queue_id
        });
        env.ledger().with_mut(|l| l.sequence_number += 11);
        crate::MultisigAccountClient::new(&env, &id).pause(&id);

        env.as_contract(&id, || {
            assert_eq!(
                execute_queued_transaction(&env, queue_id),
                Err(Error::Paused)
            );
            // Still queued: a paused execute must not consume the entry.
            assert!(get_queued_transaction(&env, queue_id).is_ok());
        });
    }
}
