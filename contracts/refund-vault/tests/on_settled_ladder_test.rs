//! Regression test for issue #689: `on_settled` must obtain the promoting
//! rung's fee without indexing (and therefore without unwrapping) the merchant
//! fee ladder.
//!
//! A single claim that crosses several rungs at once is the case where the old
//! `ladder.get(new_tier).unwrap()` relied on a manual argument about the loop
//! bound. This test pins that such a claim promotes straight to the highest
//! qualifying rung, records that rung's fee, and charges it from the next
//! claim onward.

use accensa_common::VaultInit;
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use refund_vault::tiers::MerchantTier;
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;

struct Ctx {
    env: Env,
    client: RefundVaultClient<'static>,
    token: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(merchant.clone())
        .address();

    let init = VaultInit {
        merchant: merchant.clone(),
        token: token.clone(),
        time_policy: Some(env.register(TimePolicy, ())),
        vdf_policy: Some(env.register(VdfPolicy, ())),
        fee_bps: 0,
        fee_recipient: None,
        refund_window: 0,
        deadline: 0,
        vdf_delay: 0,
    };
    let vault = env.register(RefundVault, (init,));
    let client = RefundVaultClient::new(&env, &vault);

    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);
    client.deposit(&merchant, &FLOAT, &None);

    Ctx { env, client, token }
}

fn ladder(ctx: &Ctx, rungs: &[(i128, u32)]) -> soroban_sdk::Vec<MerchantTier> {
    let mut tiers = soroban_sdk::Vec::new(&ctx.env);
    for (min_settled, fee_bps) in rungs {
        tiers.push_back(MerchantTier {
            min_settled: *min_settled,
            fee_bps: *fee_bps,
        });
    }
    tiers
}

fn payment_ref(ctx: &Ctx, seed: u8) -> BytesN<32> {
    BytesN::from_array(&ctx.env, &[seed; 32])
}

fn balance(ctx: &Ctx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.token).balance(who)
}

#[test]
fn a_claim_that_skips_rungs_promotes_to_the_highest_qualifying_rung() {
    let ctx = setup();
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (100_000, 100), (200_000, 50)]));
    assert_eq!(ctx.client.get_effective_fee_bps(), 500);

    // One claim jumps from rung 0 straight past rung 1 to rung 2. The fee for
    // that rung must come from the ladder walk, not an unchecked index.
    let recipient = Address::generate(&ctx.env);
    ctx.client.refund(
        &payment_ref(&ctx, 1),
        &recipient,
        &200_000,
        &0,
        &FLOAT,
        &None,
        &0,
    );

    let state = ctx.client.get_tier_state().unwrap();
    assert_eq!(state.settled_volume, 200_000);
    assert_eq!(state.current_tier, 2, "highest qualifying rung wins");
    assert_eq!(state.fee_bps, 50, "the rung's fee is recorded");
    assert_eq!(
        state.next_threshold,
        i128::MAX,
        "the merchant is on the top rung"
    );
    assert_eq!(ctx.client.get_effective_fee_bps(), 50);

    // The promoting claim was charged the pre-promotion fee of 500 bps; the
    // next claim is charged the promoted rung's 50 bps.
    assert_eq!(balance(&ctx, &recipient), 200_000 - 10_000);
    ctx.client.refund(
        &payment_ref(&ctx, 2),
        &recipient,
        &100_000,
        &0,
        &FLOAT,
        &None,
        &1,
    );
    assert_eq!(balance(&ctx, &recipient), 190_000 + 99_500);
}
