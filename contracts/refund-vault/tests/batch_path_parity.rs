//! #973 — the two batch paths must disagree about atomicity and nothing else.
//!
//! `claim_batch` and `process_batch` route every element through the same
//! `claim_single`, then consume the verdict differently: `claim_batch` does
//! `claim_single(..)?`, so the first rejection reverts the whole invocation,
//! while `process_batch` records `claim_single(..).is_ok()`, so a rejection
//! becomes `false` and the rest of the batch runs. That relationship — the one
//! thing clients integrate against — lived only in the doc comments. Each path
//! had its own tests, against its own vectors, so a rule added to one path (a
//! new ceiling check, a reordered gate) could not be caught by either suite.
//!
//! Here one table of rows is built once and fed to both paths:
//!
//! | row | verdict |
//! |-----|---------|
//! | inside every gate | admitted |
//! | `amount <= 0` | `InvalidAmount` |
//! | recipient is the vault | `SelfTransfer` |
//! | amount above the payment | `ExceedsPayment` |
//! | outside the refund window | `WindowExpired` |
//!
//! For the rows `claim_single` rejects itself — an inline `return Err(..)` —
//! the boolean from `process_batch` must equal the `Ok`/`Err` from a
//! single-element `claim_batch` on a fresh vault, *including the error
//! variant*: a path that swallowed a rejection, or started aborting where it
//! should record `false`, fails.
//!
//! The window row does not behave that way, and the reason is worth pinning.
//! `claim_single` delegates the time gate to the policy *contract*, so its
//! rejection is a nested contract error: it aborts the host frame and never
//! reaches the Rust `.is_ok()`. `process_batch` therefore reverts the batch —
//! the element admitted before it, its payout, its storage record and the
//! nonce bump all unwound — exactly like `claim_batch`. Both paths still agree
//! on the code, so parity holds; what does not hold is `process_batch`'s
//! promise to "record `false` and continue" for a rejection that arrives from
//! a policy contract. `a_delegated_gate_rejection_reverts_process_batch_too`
//! pins what actually happens rather than what the comment says, so a fix on
//! either side cannot be silent.
//!
//! The deliberately repeated ref is the row whose verdict depends on in-batch
//! state, so it gets its own pair: `process_batch` answers `[true, false]` and
//! keeps the first refund (that rejection is inline), while `claim_batch`
//! reverts the pair and keeps nothing.
//!
//! Every probe passes the merchant's current `get_user_nonce`, and the last
//! test shows why: with a stale nonce both paths answer `StaleState` for every
//! row, which would let a nonce rejection masquerade as a claim rejection.

use accensa_common::{Error, VaultInit};
use refund_policy_time::TimePolicy;
use refund_vault::{RefundClaim, RefundParam, RefundVault, RefundVaultClient};
use soroban_sdk::token::{Client as TokenClient, StellarAssetClient};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Address, BytesN, Env, Vec};

const FLOAT: i128 = 1_000_000;

/// The vault's refund window, in ledgers.
const WINDOW: u32 = 100;

/// The ledger every claim in the table is made at.
const NOW: u32 = 1_000;

/// One row: the inputs both paths are given, plus the verdict they must agree
/// on. `Ok(())` means the element is admitted, so `process_batch` records
/// `true`.
struct Case {
    name: &'static str,
    /// Distinguishes the payment ref; unique except in the repeated-ref pair.
    ref_byte: u8,
    amount: i128,
    payment_amount: i128,
    paid_at_ledger: u32,
    /// Send the payout to the vault itself, which is the self-transfer rule.
    to_vault: bool,
    /// Whether `claim_single` reaches this verdict by calling the policy
    /// contract rather than through one of its own `return Err(..)`
    /// statements. Such a rejection aborts the invocation on both paths.
    delegated_gate: bool,
    expected: Result<(), Error>,
}

