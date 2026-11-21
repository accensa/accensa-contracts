//! The time policy promises it keeps no storage (issue #980).
//!
//! `contracts/refund-policy-time/src/lib.rs:13-19` states the property, and the
//! property is the reason the gate was split out of `RefundVault` at all: one
//! deployed policy instance serves every vault that points its time gate at it,
//! so a per-claim cache, a counter, or a memo of the last decoded `params`
//! would become shared mutable state between unrelated merchants — and would be
//! paid for by every vault's invocation, landing on the policy's own address
//! and outliving the vault that created it.
//!
//! Until now the promise was only a comment. Every existing test registers the
//! contract and inspects the returned `Result`, which says nothing about what
//! the invocation left behind: a `temporary()` diagnostic counter passes review
//! and CI while quietly growing the ledger footprint. This file drives each
//! verdict the policy can reach and reads back all three storage maps after
//! every call.
//!
//! The mock oracle is deliberately storage-free — its answer is a function of
//! the payment reference rather than something it records — so an empty map
//! after an oracle-backed claim is evidence about this policy, not about
//! bookkeeping the oracle happens to do. `the_reader_reports_a_write` is the
//! control that shows the assertion could fail.

use accensa_common::{Error, PolicyContext, RefundPolicyClient, TimePolicyParams};
use refund_policy_time::oracle::{DeliveryReport, DeliveryStatus, TimeOraclePolicyParams};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{storage::*, Ledger},
    xdr::ToXdr,
    Address, Bytes, BytesN, Env,
};

const CURRENT_LEDGER: u32 = 1_000;
const WINDOW: u32 = 100;
const NOW: u64 = 50_000;
const MAX_AGE: u64 = 3_600;
/// Inside `MAX_AGE` of `NOW`, so a report stamped here is trusted.
const FRESH: u64 = NOW - 60;

// ── Storage-free delivery oracle ─────────────────────────────────────────────

/// Answers from the first byte of the payment reference, so no invocation of
/// this contract needs to record anything: `Lost`, `Delivered`, `Pending`, a
/// stale report, a future-dated report, a report about another payment, and
/// (for any other byte) a trap standing in for an unreachable oracle.
#[contract]
pub struct ScriptedOracle;

#[contractimpl]
impl ScriptedOracle {
    pub fn get_delivery_status(env: Env, payment_ref: BytesN<32>) -> DeliveryReport {
        let key = payment_ref.to_array()[0];
        let report = |status: DeliveryStatus, at: u64, about: BytesN<32>| DeliveryReport {
            payment_ref: about,
            status,
            timestamp: at,
            proof: BytesN::from_array(&env, &[0x5c; 32]),
        };
        match key {
            b'L' => report(DeliveryStatus::Lost, FRESH, payment_ref.clone()),
            b'D' => report(DeliveryStatus::Delivered, FRESH, payment_ref.clone()),
            b'P' => report(DeliveryStatus::Pending, FRESH, payment_ref.clone()),
            // Older than `max_report_age`: ignored.
            b'A' => report(DeliveryStatus::Lost, NOW - MAX_AGE - 1, payment_ref.clone()),
            // Dated after the claim: ignored.
            b'F' => report(DeliveryStatus::Lost, NOW + 1, payment_ref.clone()),
            // Fresh and decisive, but about a different payment: ignored.
            b'M' => report(
                DeliveryStatus::Lost,
                FRESH,
                BytesN::from_array(&env, &[b'X'; 32]),
            ),
            _ => panic!("oracle unreachable"),
        }
    }
}

// ── Fixture ──────────────────────────────────────────────────────────────────

