//! `RefundVault` admin-control event family (issue #629).
//!
//! `src/lib.rs` declares five admin-control events — `PauseEvent`,
//! `UnpauseEvent`, `AdminTransferInitiatedEvent`, `AdminTransferAcceptedEvent`
//! and `FeeConfigUpdatedEvent` — and nothing outside that declaration asserted
//! their shape. Event topics and field names are a public interface consumed by
//! external indexers (`CONTRIBUTING.md`, Event Stability Policy), so the trail an
//! operator follows to see who paused a vault and who took the admin key is
//! pinned here: each test drives a real admin call and compares the published
//! event against the contract's own event struct serialised with
//! `Event::to_xdr`, exactly as `contracts/state-channel/src/hashlock_test.rs`
//! does. A change to a topic tuple or a data field fails here instead of
//! silently breaking an indexer.
//!
//! `env.events().all()` reflects **only the last top-level invocation**, so every
//! assertion below runs immediately after the call it checks and never
//! accumulates events from earlier calls in the same test.
//!
//! No new harness: the vault is registered the way
//! `tests/multisig_admin_vault.rs` and `tests/integration_test.rs` register it,
//! and the ledger is stepped explicitly rather than by the crate's TTL constants
//! (those are private to `src/lib.rs` and none of these paths depends on them).

use accensa_common::{Error, VaultInit};
use refund_vault::{
    AdminTransferAcceptedEvent, AdminTransferInitiatedEvent, FeeConfigUpdatedEvent, PauseEvent,
    RefundVault, RefundVaultClient, UnpauseEvent,
};
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, Events as _, Ledger as _},
    xdr::ContractEvent,
    Address, Env, Symbol,
};

/// A vault owned by a plain keypair admin. No policy contracts are wired: none
/// of the admin-control paths below touches the refund policy gate.
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

    let init = VaultInit {
        merchant: merchant.clone(),
        token,
        time_policy: None,
        vdf_policy: None,
        fee_bps: 0,
        fee_recipient: None,
        refund_window: 100,
        deadline: 0,
        vdf_delay: 0,
    };
    let contract = env.register(RefundVault, (init,));
    let client = RefundVaultClient::new(&env, &contract);

    Fixture {
        env,
        client,
        contract,
        merchant,
    }
}

/// Move to `sequence`, so an event's ledger-bearing topic is a value the test
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

// ── pause / unpause ────────────────────────────────────────────────────────

/// `pause` publishes one `PauseEvent` whose topic carries the ledger it happened
/// at, so a pause window is reconstructible from the log alone.
#[test]
fn pause_event_carries_the_ledger_it_was_published_at() {
    let s = setup();
    at_ledger(&s.env, 500);

    s.client.pause();

    let expected = PauseEvent { ledger: 500 }.to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, expected);
    assert!(s.client.is_paused(), "the vault must be paused");
}

/// The mirror image: `unpause` closes the window with a `UnpauseEvent` on the
/// ledger it ran at.
#[test]
fn unpause_event_carries_the_ledger_it_was_published_at() {
    let s = setup();
    at_ledger(&s.env, 500);
    s.client.pause();

    at_ledger(&s.env, 600);
    s.client.unpause();

    let expected = UnpauseEvent { ledger: 600 }.to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, expected);
    assert!(!s.client.is_paused(), "the vault must be unpaused");
}

// ── two-step admin handover ────────────────────────────────────────────────

/// The initiation names both sides of the handover — `from` the sitting admin
/// and `to` the nominee — and moves nothing yet.
#[test]
fn admin_transfer_initiated_event_names_both_sides() {
    let s = setup();
    let nominee = Address::generate(&s.env);
    at_ledger(&s.env, 700);

    s.client.transfer_admin(&nominee);

    let expected = AdminTransferInitiatedEvent {
        from: s.merchant.clone(),
        to: nominee.clone(),
    }
    .to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, expected);
    // Initiation is not the handover: the admin key is unchanged until the
    // nominee accepts.
    assert_eq!(
        s.client.get_admin(),
        s.merchant,
        "transfer_admin must not move the key on its own"
    );
}

