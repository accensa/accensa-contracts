//! Treasury yield distribution tests (RageTrade-style).
//!
//! Covers staking, unstaking, yield distribution, claiming, accumulator
//! math, and the proportional-share invariant.

extern crate std;

use crate::distribution::PRECISION;
use crate::{Error, Treasury, TreasuryClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

/// Arbitrary but fixed Unix timestamp.
const START: u64 = 1_700_000_000;
/// Tokens minted to the treasury up front for yield distribution.
const SUPPLY: i128 = 1_000_000_000;

struct DistCtx {
    env: Env,
    client: TreasuryClient<'static>,
    /// The holding token staked by users.
    holding_token: Address,
    /// The yield token distributed to stakers.
    yield_token: Address,
    treasury: Address,
    #[allow(dead_code)]
    admin: Address,
    alice: Address,
    bob: Address,
    carol: Address,
}

fn dist_setup() -> DistCtx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let carol = Address::generate(&env);

    let holding_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let yield_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let treasury = env.register(Treasury, (admin.clone(), holding_token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    // Mint holding tokens to users.
    let holding_client = StellarAssetClient::new(&env, &holding_token);
    holding_client.mint(&alice, &1_000_000);
    holding_client.mint(&bob, &1_000_000);
    holding_client.mint(&carol, &1_000_000);

    // Mint yield tokens to the treasury for distribution.
    StellarAssetClient::new(&env, &yield_token).mint(&treasury, &SUPPLY);

    // Initialize the distribution.
    client.initialize_distribution(&yield_token);

    DistCtx {
        env,
        client,
        holding_token,
        yield_token,
        treasury,
        admin,
        alice,
        bob,
        carol,
    }
}

fn yield_balance(ctx: &DistCtx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.yield_token).balance(who)
}

fn holding_balance(ctx: &DistCtx, who: &Address) -> i128 {
    TokenClient::new(&ctx.env, &ctx.holding_token).balance(who)
}

// ── Initialization ─────────────────────────────────────────────────────────

#[test]
fn distribution_requires_initialization() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let holding = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let yield_tok = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let treasury = env.register(Treasury, (admin.clone(), holding.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    // Before init: all distribution functions fail.
    assert_eq!(
        client.try_stake(&client.address, &1_000),
        Err(Ok(Error::DistributionNotInitialized))
    );
    assert_eq!(
        client.try_claim_yield(&client.address),
        Err(Ok(Error::DistributionNotInitialized))
    );
    assert_eq!(
        client.try_distribute_yield(&1_000),
        Err(Ok(Error::DistributionNotInitialized))
    );

    // After init: they work.
    client.initialize_distribution(&yield_tok);
    assert!(client.try_stake(&client.address, &1_000).is_ok());
}

#[test]
fn double_initialization_is_rejected() {
    let ctx = dist_setup();
    assert_eq!(
        ctx.client.try_initialize_distribution(&ctx.yield_token),
        Err(Ok(Error::DistributionAlreadyInitialized))
    );
}

// ── Staking ────────────────────────────────────────────────────────────────

#[test]
fn staking_tracks_user_state() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);

    let user_dist = ctx
        .client
        .get_user_distribution(&ctx.alice)
        .expect("user distribution should exist");
    assert_eq!(user_dist.staked, 100_000);
    assert_eq!(user_dist.checkpoint, 0);
    assert_eq!(user_dist.claimed, 0);

    let state = ctx.client.get_distribution_state();
    assert_eq!(state.total_staked, 100_000);
    assert_eq!(state.accumulator, 0);

    assert_eq!(holding_balance(&ctx, &ctx.alice), 900_000);
    assert_eq!(holding_balance(&ctx, &ctx.treasury), 100_000);
}

#[test]
fn staking_zero_is_rejected() {
    let ctx = dist_setup();
    assert_eq!(
        ctx.client.try_stake(&ctx.alice, &0),
        Err(Ok(Error::NothingToStake))
    );
}

#[test]
fn multiple_users_can_stake() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &300_000);
    ctx.client.stake(&ctx.bob, &100_000);

    let state = ctx.client.get_distribution_state();
    assert_eq!(state.total_staked, 400_000);

    let alice_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    let bob_dist = ctx.client.get_user_distribution(&ctx.bob).unwrap();
    assert_eq!(alice_dist.staked, 300_000);
    assert_eq!(bob_dist.staked, 100_000);
}

// ── Yield distribution & claiming ─────────────────────────────────────────

#[test]
fn distribute_yield_updates_accumulator() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    let state = ctx.client.get_distribution_state();
    assert_eq!(state.accumulator, 1_000 * PRECISION / 100_000);
    assert_eq!(state.total_distributed, 1_000);
}