/// The rows whose verdicts cannot depend on what an earlier element of the
/// same batch did.
const ROWS: &[Case] = &[
    Case {
        name: "inside every gate",
        ref_byte: 1,
        amount: 100,
        payment_amount: 100,
        paid_at_ledger: NOW - 1,
        to_vault: false,
        delegated_gate: false,
        expected: Ok(()),
    },
    Case {
        name: "amount is not positive",
        ref_byte: 2,
        amount: 0,
        payment_amount: 100,
        paid_at_ledger: NOW - 1,
        to_vault: false,
        delegated_gate: false,
        expected: Err(Error::InvalidAmount),
    },
    Case {
        name: "recipient is the vault",
        ref_byte: 3,
        amount: 100,
        payment_amount: 100,
        paid_at_ledger: NOW - 1,
        to_vault: true,
        delegated_gate: false,
        expected: Err(Error::SelfTransfer),
    },
    Case {
        name: "amount exceeds the payment",
        ref_byte: 4,
        amount: 200,
        payment_amount: 100,
        paid_at_ledger: NOW - 1,
        to_vault: false,
        delegated_gate: false,
        expected: Err(Error::ExceedsPayment),
    },
    Case {
        name: "outside the refund window",
        ref_byte: 5,
        amount: 100,
        payment_amount: 100,
        paid_at_ledger: NOW - WINDOW - 1,
        to_vault: false,
        delegated_gate: true,
        expected: Err(Error::WindowExpired),
    },
];

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    buyer: Address,
    token: Address,
}

/// A funded vault on a fresh test host, with the time policy wired so the
/// window row reaches the gate instead of a configuration error.
fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: Some(env.register(TimePolicy, ())),
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: WINDOW,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &FLOAT, &None);
    env.ledger().set_sequence_number(NOW);

    Vault {
        env,
        contract,
        client,
        merchant,
        buyer,
        token,
    }
}

fn payment_ref(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

/// Both shapes for one row, from one set of fields, so the inputs the two
/// paths are given cannot drift apart.
fn shapes(v: &Vault, case: &Case) -> (RefundParam, RefundClaim) {
    let recipient = if case.to_vault {
        v.contract.clone()
    } else {
        v.buyer.clone()
    };
    let payment_ref = payment_ref(&v.env, case.ref_byte);
    (
        RefundParam {
            payment_ref: payment_ref.clone(),
            recipient: recipient.clone(),
            amount: case.amount,
            paid_at_ledger: case.paid_at_ledger,
            payment_amount: case.payment_amount,
            vdf_proof: None,
        },
        RefundClaim {
            payment_ref,
            recipient,
            amount: case.amount,
            paid_at_ledger: case.paid_at_ledger,
            payment_amount: case.payment_amount,
            vdf_proof: None,
        },
    )
}

/// The one-argument difference between the two paths: the element list. Both
/// vectors are built from the same rows, in the same order.
fn param_vec<'c, I>(v: &Vault, cases: I) -> Vec<RefundParam>
where
    I: IntoIterator<Item = &'c Case>,
{
    let mut out = Vec::new(&v.env);
    for case in cases {
        out.push_back(shapes(v, case).0);
    }
    out
}

fn claim_vec<'c, I>(v: &Vault, cases: I) -> Vec<RefundClaim>
where
    I: IntoIterator<Item = &'c Case>,
{
    let mut out = Vec::new(&v.env);
    for case in cases {
        out.push_back(shapes(v, case).1);
    }
    out
}

/// The generated client answers a `Result`-returning contract call three ways:
/// a value, `Err(Ok(code))` for a typed refusal from the contract, and
/// otherwise a host failure or an undecodable return. Only the first two are
/// verdicts, so these two helpers report the rest as themselves rather than
/// letting a panic be read as a rejection.
fn claim_batch_on(v: &Vault, claims: Vec<RefundClaim>, nonce: u64) -> Result<(), Error> {
    match v.client.try_claim_batch(&claims, &nonce) {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        Ok(Err(conv)) => panic!("`claim_batch` returned an undecodable value: {conv:?}"),
        Err(Err(host)) => panic!("`claim_batch` failed as a host error: {host:?}"),
    }
}

fn process_batch_on(v: &Vault, params: Vec<RefundParam>, nonce: u64) -> Result<Vec<bool>, Error> {
    match v.client.try_process_batch(&params, &nonce) {
        Ok(Ok(booleans)) => Ok(booleans),
        Err(Ok(error)) => Err(error),
        Ok(Err(conv)) => panic!("`process_batch` returned an undecodable value: {conv:?}"),
        Err(Err(host)) => panic!("`process_batch` failed as a host error: {host:?}"),
    }
}

/// `claim_batch` over `cases` on a vault that has seen nothing else, with
/// `nonce_offset` added to the merchant's current nonce so the same probe can
/// be run honestly (offset 0) or with a stale counter (offset 1).
fn claim_batch_fresh<'c, I>(cases: I, nonce_offset: u64) -> Result<(), Error>
where
    I: IntoIterator<Item = &'c Case>,
{
    let v = setup();
    let nonce = v.client.get_user_nonce(&v.merchant) + nonce_offset;
    let claims = claim_vec(&v, cases);
    claim_batch_on(&v, claims, nonce)
}

/// A row by name, so a test cites the case it means instead of an index.
fn row(name: &str) -> &'static Case {
    ROWS.iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("`{name}` is not in the row table"))
}

