//! `RefundVault` policy and oracle-config event family (issue #631).
//!
//! `src/lib.rs` declares four policy events — `PolicyProposedEvent`,
//! `PolicyExecutedEvent`, `OraclePolicySetEvent` and `OraclePolicyClearedEvent`
//! — and nothing outside `src/oracle_tests.rs`'s map-shaped oracle assertions
//! pinned their exact form. These four describe which refund rules govern a
//! vault, and the clear-vs-set pair is the observable difference between
//! "policy enforced" and "policy removed", which the refund path then depends
//! on. Event topics and field names are a public interface consumed by external
//! indexers (`CONTRIBUTING.md`, Event Stability Policy), so each test below
//! drives a real admin call and compares the published event against the
//! contract's own event struct serialised with `Event::to_xdr`, exactly as
//! `contracts/state-channel/src/hashlock_test.rs` does. This file asserts the
//! emitted trail only — how a policy is enforced stays in `src/oracle.rs` and
//! the settlement path, untouched here.
//!
//! `env.events().all()` reflects **only the last top-level invocation**, so
//! every assertion below runs immediately after the call it checks and never
//! accumulates events from earlier calls in the same test.
//!
//! Setup mirrors `tests/multisig_admin_vault.rs` / `tests/admin_events.rs`,
//! with one addition: a `TimePolicy` contract is wired so that a proposal
//! with a non-zero window can actually be executed (`execute_policy` refuses
//! to activate a gate with no policy contract, `Error::PolicyContractsNotConfigured`).

use accensa_common::{Error, VaultInit};
use refund_policy_time::TimePolicy;
use refund_vault::oracle::OraclePolicy;
use refund_vault::{
    OraclePolicyClearedEvent, OraclePolicySetEvent, PolicyExecutedEvent, PolicyProposal,
    PolicyProposedEvent, RefundVault, RefundVaultClient,
};
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, Events as _, Ledger as _},
    token::StellarAssetClient,
    xdr::ContractEvent,
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;
/// Matches the constant `POLICY_TIMELOCK` in `src/lib.rs`, read through the
/// contract's own getter rather than duplicated.
const WINDOW: u32 = 100;

struct Fixture {
    env: Env,
    client: RefundVaultClient<'static>,
    /// The vault's own address — the contract whose events are asserted.
    contract: Address,
    merchant: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let init = VaultInit {
        merchant: merchant.clone(),
        token,
        time_policy: Some(env.register(TimePolicy, ())),
        vdf_policy: None,
        fee_bps: 0,
        fee_recipient: None,
        refund_window: WINDOW,
        deadline: 0,
        vdf_delay: 0,
    };
    let contract = env.register(RefundVault, (init,));
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &FLOAT, &None);

    Fixture {
        env,
        client,
        contract,
        merchant,
    }
}

/// Move to `sequence`, so an event's ledger-bearing field is a value the test
/// chose rather than whatever the default ledger happens to be.
fn at_ledger(env: &Env, sequence: u32) {
    env.ledger().with_mut(|li| li.sequence_number = sequence);
}

/// Assert the last invocation published exactly one event on `contract`, and
/// that it serialises to `expected` (the full XDR form: topics and data).
fn assert_only_event(env: &Env, contract: &Address, expected: ContractEvent) {
    let events = env.events().all().filter_by_contract(contract);
    assert_eq!(events.events().len(), 1, "expected exactly one event");
    assert_eq!(&events.events()[0], &expected);
}

/// Assert the last invocation published no event at all on `contract`.
fn assert_no_event(env: &Env, contract: &Address) {
    let events = env.events().all().filter_by_contract(contract);
    assert_eq!(events.events().len(), 0, "expected no event");
}

// ── PolicyProposedEvent / PolicyExecutedEvent ──────────────────────────────

/// `propose_policy` publishes `PolicyProposedEvent` carrying the window, the
/// deadline, the ledger it was proposed at and the ledger after which it may
/// be executed — and the proposal lands in storage exactly as announced
/// (getter: `get_pending_policy`).
#[test]
fn propose_event_is_paired_with_the_pending_policy_getter() {
    let s = setup();
    at_ledger(&s.env, 500);
    let timelock = s.client.get_policy_timelock();

    s.client.propose_policy(&50, &1_700_000_000, &0);

    assert_only_event(
        &s.env,
        &s.contract,
        PolicyProposedEvent {
            window: 50,
            deadline: 1_700_000_000,
            proposed_at_ledger: 500,
            execute_after_ledger: 500 + timelock,
        }
        .to_xdr(&s.env, &s.contract),
    );
    assert_eq!(
        s.client.get_pending_policy(),
        Some(PolicyProposal {
            window: 50,
            deadline: 1_700_000_000,
            vdf_delay: 0,
            proposed_at_ledger: 500,
        }),
        "the stored proposal must match what the event announced"
    );
}

/// `execute_policy` publishes `PolicyExecutedEvent`, and the policy getters
/// afterwards agree with it: the window and deadline are in force, the pending
/// proposal is gone.
#[test]
fn execute_event_is_paired_with_the_policy_getters() {
    let s = setup();
    at_ledger(&s.env, 500);
    s.client.propose_policy(&50, &1_700_000_000, &0);

    at_ledger(&s.env, 500 + s.client.get_policy_timelock());
    s.client.execute_policy();

    assert_only_event(
        &s.env,
        &s.contract,
        PolicyExecutedEvent {
            window: 50,
            deadline: 1_700_000_000,
        }
        .to_xdr(&s.env, &s.contract),
    );
    assert_eq!(s.client.get_refund_window(), 50);
    assert_eq!(s.client.get_refund_deadline(), 1_700_000_000);
    assert_eq!(s.client.get_pending_policy(), None);
}

