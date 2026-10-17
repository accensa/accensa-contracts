//! Regression test for issue #690: `set_tier_ladder` must obtain the selected
//! rung's fee without indexing (and therefore without unwrapping) the ladder.
//!
//! Re-installing a ladder evaluates the volume already settled, so the
//! merchant can land on a rung other than `0`. This test pins that the fee
//! stored with the new state is the fee of that rung, read through the typed
//! missing-rung arm.

use accensa_common::VaultInit;
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use refund_vault::tiers::MerchantTier;
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, BytesN, Env};

const FLOAT: i128 = 1_000_000;

struct Ctx {
    env: Env,
    client: RefundVaultClient<'static>,
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

    Ctx { env, client }
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

#[test]
fn reinstalling_a_ladder_reads_the_rung_its_volume_qualifies_for() {
    let ctx = setup();
    ctx.client.set_tier_ladder(&ladder(&ctx, &[(0, 500)]));

    // Accrue 200_000 of settled volume under the one-rung ladder.
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
    assert_eq!(ctx.client.get_tier_state().unwrap().settled_volume, 200_000);

    // The replacement ladder puts that preserved volume on rung 1, not rung 0
    // and not the top rung: the stored fee must come from rung 1.
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (100_000, 100), (300_000, 50)]));

    let state = ctx.client.get_tier_state().unwrap();
    assert_eq!(state.settled_volume, 200_000);
    assert_eq!(state.current_tier, 1);
    assert_eq!(state.fee_bps, 100);
    assert_eq!(state.next_threshold, 300_000);
    assert_eq!(ctx.client.get_effective_fee_bps(), 100);
}
