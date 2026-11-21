//! #963 — `claim_batch` rejects an oversized vector before it takes the lock,
//! reads the nonce, or moves a token, and the cap is inclusive at 100.
//!
//! `MAX_BATCH_SIZE` is 100 (`src/lib.rs:573`) and the length guard is the *first*
//! statement of `claim_batch` (`src/lib.rs:1419-1421`), ahead of the reentrancy
//! `acquire` (`:1423`), the pause read (`:1425`), the admin lookup and its auth,
//! the caller's nonce bump (`:1434`) and the loop. So a rejected batch has to be
//! indistinguishable from a call that never arrived, and the boundary is
//! 100 accepted / 101 refused rather than "roughly a hundred".
//!
//! Both rejected sizes are one code path at different distances from the guard,
//! and `CAP + 1` is the real boundary; 1001 is kept because it is the number the
//! issue reports and what a caller who misread the cap would actually send.
//!
//! Coverage before this file was `src/test.rs::test_process_batch_exceeds_max_size_fails`,
//! which exercises the sibling `process_batch` entry point; the two guards sit in
//! different functions, so nothing tested this one's position relative to the
//! nonce bump. That position is the point: an implementation that bumped the
//! caller's `UserNonce` first would burn a nonce on every oversized batch and
//! strand a client that retries, and `get_refund`/balances alone would not show it.
//!
//! The accepted case runs with the host's per-invocation resource limits turned
//! off (`disable_network_limits`), which is how the repo's `*/src/budget_test.rs`
//! harnesses already work. Amounts stay at one unit and the float at `CAP` units
//! so it cannot fail on `InsufficientFloat` instead of proving the cap.
//!
//! The last test is the reason for the caveat: every element publishes events of
//! its own, so a contract-legal batch of `CAP` claims does not fit the mainnet
//! per-invocation event ceiling at all. That is a property of the contract plus
//! the network settings, not of the guard, so the guard is pinned with the ceiling
//! lifted and the ceiling is pinned separately from measured numbers.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundClaim, RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, Vec,
};

/// `MAX_BATCH_SIZE` at `src/lib.rs:573`.
const CAP: u32 = 100;
/// One unit per claim, so a full batch spends exactly `CAP` of float.
const AMOUNT: i128 = 1;

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    recipient: Address,
    token: TokenClient<'static>,
}

fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let recipient = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &1_000_000);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            // The time gate stays out of this: a batch that had to fail on the
            // window would not say anything about the cap.
            refund_window: 0,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &(AMOUNT * CAP as i128), &None);

    let token = TokenClient::new(&env, &token);

    Vault {
        env,
        contract,
        client,
        merchant,
        recipient,
        token,
    }
}

/// A distinct payment ref for element `i`, so a batch of `len` elements is `len`
/// independent payments rather than one payment refunded `len` times.
fn ref_at(env: &Env, i: u32) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&i.to_be_bytes());
    BytesN::from_array(env, &bytes)
}

/// `len` one-unit claims to one reused recipient address.
fn batch(v: &Vault, len: u32) -> Vec<RefundClaim> {
    let mut claims = Vec::new(&v.env);
    for i in 0..len {
        claims.push_back(RefundClaim {
            payment_ref: ref_at(&v.env, i),
            recipient: v.recipient.clone(),
            amount: AMOUNT,
            paid_at_ledger: 0,
            payment_amount: AMOUNT,
            vdf_proof: None,
        });
    }
    claims
}

/// Turns off the per-invocation limits the host applies in tests by default,
/// which are the mainnet transaction limits plus the CPU/memory budget they
/// imply.
///
/// `reset_limits` on the budget alone is not enough for a full batch: the host
/// keeps a separate `contract_events_size_bytes` ceiling of 16 KiB
/// (`soroban-sdk-29.0.1/src/testutils/cost_estimate.rs:155`), and exceeding it
/// panics the test binary instead of returning a contract error. See
/// `the_mainnet_event_ceiling_binds_well_before_the_contract_cap`.
fn disable_network_limits(v: &Vault) {
    v.env.cost_estimate().disable_resource_limits();
}

/// The state a call that returned before doing anything must leave behind: float
/// intact, nobody paid, no ref written, neither counter moved, nothing published.
fn assert_untouched(v: &Vault, vault_nonce: u64) {
    assert_eq!(v.token.balance(&v.contract), AMOUNT * CAP as i128);
    assert_eq!(v.token.balance(&v.recipient), 0);
    assert_eq!(
        v.env.events().all().filter_by_contract(&v.contract),
        vec![&v.env],
        "the guard runs before the loop, so no element can have published",
    );
    assert_eq!(
        v.client.get_nonce(),
        vault_nonce,
        "`get_nonce` is bumped per claim; the rejected batch must not have consumed one",
    );
    assert_eq!(
        v.client.get_user_nonce(&v.merchant),
        0,
        "the guard precedes the caller's nonce bump, so a retry can reuse nonce 0",
    );
    assert!(v.client.get_refund(&ref_at(&v.env, 0)).is_none());
}

/// 101 is one past the cap, and the answer is typed rather than a host abort.
///
/// This one runs with the mainnet limits still in force on purpose: a call that
/// returns from the guard costs nothing per element, so refusing an oversized
/// batch is cheap enough to be true on a real network.
#[test]
fn one_element_past_the_cap_is_rejected_before_anything_is_read() {
    let v = setup();
    let vault_nonce = v.client.get_nonce();
    let claims = batch(&v, CAP + 1);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::BatchTooLarge)),
    );

    assert_untouched(&v, vault_nonce);
}

