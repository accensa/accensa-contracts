//! The window gate must fail closed, not trap, when `paid_at_ledger + window`
//! overflows `u32` (issue #975).
//!
//! The workspace builds with `overflow-checks = true` in both the `dev` and
//! `release` profiles, so the unchecked addition `check_time_gates` performed
//! became a host panic for a payment anchor within `window` of `u32::MAX`. A
//! panic is not a verdict: it bypasses the caller's error handling and turns a
//! denial into an uncallable claim, and because one deployed policy instance
//! serves every vault, a single malformed anchor disables a class of refunds.
//!
//! These tests pin the typed `WindowExpired` on the overflow path and show the
//! guard is specific to it: the last representable sum keeps the verdict the
//! plain comparison gives, on both sides of its own boundary.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams};
use soroban_sdk::{testutils::Ledger, xdr::ToXdr, Address, BytesN, Env};

const PAYMENT_REF: [u8; 32] = [7u8; 32];

/// Highest ledger sequence this test host will run a contract at.
///
/// The test ledger gives every entry a minimum persistent lifetime of `4096`
/// ledgers (`soroban-env-host`'s default `min_persistent_entry_ttl`), and the
/// host refuses to compute a `live_until` that leaves the `u32` range —
/// "persistent entry TTL overflow, ledger is mis-configured" — before the
/// policy is entered. Anchors still go to the very top of the range, because
/// it is `paid_at_ledger + window`, not the clock, whose overflow is under
/// test.
const TOP: u32 = u32::MAX - 4_096;

/// Register the policy, then move the ledger to `sequence`.
///
/// The order matters: the sequence has to be raised *after* registration for
/// the same reason `TOP` is not `u32::MAX` — a registration at the very top
/// traps in host bookkeeping. The policy keeps no storage, so its verdict does
/// not depend on when the clock moves.
fn setup(sequence: u32) -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let policy = env.register(refund_policy_time::TimePolicy, ());
    env.ledger().set_sequence_number(sequence);
    env.ledger().set_timestamp(0);
    (env, policy)
}

fn verdict(env: &Env, policy: &Address, window: u32, paid_at_ledger: u32) -> Result<(), Error> {
    let params = TimePolicyParams {
        window,
        deadline: 0,
    }
    .to_xdr(env);
    let ctx = PolicyContext {
        payment_ref: BytesN::from_array(env, &PAYMENT_REF),
        amount: 100,
        paid_at_ledger,
        current_ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
        vdf_proof: None,
    };
    match RefundPolicyClient::new(env, policy).try_evaluate(&params, &ctx) {
        Ok(Ok(())) => Ok(()),
        // A typed verdict from the policy.
        Err(Ok(error)) => Err(error),
        // `Err(Err(_))` is a host failure: the pre-fix code reached this arm by
        // overflowing, so asserting here is what separates "denied" from
        // "trapped" instead of letting both read as a failure.
        other => panic!("expected a policy verdict, got {other:?}"),
    }
}

#[test]
fn an_overflowing_anchor_is_a_verdict_not_a_trap() {
    // The issue's literal case: `u32::MAX - 5 + 100` is not representable.
    // Before the fix this invocation panicked inside the policy.
    let (env, policy) = setup(1_000);
    assert_eq!(
        verdict(&env, &policy, 100, u32::MAX - 5),
        Err(Error::WindowExpired)
    );
}

#[test]
fn an_overflowing_anchor_is_denied_whatever_the_current_ledger_is() {
    // The overflowing sum alone decides the verdict, so it does not depend on
    // how far the claim has run: the same inputs high in the ledger range and
    // at the first ledger must agree.
    let (env, policy) = setup(TOP);
    assert_eq!(
        verdict(&env, &policy, 100, u32::MAX - 5),
        Err(Error::WindowExpired)
    );
    let (env, policy) = setup(1);
    assert_eq!(
        verdict(&env, &policy, 100, u32::MAX - 5),
        Err(Error::WindowExpired)
    );
}

#[test]
fn a_zero_window_stays_disabled_for_a_top_of_range_anchor() {
    // `window = 0` means "no window gate". The overflow path must not turn the
    // disabled gate into a rejection for an anchor at the very top.
    let (env, policy) = setup(1_000);
    assert_eq!(verdict(&env, &policy, 0, u32::MAX), Ok(()));
}

#[test]
fn the_last_representable_sum_admits() {
    // `u32::MAX - 100 + 100 == u32::MAX`: the sum is representable and larger
    // than any ledger, so the claim is inside the window and admits.
    let (env, policy) = setup(1_000);
    assert_eq!(verdict(&env, &policy, 100, u32::MAX - 100), Ok(()));
}

#[test]
fn one_ledger_of_anchor_earlier_and_the_verdict_flips_to_a_denial() {
    // `u32::MAX - 99 + 100` is the first non-representable sum. Same ledger,
    // same window, one ledger of anchor apart: the fail-closed branch answers
    // the question the comparison could not evaluate.
    let (env, policy) = setup(1_000);
    assert_eq!(
        verdict(&env, &policy, 100, u32::MAX - 99),
        Err(Error::WindowExpired)
    );
}

#[test]
fn a_representable_boundary_rejection_at_the_top_still_uses_the_comparison() {
    // `TOP - 101 + 100 == TOP - 1`: no overflow, and claiming at `TOP` is one
    // ledger late. The fix did not soften the representable side of the bound.
    let (env, policy) = setup(TOP);
    assert_eq!(
        verdict(&env, &policy, 100, TOP - 101),
        Err(Error::WindowExpired)
    );
}

#[test]
fn a_representable_boundary_admission_at_the_top_is_unchanged() {
    // The pair to the test above: `TOP - 100 + 100 == TOP`, so claiming at
    // `TOP` is the last ledger inside the window and admits.
    let (env, policy) = setup(TOP);
    assert_eq!(verdict(&env, &policy, 100, TOP - 100), Ok(()));
}

#[test]
fn ordinary_window_verdicts_are_unchanged() {
    // The control: the same comparison mid-range, where the old and new code
    // must agree exactly, matching the cases already pinned in `src/test.rs`.
    let (env, policy) = setup(100);
    assert_eq!(verdict(&env, &policy, 100, 0), Ok(()));
    let (env, policy) = setup(101);
    assert_eq!(verdict(&env, &policy, 100, 0), Err(Error::WindowExpired));
    let (env, policy) = setup(50);
    assert_eq!(verdict(&env, &policy, 100, 0), Ok(()));
}