/// Executing a proposal before its timelock expires is rejected and leaves
/// the trail silent: no `PolicyExecutedEvent` for a policy change that never
/// happened.
#[test]
fn execute_before_timelock_is_rejected_and_emits_nothing() {
    let s = setup();
    at_ledger(&s.env, 500);
    s.client.propose_policy(&50, &0, &0);

    at_ledger(&s.env, 500 + s.client.get_policy_timelock() - 1);
    assert_eq!(
        s.client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );
    assert_no_event(&s.env, &s.contract);
    assert_eq!(s.client.get_refund_window(), WINDOW);
}

/// A proposal from a caller that is not the merchant is rejected before any
/// state is touched: no `PolicyProposedEvent` is published.
#[test]
fn rejected_proposal_emits_no_policy_proposed_event() {
    let s = setup();
    at_ledger(&s.env, 500);

    // No mock for this call: the merchant's `require_auth` fails.
    s.env.set_auths(&[]);
    assert!(s.client.try_propose_policy(&50, &0, &0).is_err());

    assert_no_event(&s.env, &s.contract);
    assert_eq!(s.client.get_pending_policy(), None);
}

/// `execute_policy` consumes its proposal: a second execution finds nothing
/// pending and emits no `PolicyExecutedEvent`, so the trail cannot replay a
/// policy change.
#[test]
fn executing_an_already_executed_proposal_emits_nothing() {
    let s = setup();
    at_ledger(&s.env, 500);
    s.client.propose_policy(&50, &0, &0);

    at_ledger(&s.env, 500 + s.client.get_policy_timelock());
    s.client.execute_policy();

    assert_eq!(
        s.client.try_execute_policy(),
        Err(Ok(Error::NoPendingPolicy))
    );
    assert_no_event(&s.env, &s.contract);
}

// ── OraclePolicySetEvent / OraclePolicyClearedEvent ────────────────────────

/// The set/clear pair, asserted against the vault's own `get_oracle_policy`
/// getter: the set event must report the policy now in force, and the clear
/// event must report the feed of the policy that was in force — an event
/// carrying a stale policy or a stale feed fails here.
#[test]
fn oracle_policy_set_and_clear_events_match_the_getter() {
    let s = setup();
    let feed_id = BytesN::from_array(&s.env, &[0x42u8; 32]);
    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 100,
        refund_when_below: true,
    };

    s.client.set_oracle_policy(&policy);

    assert_only_event(
        &s.env,
        &s.contract,
        OraclePolicySetEvent {
            feed_id: feed_id.clone(),
            threshold: 250,
            refund_when_below: true,
            max_staleness_ledgers: 100,
        }
        .to_xdr(&s.env, &s.contract),
    );
    assert_eq!(
        s.client.get_oracle_policy(),
        Some(policy),
        "the stored policy must match what the set event reported"
    );

    s.client.clear_oracle_policy();

    assert_only_event(
        &s.env,
        &s.contract,
        OraclePolicyClearedEvent {
            feed_id: feed_id.clone(),
        }
        .to_xdr(&s.env, &s.contract),
    );
    assert_eq!(
        s.client.get_oracle_policy(),
        None,
        "clearing must actually remove the policy the event reported"
    );
}

/// Clearing the oracle policy is not just bookkeeping: while the policy is in
/// force the refund path is oracle-gated and fails closed (no oracle is
/// whitelisted here), and after `clear_oracle_policy` the same refund succeeds
/// on the plain time-window path. The clear event is the observable marker of
/// that transition.
#[test]
fn clearing_the_oracle_policy_restores_the_time_window_refund_path() {
    let s = setup();
    let feed_id = BytesN::from_array(&s.env, &[0x43u8; 32]);
    let pref = BytesN::from_array(&s.env, &[0x77u8; 32]);

    s.client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });

    // Oracle-gated: no oracle is whitelisted, so the vault fails closed.
    let nonce = s.client.get_user_nonce(&s.merchant);
    assert!(s
        .client
        .try_refund(
            &pref,
            &Address::generate(&s.env),
            &100,
            &0,
            &1_000,
            &None,
            &nonce
        )
        .is_err());
    assert!(s.client.get_refund(&pref).is_none());

    // Removing the policy restores the time-window path.
    s.client.clear_oracle_policy();
    assert_only_event(
        &s.env,
        &s.contract,
        OraclePolicyClearedEvent {
            feed_id: feed_id.clone(),
        }
        .to_xdr(&s.env, &s.contract),
    );
    assert_eq!(s.client.get_oracle_policy(), None);

    // The identical refund now succeeds within the configured window.
    let nonce = s.client.get_user_nonce(&s.merchant);
    s.client.refund(
        &pref,
        &Address::generate(&s.env),
        &100,
        &0,
        &1_000,
        &None,
        &nonce,
    );
    assert!(s.client.get_refund(&pref).is_some());
}
