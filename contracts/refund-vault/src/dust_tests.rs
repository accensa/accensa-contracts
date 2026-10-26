//! Dust sweep tests (issue #427).

use super::*;
use crate::dust::{DEFAULT_DUST_THRESHOLD, DUST_SWEEP_DELAY_LEDGERS};
use crate::test_helpers::vault_init;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;
const WINDOW: u32 = 100;
const PAYMENT: i128 = 10_000;

struct Setup {
    env: Env,
    client: RefundVaultClient<'static>,
    token: TokenClient<'static>,
    vault: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();
    // Sweeping needs > 90 days of ledgers to pass; keep every entry live for
    // the whole test instead of archiving it.
    env.ledger().with_mut(|l| {
        l.min_persistent_entry_ttl = 4_000_000;
        l.max_entry_ttl = 10_000_000;
    });

    let merchant = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault = env.register(RefundVault, (vault_init(&env, &merchant, &token, WINDOW),));
    let client = RefundVaultClient::new(&env, &vault);
    client.deposit(&merchant, &FLOAT, &None);

    Setup {
        token: TokenClient::new(&env, &token),
        env,
        client,
        vault,
    }
}

fn payment_ref(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

/// Refund `amount` of a `PAYMENT`-sized payment made at the current ledger.
fn refund(s: &Setup, pref: &BytesN<32>, amount: i128) {
    let merchant = s.client.get_admin();
    let nonce = s.client.get_user_nonce(&merchant);
    s.client.refund(
        pref,
        &Address::generate(&s.env),
        &amount,
        &s.env.ledger().sequence(),
        &PAYMENT,
        &None,
        &nonce,
    );
}

/// Advance to the first ledger at which a record paid (and last refunded)
/// at `paid_at` becomes sweepable.
fn advance_past_delay(env: &Env, paid_at: u32) {
    env.ledger()
        .set_sequence_number(paid_at + WINDOW + DUST_SWEEP_DELAY_LEDGERS + 1);
}

#[test]
fn default_threshold_and_treasury() {
    let s = setup();
    assert_eq!(s.client.get_dust_threshold(), DEFAULT_DUST_THRESHOLD);
    // Falls back to the fee recipient, i.e. the merchant.
    assert_eq!(s.client.get_dust_treasury(), s.client.get_admin());
}

#[test]
fn sweep_moves_dust_to_treasury_and_reclaims_record() {
    let s = setup();
    let treasury = Address::generate(&s.env);
    s.client.set_dust_config(&100, &treasury);

    let pref = payment_ref(&s.env, 1);
    let paid_at = s.env.ledger().sequence();
    refund(&s, &pref, PAYMENT - 42);

    advance_past_delay(&s.env, paid_at);
    let before = s.token.balance(&s.vault);
    assert_eq!(s.client.sweep_dust(&pref), 42);

    assert_eq!(s.token.balance(&treasury), 42);
    assert_eq!(s.token.balance(&s.vault), before - 42);
    assert_eq!(s.client.get_refund(&pref), None);
    assert_eq!(
        s.client.try_sweep_dust(&pref),
        Err(Ok(Error::RefundNotFound))
    );
}

#[test]
fn fully_refunded_record_is_reclaimed_without_transfer() {
    let s = setup();
    let treasury = Address::generate(&s.env);
    s.client.set_dust_config(&100, &treasury);

    let pref = payment_ref(&s.env, 2);
    let paid_at = s.env.ledger().sequence();
    refund(&s, &pref, PAYMENT);

    advance_past_delay(&s.env, paid_at);
    assert_eq!(s.client.sweep_dust(&pref), 0);
    assert_eq!(s.token.balance(&treasury), 0);
    assert_eq!(s.client.get_refund(&pref), None);
}

#[test]
fn non_dust_residual_is_rejected() {
    let s = setup();
    let pref = payment_ref(&s.env, 3);
    let paid_at = s.env.ledger().sequence();
    // Residual equals the threshold: not strictly below it.
    refund(&s, &pref, PAYMENT - DEFAULT_DUST_THRESHOLD);

    advance_past_delay(&s.env, paid_at);
    assert_eq!(
        s.client.try_sweep_dust(&pref),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(s.client.get_refund(&pref).is_some());
}

#[test]
fn sweep_before_ninety_days_is_rejected() {
    let s = setup();
    let pref = payment_ref(&s.env, 4);
    let paid_at = s.env.ledger().sequence();
    refund(&s, &pref, PAYMENT - 1);

    // Exactly at the boundary: closed for 90 days, not *more* than 90 days.
    s.env
        .ledger()
        .set_sequence_number(paid_at + WINDOW + DUST_SWEEP_DELAY_LEDGERS);
    assert_eq!(
        s.client.try_sweep_dust(&pref),
        Err(Ok(Error::TimelockNotExpired))
    );
}

#[test]
fn later_refund_restarts_the_delay() {
    let s = setup();
    let pref = payment_ref(&s.env, 5);
    let paid_at = s.env.ledger().sequence();
    refund(&s, &pref, PAYMENT - 500);

    // A partial refund processed after the window closes (e.g. with no
    // time policy gate) moves the closing point to that refund.
    let later = paid_at + WINDOW + 1_000;
    s.env.ledger().set_sequence_number(later);
    s.env.as_contract(&s.vault, || {
        let mut record: RefundRecord = s
            .env
            .storage()
            .persistent()
            .get(&DataKey::RefundV2(pref.clone()))
            .unwrap();
        record.amount_refunded = PAYMENT - 1;
        record.ledger = later;
        s.env
            .storage()
            .persistent()
            .set(&DataKey::RefundV2(pref.clone()), &record);
    });

    advance_past_delay(&s.env, paid_at);
    assert_eq!(
        s.client.try_sweep_dust(&pref),
        Err(Ok(Error::TimelockNotExpired))
    );
    s.env
        .ledger()
        .set_sequence_number(later + DUST_SWEEP_DELAY_LEDGERS + 1);
    assert_eq!(s.client.sweep_dust(&pref), 1);
}

#[test]
fn unknown_payment_ref_is_rejected() {
    let s = setup();
    assert_eq!(
        s.client.try_sweep_dust(&payment_ref(&s.env, 9)),
        Err(Ok(Error::RefundNotFound))
    );
}

#[test]
fn invalid_dust_config_is_rejected() {
    let s = setup();
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.client.try_set_dust_config(&0, &treasury),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_set_dust_config(&100, &s.vault),
        Err(Ok(Error::SelfTransfer))
    );
}

#[test]
#[should_panic]
fn sweep_requires_merchant_auth() {
    let s = setup();
    let pref = payment_ref(&s.env, 6);
    let paid_at = s.env.ledger().sequence();
    refund(&s, &pref, PAYMENT - 1);
    advance_past_delay(&s.env, paid_at);

    s.env.set_auths(&[]);
    s.client.sweep_dust(&pref);
}

// ── Dust event XDR shapes (issue #637) ─────────────────────────────────────
//
// `src/dust.rs` publishes `DustSweptEvent` on every successful `sweep_dust`;
// the existing cases above prove the state transitions by balances and
// `Option` reads, but never touched `env.events()`, leaving the trail an
// off-chain reconciler follows unasserted. These cases pin the exact
// `("dust_swept_event", payment_ref)` topic tuple and the amount/treasury
// data — including the reclaim branch, where `amount == 0` and the event is
// the only evidence anything happened — the way
// `state-channel/src/hashlock_test.rs` compares event XDR.

/// Exact-XDR assertions for `DustSweptEvent` (issue #637).
mod dust_event_shapes {
    use super::*;
    use crate::dust::DustSweptEvent;
    use soroban_sdk::{events::Event as _, testutils::Events, xdr::ContractEvent};

    /// Assert the last invocation published exactly one event on `vault`,
    /// and that it serialises to `expected` (full XDR: topics and data).
    fn assert_only_event(env: &Env, vault: &Address, expected: ContractEvent) {
        let events = env.events().all().filter_by_contract(vault);
        assert_eq!(events.events().len(), 1, "expected exactly one event");
        assert_eq!(&events.events()[0], &expected);
    }

    /// Assert the last invocation published no event at all on `vault`.
    fn assert_no_event(env: &Env, vault: &Address) {
        let events = env.events().all().filter_by_contract(vault);
        assert_eq!(events.events().len(), 0, "expected no event");
    }

    /// The transfer branch: a partial-dust sweep moves the residual to the
    /// treasury and reports exactly that amount and that treasury.
    #[test]
    fn partial_sweep_event_carries_the_amount_and_treasury() {
        let s = setup();
        let treasury = Address::generate(&s.env);
        s.client.set_dust_config(&100, &treasury);

        let pref = payment_ref(&s.env, 1);
        let paid_at = s.env.ledger().sequence();
        refund(&s, &pref, PAYMENT - 42);

        advance_past_delay(&s.env, paid_at);
        assert_eq!(s.client.sweep_dust(&pref), 42);

        // `events().all()` covers the last invocation only: the sweep itself.
        assert_only_event(
            &s.env,
            &s.vault,
            DustSweptEvent {
                payment_ref: pref.clone(),
                amount: 42,
                treasury: treasury.clone(),
            }
            .to_xdr(&s.env, &s.vault),
        );
        // The event's amount is what actually moved.
        assert_eq!(s.token.balance(&treasury), 42);
    }

    /// The reclaim branch: a fully refunded record transfers nothing, but
    /// still emits the event with `amount == 0` — a silent no-event sweep
    /// would leave the deletion unevidenced, and must fail this test.
    #[test]
    fn reclaimed_record_emits_the_event_with_a_zero_amount() {
        let s = setup();
        let treasury = Address::generate(&s.env);
        s.client.set_dust_config(&100, &treasury);

        let pref = payment_ref(&s.env, 2);
        let paid_at = s.env.ledger().sequence();
        refund(&s, &pref, PAYMENT);

        advance_past_delay(&s.env, paid_at);
        assert_eq!(s.client.sweep_dust(&pref), 0);

        assert_only_event(
            &s.env,
            &s.vault,
            DustSweptEvent {
                payment_ref: pref.clone(),
                amount: 0,
                treasury: treasury.clone(),
            }
            .to_xdr(&s.env, &s.vault),
        );
        // Nothing was transferred, but the record was still reclaimed.
        assert_eq!(s.token.balance(&treasury), 0);
        assert_eq!(s.client.get_refund(&pref), None);
    }

    /// Every rejection path is silent: a non-dust residual, a sweep inside
    /// the 90-day delay, and an unknown record each emit no dust event, so
    /// the trail never reports a sweep that did not happen.
    #[test]
    fn rejected_sweeps_emit_no_dust_event() {
        let s = setup();
        let treasury = Address::generate(&s.env);
        s.client.set_dust_config(&100, &treasury);

        // Residual not strictly below the threshold.
        let pref = payment_ref(&s.env, 3);
        let paid_at = s.env.ledger().sequence();
        refund(&s, &pref, PAYMENT - DEFAULT_DUST_THRESHOLD);
        advance_past_delay(&s.env, paid_at);
        assert_eq!(
            s.client.try_sweep_dust(&pref),
            Err(Ok(Error::InvalidAmount))
        );
        assert_no_event(&s.env, &s.vault);

        // Sweep attempted before the delay has elapsed.
        let pref = payment_ref(&s.env, 4);
        let paid_at = s.env.ledger().sequence();
        refund(&s, &pref, PAYMENT - 1);
        s.env
            .ledger()
            .set_sequence_number(paid_at + WINDOW + DUST_SWEEP_DELAY_LEDGERS);
        assert_eq!(
            s.client.try_sweep_dust(&pref),
            Err(Ok(Error::TimelockNotExpired))
        );
        assert_no_event(&s.env, &s.vault);

        // No record under that reference at all.
        assert_eq!(
            s.client.try_sweep_dust(&payment_ref(&s.env, 9)),
            Err(Ok(Error::RefundNotFound))
        );
        assert_no_event(&s.env, &s.vault);
    }
}
