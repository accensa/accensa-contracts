//! Streaming micro-disbursement tests (issue #410).

use super::*;
use crate::streaming::{streamed_amount, DisbursementStream};
use crate::test_helpers::vault_init;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, IntoVal,
};

const FLOAT: i128 = 1_000_000;
const BUYER_FUNDS: i128 = 1_000_000;
const WINDOW: u32 = 1_000;
const START: u32 = 100;
const STOP: u32 = 200;
const RATE: i128 = 10;
const DEPOSIT: i128 = 1_000; // (STOP - START) * RATE

struct Setup {
    env: Env,
    client: RefundVaultClient<'static>,
    token: TokenClient<'static>,
    vault: Address,
    merchant: Address,
    buyer: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(START - 10);

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&buyer, &BUYER_FUNDS);

    let vault = env.register(RefundVault, (vault_init(&env, &merchant, &token, WINDOW),));
    let client = RefundVaultClient::new(&env, &vault);

    Setup {
        token: TokenClient::new(&env, &token),
        env,
        client,
        vault,
        merchant,
        buyer,
    }
}

fn open(s: &Setup) -> u64 {
    s.client
        .create_stream(&s.buyer, &START, &STOP, &RATE, &DEPOSIT)
}

/// Mint `amount` to the merchant and deposit it as refund float.
fn fund_float(s: &Setup, amount: i128) {
    StellarAssetClient::new(&s.env, &s.token.address).mint(&s.merchant, &amount);
    s.client.deposit(&s.merchant, &amount);
}

/// Total stream principal the vault holds in escrow.
fn escrow(s: &Setup) -> i128 {
    s.env.as_contract(&s.vault, || streaming::escrowed(&s.env))
}

fn at(s: &Setup, ledger: u32) {
    s.env.ledger().set_sequence_number(ledger);
}

// ── Creation ─────────────────────────────────────────────────────────────

#[test]
fn create_stream_escrows_deposit_and_stores_params() {
    let s = setup();
    let id = open(&s);

    assert_eq!(id, 0);
    assert_eq!(
        s.client.get_stream(&id),
        Some(DisbursementStream {
            buyer: s.buyer.clone(),
            merchant: s.merchant.clone(),
            start_ledger: START,
            stop_ledger: STOP,
            rate_per_ledger: RATE,
            deposit: DEPOSIT,
            claimed: 0,
            paused_at: None,
        })
    );
    assert_eq!(s.token.balance(&s.vault), DEPOSIT);
    assert_eq!(s.token.balance(&s.buyer), BUYER_FUNDS - DEPOSIT);
    assert_eq!(escrow(&s), DEPOSIT);

    // Ids are sequential.
    assert_eq!(open(&s), 1);
    assert_eq!(escrow(&s), 2 * DEPOSIT);
}

#[test]
fn create_stream_requires_buyer_auth() {
    let s = setup();
    open(&s);
    let auths = s.env.auths();
    assert_eq!(auths[0].0, s.buyer);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            s.vault.clone(),
            Symbol::new(&s.env, "create_stream"),
            (s.buyer.clone(), START, STOP, RATE, DEPOSIT).into_val(&s.env),
        ))
    );
}

#[test]
fn create_stream_rejects_invalid_params() {
    let s = setup();
    let now = s.env.ledger().sequence();
    let cases = [
        (START, STOP, RATE, 0),           // zero deposit
        (START, STOP, RATE, -1),          // negative deposit
        (START, STOP, 0, DEPOSIT),        // zero rate
        (START, START, RATE, DEPOSIT),    // empty schedule
        (STOP, START, RATE, DEPOSIT),     // reversed schedule
        (now - 1, STOP, RATE, DEPOSIT),   // backdated start
        (START, STOP, RATE, DEPOSIT + 1), // deposit exceeds capacity
    ];
    for (start, stop, rate, deposit) in cases {
        assert_eq!(
            s.client
                .try_create_stream(&s.buyer, &start, &stop, &rate, &deposit),
            Err(Ok(Error::InvalidAmount))
        );
    }
    assert_eq!(escrow(&s), 0);
}

