//! #972 — the four things the commit-delay suite still cannot see.
//!
//! `commit` stores `committed_at_ledger` and advertises
//! `reveal_at_ledger = committed_at_ledger + COMMIT_MIN_DELAY_LEDGERS` in the
//! `CommitEvent`; `reveal` re-derives the hash, then refuses anything earlier
//! than that boundary with `CommitDelayNotElapsed`. The issue was written when
//! neither entry point had any test. `src/commit_reveal_tests.rs` has since
//! landed and pins the switch itself — the refusal at `L + delay - 1`, the
//! acceptance at exactly `L + delay`, the mismatch and the event shapes — so
//! rewriting `reveal`'s comparison as `<=` is already caught there.
//!
//! What that suite structurally cannot see, and what is pinned here instead:
//!
//!   1. **a refusal must not consume.** Every in-source reveal lands on a
//!      ledger where the delay is already met, so none of them asks whether a
//!      *refused* reveal leaves the commitment behind. Here the record is
//!      compared field by field, `committed_at_ledger` included, which also
//!      rules out re-stamping it on refusal — the rewrite that silently
//!      extends the wait by `delay` ledgers on every early attempt.
//!   2. **the boundary is a lower bound.** Each in-source ledger move is
//!      `committed_at + COMMIT_MIN_DELAY_LEDGERS`, exactly. Replacing `<` with
//!      `!=` therefore leaves all of them green while making a reveal that
//!      waits *longer* — the safe direction for a front-runner, the broken one
//!      for the merchant — fail forever. A reveal at `L + 2 * delay` is asked
//!      here.
//!   3. **the hash check precedes the delay check.** The in-source mismatch
//!      test advances past the delay first, and its delay test reveals the
//!      *right* plaintext, so no case has both rejections available at once.
//!      Swapping the two blocks in `reveal` is invisible there and changes
//!      which code a front-runner gets. Here a wrong plaintext is offered one
//!      ledger early.
//!   4. **the advertised ledger is the advertised *getter*.** The in-source
//!      event test compares `reveal_at_ledger` against the private const; if
//!      the published number and `get_commit_min_delay()` disagreed, that test
//!      could not notice. Every ledger in this file is computed from the
//!      public getter, so the event and the enforced boundary are checked
//!      against the same value a client reads.
//!
//! Each case gets its own vault, so moving the ledger cannot disturb the
//! previous one.
//!
//! The delay comes from `get_commit_min_delay()` rather than a hard-coded 7, so
//! every number below is the contract's own arithmetic and moves with it.

use accensa_common::{Error, VaultInit};
use refund_vault::{
    CommitEvent, CommitRecord, CommitRevealedEvent, RefundVault, RefundVaultClient,
};
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Bytes, BytesN, Env, Symbol,
};

/// The ledger `commit` is called at. Arbitrary: every assertion is relative to
/// this and to the delay the contract reports.
const COMMIT_LEDGER: u32 = 100;

/// The plaintext whose SHA-256 is the commitment, kept as a constant so a
/// failure message can name the hash that was expected.
const PLAINTEXT: &[u8] = b"refund:commit-delay-boundary:120000";

/// A plaintext that hashes to something else, for the mismatch cases.
const WRONG_PLAINTEXT: &[u8] = b"refund:commit-delay-boundary:999999";

struct Fixture {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    operation: Symbol,
    commitment: BytesN<32>,
    /// The delay the contract itself reports, in ledgers.
    delay: u32,
}

/// A vault parked at `COMMIT_LEDGER` with one commitment pending. No token
/// funding: `commit` and `reveal` move no assets, so the float is irrelevant to
/// the boundary.
///
/// `get_commit_min_delay()` is read *before* the commit: `env.events().all()`
/// reports only the last invocation, so the commit has to be that last call for
/// the advertised boundary to still be observable on return.
fn committed() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = Address::generate(&env);
    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token,
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    env.ledger().set_sequence_number(COMMIT_LEDGER);
    let delay = client.get_commit_min_delay();

    // The same derivation `reveal` performs, so the commitment is the real one
    // and a mismatch failure cannot be an artifact of a hand-written hash.
    let commitment = env
        .crypto()
        .sha256(&Bytes::from_slice(&env, PLAINTEXT))
        .to_bytes();

    let operation = Symbol::new(&env, "refund");
    client.commit(&operation, &commitment);

    Fixture {
        env,
        contract,
        client,
        merchant,
        operation,
        commitment,
        delay,
    }
}

/// `reveal`'s verdict. The generated client answers with three shapes —
/// `Ok(Ok(()))` accepted, `Err(Ok(code))` a typed refusal from the contract,
/// anything else a host failure or an undecodable return — and only the first
/// two are verdicts, so the rest is reported as itself rather than flattened
/// into an error a test might mistake for a refusal.
fn reveal(f: &Fixture, plaintext: &[u8]) -> Result<(), Error> {
    match f.client.try_reveal(
        &f.operation,
        &f.commitment,
        &Bytes::from_slice(&f.env, plaintext),
    ) {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        Ok(Err(conv)) => panic!("`reveal` returned an undecodable value: {conv:?}"),
        Err(Err(host)) => panic!("`reveal` failed as a host error: {host:?}"),
    }
}

fn set_ledger(f: &Fixture, sequence: u32) {
    f.env.ledger().set_sequence_number(sequence);
}

