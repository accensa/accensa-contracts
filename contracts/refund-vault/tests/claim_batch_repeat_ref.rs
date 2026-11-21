//! #962 — repeated `payment_ref`s inside one `claim_batch` share the ceiling the
//! *first* element created, not the `payment_amount` the later element supplies.
//!
//! `claim_batch`'s docs (`src/lib.rs:1408-1409`) say repeated refs "accumulate
//! against the same ceiling", and `settlement::resolve_ceiling`
//! (`src/settlement.rs:66`) implements that by taking the ceiling from the stored
//! record — `rec.payment_amount` at `:77` — and using the caller's `payment_amount`
//! only for a ref that has no record yet. The field is therefore *ignored* on
//! every repeat, which is the security-relevant direction: a merchant (or a
//! compromised client) that re-sends a ref with a bigger `payment_amount` must not
//! buy itself more headroom mid-batch.
//!
//! That is precisely what no existing test can see. `src/test.rs::
//! test_claim_batch_same_ref_accumulates_and_excess_reverts` sends both elements
//! with `payment_amount = 1_000_000`, so an implementation that trusted the
//! element's field would compute the same 1,000,000 ceiling and pass. Here the
//! second element carries an inflated 2,000,000, and the ceiling is probed twice:
//!
//!   * through the record, whose `payment_amount` is written from
//!     `record_ceiling` (`src/lib.rs:1029`) and so reports which value won; and
//!   * behaviorally, with a later 300,000 claim that fits under 2,000,000 but not
//!     under 1,000,000. `ExceedsPayment` is the only answer consistent with the
//!     first element's ceiling.
//!
//! The 300,000 probe needs the float to be able to pay it, or the rejection would
//! prove nothing about the ceiling — so the vault holds 2,000,000 while the whole
//! test never asks for more than 1,100,000 of refunds.
//!
//! `refund_window: 0` and no policy mirrors, as in the sibling files: the only
//! gate allowed to reject these claims is the ceiling.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundClaim, RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, Vec,
};

const MINT: i128 = 3_000_000;
const FLOAT: i128 = 2_000_000;
/// The ref both elements share.
const REF: u8 = 0x62;
/// The first element's payment, and the ceiling every assertion below is about.
const CEILING: i128 = 1_000_000;
const PARTIAL: i128 = 400_000;
/// The inflated `payment_amount` on the second element.
const INFLATED: i128 = 2_000_000;

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    buyer: Address,
    token: TokenClient<'static>,
}

fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &MINT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 0,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &FLOAT, &None);

    let token = TokenClient::new(&env, &token);

    Vault {
        env,
        contract,
        client,
        merchant,
        buyer,
        token,
    }
}

fn payment_ref(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[REF; 32])
}

/// One element against the shared ref, carrying `payment_amount`.
fn element(v: &Vault, amount: i128, payment_amount: i128) -> RefundClaim {
    RefundClaim {
        payment_ref: payment_ref(&v.env),
        recipient: v.buyer.clone(),
        amount,
        paid_at_ledger: 0,
        payment_amount,
        vdf_proof: None,
    }
}

/// The batch the issue describes: two 400,000 partials, the second claiming the
/// payment was for 2,000,000.
fn inflated_batch(v: &Vault) -> Vec<RefundClaim> {
    vec![
        &v.env,
        element(v, PARTIAL, CEILING),
        element(v, PARTIAL, INFLATED),
    ]
}

/// A different ref, same recipient. Used to show a rejection was about *this*
/// payment's ceiling rather than a vault-wide limit.
fn other_ref_batch(v: &Vault, amount: i128) -> Vec<RefundClaim> {
    vec![
        &v.env,
        RefundClaim {
            payment_ref: BytesN::from_array(&v.env, &[0x63; 32]),
            recipient: v.buyer.clone(),
            amount,
            paid_at_ledger: 0,
            payment_amount: CEILING,
            vdf_proof: None,
        },
    ]
}

/// The batch succeeds, and the record says the ceiling is the first element's.
///
/// `payment_amount` is the assertion with teeth: it is stored from
/// `record_ceiling`, so reading 2,000,000 here would mean an element field
/// overwrote the ceiling that the first element established.
#[test]
fn the_inflated_second_field_never_reaches_the_record() {
    let v = setup();

    v.client.claim_batch(&inflated_batch(&v), &0);

    let record = v.client.get_refund(&payment_ref(&v.env)).unwrap();
    assert_eq!(record.amount_refunded, PARTIAL * 2);
    assert_eq!(
        record.payment_amount, CEILING,
        "the ceiling came from the record, not from the second element's field",
    );
    assert_eq!(v.token.balance(&v.buyer), PARTIAL * 2);
    assert_eq!(v.token.balance(&v.contract), FLOAT - PARTIAL * 2);
}