#[test]
fn single_claimer_gets_full_yield() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 1_000);
    assert_eq!(yield_balance(&ctx, &ctx.alice), 1_000);
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 0);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.claimed, 1_000);
}

#[test]
fn yield_is_proportional_to_stake() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &300_000);
    ctx.client.stake(&ctx.bob, &100_000);

    ctx.client.distribute_yield(&4_000);

    // Alice has 300k of 400k total = 75%.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 3_000);
    // Bob has 100k of 400k total = 25%.
    assert_eq!(ctx.client.pending_yield(&ctx.bob), 1_000);

    // Both claim their shares.
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 3_000);
    assert_eq!(ctx.client.claim_yield(&ctx.bob), 1_000);

    assert_eq!(yield_balance(&ctx, &ctx.alice), 3_000);
    assert_eq!(yield_balance(&ctx, &ctx.bob), 1_000);
}

#[test]
fn proportional_split_with_three_stakers() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &500_000);
    ctx.client.stake(&ctx.bob, &300_000);
    ctx.client.stake(&ctx.carol, &200_000);

    ctx.client.distribute_yield(&10_000);

    // 50%, 30%, 20% split.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 5_000);
    assert_eq!(ctx.client.pending_yield(&ctx.bob), 3_000);
    assert_eq!(ctx.client.pending_yield(&ctx.carol), 2_000);

    ctx.client.claim_yield(&ctx.alice);
    ctx.client.claim_yield(&ctx.bob);
    ctx.client.claim_yield(&ctx.carol);

    assert_eq!(yield_balance(&ctx, &ctx.alice), 5_000);
    assert_eq!(yield_balance(&ctx, &ctx.bob), 3_000);
    assert_eq!(yield_balance(&ctx, &ctx.carol), 2_000);
}

#[test]
fn claiming_with_no_pending_yield_is_rejected() {
    let ctx = dist_setup();
    ctx.client.stake(&ctx.alice, &100_000);

    assert_eq!(
        ctx.client.try_claim_yield(&ctx.alice),
        Err(Ok(Error::NoYieldToClaim))
    );
}

#[test]
fn distribute_zero_is_rejected() {
    let ctx = dist_setup();
    ctx.client.stake(&ctx.alice, &100_000);

    assert_eq!(
        ctx.client.try_distribute_yield(&0),
        Err(Ok(Error::NothingToDistribute))
    );
}

// ── Unstaking ──────────────────────────────────────────────────────────────

#[test]
fn unstaking_preserves_claimable_yield() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    ctx.client.unstake(&ctx.alice, &50_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.staked, 50_000);

    // Can still claim the full pending yield.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 1_000);
}

#[test]
fn unstaking_more_than_staked_is_rejected() {
    let ctx = dist_setup();
    ctx.client.stake(&ctx.alice, &100_000);

    assert_eq!(
        ctx.client.try_unstake(&ctx.alice, &200_000),
        Err(Ok(Error::UnstakeExceedsStaked))
    );
}

#[test]
fn unstaking_zero_is_rejected() {
    let ctx = dist_setup();
    ctx.client.stake(&ctx.alice, &100_000);

    assert_eq!(
        ctx.client.try_unstake(&ctx.alice, &0),
        Err(Ok(Error::NothingToUnstake))
    );
}

#[test]
fn unstaking_without_stake_is_rejected() {
    let ctx = dist_setup();
    assert_eq!(
        ctx.client.try_unstake(&ctx.alice, &1_000),
        Err(Ok(Error::NothingToUnstake))
    );
}

// ── Checkpoint refresh ─────────────────────────────────────────────────────

#[test]
fn checkpoint_refreshes_on_stake() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    // Stake more — checkpoint refreshes, so old pending is "locked in".
    ctx.client.stake(&ctx.alice, &100_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.staked, 200_000);

    // The pending yield from before the second stake is still claimable.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
}

#[test]
fn checkpoint_refreshes_on_unstake() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    ctx.client.unstake(&ctx.alice, &50_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.staked, 50_000);

    // Pending yield is still claimable.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
}

#[test]
fn checkpoint_prevents_double_claiming() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);
    ctx.client.claim_yield(&ctx.alice);

    // Second claim in same state has nothing to pay.
    assert_eq!(
        ctx.client.try_claim_yield(&ctx.alice),
        Err(Ok(Error::NoYieldToClaim))
    );
}

// ── Multiple distributions ─────────────────────────────────────────────────

#[test]
fn multiple_distributions_accumulate() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);

    ctx.client.distribute_yield(&1_000);
    ctx.client.distribute_yield(&2_000);
    ctx.client.distribute_yield(&3_000);

    let state = ctx.client.get_distribution_state();
    assert_eq!(state.total_distributed, 6_000);
    assert_eq!(state.accumulator, 6_000 * PRECISION / 100_000);

    assert_eq!(ctx.client.pending_yield(&ctx.alice), 6_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 6_000);
}