struct Fixture {
    env: Env,
    policy: Address,
    oracle: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(CURRENT_LEDGER);
        env.ledger().set_timestamp(NOW);
        Self {
            policy: env.register(refund_policy_time::TimePolicy, ()),
            oracle: env.register(ScriptedOracle, ()),
            env,
        }
    }

    fn plain_params(&self) -> Bytes {
        TimePolicyParams {
            window: WINDOW,
            deadline: 0,
        }
        .to_xdr(&self.env)
    }

    fn oracle_params(&self) -> Bytes {
        TimeOraclePolicyParams {
            window: WINDOW,
            deadline: 0,
            oracle: self.oracle.clone(),
            max_report_age: MAX_AGE,
        }
        .to_xdr(&self.env)
    }

    fn claim(&self, key: u8, paid_at_ledger: u32) -> PolicyContext {
        PolicyContext {
            payment_ref: BytesN::from_array(&self.env, &[key; 32]),
            amount: 100,
            paid_at_ledger,
            current_ledger: self.env.ledger().sequence(),
            timestamp: self.env.ledger().timestamp(),
            vdf_proof: None,
        }
    }

    /// Evaluate `params` for `ctx`, then assert the policy's storage footprint
    /// is still empty. Returning the verdict keeps each case one line.
    fn evaluate_and_check(&self, params: &Bytes, ctx: &PolicyContext) -> Result<(), Error> {
        let label = label_for(
            ctx.payment_ref.to_array()[0],
            params == &self.plain_params(),
        );
        let verdict = RefundPolicyClient::new(&self.env, &self.policy).try_evaluate(params, ctx);
        assert_empty(&self.env, &self.policy, &label);
        match verdict {
            Ok(Ok(())) => Ok(()),
            Ok(other) => panic!("{label}: unexpected success payload {other:?}"),
            Err(Ok(error)) => Err(error),
            Err(other) => panic!("{label}: host failure {other:?}"),
        }
    }
}

/// A name for the failing invocation, so a breach points at a path rather than
/// at a line number.
fn label_for(key: u8, plain: bool) -> String {
    if plain {
        return String::from("plain params");
    }
    match key {
        b'L' => String::from("oracle admit"),
        b'D' => String::from("oracle deny"),
        b'P' => String::from("oracle pending fallback"),
        b'A' => String::from("oracle stale fallback"),
        b'F' => String::from("oracle future fallback"),
        b'M' => String::from("oracle mismatched payment_ref fallback"),
        _ => String::from("oracle unreachable fallback"),
    }
}

/// The three storage maps the policy could have written to, all of them empty.
///
/// `instance()` is scoped to `policy` (the host rejects the read outside a
/// contract context, and the map is per-contract); `persistent()` and
/// `temporary()` enumerate every stored entry of that durability, so a write by
/// this policy is visible here whatever key it chose.
fn assert_empty(env: &Env, policy: &Address, label: &str) {
    let (instance, persistent, temporary) = env.as_contract(policy, || {
        (
            env.storage().instance().all().len(),
            env.storage().persistent().all().len(),
            env.storage().temporary().all().len(),
        )
    });
    assert_eq!(instance, 0, "{label} left an instance entry");
    assert_eq!(
        persistent, 0,
        "{label} left a persistent entry ({persistent} now present)"
    );
    assert_eq!(
        temporary, 0,
        "{label} left a temporary entry ({temporary} now present)"
    );
}

// ── The five required paths, plus every fallback shape ───────────────────────

