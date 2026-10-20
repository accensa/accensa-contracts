//! Schedule-entry TTL tests (issue #656).
//!
//! [`treasury::Treasury`] keeps every vesting record alive through one
//! helper — `bump_ttl`, which applies the shared
//! [`accensa_common::storage`] policy (`DEFAULT_TTL_LOW_WATER` /
//! `DEFAULT_TTL_BUMP`) at `add_schedule` and at every successful
//! `claim_vested`. These tests pin that policy on the schedule entry itself:
//!
//! 1. registering a schedule leaves `DataKey::Schedule(b)` at or above
//!    `DEFAULT_TTL_BUMP`;
//! 2. after the entry decays below `DEFAULT_TTL_LOW_WATER`, a successful
//!    claim re-extends the same entry back to at least `DEFAULT_TTL_BUMP`;
//! 3. a rejected claim (nothing unlocked) leaves the recorded TTL no shorter
//!    than it was.

#![cfg(test)]

use accensa_common::storage::{DEFAULT_TTL_BUMP, DEFAULT_TTL_LOW_WATER};
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};
use treasury::{DataKey, Error, Treasury, TreasuryClient};

/// Arbitrary but fixed Unix timestamp the schedules start from.
const START: u64 = 1_700_000_000;
/// Tokens minted to the treasury before a claim pays out.
const SUPPLY: i128 = 100_000_000;
/// Cliff far enough in the future that "before the cliff" is a distinct state.
const CLIFF: u64 = 1_000;
/// Short linear window so a partial unlock needs only a small timestamp step.
const DURATION: u64 = 10_000;
const TOTAL: i128 = 1_000_000;

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    token: Address,
    treasury: Address,
    beneficiary: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);
    // A freshly written persistent entry normally starts at the network's
    // `min_persistent_entry_ttl` floor (4,096 ledgers in the mock), which sits
    // far above `DEFAULT_TTL_LOW_WATER` — so `extend_ttl(low_water, bump)` is
    // a documented no-op right after `set` (see the same reasoning in
    // refund-vault's `refund_record_ttl_extend_to`). Lowering the mock's floor
    // below the low-water mark makes a fresh schedule eligible for the bump in
    // the same call, so what these tests observe is the TTL policy itself.
    env.ledger()
        .set_min_persistent_entry_ttl(DEFAULT_TTL_LOW_WATER / 2);

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin, token.clone()));
    let client = TreasuryClient::new(&env, &treasury);

    Ctx {
        env,
        client,
        token,
        treasury,
        beneficiary,
    }
}

fn register_schedule(ctx: &Ctx) {
    ctx.client
        .add_schedule(&ctx.beneficiary, &TOTAL, &START, &CLIFF, &DURATION);
}

/// The persistent-entry TTL of `DataKey::Schedule(b)`, read from outside the
/// crate exactly as the issue prescribes: `env.as_contract` +
/// `env.storage().persistent().get_ttl`.
fn schedule_ttl(ctx: &Ctx) -> u32 {
    let beneficiary = ctx.beneficiary.clone();
    ctx.env.as_contract(&ctx.treasury, || {
        ctx.env
            .storage()
            .persistent()
            .get_ttl(&DataKey::Schedule(beneficiary))
    })
}

fn decay_schedule_below_low_water(ctx: &Ctx) {
    // The schedule was written at sequence 0 and bumped to a live-until of
    // `DEFAULT_TTL_BUMP`, so landing `DEFAULT_TTL_LOW_WATER / 2` ledgers short
    // of that leaves the entry below the low-water mark, where the next
    // successful claim's `bump_ttl` must fire.
    let sequence = DEFAULT_TTL_BUMP - DEFAULT_TTL_LOW_WATER / 2;
    ctx.env
        .ledger()
        .with_mut(|li| li.sequence_number = sequence);
}

#[test]
fn registering_a_schedule_leaves_the_entry_at_or_above_the_ttl_bump_target() {
    let ctx = setup();
    register_schedule(&ctx);

    let ttl = schedule_ttl(&ctx);
    assert!(
        ttl >= DEFAULT_TTL_BUMP,
        "schedule TTL ({ttl}) right after add_schedule must be at or above \
         DEFAULT_TTL_BUMP ({DEFAULT_TTL_BUMP})"
    );
}

#[test]
fn a_successful_claim_re_extends_a_decayed_schedule_entry_to_the_bump_target() {
    let ctx = setup();
    register_schedule(&ctx);

    // Age the entry until only half the low-water mark remains.
    decay_schedule_below_low_water(&ctx);
    let decayed = schedule_ttl(&ctx);
    assert!(
        decayed <= DEFAULT_TTL_LOW_WATER,
        "schedule TTL ({decayed}) must be at or below the low-water mark \
         ({DEFAULT_TTL_LOW_WATER}) before the claim"
    );

    // Unlock a partial claim and fund the treasury (minted after the ledger
    // jump so the payout transfer works against a live balance entry).
    ctx.env
        .ledger()
        .with_mut(|li| li.timestamp = START + CLIFF + DURATION / 2);
    StellarAssetClient::new(&ctx.env, &ctx.token).mint(&ctx.treasury, &SUPPLY);

    let paid = ctx.client.claim_vested(&ctx.beneficiary);
    assert!(paid > 0, "the claim must pay out something to prove it ran");

    let after = schedule_ttl(&ctx);
    assert!(
        after >= DEFAULT_TTL_BUMP,
        "a successful claim must re-extend the same schedule entry ({decayed} \
         -> {after}) to at least DEFAULT_TTL_BUMP ({DEFAULT_TTL_BUMP})"
    );
}

#[test]
fn a_rejected_claim_leaves_the_schedule_entry_ttl_no_shorter_than_it_was() {
    let ctx = setup();
    register_schedule(&ctx);

    // Decay below the low-water mark so a bump *would* fire if the rejected
    // claim touched the entry.
    decay_schedule_below_low_water(&ctx);
    let before = schedule_ttl(&ctx);
    assert!(
        before <= DEFAULT_TTL_LOW_WATER,
        "schedule TTL ({before}) must be at or below the low-water mark \
         ({DEFAULT_TTL_LOW_WATER}) for this test to be meaningful"
    );

    // Timestamp still at `START`, well before the cliff: nothing is unlocked.
    let outcome = ctx.client.try_claim_vested(&ctx.beneficiary);
    assert_eq!(
        outcome,
        Err(Ok(Error::NothingToClaim)),
        "the claim must be rejected with the typed NothingToClaim error"
    );

    let after = schedule_ttl(&ctx);
    assert!(
        after >= before,
        "a rejected claim must not shorten the schedule TTL ({before} -> {after})"
    );
}
