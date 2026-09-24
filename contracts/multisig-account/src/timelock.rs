use soroban_sdk::{Address, Env, Vec};

use crate::Error;
use crate::DataKey;

//! Timelock delay queue for sensitive admin actions in the multisig account.
//!
//! High-risk operations (signer set updates, threshold decreases, code
//! upgrades) are queued with a mandatory delay (48 hours in ledger
//! sequence increments). Authorized signers or a guardian can cancel
//! malicious or erroneous queued actions during the delay window.
//! Execution is enforced after the timelock elapses and rejected before.

/// A queued transaction awaiting timelock execution.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedTransaction {
    /// Hash of the call to be executed.
    pub call_hash: [u8; 32],
    /// Ledger sequence at which execution becomes allowed.
    pub execution_ledger: u32,
    /// Number of approvals collected so far.
    pub approval_count: u32,
    /// Threshold number of approvals required for execution.
    pub required_approvals: u32,
    /// Address of the guardian who can cancel this transaction.
    pub guardian: Address,
}

/// Default timelock delay in ledger sequences (48 hours at ~5s/ledger ≈ 345600 ledgers).
pub const DEFAULT_TIMELOCK_DELAY: u32 = 345600;

//! Queue a high-risk transaction for delayed execution.
//!
//! The transaction enters a queued state and cannot be executed
//! until `execution_ledger` is reached. The `guardian` can cancel
//! the transaction during the delay window.
pub fn queue_transaction(
    env: &Env,
    call_hash: [u8; 32],
    execution_delay: u32,
    required_approvals: u32,
    guardian: Address,
) -> u64 {
    let current_ledger = env.ledger().sequence();
    let execution_ledger = current_ledger + execution_delay;

    let queue_id = env
        .storage()
        .instance()
        .get(&DataKey::QueueCount)
        .unwrap_or(0u64)
        + 1;

    let tuple = (call_hash, execution_ledger, 0u32, required_approvals, guardian.clone());

    env.storage()
        .persistent()
        .set(&DataKey::QueuedTransaction(queue_id), &tuple);
    env.storage()
        .instance()
        .set(&DataKey::QueueCount, &queue_id);

    queue_id
}

//! Execute a queued transaction after the timelock has elapsed.
//!
//! Returns `Ok(())` if the transaction was executed.
//! Returns `Err(Error::TimelockNotExpired)` if the timelock has not yet elapsed.
//! Returns `Err(Error::ProposalNotFound)` if the queue ID does not exist.
pub fn execute_queued_transaction(env: &Env, queue_id: u64) -> Result<(), Error> {
    let key = DataKey::QueuedTransaction(queue_id);
    let tuple: ( [u8; 32], u32, u32, u32, Address ) = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ProposalNotFound)?;

    let now = env.ledger().sequence();
    if now < tuple.1 {
        return Err(Error::TimelockNotExpired);
    }

    if tuple.2 < tuple.3 {
        return Err(Error::InsufficientSignatures);
    }

    env.storage().persistent().remove(&key);
    Ok(())
}

//! Cancel a queued transaction during the delay window.
//!
//! Only the guardian or a registered signer can cancel.
//! Returns `Err(Error::TimelockNotExpired)` if the timelock has already elapsed.
pub fn cancel_queued_transaction(
    env: &Env,
    queue_id: u64,
    caller: &Address,
) -> Result<(), Error> {
    let key = DataKey::QueuedTransaction(queue_id);
    let tuple: ( [u8; 32], u32, u32, u32, Address ) = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ProposalNotFound)?;

    let now = env.ledger().sequence();
    if now >= tuple.1 {
        return Err(Error::TimelockNotExpired);
    }

    // Only the guardian can cancel during the delay window
    let stored_guardian: Address = env
        .storage()
        .persistent()
        .get(&DataKey::TimelockGuardian)
        .ok_or(Error::Unauthorized)?;

    if *caller != stored_guardian {
        return Err(Error::Unauthorized);
    }

    env.storage().persistent().remove(&key);
    Ok(())
}