#[test]
fn create_stream_rejected_while_vault_paused() {
    let s = setup();
    s.client.pause();
    assert_eq!(
        s.client
            .try_create_stream(&s.buyer, &START, &STOP, &RATE, &DEPOSIT),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn create_stream_rejects_vault_as_buyer() {
    let s = setup();
    assert_eq!(
        s.client
            .try_create_stream(&s.vault, &START, &STOP, &RATE, &DEPOSIT),
        Err(Ok(Error::SelfTransfer))
    );
}

// ── Claiming and boundary ledgers ────────────────────────────────────────

#[test]
fn nothing_claimable_before_or_at_start_ledger() {
    let s = setup();
    let id = open(&s);

    assert_eq!(s.client.get_stream_claimable(&id), 0);
    at(&s, START);
    assert_eq!(s.client.get_stream_claimable(&id), 0);
    assert_eq!(
        s.client.try_claim_stream(&id),
        Err(Ok(Error::NothingToWithdraw))
    );

    at(&s, START + 1);
    assert_eq!(s.client.get_stream_claimable(&id), RATE);
}

#[test]
fn claim_pays_linear_accrual_to_merchant() {
    let s = setup();
    let id = open(&s);

    at(&s, START + 25);
    assert_eq!(s.client.claim_stream(&id), 25 * RATE);
    assert_eq!(s.token.balance(&s.merchant), 25 * RATE);

    // A second claim in the same ledger has nothing left.
    assert_eq!(
        s.client.try_claim_stream(&id),
        Err(Ok(Error::NothingToWithdraw))
    );

    at(&s, START + 40);
    assert_eq!(s.client.claim_stream(&id), 15 * RATE);
    assert_eq!(s.token.balance(&s.merchant), 40 * RATE);
    assert_eq!(s.client.get_stream(&id).unwrap().claimed, 40 * RATE);
    assert_eq!(escrow(&s), DEPOSIT - 40 * RATE);
}

#[test]
fn claim_is_permissionless() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 10);
    s.client.claim_stream(&id);
    // No party had to authorize the claim.
    assert!(s.env.auths().is_empty());
    assert_eq!(s.token.balance(&s.merchant), 10 * RATE);
}

#[test]
fn claim_one_ledger_before_stop_leaves_stream_open() {
    let s = setup();
    let id = open(&s);
    at(&s, STOP - 1);
    assert_eq!(s.client.claim_stream(&id), DEPOSIT - RATE);
    assert!(s.client.get_stream(&id).is_some());
    assert_eq!(escrow(&s), RATE);
}

#[test]
fn claim_at_stop_ledger_closes_stream() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 30);
    s.client.claim_stream(&id);

    at(&s, STOP);
    assert_eq!(s.client.claim_stream(&id), DEPOSIT - 30 * RATE);
    assert_eq!(s.token.balance(&s.merchant), DEPOSIT);
    assert_eq!(s.client.get_stream(&id), None);
    assert_eq!(escrow(&s), 0);
    assert_eq!(s.token.balance(&s.vault), 0);
    assert_eq!(
        s.client.try_claim_stream(&id),
        Err(Ok(Error::StreamNotFound))
    );
}

#[test]
fn claim_long_after_stop_pays_only_the_deposit() {
    let s = setup();
    let id = open(&s);
    at(&s, STOP + 10_000);
    assert_eq!(s.client.get_stream_claimable(&id), DEPOSIT);
    assert_eq!(s.client.claim_stream(&id), DEPOSIT);
    assert_eq!(s.client.get_stream(&id), None);
}

#[test]
fn fractional_final_ledger_is_clamped_to_deposit() {
    // 3 per ledger over 4 ledgers could stream 12, but only 10 was
    // committed: the last ledger streams the 1-unit remainder.
    let s = setup();
    let id = s
        .client
        .create_stream(&s.buyer, &START, &(START + 4), &3, &10);

    at(&s, START + 1);
    assert_eq!(s.client.get_stream_claimable(&id), 3);
    at(&s, START + 3);
    assert_eq!(s.client.claim_stream(&id), 9);
    // Deposit is exhausted before the stop ledger; the stream stays open
    // until the final unit is claimed.
    at(&s, START + 4);
    assert_eq!(s.client.claim_stream(&id), 1);
    assert_eq!(s.client.get_stream(&id), None);
    assert_eq!(s.token.balance(&s.merchant), 10);
}

#[test]
fn deposit_exhausted_before_stop_closes_on_claim() {
    // Capacity 6 * 3 = 18 >= 13: the deposit is fully streamed after 5
    // ledgers (15 clamped to 13), one ledger before the stop ledger.
    let s = setup();
    let id = s
        .client
        .create_stream(&s.buyer, &START, &(START + 6), &3, &13);
    at(&s, START + 5);
    assert_eq!(s.client.claim_stream(&id), 13);
    assert_eq!(s.client.get_stream(&id), None);
}

#[test]
fn claim_rejected_while_vault_paused() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 10);
    s.client.pause();
    assert_eq!(s.client.try_claim_stream(&id), Err(Ok(Error::Paused)));
}