#[test]
fn every_inline_rule_answers_the_same_way_through_both_paths() {
    let v = setup();
    let inline = ROWS.iter().filter(|case| !case.delegated_gate);
    let params = param_vec(&v, inline);
    let nonce = v.client.get_user_nonce(&v.merchant);

    // On an inline rule, a typed refusal from `process_batch` *is* the
    // mutation: the best-effort path answers with a verdict per element.
    let booleans = process_batch_on(&v, params, nonce).unwrap_or_else(|error| {
        panic!("`process_batch` aborted with {error:?} instead of recording a per-element verdict")
    });

    let mut compared = 0u32;
    for case in ROWS.iter().filter(|case| !case.delegated_gate) {
        let batch = booleans.get(compared) == Some(true);
        compared += 1;
        let solo = claim_batch_fresh([case], 0);

        assert_eq!(
            batch,
            case.expected.is_ok(),
            "`process_batch` recorded {batch} for `{}`, which should be {:?}",
            case.name,
            case.expected,
        );
        assert_eq!(
            solo, case.expected,
            "the parity row `{}`: `process_batch` said {batch}, `claim_batch` said {solo:?}",
            case.name,
        );
    }

    // The count is asserted last, so a `process_batch` that quietly dropped an
    // element fails on the count rather than on a shifted boolean.
    assert_eq!(
        compared,
        booleans.len(),
        "`process_batch` answered {} elements for {compared} inline rows",
        booleans.len(),
    );
}

/// The window row is the one the doc comments get wrong: `claim_single`
/// delegates the time gate to the policy contract, so the rejection is a
/// nested contract error that aborts the host frame before `.is_ok()` can turn
/// it into `false` — and everything already admitted in the batch goes with it.
#[test]
fn a_delegated_gate_rejection_reverts_process_batch_too() {
    let admitted = row("inside every gate");
    let window = row("outside the refund window");

    let v = setup();
    let params = param_vec(&v, [admitted, window]);
    let nonce = v.client.get_user_nonce(&v.merchant);
    assert_eq!(
        process_batch_on(&v, params, nonce),
        Err(Error::WindowExpired),
        "`process_batch` records `false` for an inline rejection; for a delegated one it aborts",
    );

    // The full extent of the abort, so this cannot be mistaken for a partial
    // failure: the element admitted *before* the gate leaves no trace.
    assert_eq!(
        v.client.get_refund(&payment_ref(&v.env, admitted.ref_byte)),
        None,
        "the admitted element's refund record must be reverted with the batch",
    );
    assert_eq!(
        TokenClient::new(&v.env, &v.token).balance(&v.buyer),
        0,
        "the admitted element's payout must be reversed",
    );
    assert_eq!(
        v.client.get_user_nonce(&v.merchant),
        nonce,
        "the nonce bump is unwound with the invocation",
    );

    // Parity still holds on the code: the atomic path answers the same thing,
    // for the reason the best-effort path now shares it.
    let v2 = setup();
    let claims = claim_vec(&v2, [admitted, window]);
    let nonce2 = v2.client.get_user_nonce(&v2.merchant);
    assert_eq!(
        claim_batch_on(&v2, claims, nonce2),
        Err(Error::WindowExpired),
        "`claim_batch` must agree with `process_batch` on the code",
    );
}

