//! Stale signature / approval expiration (issue #449).
//!
//! A partial quorum must not stay valid forever: approvals that linger for
//! weeks let an attacker keep assembling a transaction's signatures against
//! an intent nobody stands behind anymore. Every approval of a queued
//! transaction is therefore stamped with a `created_at` ledger timestamp
//! ([`ApprovalRecord`]) and only counts toward the threshold while its age is
//! strictly below [`APPROVAL_TTL_SECONDS`] (14 days).
//!
//! # Model
//!
//! - **Record.** [`record_approval`] writes an [`ApprovalRecord`] under
//!   [`DataKey::TimelockApproval`]. It lives in *persistent* storage: a
//!   14-day window is far longer than a temporary entry's default TTL, and
//!   pruning is explicit instead of delegated to archival —
//!   [`prune_stale_approvals`] drops expired records the moment the threshold
//!   is evaluated, and [`clear_approvals`] drops the remainder when the queue
//!   entry executes or is cancelled.
//! - **Evaluation.** `age = now.saturating_sub(created_at)`; the approval is
//!   stale when `age >= APPROVAL_TTL_SECONDS`, so the boundary itself
//!   (`age == TTL`) is already expired. A timestamp in the future (clock
//!   skew) saturates to an age of `0` and stays fresh.
//! - **Consequences.** Stale approvals are removed from `approval_count` and
//!   from the materialized approver list, and a shortfall caused by pruning
//!   surfaces as [`crate::Error::StaleSignature`] rather than a bare
//!   [`crate::Error::InsufficientSignatures`], so a caller can tell "you
//!   never had quorum" from "your quorum expired". An expired approval does
//!   not block its signer from voting again.

use soroban_sdk::{contracttype, Address, Env, Vec};

use crate::timelock::QueuedTransaction;
use crate::DataKey;

/// Maximum age of an approval before it goes stale: 14 days in seconds
/// (`14 * 24 * 60 * 60 = 1_209_600`).
pub const APPROVAL_TTL_SECONDS: u64 = 14 * 24 * 60 * 60;

/// A recorded approval, stamped with the ledger timestamp it was cast at.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalRecord {
    /// `env.ledger().timestamp()` when the approval was registered.
    pub created_at: u64,
}

/// The current ledger timestamp (Unix seconds).
pub fn now(env: &Env) -> u64 {
    env.ledger().timestamp()
}

/// True when an approval recorded at `created_at` has aged out of the
/// [`APPROVAL_TTL_SECONDS`] window.
///
/// Ages are computed with `saturating_sub`, so a timestamp ahead of the
/// ledger clock reads as fresh rather than wrapping into "stale".
pub fn is_stale(env: &Env, created_at: u64) -> bool {
    now(env).saturating_sub(created_at) >= APPROVAL_TTL_SECONDS
}

fn approval_key(queue_id: u64, signer: &Address) -> DataKey {
    DataKey::TimelockApproval(queue_id, signer.clone())
}

/// Register `signer`'s approval of `queue_id`, stamped with [`now`].
pub(crate) fn record_approval(env: &Env, queue_id: u64, signer: &Address) {
    env.storage().persistent().set(
        &approval_key(queue_id, signer),
        &ApprovalRecord {
            created_at: now(env),
        },
    );
}

/// The stored record for `signer`'s approval of `queue_id`, if any.
pub fn approval_record(env: &Env, queue_id: u64, signer: &Address) -> Option<ApprovalRecord> {
    env.storage()
        .persistent()
        .get(&approval_key(queue_id, signer))
}

/// True when `signer` has a recorded approval of `queue_id` that is still
/// inside the TTL. A missing record (never cast, pruned, or cleared) is not
/// a fresh approval.
pub fn has_fresh_approval(env: &Env, queue_id: u64, signer: &Address) -> bool {
    approval_record(env, queue_id, signer)
        .map(|record| !is_stale(env, record.created_at))
        .unwrap_or(false)
}