#[test]
fn unknown_stream_errors() {
    let s = setup();
    assert_eq!(s.client.get_stream(&7), None);
    assert_eq!(
        s.client.try_get_stream_claimable(&7),
        Err(Ok(Error::StreamNotFound))
    );
    assert_eq!(
        s.client.try_claim_stream(&7),
        Err(Ok(Error::StreamNotFound))
    );
    assert_eq!(
        s.client.try_pause_stream(&7),
        Err(Ok(Error::StreamNotFound))
    );
    assert_eq!(
        s.client.try_resume_stream(&7),
        Err(Ok(Error::StreamNotFound))
    );
    assert_eq!(
        s.client.try_cancel_stream(&7),
        Err(Ok(Error::StreamNotFound))
    );
}

// ── Pause / resume ───────────────────────────────────────────────────────

#[test]
fn pause_freezes_accrual_and_resume_shifts_schedule() {
    let s = setup();
    let id = open(&s);

    at(&s, START + 20);
    s.client.pause_stream(&id);
    at(&s, START + 70);
    assert_eq!(s.client.get_stream_claimable(&id), 20 * RATE);

    s.client.resume_stream(&id);
    let stream = s.client.get_stream(&id).unwrap();
    assert_eq!(stream.paused_at, None);
    assert_eq!(stream.start_ledger, START + 50);
    assert_eq!(stream.stop_ledger, STOP + 50);
    assert_eq!(s.client.get_stream_claimable(&id), 20 * RATE);

    at(&s, START + 80);
    assert_eq!(s.client.get_stream_claimable(&id), 30 * RATE);

    // The old stop ledger no longer closes the stream; the shifted one does.
    at(&s, STOP);
    assert_eq!(s.client.claim_stream(&id), 50 * RATE);
    assert!(s.client.get_stream(&id).is_some());
    at(&s, STOP + 50);
    assert_eq!(s.client.claim_stream(&id), DEPOSIT - 50 * RATE);
    assert_eq!(s.client.get_stream(&id), None);
}

#[test]
fn claim_while_paused_pays_only_pre_pause_accrual() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 10);
    s.client.pause_stream(&id);
    at(&s, STOP + 100);
    assert_eq!(s.client.claim_stream(&id), 10 * RATE);
    assert_eq!(
        s.client.try_claim_stream(&id),
        Err(Ok(Error::NothingToWithdraw))
    );
}

#[test]
fn pause_before_start_and_resume_after_start() {
    let s = setup();
    let id = open(&s);
    s.client.pause_stream(&id); // at START - 10

    at(&s, START + 30);
    assert_eq!(s.client.get_stream_claimable(&id), 0);
    s.client.resume_stream(&id);

    let stream = s.client.get_stream(&id).unwrap();
    assert_eq!(stream.start_ledger, START + 30);
    assert_eq!(stream.stop_ledger, STOP + 30);
}

#[test]
fn pause_and_resume_before_start_keep_schedule() {
    let s = setup();
    let id = open(&s);
    s.client.pause_stream(&id);
    at(&s, START - 1);
    s.client.resume_stream(&id);
    let stream = s.client.get_stream(&id).unwrap();
    assert_eq!((stream.start_ledger, stream.stop_ledger), (START, STOP));
}

#[test]
fn pause_resume_state_errors() {
    let s = setup();
    let id = open(&s);

    assert_eq!(
        s.client.try_resume_stream(&id),
        Err(Ok(Error::StreamNotActive))
    );
    s.client.pause_stream(&id);
    assert_eq!(
        s.client.try_pause_stream(&id),
        Err(Ok(Error::StreamNotActive))
    );
    s.client.resume_stream(&id);

    // Nothing left to pause once the schedule has ended.
    at(&s, STOP);
    assert_eq!(
        s.client.try_pause_stream(&id),
        Err(Ok(Error::StreamNotActive))
    );
}

#[test]
fn pause_requires_buyer_auth() {
    let s = setup();
    let id = open(&s);
    s.client.pause_stream(&id);
    assert_eq!(s.env.auths()[0].0, s.buyer);
    s.client.resume_stream(&id);
    assert_eq!(s.env.auths()[0].0, s.buyer);
}

// ── Cancellation ─────────────────────────────────────────────────────────

#[test]
fn cancel_mid_stream_splits_between_merchant_and_buyer() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 10);
    s.client.claim_stream(&id);

    at(&s, START + 35);
    assert_eq!(
        s.client.cancel_stream(&id),
        (25 * RATE, DEPOSIT - 35 * RATE)
    );
    assert_eq!(s.env.auths()[0].0, s.buyer);
    assert_eq!(s.token.balance(&s.merchant), 35 * RATE);
    assert_eq!(s.token.balance(&s.buyer), BUYER_FUNDS - 35 * RATE);
    assert_eq!(s.token.balance(&s.vault), 0);
    assert_eq!(s.client.get_stream(&id), None);
    assert_eq!(escrow(&s), 0);
}