#[test]
fn claim_between_distributions() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);

    ctx.client.distribute_yield(&1_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 1_000);

    ctx.client.distribute_yield(&2_000);
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 2_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 2_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.claimed, 3_000);
}

// ── Edge cases ─────────────────────────────────────────────────────────────

#[test]
fn distribute_with_no_stakers_does_not_update_accumulator() {
    let ctx = dist_setup();

    // No one stakes — accumulator stays at 0.
    ctx.client.distribute_yield(&1_000);

    let state = ctx.client.get_distribution_state();
    assert_eq!(state.accumulator, 0);
    assert_eq!(state.total_distributed, 1_000);
}

#[test]
fn stake_after_distribution_earns_from_that_point() {
    let ctx = dist_setup();

    // Alice stakes and earns some yield.
    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);
    ctx.client.claim_yield(&ctx.alice);

    // Bob stakes after the distribution.
    ctx.client.stake(&ctx.bob, &100_000);

    // New distribution splits between both.
    ctx.client.distribute_yield(&1_000);

    // Each has 50% of the stake, so each gets 500.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 500);
    assert_eq!(ctx.client.pending_yield(&ctx.bob), 500);
}

#[test]
fn full_unstake_and_restake_cycle() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.distribute_yield(&1_000);

    // Unstake everything.
    ctx.client.unstake(&ctx.alice, &100_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.staked, 0);

    // Pending yield is still claimable.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
    assert_eq!(ctx.client.claim_yield(&ctx.alice), 1_000);

    // Restake.
    ctx.client.stake(&ctx.alice, &100_000);

    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();
    assert_eq!(user_dist.staked, 100_000);

    // New distribution only applies to the new stake period.
    ctx.client.distribute_yield(&1_000);
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
}

#[test]
fn unstake_reduces_future_yield_share() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);
    ctx.client.stake(&ctx.bob, &100_000);

    // Alice unstakes half.
    ctx.client.unstake(&ctx.alice, &50_000);

    // New distribution: Alice has 50k, Bob has 100k.
    ctx.client.distribute_yield(&3_000);

    // Alice: 50k/150k = 33.33%, Bob: 100k/150k = 66.67%.
    // With PRECISION scaling, Alice gets 1000, Bob gets 2000.
    assert_eq!(ctx.client.pending_yield(&ctx.alice), 1_000);
    assert_eq!(ctx.client.pending_yield(&ctx.bob), 2_000);
}

// ── Admin guards ───────────────────────────────────────────────────────────

#[test]
fn only_admin_can_initialize_distribution() {
    let env = Env::default();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let holding = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let yield_tok = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let treasury = env.register(Treasury, (admin.clone(), holding.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    env.set_auths(&[]);
    assert!(client.try_initialize_distribution(&yield_tok).is_err());
    env.mock_all_auths();

    client.initialize_distribution(&yield_tok);
}

#[test]
fn only_admin_can_distribute_yield() {
    let ctx = dist_setup();
    ctx.client.stake(&ctx.alice, &100_000);

    ctx.env.set_auths(&[]);
    assert!(ctx.client.try_distribute_yield(&1_000).is_err());
    ctx.env.mock_all_auths();

    assert!(ctx.client.try_distribute_yield(&1_000).is_ok());
}

// ── State consistency ──────────────────────────────────────────────────────

#[test]
fn total_claimed_never_exceeds_total_distributed() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);

    ctx.client.distribute_yield(&1_000);
    ctx.client.claim_yield(&ctx.alice);

    ctx.client.distribute_yield(&2_000);
    ctx.client.claim_yield(&ctx.alice);

    let state = ctx.client.get_distribution_state();
    let user_dist = ctx.client.get_user_distribution(&ctx.alice).unwrap();

    assert!(user_dist.claimed <= state.total_distributed);
    assert_eq!(user_dist.claimed, 3_000);
    assert_eq!(state.total_distributed, 3_000);
}

#[test]
fn accumulator_is_monotonically_increasing() {
    let ctx = dist_setup();

    ctx.client.stake(&ctx.alice, &100_000);

    let acc0 = ctx.client.get_accumulator();
    ctx.client.distribute_yield(&1_000);
    let acc1 = ctx.client.get_accumulator();
    ctx.client.distribute_yield(&2_000);
    let acc2 = ctx.client.get_accumulator();

    assert!(acc1 > acc0);
    assert!(acc2 > acc1);
}

#[test]
fn user_distribution_defaults_for_non_staker() {
    let ctx = dist_setup();

    // Carol never staked — no user distribution record.
    assert!(ctx.client.get_user_distribution(&ctx.carol).is_none());
    assert_eq!(ctx.client.pending_yield(&ctx.carol), 0);
}