/// Drop every expired approval of `queue_id`, storage included, and leave
/// `queued.approvers` / `queued.approval_count` describing exactly the
/// approvals that are still fresh.
///
/// Returns how many approvals were removed; the caller must persist `queued`
/// whenever that is non-zero (the record deletions are already committed).
/// A record that has disappeared from storage without being pruned — it can
/// only happen for entries written before this logic existed — counts as
/// expired too, so the materialized list never credits an approval nobody can
/// substantiate.
pub(crate) fn prune_stale_approvals(
    env: &Env,
    queue_id: u64,
    queued: &mut QueuedTransaction,
) -> u32 {
    let mut fresh: Vec<Address> = Vec::new(env);
    let mut removed: u32 = 0;

    for signer in queued.approvers.iter() {
        match approval_record(env, queue_id, &signer) {
            Some(record) if !is_stale(env, record.created_at) => fresh.push_back(signer),
            _ => {
                env.storage()
                    .persistent()
                    .remove(&approval_key(queue_id, &signer));
                removed = removed.saturating_add(1);
            }
        }
    }

    if removed > 0 {
        queued.approval_count = fresh.len();
        queued.approvers = fresh;
    }
    removed
}

/// Delete every approval record of `queue_id`, called when the queue entry
/// itself is consumed so no approval can outlive the transaction it approved.
pub(crate) fn clear_approvals(env: &Env, queue_id: u64, approvers: &Vec<Address>) {
    for signer in approvers.iter() {
        env.storage()
            .persistent()
            .remove(&approval_key(queue_id, &signer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timelock::{
        approve_queued_transaction, execute_queued_transaction, get_queued_transaction,
        queue_transaction,
    };
    use crate::{Error, MultisigAccount};
    use soroban_sdk::testutils::{Address as _, Ledger as _};
    use soroban_sdk::{vec, Address, BytesN, Env};

    /// 3 signers; the timelock `required_approvals` is set per test.
    fn setup() -> (Env, Address, Vec<Address>) {
        let env = Env::default();
        env.mock_all_auths();
        let signers: Vec<Address> = vec![
            &env,
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ];
        let id = env.register(MultisigAccount, (signers.clone(), 3u32));
        (env, id, signers)
    }

    fn hash(env: &Env) -> BytesN<32> {
        BytesN::from_array(env, &[9u8; 32])
    }

    /// Advance the ledger clock by `seconds` (real time, not ledgers).
    fn age(env: &Env, seconds: u64) {
        env.ledger().with_mut(|l| l.timestamp += seconds);
    }

    /// Queue `required` approvals and collect them from `signers` at the
    /// current timestamp. Returns the queue ID.
    fn queue_with_approvals(env: &Env, id: &Address, required: u32, signers: &[&Address]) -> u64 {
        env.as_contract(id, || {
            let queue_id = queue_transaction(env, hash(env), 0, required, Address::generate(env));
            for signer in signers {
                approve_queued_transaction(env, queue_id, signer).unwrap();
            }
            queue_id
        })
    }

    #[test]
    fn the_ttl_is_fourteen_days_in_seconds() {
        assert_eq!(APPROVAL_TTL_SECONDS, 14 * 24 * 60 * 60);
        assert_eq!(APPROVAL_TTL_SECONDS, 1_209_600);
    }

    #[test]
    fn approvals_are_stamped_with_the_registration_timestamp() {
        let (env, id, signers) = setup();
        env.ledger().with_mut(|l| l.timestamp = 5_000);
        let s1 = signers.get(0).unwrap();

        let queue_id = env.as_contract(&id, || {
            let queue_id = queue_transaction(&env, hash(&env), 0, 1, Address::generate(&env));
            approve_queued_transaction(&env, queue_id, &s1).unwrap();
            queue_id
        });

        env.as_contract(&id, || {
            let record = approval_record(&env, queue_id, &s1).unwrap();
            assert_eq!(record.created_at, 5_000);
            assert!(has_fresh_approval(&env, queue_id, &s1));
            assert!(!is_stale(&env, record.created_at));
        });
    }

    #[test]
    fn fresh_approvals_count_toward_the_threshold() {
        let (env, id, signers) = setup();
        let (s1, s2) = (signers.get(0).unwrap(), signers.get(1).unwrap());
        let queue_id = queue_with_approvals(&env, &id, 2, &[&s1, &s2]);

        // One second short of the TTL: both approvals are still fresh and
        // execution succeeds.
        age(&env, APPROVAL_TTL_SECONDS - 1);
        env.as_contract(&id, || {
            let queued = get_queued_transaction(&env, queue_id).unwrap();
            assert_eq!(queued.approval_count, 2);
            assert!(execute_queued_transaction(&env, queue_id).is_ok());
            // Consumed: the queue entry is gone.
            assert!(get_queued_transaction(&env, queue_id).is_err());
        });
    }

    #[test]
    fn stale_approvals_are_pruned_and_do_not_count() {
        let (env, id, signers) = setup();
        let (s1, s2) = (signers.get(0).unwrap(), signers.get(1).unwrap());
        let queue_id = queue_with_approvals(&env, &id, 2, &[&s1, &s2]);

        // Exactly at the TTL boundary: `age >= APPROVAL_TTL_SECONDS`.
        age(&env, APPROVAL_TTL_SECONDS);
        env.as_contract(&id, || {
            assert_eq!(
                execute_queued_transaction(&env, queue_id),
                Err(Error::StaleSignature)
            );

            // Pruned, not merely ignored: neither the counter, the approver
            // list nor the approval records survive.
            let queued = get_queued_transaction(&env, queue_id).unwrap();
            assert_eq!(queued.approval_count, 0);
            assert!(queued.approvers.is_empty());
            assert!(!env
                .storage()
                .persistent()
                .has(&DataKey::TimelockApproval(queue_id, s1.clone())));
            assert!(!env
                .storage()
                .persistent()
                .has(&DataKey::TimelockApproval(queue_id, s2.clone())));
            assert!(!has_fresh_approval(&env, queue_id, &s1));
        });
    }

    #[test]
    fn stale_approvals_are_pruned_while_fresh_ones_still_execute() {
        let (env, id, signers) = setup();
        let (s1, s2) = (signers.get(0).unwrap(), signers.get(1).unwrap());

        // s1 approves alone, then the approval expires; s2 votes afterwards.
        let queue_id = queue_with_approvals(&env, &id, 1, &[&s1]);
        age(&env, APPROVAL_TTL_SECONDS);

        env.as_contract(&id, || {
            approve_queued_transaction(&env, queue_id, &s2).unwrap();

            // The stale s1 approval was dropped when the fresh one arrived,
            // so the single fresh approval is what remains.
            let queued = get_queued_transaction(&env, queue_id).unwrap();
            assert_eq!(queued.approval_count, 1);
            assert_eq!(queued.approvers.len(), 1);
            assert!(!has_fresh_approval(&env, queue_id, &s1));
            assert!(has_fresh_approval(&env, queue_id, &s2));

            assert!(execute_queued_transaction(&env, queue_id).is_ok());
        });
    }

    #[test]
    fn an_expired_approval_does_not_block_the_signer_from_voting_again() {
        let (env, id, signers) = setup();
        let s1 = signers.get(0).unwrap();
        let queue_id = queue_with_approvals(&env, &id, 1, &[&s1]);
        age(&env, APPROVAL_TTL_SECONDS);

        env.as_contract(&id, || {
            // The old approval no longer counts, so it no longer blocks a
            // fresh one: no `AlreadyVoted`.
            assert!(approve_queued_transaction(&env, queue_id, &s1).is_ok());

            let record = approval_record(&env, queue_id, &s1).unwrap();
            assert_eq!(record.created_at, APPROVAL_TTL_SECONDS);
            assert_eq!(
                get_queued_transaction(&env, queue_id)
                    .unwrap()
                    .approval_count,
                1
            );

            assert!(execute_queued_transaction(&env, queue_id).is_ok());
        });
    }

    #[test]
    fn a_fresh_duplicate_approval_is_still_rejected() {
        let (env, id, signers) = setup();
        let s1 = signers.get(0).unwrap();
        let queue_id = queue_with_approvals(&env, &id, 1, &[&s1]);

        env.as_contract(&id, || {
            assert_eq!(
                approve_queued_transaction(&env, queue_id, &s1),
                Err(Error::AlreadyVoted)
            );
        });
    }

    #[test]
    fn a_shortfall_without_staleness_is_insufficient_not_stale() {
        let (env, id, signers) = setup();
        let (s1, s2) = (signers.get(0).unwrap(), signers.get(1).unwrap());
        // Two approvals collected, three required — and still fresh.
        let queue_id = queue_with_approvals(&env, &id, 3, &[&s1, &s2]);
        age(&env, APPROVAL_TTL_SECONDS / 2);

        env.as_contract(&id, || {
            assert_eq!(
                execute_queued_transaction(&env, queue_id),
                Err(Error::InsufficientSignatures)
            );
            // Nothing was pruned, so the recorded quorum is untouched.
            assert_eq!(
                get_queued_transaction(&env, queue_id)
                    .unwrap()
                    .approval_count,
                2
            );
        });
    }
}
