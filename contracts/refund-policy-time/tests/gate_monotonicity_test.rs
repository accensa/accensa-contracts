//! #976 — the window gate is a threshold, and a threshold is monotone.
//!
//! `check_time_gates` rejects once `current_ledger > paid_at_ledger + window`.
//! That single comparison carries the whole contract: a claim refused at one
//! ledger must be refused at every later ledger, and a claim admitted must be
//! admitted at every earlier one. An inverted comparison or a boundary that is
//! off by one is invisible to a handful of fixed ledgers — which is all the
//! existing coverage is (`src/test.rs` has five fixed points, and the overflow
//! work in #975 added bounds, not a sweep).
//!
//! These properties state the claim against the deployed contract through the
//! generated client rather than against a re-typed copy of the comparison, so
//! they cannot agree with a wrong implementation by construction:
//!
//! * admission propagates backwards, refusal propagates forwards;
//! * `paid + window` admits and `paid + window + 1` refuses, for every
//!   generated pair — the boundary is included in the sweep deliberately rather
//!   than sampled near;
//! * the same for `(deadline, timestamp)`;
//! * `window == 0` / `deadline == 0` disable their gate for every input.
//!
//! Inputs whose `paid_at_ledger + window` is not representable in `u32` are
//! excluded: that is #975's overflow path, a fail-closed branch that by design
//! refuses at *every* ledger, and mixing it into a monotonicity claim would let
//! the sweep pass by proving the wrong thing. The ranges are therefore kept
//! inside the representable region instead of filtered after the fact.
//!
//! Snapshot capture is switched off per env: a property run is 256 generated
//! cases, and recording a golden file for each would bury the diffs the rest of
//! the suite relies on.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams};
use proptest::prelude::*;
use soroban_sdk::{testutils::EnvTestConfig, xdr::ToXdr, Address, BytesN, Env};

const PAYMENT_REF: [u8; 32] = [3u8; 32];

/// `paid_at_ledger` up to 2_000 and `window` up to 1_000: their sum is at most
/// 3_000, so every generated triple stays inside `u32` and the sweep is about
/// the comparison, not about overflow.
const MAX_PAID: u32 = 2_000;
const MAX_WINDOW: u32 = 1_000;

/// The highest `current_ledger` the window sweep generates.
const MAX_CLAIM: u32 = MAX_PAID + MAX_WINDOW + 1;

