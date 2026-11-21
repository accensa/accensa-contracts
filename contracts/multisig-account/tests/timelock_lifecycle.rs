//! Integration tests for multisig timelock queue lifecycle (issue #1027).

#![cfg(test)]

use multisig_account::timelock::{
    approve_queued_transaction, cancel_queued_transaction, execute_queued_transaction,
    get_queued_transaction, queue_transaction, DEFAULT_TIMELOCK_DELAY,
};
use multisig_account::{Error, MultisigAccount};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    vec, Address, BytesN, Env,
};

fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let guardian = Address::generate(&env);
    let account = env.register(MultisigAccount, (vec![&env, s1.clone(), s2.clone()], 2u32));
    (env, account, s1, s2, guardian)
}

fn call_hash(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

#[test]
fn test_timelock_lifecycle_queue_delay_execute_and_prevent_duplicate() {
    let (env, account, s1, s2, guardian) = setup();

    let queue_id = env.as_contract(&account, || {
        let qid = queue_transaction(
            &env,
            call_hash(&env, 1),
            DEFAULT_TIMELOCK_DELAY,
            2,
            guardian.clone(),
        );
        approve_queued_transaction(&env, qid, &s1).unwrap();
        approve_queued_transaction(&env, qid, &s2).unwrap();
        qid
    });

    // 1. Premature execution before delay has elapsed fails with TimelockNotExpired
    env.as_contract(&account, || {
        let res = execute_queued_transaction(&env, queue_id);
        assert_eq!(res, Err(Error::TimelockNotExpired));
    });

    // Advance ledger sequence just before delay expiry (DEFAULT_TIMELOCK_DELAY - 1)
    env.ledger()
        .with_mut(|li| li.sequence_number += DEFAULT_TIMELOCK_DELAY - 1);
    env.as_contract(&account, || {
        let res = execute_queued_transaction(&env, queue_id);
        assert_eq!(res, Err(Error::TimelockNotExpired));
    });

    // Advance 1 more ledger so delay is fully satisfied
    env.ledger().with_mut(|li| li.sequence_number += 1);

    // 2. Successful execution once delay has passed
    env.as_contract(&account, || {
        let res = execute_queued_transaction(&env, queue_id);
        assert_eq!(res, Ok(()));
    });

    // 3. Duplicate execution prevention: entry is consumed, re-execution fails with ProposalNotFound
    env.as_contract(&account, || {
        let res = execute_queued_transaction(&env, queue_id);
        assert_eq!(res, Err(Error::ProposalNotFound));
        assert_eq!(
            get_queued_transaction(&env, queue_id),
            Err(Error::ProposalNotFound)
        );
    });
}

#[test]
fn test_timelock_lifecycle_cancellation_prevents_future_execution() {
    let (env, account, s1, s2, guardian) = setup();

    let queue_id = env.as_contract(&account, || {
        let qid = queue_transaction(
            &env,
            call_hash(&env, 2),
            DEFAULT_TIMELOCK_DELAY,
            2,
            guardian.clone(),
        );
        approve_queued_transaction(&env, qid, &s1).unwrap();
        approve_queued_transaction(&env, qid, &s2).unwrap();
        qid
    });

    // Cancel queued transaction during delay window by guardian
    env.as_contract(&account, || {
        let res = cancel_queued_transaction(&env, queue_id, &guardian);
        assert_eq!(res, Ok(()));
        assert_eq!(
            get_queued_transaction(&env, queue_id),
            Err(Error::ProposalNotFound)
        );
    });

    // Advance ledger past timelock delay
    env.ledger()
        .with_mut(|li| li.sequence_number += DEFAULT_TIMELOCK_DELAY + 10);

    // Cancelled transaction cannot be executed
    env.as_contract(&account, || {
        let res = execute_queued_transaction(&env, queue_id);
        assert_eq!(res, Err(Error::ProposalNotFound));
    });
}
