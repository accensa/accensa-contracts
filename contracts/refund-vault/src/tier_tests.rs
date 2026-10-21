//! Merchant fee-tier ladder tests (branch `feature/vault-merchant-tier-promotion`).
//!
//! Pins the ladder validation rules, the fee each rung charges, the timing of a
//! promotion relative to the claim that triggers it, and that the preview and
//! the live refund path agree on the tier fee.

use accensa_common::Error;
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

use crate::test_helpers::{reg_vdf_policy, vault_init};
use crate::tiers::{effective_fee_bps, MerchantTier, MerchantTierState, MAX_TIERS};
use crate::{RefundVault, RefundVaultClient};

const FLOAT: i128 = 10_000_000;

struct Ctx {
    env: Env,
    client: RefundVaultClient<'static>,
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
fn without_a_ladder_the_flat_fee_applies() {
    let ctx = setup(250);

    assert!(ctx.client.get_tier_ladder().is_none());
    assert!(ctx.client.get_tier_state().is_none());
    assert_eq!(ctx.client.get_fee_bps(), 250);
    assert_eq!(ctx.client.get_effective_fee_bps(), 250);
}

#[test]
fn installing_a_ladder_activates_its_first_rung() {
    let ctx = setup(500);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 200), (100_000, 50)]));

    // The ladder, not the flat config, now drives the fee.
    assert_eq!(ctx.client.get_fee_bps(), 500);
    assert_eq!(ctx.client.get_effective_fee_bps(), 200);

    let state: MerchantTierState = ctx.client.get_tier_state().unwrap();
    assert_eq!(state.current_tier, 0);
    assert_eq!(state.fee_bps, 200);
    assert_eq!(state.settled_volume, 0);
    assert_eq!(state.next_threshold, 100_000);
}

#[test]
fn invalid_ladders_are_rejected() {
    let ctx = setup(0);

    for bad in [
        ladder(&ctx, &[]),
        ladder(&ctx, &[(1, 100)]),
        ladder(&ctx, &[(0, 100), (0, 50)]),
        ladder(&ctx, &[(0, 50), (10, 100), (5, 200)]),
        ladder(&ctx, &[(0, 10_001)]),
    ] {
        assert_eq!(
            ctx.client.try_set_tier_ladder(&bad),
            Err(Ok(Error::InvalidTierLadder))
        );
    }

    let mut too_many = soroban_sdk::Vec::new(&ctx.env);
    for i in 0..=MAX_TIERS {
        too_many.push_back(MerchantTier {
            min_settled: i as i128 * 1_000,
            fee_bps: 100,
        });
    }
    assert_eq!(
        ctx.client.try_set_tier_ladder(&too_many),
        Err(Ok(Error::InvalidTierLadder))
    );
}

#[test]
fn crossing_a_threshold_promotes_and_lowers_the_fee() {
    let ctx = setup(900);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (100_000, 100)]));
    assert_eq!(ctx.client.get_effective_fee_bps(), 500);

    let buyer = Address::generate(&ctx.env);
    ctx.client.refund(
        &payment_ref(&ctx, 1),
        &buyer,
        &100_000,
        &0,
        &1_000_000,
        &None,
        &0,
    );

    let state = ctx.client.get_tier_state().unwrap();
    assert_eq!(state.current_tier, 1);
    assert_eq!(state.fee_bps, 100);
    assert_eq!(state.settled_volume, 100_000);
    assert_eq!(
        state.next_threshold,
        i128::MAX,
        "the merchant is on the top rung"
    );
    assert_eq!(ctx.client.get_effective_fee_bps(), 100);
}

#[test]
fn the_promoting_claim_still_pays_the_old_tier_fee() {
    let ctx = setup(0);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (100_000, 100)]));
    let fee_recipient = Address::generate(&ctx.env);
    ctx.client.set_fee_recipient(&fee_recipient);

    let buyer = Address::generate(&ctx.env);
    ctx.client.refund(
        &payment_ref(&ctx, 2),
        &buyer,
        &100_000,
        &0,
        &1_000_000,
        &None,
        &0,
    );
    assert_eq!(
        balance(&ctx, &fee_recipient),
        5_000,
        "the promoting claim is charged the pre-promotion tier's 500 bps"
    );

    // From the next claim the promoted tier's 100 bps applies.
    ctx.client.refund(
        &payment_ref(&ctx, 3),
        &buyer,
        &100_000,
        &0,
        &1_000_000,
        &None,
        &1,
    );
    assert_eq!(balance(&ctx, &fee_recipient), 5_000 + 1_000);
}

#[test]
fn promotion_emits_an_event() {
    let ctx = setup(0);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (50_000, 100)]));

    let buyer = Address::generate(&ctx.env);
    ctx.client.refund(
        &payment_ref(&ctx, 4),
        &buyer,
        &50_000,
        &0,
        &1_000_000,
        &None,
        &0,
    );

    assert!(
        !ctx.env
            .events()
            .all()
            .filter_by_contract(&ctx.client.address)
            .events()
            .is_empty(),
        "MerchantTierPromoted must be published"
    );
}

#[test]
fn clearing_the_ladder_restores_the_flat_fee() {
    let ctx = setup(300);
    ctx.client.set_tier_ladder(&ladder(&ctx, &[(0, 200)]));
    assert_eq!(ctx.client.get_effective_fee_bps(), 200);

    ctx.client.clear_tier_ladder();
    assert!(ctx.client.get_tier_ladder().is_none());
    assert!(ctx.client.get_tier_state().is_none());
    assert_eq!(ctx.client.get_effective_fee_bps(), 300);
}