/// The number the issue reports, same guard, same nothing-changed guarantee.
#[test]
fn a_thousand_and_one_claims_are_rejected_too() {
    let v = setup();
    let vault_nonce = v.client.get_nonce();
    disable_network_limits(&v);
    let claims = batch(&v, 1_001);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::BatchTooLarge)),
    );

    assert_untouched(&v, vault_nonce);
}

/// The cap is inclusive: exactly `CAP` claims is accepted and every one of them
/// is paid, which is what stops `>` in the guard from being read as `>=`.
///
/// The network limits are off here, so this pins the contract's own boundary.
/// Whether a real network can carry the batch it admits is the last test.
#[test]
fn a_batch_of_exactly_the_cap_is_accepted_and_paid() {
    let v = setup();
    let vault_nonce = v.client.get_nonce();
    disable_network_limits(&v);
    let claims = batch(&v, CAP);

    v.client.claim_batch(&claims, &0);

    assert_eq!(
        v.token.balance(&v.recipient),
        AMOUNT * CAP as i128,
        "one unit per claim, `CAP` claims",
    );
    assert_eq!(v.token.balance(&v.contract), 0);
    // Every ref exists, so the loop ran to the end rather than short-circuiting.
    for i in [0u32, 1, CAP / 2, CAP - 1] {
        let record = v.client.get_refund(&ref_at(&v.env, i)).unwrap();
        assert_eq!(record.amount_refunded, AMOUNT);
    }
    assert_eq!(v.client.get_user_nonce(&v.merchant), 1);
    assert_eq!(v.client.get_nonce(), vault_nonce + CAP as u64);
}

/// The boundary is the guard's only input: `CAP + 1` fails and `CAP` succeeds
/// from the same vault, in the same test, with the same nonce.
#[test]
fn the_boundary_is_100_accepted_and_101_refused() {
    let v = setup();
    let vault_nonce = v.client.get_nonce();
    disable_network_limits(&v);

    assert_eq!(
        v.client.try_claim_batch(&batch(&v, CAP + 1), &0),
        Err(Ok(Error::BatchTooLarge)),
    );
    assert_untouched(&v, vault_nonce);

    v.client.claim_batch(&batch(&v, CAP), &0);
    assert_eq!(v.token.balance(&v.recipient), AMOUNT * CAP as i128);
    assert_eq!(v.client.get_user_nonce(&v.merchant), 1);

    // And the size check still wins on the very next call: over-cap is refused
    // regardless of how much float or how many nonces are left.
    assert_eq!(
        v.client.try_claim_batch(&batch(&v, CAP + 1), &1),
        Err(Ok(Error::BatchTooLarge)),
    );
    assert_eq!(v.client.get_user_nonce(&v.merchant), 1);
}

/// The event ceiling of `InvocationResourceLimits::mainnet()`, hardcoded in the
/// pinned SDK (`soroban-sdk-29.0.1/src/testutils/cost_estimate.rs:155`) and the
/// reason the accepted cases above run with the network limits off.
const MAINNET_EVENT_BYTES: u32 = 16_384;

/// A small enough batch to run inside the mainnet limits, used to measure what a
/// claim costs in event bytes rather than to prove anything about size.
const SAMPLE: u32 = 10;

/// Event bytes published by a `len`-element batch on a fresh vault, as metered by
/// the host. Only the CPU/memory budget is raised; the event ceiling, which is
/// what this measures, is a separate limit `reset_limits` does not touch.
fn event_bytes_of_a_batch(len: u32) -> u32 {
    let v = setup();
    v.env
        .cost_estimate()
        .budget()
        .reset_limits(2_000_000_000, 2_000_000_000);
    v.client.claim_batch(&batch(&v, len), &0);
    v.env.cost_estimate().resources().contract_events_size_bytes
}

/// `MAX_BATCH_SIZE` and the network's event ceiling disagree, and the record of
/// that belongs in the suite: each element publishes its own `refund_event` plus
/// the token's `transfer` — 588 bytes per claim at these sizes — so a batch the
/// contract happily admits is several times the 16 KiB one invocation may emit.
/// The cap is therefore a ceiling on the argument, not a batch anyone can settle
/// in a single transaction.
///
/// Extrapolated from two measured sizes instead of running `CAP` claims: exceeding
/// the ceiling aborts the host with a panic rather than a contract error, so a
/// direct attempt would take the test binary down with it. If the per-claim event
/// is ever slimmed — or published once per batch — the first assert fails and the
/// fix is to re-measure, not to widen `MAINNET_EVENT_BYTES`.
#[test]
fn the_mainnet_event_ceiling_binds_well_before_the_contract_cap() {
    let single = event_bytes_of_a_batch(1);
    let sample = event_bytes_of_a_batch(SAMPLE);
    assert!(
        sample > single,
        "each claim has to add its own events, got {single} then {sample}"
    );

    let per_claim = (sample - single) / (SAMPLE - 1);
    let largest_batch_that_fits = MAINNET_EVENT_BYTES / per_claim;

    assert!(
        per_claim * CAP > MAINNET_EVENT_BYTES,
        "a full batch is ~{} event bytes against a {MAINNET_EVENT_BYTES} ceiling",
        per_claim * CAP,
    );
    assert!(
        largest_batch_that_fits < CAP,
        "the guard admits {CAP} claims but the network fits ~{largest_batch_that_fits}",
    );
}