/// Both elements are charged against that one ceiling, so the batch's own
/// cumulative total is what is bounded: 400,000 + 400,000 = 800,000 of a 1,000,000
/// payment is inside it, and the same pair repeated a third time would not be.
#[test]
fn three_partials_in_one_batch_cross_the_shared_ceiling() {
    let v = setup();

    // 3 x 400,000 = 1,200,000 against a 1,000,000 ceiling: the third element
    // fails, and the revert takes the two that had already paid.
    let too_far = vec![
        &v.env,
        element(&v, PARTIAL, CEILING),
        element(&v, PARTIAL, INFLATED),
        element(&v, PARTIAL, INFLATED),
    ];
    assert_eq!(
        v.client.try_claim_batch(&too_far, &0),
        Err(Ok(Error::ExceedsPayment)),
    );
    assert_eq!(
        v.env.events().all().filter_by_contract(&v.contract),
        vec![&v.env],
    );
    assert!(v.client.get_refund(&payment_ref(&v.env)).is_none());
    assert_eq!(v.token.balance(&v.buyer), 0);

    // The two-element batch of the same amounts does fit.
    v.client.claim_batch(&inflated_batch(&v), &0);
    assert_eq!(
        v.client
            .get_refund(&payment_ref(&v.env))
            .unwrap()
            .amount_refunded,
        PARTIAL * 2
    );
}

/// The behavioral half of the same claim: 800,000 already refunded, so a further
/// 300,000 is over the first element's 1,000,000 and under the inflated 2,000,000.
/// `ExceedsPayment` can only come from the record's ceiling.
#[test]
fn a_later_claim_is_still_bounded_by_the_first_elements_ceiling() {
    let v = setup();
    v.client.claim_batch(&inflated_batch(&v), &0);

    let over = vec![&v.env, element(&v, 300_000, INFLATED)];
    assert_eq!(
        v.client.try_claim_batch(&over, &1),
        Err(Ok(Error::ExceedsPayment)),
    );

    // Rejected, so nothing moved: the record is still at 800,000 and the float
    // still holds the 1,200,000 the two partials left.
    assert_eq!(
        v.client
            .get_refund(&payment_ref(&v.env))
            .unwrap()
            .amount_refunded,
        PARTIAL * 2
    );
    assert_eq!(v.token.balance(&v.buyer), PARTIAL * 2);
    assert_eq!(v.token.balance(&v.contract), FLOAT - PARTIAL * 2);
    assert_eq!(v.client.get_user_nonce(&v.merchant), 1);
}

/// The exact remainder *is* refundable, which pins the ceiling at 1,000,000
/// rather than merely below 1,100,000: 800,000 + 200,000 settles the payment in
/// full, and one unit more would be refused.
#[test]
fn the_ceiling_is_exactly_the_first_elements_payment_amount() {
    let v = setup();
    v.client.claim_batch(&inflated_batch(&v), &0);

    let remainder = CEILING - PARTIAL * 2;
    let fits = vec![&v.env, element(&v, remainder, INFLATED)];
    v.client.claim_batch(&fits, &1);
    assert_eq!(
        v.client
            .get_refund(&payment_ref(&v.env))
            .unwrap()
            .amount_refunded,
        CEILING
    );

    // A settled payment now admits nothing, not even one unit.
    let one_more = vec![&v.env, element(&v, 1, INFLATED)];
    assert_eq!(
        v.client.try_claim_batch(&one_more, &2),
        Err(Ok(Error::ExceedsPayment)),
    );
    assert_eq!(
        v.client
            .get_refund(&payment_ref(&v.env))
            .unwrap()
            .amount_refunded,
        CEILING
    );
}

/// The refusals above are about this payment ref: a different ref with the same
/// recipient and the same inflated field is still bounded by *its* own first
/// element, so no vault-wide limit is doing the work.
#[test]
fn a_fresh_ref_takes_its_ceiling_from_its_own_first_element() {
    let v = setup();
    v.client.claim_batch(&inflated_batch(&v), &0);

    // The exhausted ref refuses 300,000; a fresh ref accepts more than that.
    let over = vec![&v.env, element(&v, 300_000, INFLATED)];
    assert_eq!(
        v.client.try_claim_batch(&over, &1),
        Err(Ok(Error::ExceedsPayment)),
    );

    v.client.claim_batch(&other_ref_batch(&v, 700_000), &1);
    assert_eq!(
        v.client
            .get_refund(&BytesN::from_array(&v.env, &[0x63; 32]))
            .unwrap()
            .payment_amount,
        CEILING,
    );
    assert_eq!(v.token.balance(&v.buyer), PARTIAL * 2 + 700_000);
}