/// One policy instance per generated case, reused across that case's verdicts.
fn fresh() -> (Env, Address) {
    let env = Env::new_with_config(EnvTestConfig {
        // 256 cases per property, and none of them is a golden behaviour worth
        // freezing as a file.
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    let policy = env.register(refund_policy_time::TimePolicy, ());
    (env, policy)
}

/// The window gate's verdict, with the deadline disabled.
fn window_verdict(
    env: &Env,
    policy: &Address,
    window: u32,
    paid_at_ledger: u32,
    current_ledger: u32,
) -> Result<(), Error> {
    gate(env, policy, window, 0, paid_at_ledger, current_ledger, 0)
}

/// The deadline gate's verdict, with the window disabled.
fn deadline_verdict(
    env: &Env,
    policy: &Address,
    deadline: u64,
    timestamp: u64,
) -> Result<(), Error> {
    gate(env, policy, 0, deadline, 0, 0, timestamp)
}

fn gate(
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
        // A host failure is not a verdict. The gate is expected to answer with
        // `WindowExpired` / `RefundExpired` or admit, never to trap.
        other => panic!("expected a policy verdict, got {other:?}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Admission is downward closed and refusal is upward closed: moving the
    /// claim earlier can only make the gate more willing, moving it later can
    /// only make it less willing.
    #[test]
    fn admission_propagates_backwards_and_refusal_forwards(
        paid in 0u32..=MAX_PAID,
        window in 0u32..=MAX_WINDOW,
        claimed in 0u32..=MAX_CLAIM,
        later in 1u32..512,
    ) {
        let (env, policy) = fresh();
        let end = paid + window;
        match window_verdict(&env, &policy, window, paid, claimed) {
            Ok(()) => {
                prop_assert!(
                    claimed <= end,
                    "admitted at ledger {}, which is past the window end {}",
                    claimed,
                    end,
                );
                // Every earlier ledger admits, down to the payment itself.
                for back in [1u32, later, claimed] {
                    if claimed >= back {
                        let earlier = claimed - back;
                        prop_assert_eq!(
                            window_verdict(&env, &policy, window, paid, earlier),
                            Ok(()),
                            "admission at {} did not hold at {}",
                            claimed,
                            earlier,
                        );
                    }
                }
            }
            Err(Error::WindowExpired) => {
                prop_assert!(
                    claimed > end,
                    "refused at ledger {}, which is not past the window end {}",
                    claimed,
                    end,
                );
                if let Some(ahead) = claimed.checked_add(later) {
                    prop_assert_eq!(
                        window_verdict(&env, &policy, window, paid, ahead),
                        Err(Error::WindowExpired),
                        "refusal at {} did not hold at the later ledger {}",
                        claimed,
                        ahead,
                    );
                }
            }
            Err(other) => prop_assert!(
                false,
                "the window gate answered {:?}, not a window verdict",
                other,
            ),
        }
    }

    /// The boundary is exactly `paid + window`: the ledger before it admits,
    /// the boundary itself admits, one past it refuses. Generated for every
    /// pair, so an off-by-one anywhere in the range is caught — not only at the
    /// five fixed points the suite had before.
    #[test]
    fn the_window_boundary_is_paid_plus_window(
        paid in 0u32..=MAX_PAID,
        window in 1u32..=MAX_WINDOW,
    ) {
        let (env, policy) = fresh();
        let end = paid + window;
        prop_assert_eq!(
            window_verdict(&env, &policy, window, paid, end - 1),
            Ok(()),
            "one ledger before the end of a {}-ledger window from {}",
            window,
            paid,
        );
        prop_assert_eq!(
            window_verdict(&env, &policy, window, paid, end),
            Ok(()),
            "the last ledger inside the window must still admit",
        );
        prop_assert_eq!(
            window_verdict(&env, &policy, window, paid, end + 1),
            Err(Error::WindowExpired),
            "the first ledger past the window must refuse",
        );
    }

    /// A window of `0` is "no window gate": it must admit whatever the ledger
    /// pair says, otherwise disabling the gate has become a rejection rule.
    /// `paid` deliberately spans the whole `u32` range here, because with the
    /// gate disabled the overflow branch must not answer either.
    #[test]
    fn a_zero_window_admits_at_every_ledger(
        paid in 0u32..=u32::MAX,
        claimed in 0u32..=u32::MAX,
    ) {
        let (env, policy) = fresh();
        prop_assert_eq!(window_verdict(&env, &policy, 0, paid, claimed), Ok(()));
    }

    /// The deadline gate, in wall-clock seconds: admission propagates to
    /// earlier timestamps, refusal propagates to later ones.
    #[test]
    fn the_deadline_gate_is_monotone_in_the_timestamp(
        deadline in 1u64..100_000,
        stamp in 0u64..200_000,
        later in 1u64..1_000,
    ) {
        let (env, policy) = fresh();
        match deadline_verdict(&env, &policy, deadline, stamp) {
            Ok(()) => {
                prop_assert!(
                    stamp <= deadline,
                    "admitted at timestamp {}, past the deadline {}",
                    stamp,
                    deadline,
                );
                for back in [1u64, later, stamp] {
                    if stamp >= back {
                        let earlier = stamp - back;
                        prop_assert_eq!(
                            deadline_verdict(&env, &policy, deadline, earlier),
                            Ok(()),
                            "admission at {} did not hold at {}",
                            stamp,
                            earlier,
                        );
                    }
                }
            }
            Err(Error::RefundExpired) => {
                prop_assert!(
                    stamp > deadline,
                    "refused at timestamp {}, which is not past the deadline {}",
                    stamp,
                    deadline,
                );
                prop_assert_eq!(
                    deadline_verdict(&env, &policy, deadline, stamp + later),
                    Err(Error::RefundExpired),
                    "refusal at {} did not hold at the later timestamp {}",
                    stamp,
                    stamp + later,
                );
            }
            Err(other) => prop_assert!(
                false,
                "the deadline gate answered {:?}, not a deadline verdict",
                other,
            ),
        }
    }

    /// The deadline's own boundary, and the disabled gate.
    #[test]
    fn the_deadline_boundary_is_the_deadline_itself(
        deadline in 0u64..1_000_000,
    ) {
        let (env, policy) = fresh();
        prop_assert_eq!(
            deadline_verdict(&env, &policy, deadline, deadline),
            Ok(()),
            "a claim landing exactly on the deadline succeeds",
        );
        prop_assert_eq!(
            deadline_verdict(&env, &policy, deadline, deadline + 1),
            if deadline == 0 { Ok(()) } else { Err(Error::RefundExpired) },
            "one second past a configured deadline refuses; deadline 0 disables the gate",
        );
        prop_assert_eq!(
            deadline_verdict(&env, &policy, 0, u64::MAX),
            Ok(()),
            "deadline 0 disables the gate for every timestamp",
        );
    }
}