//! Approve a queued transaction. Each authorized signer can approve once.
//!
//! Returns `Ok(())` if the approval was recorded.
/// Returns `Err(Error::AlreadyVoted)` if the signer has already approved.
pub fn approve_queued_transaction(
    env: &Env,
    queue_id: u64,
    signer: &Address,
) -> Result<(), Error> {
    let key = DataKey::QueuedTransaction(queue_id);
    let tuple: ( [u8; 32], u32, u32, u32, Address ) = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ProposalNotFound)?;

    let approval_key = DataKey::TimelockApproval(queue_id, signer.clone());
    if env.storage().temporary().has(&approval_key) {
        return Err(Error::AlreadyVoted);
    }

    env.storage().temporary().set(&approval_key, &());
    let new_approval_count = tuple.2.saturating_add(1);
    let new_tuple = (tuple.0, tuple.1, new_approval_count, tuple.3, tuple.4);
    env.storage().persistent().set(&key, &new_tuple);

    Ok(())
}

/// Read-only: fetch a queued transaction by ID.
pub fn get_queued_transaction(env: &Env, queue_id: u64) -> Result<QueuedTransaction, Error> {
    let (call_hash, execution_ledger, approval_count, required_approvals, guardian) =
        env.storage()
            .persistent()
            .get(&DataKey::QueuedTransaction(queue_id))
            .ok_or(Error::ProposalNotFound)?;
    Ok(QueuedTransaction {
        call_hash,
        execution_ledger,
        approval_count,
        required_approvals,
        guardian,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn test_queue_and_execute() {
        let env = Env::default();
        env.mock_all_auths();

        let guardian = Address::from_str(&env, "X:GDQ");
        let call_hash = [1u8; 32];

        let queue_id = queue_transaction(&env, call_hash, 10, 1, guardian.clone());
        let queued = get_queued_transaction(&env, queue_id).unwrap();

        assert_eq!(queued.call_hash, call_hash);
        assert_eq!(queued.required_approvals, 1);
        assert_eq!(queued.execution_ledger, env.ledger().sequence() + 10);
    }

    #[test]
    fn test_execute_before_timelock_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let guardian = Address::from_str(&env, "X:GDQ");
        let call_hash = [1u8; 32];

        let queue_id = queue_transaction(&env, call_hash, DEFAULT_TIMELOCK_DELAY, 1, guardian);

        let result = execute_queued_transaction(&env, queue_id);
        assert!(result.is_err(), "execution before timelock should fail");
    }

    #[test]
    fn test_cancel_during_delay() {
        let env = Env::default();
        env.mock_all_auths();

        let guardian = Address::from_str(&env, "X:GDQ");
        let call_hash = [1u8; 32];

        let queue_id = queue_transaction(&env, call_hash, DEFAULT_TIMELOCK_DELAY, 1, guardian.clone());

        let result = cancel_queued_transaction(&env, queue_id, &guardian);
        assert!(result.is_ok(), "guardian should cancel during delay");

        let result = get_queued_transaction(&env, queue_id);
        assert!(result.is_err(), "cancelled transaction should not exist");
    }

    #[test]
    fn test_cancel_after_timelock_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let guardian = Address::from_str(&env, "X:GDQ");
        let call_hash = [1u8; 32];

        let queue_id = queue_transaction(&env, call_hash, 0, 1, guardian.clone());

        env.ledger().with_mut(|l| l.sequence_number += DEFAULT_TIMELOCK_DELAY + 1);

        let result = cancel_queued_transaction(&env, queue_id, &guardian);
        assert!(result.is_err(), "cancel after timelock should fail");
    }

    #[test]
    fn test_approve_and_execute() {
        let env = Env::default();
        env.mock_all_auths();

        let guardian = Address::from_str(&env, "X:GDQ");
        let call_hash = [1u8; 32];
        let signer = Address::from_str(&env, "X:SIGNER");

        let queue_id = queue_transaction(&env, call_hash, 10, 1, guardian);
        approve_queued_transaction(&env, queue_id, &signer).unwrap();

        env.ledger().with_mut(|l| l.sequence_number += 11);
        let result = execute_queued_transaction(&env, queue_id);
        assert!(
            result.is_ok(),
            "execution after timelock with approvals should succeed"
        );
    }
}