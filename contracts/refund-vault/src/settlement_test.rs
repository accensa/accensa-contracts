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

use crate::settlement::RefundSettlement;
use crate::test_helpers::vault_init;
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
    client.deposit(&merchant, &FLOAT);

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