/// The row that is *meant* to differ: a repeated payment ref accumulates
/// against the ceiling inside one batch — an inline rejection, so it really
/// does become `false` — and the atomic path reverts the pair instead of
/// leaving the first refund behind.
#[test]
fn a_repeated_ref_accumulates_in_process_batch_and_reverts_in_claim_batch() {
    let first = Case {
        name: "first partial",
        ref_byte: 9,
        amount: 400,
        payment_amount: 1_000,
        paid_at_ledger: NOW - 1,
        to_vault: false,
        delegated_gate: false,
        expected: Ok(()),
    };
    let repeat = Case {
        name: "repeat past the ceiling",
        ref_byte: 9,
        amount: 700,
        payment_amount: 1_000,
        paid_at_ledger: NOW - 1,
        to_vault: false,
        // 400 already refunded against a ceiling of 1_000.
        delegated_gate: false,
        expected: Err(Error::ExceedsPayment),
    };

    // Best-effort path: the pair is `[true, false]` and the admitted element
    // keeps its effect.
    let v = setup();
    let pair = param_vec(&v, [&first, &repeat]);
    let nonce = v.client.get_user_nonce(&v.merchant);
    assert_eq!(
        process_batch_on(&v, pair, nonce),
        Ok(vec![&v.env, true, false]),
        "`process_batch` must record the rejection and keep going",
    );
    let record = v
        .client
        .get_refund(&payment_ref(&v.env, 9))
        .expect("the first partial of the pair must survive the second's rejection");
    assert_eq!(record.amount_refunded, 400);
    assert_eq!(TokenClient::new(&v.env, &v.token).balance(&v.buyer), 400);

    // Atomic path: the same pair reverts, and nothing is left behind — the
    // balance and the record are the observable half of "reverts the pair".
    let v2 = setup();
    let claims = claim_vec(&v2, [&first, &repeat]);
    let nonce2 = v2.client.get_user_nonce(&v2.merchant);
    let before = TokenClient::new(&v2.env, &v2.token).balance(&v2.buyer);
    assert_eq!(
        claim_batch_on(&v2, claims, nonce2),
        Err(Error::ExceedsPayment),
        "`claim_batch` must abort the invocation on the second element",
    );
    assert_eq!(
        v2.client.get_refund(&payment_ref(&v2.env, 9)),
        None,
        "a reverted batch leaves no refund record",
    );
    assert_eq!(
        TokenClient::new(&v2.env, &v2.token).balance(&v2.buyer),
        before,
        "a reverted batch moves no tokens",
    );
}

/// Each row's verdict is reached only because the probe carries the merchant's
/// current nonce: a stale one short-circuits every row to `StaleState`, so a
/// hardcoded nonce would turn this whole file into a nonce test.
#[test]
fn a_stale_nonce_masks_every_row_with_the_same_error() {
    for case in ROWS {
        assert_eq!(
            claim_batch_fresh([case], 1),
            Err(Error::StaleState),
            "`{}` was not masked by a nonce rejection, so the probes below would pass for \
             the wrong reason",
            case.name,
        );

        // And with the current nonce the same element reaches its own rule.
        assert_eq!(
            claim_batch_fresh([case], 0),
            case.expected,
            "`{}` with the merchant's current nonce",
            case.name,
        );
    }
}
