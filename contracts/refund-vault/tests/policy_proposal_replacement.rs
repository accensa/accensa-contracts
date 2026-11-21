//! #968 — A second `propose_policy` replaces the pending one *and its clock*.
//!
//! `propose_policy` writes the whole `PolicyProposal` — including
//! `proposed_at_ledger` — under the single `DataKey::PendingPolicy` key, and
//! `execute_policy` measures the timelock from the ledger stored there. The
//! doc comment promises that a new proposal overwrites the pending one, but
//! the only existing coverage (`src/test.rs`) asserted the *window* field, so
//! a merge that restored the first proposal's clock would have passed: the
//! vault would then execute on the older, already-elapsed clock.
//!
//! The pair below drives both halves of that contract:
//!   * `get_pending_policy` reports the second proposal only, and
//!   * execution is timed by the second `proposed_at_ledger`, never the first.
//!
//! Ledger movement uses `env.ledger().with_mut(…)`, as the other policy tests
//! in this directory do. The second proposal is `(0, 0, 0)` so activating it
//! needs no time-policy contract: `execute_policy` only refuses a non-zero
//! window or deadline when no policy contract is wired
//! (`Error::PolicyContractsNotConfigured`), and that gate is not what this
//! file is about.

use accensa_common::{Error, VaultInit};
use refund_vault::{PolicyProposal, PolicyProposedEvent, RefundVault, RefundVaultClient};
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, Events as _, Ledger as _},
    token::StellarAssetClient,
    Address, Env,
};

const FLOAT: i128 = 1_000_000;
/// The ledger the first proposal is made at.
const FIRST_LEDGER: u32 = 10;
/// How far the vault advances before the second proposal — deliberately less
/// than the timelock, so the first proposal is still pending when replaced.
const ADVANCE: u32 = 10_000;

struct Fixture {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant,
            token,
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 500,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);

    Fixture {
        env,
        contract,
        client,
    }
}

fn at_ledger(env: &Env, sequence: u32) {
    env.ledger().with_mut(|li| li.sequence_number = sequence);
}

/// The second proposal, one window/deadline pair away from the first so the
/// fields cannot be confused with it.
fn second() -> PolicyProposal {
    PolicyProposal {
        window: 0,
        deadline: 0,
        vdf_delay: 0,
        proposed_at_ledger: FIRST_LEDGER + ADVANCE,
    }
}

/// Propose at `FIRST_LEDGER`, advance `ADVANCE` ledgers, propose again.
fn propose_twice(s: &Fixture) {
    at_ledger(&s.env, FIRST_LEDGER);
    s.client.propose_policy(&500, &1_700_000_000, &0);

    at_ledger(&s.env, FIRST_LEDGER + ADVANCE);
    s.client.propose_policy(&0, &0, &0);
}

#[test]
fn timelock_is_the_constant_the_contract_advertises() {
    let s = setup();
    assert_eq!(s.client.get_policy_timelock(), 17_280);

    at_ledger(&s.env, FIRST_LEDGER);
    assert_eq!(s.env.ledger().sequence(), FIRST_LEDGER);
}

/// Only the second proposal survives: every field it carries — and only it —
/// is what `get_pending_policy` returns.
#[test]
fn the_second_proposal_replaces_the_first_entirely() {
    let s = setup();
    propose_twice(&s);

    assert_eq!(s.client.get_pending_policy(), Some(second()));
}

/// The first proposal's window and deadline are gone, not merely shadowed: a
/// merge that kept the older `window`/`deadline` under the newer clock (or the
/// newer under the older clock) fails here.
#[test]
fn no_field_of_the_first_proposal_survives() {
    let s = setup();
    at_ledger(&s.env, FIRST_LEDGER);
    s.client.propose_policy(&500, &1_700_000_000, &0);
    assert_eq!(
        s.client.get_pending_policy(),
        Some(PolicyProposal {
            window: 500,
            deadline: 1_700_000_000,
            vdf_delay: 0,
            proposed_at_ledger: FIRST_LEDGER,
        }),
    );

    at_ledger(&s.env, FIRST_LEDGER + ADVANCE);
    s.client.propose_policy(&0, &0, &0);

    let pending = s
        .client
        .get_pending_policy()
        .expect("a proposal is pending");
    assert_eq!(pending.window, 0, "the first window is still in the slot");
    assert_eq!(
        pending.deadline, 0,
        "the first deadline is still in the slot"
    );
    assert_eq!(
        pending.proposed_at_ledger,
        FIRST_LEDGER + ADVANCE,
        "the first clock is still in the slot"
    );
}

/// The replacement is announced in the event trail with its own clock, which
/// is what an indexer would use to compute the same `execute_after_ledger`.
#[test]
fn the_replacement_proposal_publishes_its_own_clock() {
    let s = setup();
    at_ledger(&s.env, FIRST_LEDGER);
    s.client.propose_policy(&500, &1_700_000_000, &0);

    let timelock = s.client.get_policy_timelock();
    at_ledger(&s.env, FIRST_LEDGER + ADVANCE);
    s.client.propose_policy(&0, &0, &0);

    let events = s.env.events().all().filter_by_contract(&s.contract);
    assert_eq!(events.events().len(), 1, "one event for the last call");
    assert_eq!(
        &events.events()[0],
        &PolicyProposedEvent {
            window: 0,
            deadline: 0,
            proposed_at_ledger: FIRST_LEDGER + ADVANCE,
            execute_after_ledger: FIRST_LEDGER + ADVANCE + timelock,
        }
        .to_xdr(&s.env, &s.contract),
    );
}

/// If the pending slot still carried the *first* clock, the vault would be
/// executable at `FIRST_LEDGER + timelock`. It is not: the timelock is measured
/// from the proposal that actually occupies the slot.
#[test]
fn execution_is_refused_at_the_first_proposals_expiry() {
    let s = setup();
    propose_twice(&s);

    let timelock = s.client.get_policy_timelock();
    at_ledger(&s.env, FIRST_LEDGER + timelock);
    assert_eq!(
        s.client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired)),
        "`execute_policy` honoured the replaced proposal's clock",
    );
    assert!(
        s.client.get_pending_policy().is_some(),
        "a refused execution must leave the proposal pending"
    );
}

/// One ledger short of the *second* proposal's expiry it still refuses; on the
/// expiry itself it applies the second proposal and clears the slot.
#[test]
fn execution_follows_the_second_proposals_timelock() {
    let s = setup();
    propose_twice(&s);

    let timelock = s.client.get_policy_timelock();
    let second_at = FIRST_LEDGER + ADVANCE;

    at_ledger(&s.env, second_at + timelock - 1);
    assert_eq!(
        s.client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );

    at_ledger(&s.env, second_at + timelock);
    s.client.execute_policy();

    assert_eq!(s.client.get_pending_policy(), None, "the slot is cleared");
    assert_eq!(
        s.client.get_refund_window(),
        0,
        "the second window is in force"
    );
    assert_eq!(
        s.client.get_refund_deadline(),
        0,
        "the second deadline is in force"
    );
}