/// The commitment exactly `delay` ledgers after the commit, still unconsumed.
fn pending(f: &Fixture) -> Option<CommitRecord> {
    Some(CommitRecord {
        caller: f.merchant.clone(),
        operation: f.operation.clone(),
        committed_at_ledger: COMMIT_LEDGER,
    })
}

#[test]
fn the_commit_event_advertises_the_boundary_the_reveal_enforces() {
    // `reveal_at_ledger` is the wait a client is told to observe. If `reveal`
    // enforced a different ledger, the front-running protection would be a
    // promise the contract does not keep.
    let f = committed();
    let events = f.env.events().all().filter_by_contract(&f.contract);
    assert_eq!(events.events().len(), 1, "one commit publishes one event");
    let expected = CommitEvent {
        operation: f.operation.clone(),
        commitment_hash: f.commitment.clone(),
        reveal_at_ledger: COMMIT_LEDGER + f.delay,
    };
    assert_eq!(
        &events.events()[0],
        &expected.to_xdr(&f.env, &f.contract),
        "the advertised reveal ledger must be `committed_at_ledger + get_commit_min_delay()`",
    );
}

#[test]
fn one_ledger_early_is_refused_and_leaves_the_commitment_pending() {
    let f = committed();
    set_ledger(&f, COMMIT_LEDGER + f.delay - 1);

    assert_eq!(
        reveal(&f, PLAINTEXT),
        Err(Error::CommitDelayNotElapsed),
        "the ledger right below the boundary must still be refused",
    );
    // A refusal is not a consumption: the merchant can still reveal later, and
    // the wait must not have been re-started by the attempt. Comparing the
    // whole record pins `committed_at_ledger` at the commit, not at the
    // refusal.
    assert_eq!(
        f.client.get_commit(&f.commitment),
        pending(&f),
        "a refused reveal must leave the original commitment pending",
    );
}

#[test]
fn the_boundary_ledger_reveals_and_consumes_the_commitment() {
    let f = committed();
    set_ledger(&f, COMMIT_LEDGER + f.delay);

    // The non-try client method panics on a typed refusal, so this call is
    // itself the assertion that the boundary ledger is accepted.
    f.client.reveal(
        &f.operation,
        &f.commitment,
        &Bytes::from_slice(&f.env, PLAINTEXT),
    );

    // Read the events first: they describe this invocation only, and the
    // read-backs below would replace them with an empty list.
    let events = f.env.events().all().filter_by_contract(&f.contract);
    assert_eq!(events.events().len(), 1);
    let expected = CommitRevealedEvent {
        operation: f.operation.clone(),
        commitment_hash: f.commitment.clone(),
        ledger: COMMIT_LEDGER + f.delay,
    };
    assert_eq!(
        &events.events()[0],
        &expected.to_xdr(&f.env, &f.contract),
        "the reveal event must report the boundary ledger it accepted",
    );

    assert_eq!(
        f.client.get_commit(&f.commitment),
        None,
        "a successful reveal is single-use",
    );

    // The consumed record then answers `NoCommit`, so nothing else in `reveal`
    // can report a delay error for a commitment that is already gone.
    assert_eq!(
        reveal(&f, PLAINTEXT),
        Err(Error::NoCommit),
        "a consumed commitment cannot be revealed again",
    );
}

#[test]
fn a_late_reveal_is_still_accepted_so_the_boundary_is_not_an_upper_bound() {
    // Every in-source ledger move is `committed_at + delay` exactly, so `<`
    // could be `!=` and all of them stay green. This is what keeps the check a
    // lower bound.
    let f = committed();
    set_ledger(&f, COMMIT_LEDGER + 2 * f.delay);

    f.client.reveal(
        &f.operation,
        &f.commitment,
        &Bytes::from_slice(&f.env, PLAINTEXT),
    );
    assert_eq!(
        f.client.get_commit(&f.commitment),
        None,
        "a reveal well past the boundary must be accepted, not refused forever",
    );
}

#[test]
fn a_wrong_plaintext_is_a_mismatch_at_the_boundary_and_costs_nothing() {
    let f = committed();
    set_ledger(&f, COMMIT_LEDGER + f.delay);

    assert_eq!(
        reveal(&f, WRONG_PLAINTEXT),
        Err(Error::CommitMismatch),
        "the re-derived hash must be checked against the commitment",
    );
    // The mismatched reveal is refused without consuming: the real plaintext
    // still reveals at the same boundary.
    assert_eq!(
        f.client.get_commit(&f.commitment),
        pending(&f),
        "a mismatched reveal must leave the commitment pending",
    );
    assert_eq!(
        reveal(&f, PLAINTEXT),
        Ok(()),
        "a refused mismatch must not consume the commitment either",
    );
    assert_eq!(f.client.get_commit(&f.commitment), None);
}

#[test]
fn the_hash_check_precedes_the_delay_check() {
    // Both rejections are available one ledger early and the contract orders
    // them, so a client can tell "you are early" from "you revealed the wrong
    // action". No in-source case has both unmet at once; moving the delay block
    // above the hash block flips only this answer.
    let f = committed();
    set_ledger(&f, COMMIT_LEDGER + f.delay - 1);

    assert_eq!(
        reveal(&f, WRONG_PLAINTEXT),
        Err(Error::CommitMismatch),
        "a wrong plaintext is reported as a mismatch even while the delay is unmet",
    );
    assert_eq!(f.client.get_commit(&f.commitment), pending(&f));
}