#[test]
fn re_installing_a_ladder_preserves_settled_volume() {
    let ctx = setup(0);
    ctx.client.set_tier_ladder(&ladder(&ctx, &[(0, 500)]));

    let buyer = Address::generate(&ctx.env);
    ctx.client.refund(
        &payment_ref(&ctx, 5),
        &buyer,
        &200_000,
        &0,
        &1_000_000,
        &None,
        &0,
    );
    assert_eq!(ctx.client.get_tier_state().unwrap().settled_volume, 200_000);

    // The replacement ladder is evaluated against the preserved volume, so the
    // merchant lands directly on the rung its history qualifies for.
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 500), (100_000, 100)]));
    let state = ctx.client.get_tier_state().unwrap();
    assert_eq!(state.settled_volume, 200_000);
    assert_eq!(state.current_tier, 1);
    assert_eq!(ctx.client.get_effective_fee_bps(), 100);
}

#[test]
fn preview_settlement_uses_the_tier_fee() {
    let ctx = setup(900);
    ctx.client.set_tier_ladder(&ladder(&ctx, &[(0, 200)]));

    let settlement = ctx
        .client
        .preview_settlement(&payment_ref(&ctx, 6), &100_000, &100_000);
    assert_eq!(settlement.fee, 2_000);
    assert_eq!(settlement.recipient_amount, 98_000);
}

#[test]
fn a_rejected_claim_does_not_accrue_volume() {
    let ctx = setup(0);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 100), (1, 50)]));

    let buyer = Address::generate(&ctx.env);
    assert_eq!(
        ctx.client
            .try_refund(&payment_ref(&ctx, 7), &buyer, &0, &0, &100, &None, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(ctx.client.get_tier_state().unwrap().settled_volume, 0);
}

#[test]
fn ladder_changes_require_the_merchant() {
    let ctx = setup(0);
    let tiers = ladder(&ctx, &[(0, 100)]);

    // No signatures: `require_auth` aborts rather than returning an error, so
    // the calls surface as host failures.
    ctx.env.set_auths(&[]);
    assert!(ctx.client.try_set_tier_ladder(&tiers).is_err());
    assert!(ctx.client.try_clear_tier_ladder().is_err());
}

#[test]
fn effective_fee_bps_returns_flat_fee_when_no_ladder() {
    let ctx = setup(350);

    let fee = ctx
        .env
        .as_contract(&ctx.client.address, || effective_fee_bps(&ctx.env));
    assert_eq!(fee, 350);
}

#[test]
fn effective_fee_bps_returns_tier_fee_when_ladder_active() {
    let ctx = setup(500);
    ctx.client
        .set_tier_ladder(&ladder(&ctx, &[(0, 200), (100_000, 50)]));

    let fee = ctx
        .env
        .as_contract(&ctx.client.address, || effective_fee_bps(&ctx.env));
    assert_eq!(fee, 200);
}

#[test]
fn reg_vdf_policy_registers_a_valid_contract() {
    let env = Env::default();
    let vdf_address = reg_vdf_policy(&env);

    assert!(!vdf_address.to_string().is_empty());

    // A live VdfPolicy answers its entry points; an unknown dispute is a typed error.
    let vdf_client = refund_policy_vdf::VdfPolicyClient::new(&env, &vdf_address);
    assert!(vdf_client.try_get_fallback_dispute(&0).is_err());
}

mod nft_owner_tests {
    use crate::nft_escrow::nft_owner;
    use soroban_sdk::{
        contract, contractimpl, contracttype, testutils::Address as _, Address, Env,
    };

    #[contract]
    struct MockNft;

    #[contracttype]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum NftKey {
        Admin,
        Owner(u128),
    }

    #[contractimpl]
    impl MockNft {
        pub fn init(env: Env, admin: Address) {
            env.storage().instance().set(&NftKey::Admin, &admin);
        }

        pub fn mint(env: Env, to: Address, token_id: u128) {
            let admin: Address = env.storage().instance().get(&NftKey::Admin).unwrap();
            admin.require_auth();
            env.storage()
                .persistent()
                .set(&NftKey::Owner(token_id), &to);
        }

        pub fn owner(env: Env, token_id: u128) -> Address {
            let zero = Address::from_string(&soroban_sdk::String::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ));
            env.storage()
                .persistent()
                .get(&NftKey::Owner(token_id))
                .unwrap_or_else(|| zero.clone())
        }

        pub fn transfer(env: Env, from: Address, to: Address, token_id: u128) {
            from.require_auth();
            let zero = Address::from_string(&soroban_sdk::String::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ));
            let owner: Address = env
                .storage()
                .persistent()
                .get(&NftKey::Owner(token_id))
                .unwrap_or_else(|| zero.clone());
            assert!(
                owner == from,
                "only the current owner may transfer the token"
            );
            env.storage()
                .persistent()
                .set(&NftKey::Owner(token_id), &to);
        }
    }

    #[test]
    fn nft_owner_returns_correct_owner() {
        let env = Env::default();
        env.mock_all_auths();

        let merchant = Address::generate(&env);
        let nft_id = env.register(MockNft, ());
        MockNftClient::new(&env, &nft_id).init(&merchant);

        let token_id: u128 = 42;
        MockNftClient::new(&env, &nft_id).mint(&merchant, &token_id);

        let owner = nft_owner(&env, &nft_id, token_id);
        assert_eq!(owner, merchant);
    }
}