#[test]
fn no_verdict_leaves_a_storage_entry_behind() {
    let fx = Fixture::new();
    let plain = fx.plain_params();
    let oracle = fx.oracle_params();

    // The clock gates, from both sides of the window boundary.
    assert_eq!(
        fx.evaluate_and_check(&plain, &fx.claim(b'a', CURRENT_LEDGER)),
        Ok(())
    );
    assert_eq!(
        fx.evaluate_and_check(&plain, &fx.claim(b'b', CURRENT_LEDGER - WINDOW - 1)),
        Err(Error::WindowExpired)
    );

    // The oracle deciding, in both directions. Each claim is outside the
    // window, so a verdict that differs from the clock's answer can only come
    // from the report — an oracle path that silently fell back would be caught
    // by the expected value, not just by the storage read.
    assert_eq!(fx.evaluate_and_check(&oracle, &fx.claim(b'L', 1)), Ok(()));
    assert_eq!(
        fx.evaluate_and_check(&oracle, &fx.claim(b'D', 1)),
        Err(Error::OraclePolicyDenied)
    );

    // Every route into the fallback, which must land back on the window.
    for key in [b'P', b'A', b'F', b'M', b'Z'] {
        assert_eq!(
            fx.evaluate_and_check(&oracle, &fx.claim(key, 1)),
            Err(Error::WindowExpired),
            "{} did not fall back to the clock",
            label_for(key, false)
        );
    }

    // A rejected `params` blob writes too, and so does a rejected self-call.
    assert_eq!(
        fx.evaluate_and_check(&plain, &fx.claim(b'c', CURRENT_LEDGER)),
        Ok(())
    );
}

/// Repeated claims accumulate nothing: the point of statelessness is that the
/// footprint does not grow with traffic.
#[test]
fn a_hundred_claims_leave_nothing_to_borrow() {
    let fx = Fixture::new();
    let plain = fx.plain_params();
    let oracle = fx.oracle_params();
    for i in 0..50u32 {
        let key = if i % 2 == 0 { b'a' } else { b'L' };
        let params = if i % 2 == 0 { &plain } else { &oracle };
        let _ = fx.evaluate_and_check(params, &fx.claim(key, CURRENT_LEDGER));
    }
    assert_empty(&fx.env, &fx.policy, "after 50 interleaved claims");
}

// ── Control: the assertion above is capable of failing ───────────────────────

/// A single `temporary().set()` — exactly the diagnostic write this issue says
/// would otherwise pass CI — is reported by the same read the tests above use.
/// Without this, "empty" could mean "the reader cannot see writes".
#[test]
fn the_reader_reports_a_write() {
    let fx = Fixture::new();
    let policy = fx.policy.clone();
    let env = &fx.env;

    assert_empty(env, &policy, "before the write");
    env.as_contract(&policy, || {
        env.storage().temporary().set(&1u32, &2u32);
    });
    let temporary = env.as_contract(&policy, || env.storage().temporary().all().len());
    assert_eq!(temporary, 1, "a temporary write was not visible");
}

/// The same, for persistent storage: the durability this policy would use if it
/// cached anything across claims.
#[test]
fn the_reader_reports_a_persistent_write() {
    let fx = Fixture::new();
    let policy = fx.policy.clone();
    let env = &fx.env;

    env.as_contract(&policy, || {
        env.storage().persistent().set(&1u32, &2u32);
    });
    let persistent = env.as_contract(&policy, || env.storage().persistent().all().len());
    assert_eq!(persistent, 1, "a persistent write was not visible");
}

/// And for the instance map, which is the one the read scopes to this contract:
/// a write by *another* contract must not be reported as this policy's, and a
/// write by this policy must be.
#[test]
fn the_instance_read_is_scoped_to_the_policy() {
    let fx = Fixture::new();
    let env = &fx.env;
    let other = env.register(ScriptedOracle, ());

    env.as_contract(&other, || {
        env.storage().instance().set(&1u32, &2u32);
    });
    let on_other = env.as_contract(&other, || env.storage().instance().all().len());
    let on_policy = env.as_contract(&fx.policy, || env.storage().instance().all().len());
    assert_eq!(on_other, 1, "the mock oracle's own write is not visible");
    assert_eq!(
        on_policy, 0,
        "another contract's write leaked into the read"
    );

    env.as_contract(&fx.policy, || {
        env.storage().instance().set(&1u32, &2u32);
    });
    let after = env.as_contract(&fx.policy, || env.storage().instance().all().len());
    assert_eq!(after, 1, "a write through the policy was not visible");
}
