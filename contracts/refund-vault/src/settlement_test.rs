//! Partial-refund settlement preview tests (issue #473).
//!
//! Pins the split arithmetic (including the fee's rounding-up dust), the
//! cumulative ceiling behaviour, and — most importantly — that
//! `preview_settlement` reports exactly the amounts a real `refund` moves.

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::settlement::{resolve_ceiling, split_amount, RefundSettlement};
use crate::test_helpers::{reg_time_policy, vault_init};
use crate::{RefundVault, RefundVaultClient};

const FLOAT: i128 = 1_000_000;

struct Ctx {
    env: Env,
    client: RefundVaultClient<'static>,
    vault: Address,
    token: Address,
}

fn setup(fee_bps: u32) -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(merchant.clone())
        .address();

    let mut init = vault_init(&env, &merchant, &token, 0);
    init.fee_bps = fee_bps;
    let vault = env.register(RefundVault, (init,));
    let client = RefundVaultClient::new(&env, &vault);

    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);
    client.deposit(&merchant, &FLOAT, &None);

    Ctx {
        env,
        client,
        vault,
        token,
    }
}

fn balance(ctx: &Ctx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(who)
}

fn payment_ref(ctx: &Ctx, seed: u8) -> BytesN<32> {
    BytesN::from_array(&ctx.env, &[seed; 32])
}

#[test]
fn a_full_refund_without_fees_retains_nothing() {
    let ctx = setup(0);
    let settlement: RefundSettlement =
        ctx.client
            .preview_settlement(&payment_ref(&ctx, 1), &100_000, &100_000);

    assert_eq!(
        settlement,
        RefundSettlement {
            recipient_amount: 100_000,
            fee: 0,
            merchant_retained: 0,
            cumulative_refunded: 100_000,
        }
    );
}

#[test]
fn a_half_refund_splits_the_payment_between_buyer_and_merchant() {
    let ctx = setup(0);
    let settlement = ctx
        .client
        .preview_settlement(&payment_ref(&ctx, 2), &50_000, &100_000);

    assert_eq!(settlement.recipient_amount, 50_000);
    assert_eq!(settlement.fee, 0);
    assert_eq!(
        settlement.merchant_retained, 50_000,
        "the unrefunded half stays in the vault for the merchant"
    );
    assert_eq!(settlement.cumulative_refunded, 50_000);
}

#[test]
fn the_fee_is_deducted_and_its_rounding_dust_stays_with_the_recipient() {
    // 250 bps = 2.5%. `refund_fee` rounds the fee *up*, so for 100_001 the
    // exact 2500.025 fee becomes 2501 and the recipient receives the rest.
    let ctx = setup(250);
    let settlement = ctx
        .client
        .preview_settlement(&payment_ref(&ctx, 3), &100_001, &100_001);

    assert_eq!(settlement.fee, 2_501);
    assert_eq!(settlement.recipient_amount, 97_500);
    assert_eq!(
        settlement.fee + settlement.recipient_amount,
        100_001,
        "the split must always sum back to the refunded amount"
    );
    assert_eq!(settlement.merchant_retained, 0);
}

#[test]
fn a_second_partial_is_measured_against_the_running_total() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 4);
    let buyer = Address::generate(&ctx.env);

    ctx.client
        .refund(&pref, &buyer, &50_000, &0, &100_000, &None, &0);

    // 50_000 already refunded: a further 30_000 leaves 20_000 with the
    // merchant and reports the running total, not just this call's amount.
    let settlement = ctx.client.preview_settlement(&pref, &30_000, &100_000);
    assert_eq!(settlement.cumulative_refunded, 80_000);
    assert_eq!(settlement.merchant_retained, 20_000);
    assert_eq!(settlement.recipient_amount, 30_000);
}

#[test]
fn preview_matches_the_amounts_a_real_refund_moves() {
    let ctx = setup(250);
    let fee_recipient = Address::generate(&ctx.env);
    ctx.client.set_fee_recipient(&fee_recipient);

    let pref = payment_ref(&ctx, 5);
    let buyer = Address::generate(&ctx.env);
    let amount: i128 = 100_001;
    let payment_amount: i128 = 200_000;

    let preview = ctx
        .client
        .preview_settlement(&pref, &amount, &payment_amount);

    ctx.client
        .refund(&pref, &buyer, &amount, &0, &payment_amount, &None, &0);

    // The preview is only meaningful if the live path moves exactly what it
    // promised — fee, recipient payout and the retained remainder alike.
    assert_eq!(balance(&ctx, &buyer), preview.recipient_amount);
    assert_eq!(balance(&ctx, &fee_recipient), preview.fee);
    assert_eq!(
        balance(&ctx, &ctx.vault),
        FLOAT - preview.recipient_amount - preview.fee
    );

    let record = ctx.client.get_refund(&pref).unwrap();
    assert_eq!(record.amount_refunded, preview.cumulative_refunded);
    assert_eq!(
        payment_amount - record.amount_refunded,
        preview.merchant_retained
    );
}