#[test]
fn cancel_before_start_refunds_everything() {
    let s = setup();
    let id = open(&s);
    assert_eq!(s.client.cancel_stream(&id), (0, DEPOSIT));
    assert_eq!(s.token.balance(&s.buyer), BUYER_FUNDS);
}

#[test]
fn cancel_after_stop_pays_merchant_everything() {
    let s = setup();
    let id = open(&s);
    at(&s, STOP + 1);
    assert_eq!(s.client.cancel_stream(&id), (DEPOSIT, 0));
    assert_eq!(s.token.balance(&s.merchant), DEPOSIT);
}

#[test]
fn cancel_while_paused_uses_pause_ledger() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 40);
    s.client.pause_stream(&id);
    at(&s, STOP);
    assert_eq!(
        s.client.cancel_stream(&id),
        (40 * RATE, DEPOSIT - 40 * RATE)
    );
}

#[test]
fn cancel_allowed_while_vault_paused() {
    let s = setup();
    let id = open(&s);
    at(&s, START + 5);
    s.client.pause();
    assert_eq!(s.client.cancel_stream(&id), (5 * RATE, DEPOSIT - 5 * RATE));
}

#[test]
fn cancel_fractional_split_conserves_deposit() {
    let s = setup();
    let id = s
        .client
        .create_stream(&s.buyer, &START, &(START + 4), &3, &10);
    at(&s, START + 2);
    let (merchant_amount, buyer_amount) = s.client.cancel_stream(&id);
    assert_eq!((merchant_amount, buyer_amount), (6, 4));
    assert_eq!(merchant_amount + buyer_amount, 10);
}

// ── Escrow isolation from the refund float ───────────────────────────────

#[test]
fn merchant_cannot_withdraw_stream_escrow() {
    let s = setup();
    fund_float(&s, FLOAT);
    open(&s);

    assert_eq!(
        s.client.try_withdraw(&(FLOAT + 1), &s.merchant),
        Err(Ok(Error::InsufficientFloat))
    );
    s.client.withdraw(&FLOAT, &s.merchant);
    assert_eq!(s.token.balance(&s.vault), DEPOSIT);
}

#[test]
fn refunds_cannot_spend_stream_escrow() {
    let s = setup();
    fund_float(&s, 100);
    open(&s);

    let pref = BytesN::from_array(&s.env, &[1; 32]);
    let nonce = s.client.get_user_nonce(&s.merchant);
    assert_eq!(
        s.client.try_refund(
            &pref,
            &Address::generate(&s.env),
            &101,
            &s.env.ledger().sequence(),
            &1_000,
            &None,
            &nonce,
        ),
        Err(Ok(Error::InsufficientFloat))
    );
    s.client.refund(
        &pref,
        &Address::generate(&s.env),
        &100,
        &s.env.ledger().sequence(),
        &1_000,
        &None,
        &nonce,
    );

    // The stream still pays out in full.
    at(&s, STOP);
    assert_eq!(s.client.claim_stream(&0), DEPOSIT);
}

// ── Property tests ───────────────────────────────────────────────────────

fn stream(start: u32, stop: u32, rate: i128, deposit: i128) -> DisbursementStream {
    let env = Env::default();
    DisbursementStream {
        buyer: Address::generate(&env),
        merchant: Address::generate(&env),
        start_ledger: start,
        stop_ledger: stop,
        rate_per_ledger: rate,
        deposit,
        claimed: 0,
        paused_at: None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `streamed_amount` equals `min(deposit, (clock - start) * rate)` with
    /// the clock clamped to the schedule, is monotonic in the ledger, and
    /// never exceeds the deposit.
    #[test]
    fn streamed_amount_matches_formula(
        start in 0u32..1_000_000,
        duration in 1u32..1_000_000,
        rate in 1i128..1_000_000_000,
        deposit_frac in 1u32..=100,
        now in 0u32..3_000_000,
    ) {
        let stop = start + duration;
        let deposit = (duration as i128 * rate * deposit_frac as i128 / 100).max(1);
        let s = stream(start, stop, rate, deposit);

        let clock = now.clamp(start, stop);
        let expected = ((clock - start) as i128 * rate).min(deposit);
        let got = streamed_amount(&s, now);
        prop_assert_eq!(got, expected);
        prop_assert!(got >= 0 && got <= deposit);
        prop_assert!(streamed_amount(&s, now.saturating_add(1)) >= got);
        prop_assert_eq!(streamed_amount(&s, stop), deposit);
    }
}
