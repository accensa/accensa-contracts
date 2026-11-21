//! #974 — when both clock gates are expired, the window answers.
//!
//! `check_time_gates` tests the window first and the deadline second, so a
//! claim that is past *both* is reported as `WindowExpired` and never as
//! `RefundExpired`. The vault documents that it relies on this order, and
//! clients branch on the code to tell a late claim (past the deadline) from a
//! window-expired one (too long after the payment).
//!
//! Nothing pinned the interaction. Every existing case expires exactly one
//! gate — a window with `deadline = 0`, a deadline with `window = 0`, or
//! neither — so swapping the two `if` blocks leaves all of them green while
//! changing the code every both-expired claim receives. This file drives the
//! four rows of that table through the public `evaluate`:
//!
//! | `window` | `deadline` | observed           |
//! |----------|------------|--------------------|
//! | expired  | expired    | `WindowExpired`    |
//! | expired  | inside     | `WindowExpired`    |
//! | inside   | expired    | `RefundExpired`    |
//! | inside   | inside     | `Ok(())`           |
//!
//! Each context is built by hand rather than read off the test host's ledger,
//! so the rows say what they claim about the inputs and not about wherever the
//! environment happens to have parked its clock.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams};
use soroban_sdk::{testutils::Ledger, xdr::ToXdr, Address, BytesN, Env};

const PAYMENT_REF: [u8; 32] = [9u8; 32];
const WINDOW: u32 = 100;
const DEADLINE: u64 = 1_000_000;

fn setup() -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let policy = env.register(refund_policy_time::TimePolicy, ());
    // Parked well away from both edges of the table so the host clock cannot
    // be what a row is actually testing.
    env.ledger().set_sequence_number(500);
    env.ledger().set_timestamp(500);
    (env, policy)
}

/// One verdict, with the four gate inputs given explicitly.
fn verdict(
    env: &Env,
    policy: &Address,
    window: u32,
    deadline: u64,
    paid_at_ledger: u32,
    current_ledger: u32,
    timestamp: u64,
) -> Result<(), Error> {
    let params = TimePolicyParams { window, deadline }.to_xdr(env);
    let ctx = PolicyContext {
        payment_ref: BytesN::from_array(env, &PAYMENT_REF),
        amount: 100,
        paid_at_ledger,
        current_ledger,
        timestamp,
        vdf_proof: None,
    };
    match RefundPolicyClient::new(env, policy).try_evaluate(&params, &ctx) {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        // A host failure is not a verdict: the gate is expected to answer with
        // one of the two codes, never to trap.
        other => panic!("expected a policy verdict, got {other:?}"),
    }
}

#[test]
fn a_claim_past_both_gates_is_reported_as_window_expired() {
    let (env, policy) = setup();
    // `100 + 100 = 200`, claimed at ledger 1_000, and `timestamp` past
    // `deadline` — both gates would refuse on their own.
    assert_eq!(
        verdict(&env, &policy, WINDOW, DEADLINE, 100, 1_000, 2_000_000),
        Err(Error::WindowExpired),
        "the window gate runs first, so it is the code a both-expired claim gets",
    );
}

#[test]
fn a_claim_past_only_the_window_is_window_expired() {
    let (env, policy) = setup();
    assert_eq!(
        verdict(&env, &policy, WINDOW, DEADLINE, 100, 1_000, 999_999),
        Err(Error::WindowExpired),
    );
}

#[test]
fn a_claim_past_only_the_deadline_is_refund_expired() {
    let (env, policy) = setup();
    // Inside the window: `900 + 100 = 1_000`, claimed at ledger 1_000 exactly.
    assert_eq!(
        verdict(&env, &policy, WINDOW, DEADLINE, 900, 1_000, 1_000_001),
        Err(Error::RefundExpired),
        "with the window satisfied, the deadline gate is the one that answers",
    );

    // The same, with the window disabled outright.
    assert_eq!(
        verdict(&env, &policy, 0, DEADLINE, 900, 1_000, 1_000_001),
        Err(Error::RefundExpired),
    );
}

#[test]
fn a_claim_inside_both_gates_is_admitted() {
    let (env, policy) = setup();
    assert_eq!(
        verdict(&env, &policy, WINDOW, DEADLINE, 900, 1_000, 1_000_000),
        Ok(()),
        "the last ledger inside the window and the deadline itself both admit",
    );
}

/// The precedence is a property of the gate order, not of one pair of numbers:
/// every both-expired row must report the window, and the window's own verdict
/// must survive whatever the deadline says.
#[test]
fn the_both_expired_verdict_does_not_depend_on_where_either_gate_is() {
    let (env, policy) = setup();
    for (paid, current) in [(0u32, 1_000u32), (1, 500), (50, 300), (100, 201)] {
        for deadline in [1u64, 1_000, 1_000_000] {
            assert_eq!(
                verdict(&env, &policy, WINDOW, deadline, paid, current, deadline + 1),
                Err(Error::WindowExpired),
                "paid {paid}, claimed {current} with window {WINDOW}, deadline {deadline} \
                 passed by one second must still read as a window expiry",
            );
        }
    }
}

/// A disabled gate cannot be the reason for a rejection, which is what keeps
/// the both-expired row a *precedence* claim rather than an artifact of the
/// `> 0` guards: with the window off, the same timestamp answer is the
/// deadline's code.
#[test]
fn disabling_one_gate_hands_the_verdict_to_the_other() {
    let (env, policy) = setup();
    assert_eq!(
        verdict(&env, &policy, 0, DEADLINE, 100, 1_000, 2_000_000),
        Err(Error::RefundExpired),
    );
    assert_eq!(
        verdict(&env, &policy, WINDOW, 0, 100, 1_000, 2_000_000),
        Err(Error::WindowExpired),
    );
    assert_eq!(
        verdict(&env, &policy, 0, 0, 100, 1_000, 2_000_000),
        Ok(()),
        "no gate configured means no verdict at all",
    );
}