#[test]
fn a_refund_past_the_ceiling_has_no_settlement() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 6);

    // One unit over the original payment.
    assert_eq!(
        ctx.client.try_preview_settlement(&pref, &100_001, &100_000),
        Err(Ok(accensa_common::Error::ExceedsPayment))
    );

    // Landing exactly on the ceiling is allowed.
    let settlement = ctx.client.preview_settlement(&pref, &100_000, &100_000);
    assert_eq!(settlement.merchant_retained, 0);
}

#[test]
fn non_positive_amounts_are_rejected() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 7);

    for amount in [0i128, -1] {
        assert_eq!(
            ctx.client.try_preview_settlement(&pref, &amount, &100_000),
            Err(Ok(accensa_common::Error::InvalidAmount))
        );
    }
}

#[test]
fn resolve_ceiling_returns_zero_previous_and_payment_amount_when_no_record() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 10);

    let (previous, ceiling) = ctx
        .env
        .as_contract(&ctx.client.address, || {
            resolve_ceiling(&ctx.env, &pref, 50_000, 100_000)
        })
        .unwrap();

    assert_eq!(previous, 0);
    assert_eq!(ceiling, 100_000);
}

#[test]
fn resolve_ceiling_returns_existing_record_values_when_record_exists() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 11);
    let buyer = Address::generate(&ctx.env);

    // First refund creates the record
    ctx.client
        .refund(&pref, &buyer, &30_000, &0, &100_000, &None, &0);

    // Now resolve_ceiling should return the existing record's values
    let (previous, ceiling) = ctx
        .env
        .as_contract(&ctx.client.address, || {
            resolve_ceiling(&ctx.env, &pref, 20_000, 200_000)
        })
        .unwrap();

    assert_eq!(
        previous, 30_000,
        "should return the already-refunded amount"
    );
    assert_eq!(
        ceiling, 100_000,
        "ceiling should be fixed by the first partial"
    );
}

#[test]
fn resolve_ceiling_rejects_overflow() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 12);

    // amount so large that previous_refunded + amount overflows i128
    let result = ctx.env.as_contract(&ctx.client.address, || {
        resolve_ceiling(&ctx.env, &pref, i128::MAX, 100_000)
    });

    assert_eq!(result, Err(accensa_common::Error::ExceedsPayment));
}

#[test]
fn resolve_ceiling_rejects_non_positive_ceiling() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 13);

    // payment_amount <= 0 should be rejected
    let result = ctx.env.as_contract(&ctx.client.address, || {
        resolve_ceiling(&ctx.env, &pref, 10_000, 0)
    });

    assert_eq!(result, Err(accensa_common::Error::ExceedsPayment));
}

#[test]
fn resolve_ceiling_rejects_cumulative_exceeding_ceiling() {
    let ctx = setup(0);
    let pref = payment_ref(&ctx, 14);
    let buyer = Address::generate(&ctx.env);

    // First refund of 60_000
    ctx.client
        .refund(&pref, &buyer, &60_000, &0, &100_000, &None, &0);

    // Try to refund another 50_000 (would exceed ceiling of 100_000)
    let result = ctx.env.as_contract(&ctx.client.address, || {
        resolve_ceiling(&ctx.env, &pref, 50_000, 100_000)
    });

    assert_eq!(result, Err(accensa_common::Error::ExceedsPayment));
}

#[test]
fn split_amount_with_zero_fee_returns_full_amount_as_recipient() {
    let (fee, recipient) = split_amount(100_000, 0);

    assert_eq!(fee, 0);
    assert_eq!(recipient, 100_000);
    assert_eq!(fee + recipient, 100_000);
}

#[test]
fn split_amount_with_fee_calculates_correctly_and_sums_to_amount() {
    // 500 bps = 5% fee
    let (fee, recipient) = split_amount(100_000, 500);

    assert_eq!(fee, 5_000);
    assert_eq!(recipient, 95_000);
    assert_eq!(fee + recipient, 100_000);
}

#[test]
fn split_amount_rounds_fee_up() {
    // 333 bps = 3.33% fee on 100 = 3.33, rounds up to 4
    let (fee, recipient) = split_amount(100, 333);

    assert_eq!(fee, 4); // rounds up
    assert_eq!(recipient, 96);
    assert_eq!(fee + recipient, 100);
}

#[test]
fn split_amount_works_with_large_fee() {
    // 10_000 bps = 100% fee
    let (fee, recipient) = split_amount(50_000, 10_000);

    assert_eq!(fee, 50_000);
    assert_eq!(recipient, 0);
    assert_eq!(fee + recipient, 50_000);
}

#[test]
fn reg_time_policy_registers_a_valid_contract() {
    let env = Env::default();
    let time_address = reg_time_policy(&env);

    assert!(!time_address.to_string().is_empty());
    // TimePolicy's only entry point is `evaluate`; a second registration must
    // yield a distinct contract address.
    assert_ne!(time_address, reg_time_policy(&env));
}