/// The acceptance names the same two addresses and is the call that actually
/// moves the admin key.
#[test]
fn admin_transfer_accepted_event_names_both_sides() {
    let s = setup();
    let nominee = Address::generate(&s.env);
    s.client.transfer_admin(&nominee);

    at_ledger(&s.env, 800);
    s.client.accept_admin();

    let expected = AdminTransferAcceptedEvent {
        from: s.merchant.clone(),
        to: nominee.clone(),
    }
    .to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, expected);
    assert_eq!(
        s.client.get_admin(),
        nominee,
        "accept_admin must install the nominee"
    );
}

// ── fee configuration ──────────────────────────────────────────────────────

/// Changing the rate publishes the **full** effective configuration, tagged with
/// the field that changed, and agrees with the vault's own getters.
#[test]
fn fee_rate_change_event_carries_the_new_rate_and_matches_the_getters() {
    let s = setup();
    let fee_recipient = Address::generate(&s.env);
    s.client.set_fee_recipient(&fee_recipient);

    at_ledger(&s.env, 900);
    s.client.set_fee_bps(&250);

    let expected = FeeConfigUpdatedEvent {
        field: Symbol::new(&s.env, "fee_bps"),
        fee_bps: 250,
        fee_recipient: fee_recipient.clone(),
    };
    let event = expected.to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, event);

    // What an indexer decodes off-chain must be the configuration the contract
    // reports on-chain afterwards.
    assert_eq!(s.client.get_fee_bps(), expected.fee_bps);
    assert_eq!(
        s.client.get_fee_recipient(),
        Some(expected.fee_recipient),
        "the event must carry the recipient the getter reports"
    );
}

/// Changing the recipient publishes the same shape, carrying the rate that is
/// still in force so a reader never has to join two events.
#[test]
fn fee_recipient_change_event_carries_the_full_effective_config() {
    let s = setup();
    s.client.set_fee_bps(&250);
    let fee_recipient = Address::generate(&s.env);

    at_ledger(&s.env, 950);
    s.client.set_fee_recipient(&fee_recipient);

    let expected = FeeConfigUpdatedEvent {
        field: Symbol::new(&s.env, "fee_recipient"),
        fee_bps: 250,
        fee_recipient: fee_recipient.clone(),
    };
    let event = expected.to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, event);

    assert_eq!(s.client.get_fee_bps(), expected.fee_bps);
    assert_eq!(s.client.get_fee_recipient(), Some(expected.fee_recipient));
}

// ── repeat pause ───────────────────────────────────────────────────────────

/// `pause` is idempotent in *state* but not in the event log: the contract sets
/// `IsPaused` and publishes unconditionally, so a second call while already
/// paused publishes a second `PauseEvent`.
///
/// The issue that requested this file listed a duplicate pause among the
/// rejected paths that should emit nothing. That is not the contract's current
/// behaviour (`src/lib.rs::pause` has no `AlreadyPaused` guard, and
/// `accensa_common::Error` defines no such variant), and adding one would be a
/// behaviour change outside this test-only issue's file ownership. The test
/// therefore pins what an indexer can actually observe — the second call leaves
/// the vault paused and emits exactly one further event for its own ledger —
/// rather than asserting a silence the contract does not produce.
#[test]
fn duplicate_pause_republishes_the_pause_event() {
    let s = setup();
    at_ledger(&s.env, 500);
    s.client.pause();

    at_ledger(&s.env, 550);
    s.client.pause();

    let expected = PauseEvent { ledger: 550 }.to_xdr(&s.env, &s.contract);
    assert_only_event(&s.env, &s.contract, expected);
    assert!(s.client.is_paused());
}

// ── rejected path ──────────────────────────────────────────────────────────

/// `accept_admin` with no pending transfer is rejected *before* anything is
/// published, so an indexer never sees an `AdminTransferAcceptedEvent` for a
/// handover that was never initiated.
#[test]
fn accept_admin_without_a_pending_transfer_emits_nothing() {
    let s = setup();

    assert_eq!(
        s.client.try_accept_admin(),
        Err(Ok(Error::NoPendingTransfer))
    );

    let events = s.env.events().all().filter_by_contract(&s.contract);
    assert_eq!(
        events.events().len(),
        0,
        "a rejected accept must publish no event"
    );
    // And no phantom handover happened either.
    assert_eq!(s.client.get_admin(), s.merchant);
}
