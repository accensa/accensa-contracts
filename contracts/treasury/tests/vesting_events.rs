//! Vesting event shape tests (issue #655).
//!
//! Pins the two `#[contractevent]` records the vesting flow publishes:
//! `ScheduleCreatedEvent` when the admin registers an allocation, and
//! `ClaimedEvent` when a beneficiary claims unlocked tokens.

use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::StellarAssetClient,
    Address, Env, Event,
};
use treasury::{ClaimedEvent, Error, ScheduleCreatedEvent, Treasury, TreasuryClient};

/// Arbitrary but fixed Unix timestamp the schedules start from.
const START: u64 = 1_700_000_000;
/// Cliff and release window chosen so every partial unlock is a whole token.
const CLIFF: u64 = 1_000;
const DURATION: u64 = 4_000;
/// Total allocation, in the token's smallest unit.
const TOTAL: i128 = 4_000_000;

struct Ctx {
    env: Env,
    client: TreasuryClient<'static>,
    treasury: Address,
    beneficiary: Address,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = START);

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = env.register(Treasury, (admin, token.clone()));
    let client = TreasuryClient::new(&env, &treasury);
    StellarAssetClient::new(&env, &token).mint(&treasury, &TOTAL);

    Ctx {
        env,
        client,
        treasury,
        beneficiary,
    }
}

/// `add_schedule` emits exactly one `ScheduleCreatedEvent`, with the
/// beneficiary as its topic and the four arguments as its payload.
#[test]
fn schedule_created_event_pins_topic_and_payload() {
    let ctx = setup();

    ctx.client
        .add_schedule(&ctx.beneficiary, &TOTAL, &START, &CLIFF, &DURATION);

    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);
    assert_eq!(
        &events.events()[0],
        &ScheduleCreatedEvent {
            beneficiary: ctx.beneficiary.clone(),
            total: TOTAL,
            start: START,
            cliff: CLIFF,
            duration: DURATION,
        }
        .to_xdr(&ctx.env, &ctx.treasury)
    );
}

/// A partial claim and the final claim each emit one `ClaimedEvent` carrying
/// the amount moved and the cumulative claimed total after the call.
#[test]
fn claimed_event_pins_amount_and_cumulative_total() {
    let ctx = setup();
    ctx.client
        .add_schedule(&ctx.beneficiary, &TOTAL, &START, &CLIFF, &DURATION);

    // Half the window has elapsed: 2,000,000 unlocked, nothing claimed yet.
    ctx.env.ledger().with_mut(|li| li.timestamp = START + 2_000);
    assert_eq!(ctx.client.claim_vested(&ctx.beneficiary), 2_000_000);

    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);
    assert_eq!(
        &events.events()[0],
        &ClaimedEvent {
            beneficiary: ctx.beneficiary.clone(),
            amount: 2_000_000,
            total_claimed: 2_000_000,
        }
        .to_xdr(&ctx.env, &ctx.treasury)
    );

    // The window has closed: the remaining 2,000,000 pays out.
    ctx.env
        .ledger()
        .with_mut(|li| li.timestamp = START + DURATION);
    assert_eq!(ctx.client.claim_vested(&ctx.beneficiary), 2_000_000);

    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 1);
    assert_eq!(
        &events.events()[0],
        &ClaimedEvent {
            beneficiary: ctx.beneficiary.clone(),
            amount: 2_000_000,
            total_claimed: TOTAL,
        }
        .to_xdr(&ctx.env, &ctx.treasury)
    );
}

/// Before the cliff a claim pays nothing, so it emits no `ClaimedEvent`.
#[test]
fn claim_paying_nothing_emits_no_claimed_event() {
    let ctx = setup();
    ctx.client
        .add_schedule(&ctx.beneficiary, &TOTAL, &START, &CLIFF, &DURATION);

    assert_eq!(
        ctx.client.try_claim_vested(&ctx.beneficiary),
        Err(Ok(Error::NothingToClaim))
    );

    let events = ctx.env.events().all().filter_by_contract(&ctx.treasury);
    assert_eq!(events.events().len(), 0);
}
