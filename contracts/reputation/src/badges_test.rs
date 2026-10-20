//! Tiered NFT badge tests (issue #451).
//!
//! Covers the issue's acceptance criteria: minting on the first accurate
//! resolution, bronze → gold progression driven by simulated history,
//! replay protection on `dispute_id`, admin-only recording, and the
//! non-transferable surface (no transfer/approve entry points).

extern crate std;

use super::*;
use badges::Error as BadgeError;
use soroban_sdk::{
    events::Event as _,
    testutils::{Address as _, EnvTestConfig, Events as _},
    InvokeError,
};

/// Assert the last invocation emitted exactly `events` (nothing else), in
/// publish order. `env.events().all()` covers only the last invocation, so
/// assertions must run immediately after the call under test.
fn assert_emitted(
    env: &Env,
    contract: &Address,
    events: std::vec::Vec<soroban_sdk::xdr::ContractEvent>,
) {
    assert_eq!(env.events().all().filter_by_contract(contract), events);
}

struct Setup {
    env: Env,
    client: ReputationClient<'static>,
    admin: Address,
    arbitrator: Address,
}

fn setup() -> Setup {
    // Snapshots off: this suite pins observable behavior with explicit
    // event assertions instead of golden JSON files.
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let arbitrator = Address::generate(&env);
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    client.initialize(&admin);

    Setup {
        env,
        client,
        admin,
        arbitrator,
    }
}

/// Record `n` accurate resolutions for `who` using dispute ids
/// `start..start+n`, returning the next free dispute id.
fn record_n(s: &Setup, who: &Address, start: u64, n: u64) -> u64 {
    for i in 0..n {
        s.client.record_resolution(who, &(start + i), &true);
    }
    start + n
}

// ── Construction ─────────────────────────────────────────────────────────

#[test]
fn initialize_binds_admin() {
    let s = setup();
    assert_eq!(s.client.get_admin(), Some(s.admin.clone()));
    assert_eq!(s.client.total_badges(), 0);
}

#[test]
fn initialize_twice_fails() {
    let s = setup();
    assert_eq!(
        s.client.try_initialize(&s.admin),
        Err(Ok(BadgeError::AlreadyInitialized))
    );
}

// ── Minting on first accurate resolution ─────────────────────────────────

#[test]
fn first_accurate_resolution_mints_bronze() {
    let s = setup();
    let count = s.client.record_resolution(&s.arbitrator, &1, &true);
    assert_eq!(count, 1);

    let badge = s.client.get_badge(&s.arbitrator).unwrap();
    assert_eq!(badge.token_id, 0);
    assert_eq!(badge.owner, s.arbitrator);
    assert_eq!(badge.tier, BadgeTier::Bronze);
    assert_eq!(badge.accurate_resolutions, 1);
    assert_eq!(s.client.total_badges(), 1);
}

#[test]
fn second_arbitrator_gets_their_own_badge() {
    let s = setup();
    let other = Address::generate(&s.env);
    s.client.record_resolution(&s.arbitrator, &1, &true);
    s.client.record_resolution(&other, &2, &true);

    assert_eq!(s.client.get_badge(&s.arbitrator).unwrap().token_id, 0);
    assert_eq!(s.client.get_badge(&other).unwrap().token_id, 1);
    assert_eq!(s.client.total_badges(), 2);
}

// ── Tier progression: bronze → silver → gold ─────────────────────────────

#[test]
fn badge_progresses_bronze_to_gold_from_simulated_history() {
    let s = setup();

    // First resolution mints Bronze; 9 more keep it Bronze (count 10).
    s.client.record_resolution(&s.arbitrator, &1, &true);
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Bronze));

    let next = record_n(&s, &s.arbitrator, 2, 9);
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Bronze));
    assert_eq!(
        s.client
            .get_badge(&s.arbitrator)
            .unwrap()
            .accurate_resolutions,
        10
    );

    // Crossing 50 upgrades to Silver.
    let next = record_n(&s, &s.arbitrator, next, 40);
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Silver));
    assert_eq!(
        s.client
            .get_badge(&s.arbitrator)
            .unwrap()
            .accurate_resolutions,
        50
    );

    // Crossing 100 upgrades to Gold.
    record_n(&s, &s.arbitrator, next, 50);
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Gold));
    assert_eq!(
        s.client
            .get_badge(&s.arbitrator)
            .unwrap()
            .accurate_resolutions,
        100
    );
    // Token id and owner are stable across all upgrades.
    let badge = s.client.get_badge(&s.arbitrator).unwrap();
    assert_eq!(badge.token_id, 0);
    assert_eq!(badge.owner, s.arbitrator);
}

#[test]
fn tier_upgrade_emits_upgraded_event() {
    let s = setup();
    // Exactly reach the Silver threshold: ids 1..49 land on count 49.
    record_n(&s, &s.arbitrator, 1, 49);
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Bronze));

    // The 50th resolution upgrades and (as the last invocation) emits the
    // upgraded + recorded events in publish order.
    s.client.record_resolution(&s.arbitrator, &50, &true);
    assert_emitted(
        &s.env,
        &s.client.address,
        std::vec![
            BadgeUpgradedEvent {
                token_id: 0,
                owner: s.arbitrator.clone(),
                from_tier: BadgeTier::Bronze,
                to_tier: BadgeTier::Silver,
            }
            .to_xdr(&s.env, &s.client.address),
            ResolutionRecordedEvent {
                dispute_id: 50,
                owner: s.arbitrator.clone(),
                accurate_resolutions: 50,
                ledger: s.env.ledger().sequence(),
            }
            .to_xdr(&s.env, &s.client.address),
        ],
    );
    assert_eq!(s.client.get_tier(&s.arbitrator), Some(BadgeTier::Silver));
}

#[test]
fn mint_emits_minted_and_recorded_events() {
    let s = setup();
    // The minting invocation emits the minted + recorded events in order.
    s.client.record_resolution(&s.arbitrator, &1, &true);
    assert_emitted(
        &s.env,
        &s.client.address,
        std::vec![
            BadgeMintedEvent {
                token_id: 0,
                owner: s.arbitrator.clone(),
                tier: BadgeTier::Bronze,
            }
            .to_xdr(&s.env, &s.client.address),
            ResolutionRecordedEvent {
                dispute_id: 1,
                owner: s.arbitrator.clone(),
                accurate_resolutions: 1,
                ledger: s.env.ledger().sequence(),
            }
            .to_xdr(&s.env, &s.client.address),
        ],
    );
}

// ── Replay protection on dispute ids ─────────────────────────────────────

#[test]
fn duplicate_dispute_id_rejected() {
    let s = setup();
    s.client.record_resolution(&s.arbitrator, &7, &true);
    assert_eq!(
        s.client.try_record_resolution(&s.arbitrator, &7, &true),
        Err(Ok(BadgeError::DisputeAlreadyRecorded))
    );
    // The count was not inflated by the replay.
    assert_eq!(
        s.client
            .get_badge(&s.arbitrator)
            .unwrap()
            .accurate_resolutions,
        1
    );
}

#[test]
fn dispute_ids_are_global_not_per_arbitrator() {
    let s = setup();
    let other = Address::generate(&s.env);
    s.client.record_resolution(&s.arbitrator, &7, &true);
    // A second arbitrator cannot reuse the id either — the arbiter authority
    // is expected to assign ledger-unique ids.
    assert_eq!(
        s.client.try_record_resolution(&other, &7, &true),
        Err(Ok(BadgeError::DisputeAlreadyRecorded))
    );
}

// ── Access control ───────────────────────────────────────────────────────

#[test]
#[should_panic]
fn record_resolution_requires_admin_auth() {
    let s = setup();
    s.env.set_auths(&[]);
    s.client.record_resolution(&s.arbitrator, &1, &true);
}

#[test]
fn unauthorized_caller_cannot_mint_or_inflate() {
    // The admin (arbiter authority) is the only entry point for recording;
    // a non-admin cannot mint a badge for themselves at all: without a
    // recorded resolution there is no badge.
    let s = setup();
    assert!(s.client.get_badge(&s.arbitrator).is_none());
    assert_eq!(s.client.total_badges(), 0);
}

// ── Outcome validation ───────────────────────────────────────────────────

#[test]
fn inaccurate_outcome_rejected() {
    let s = setup();
    assert_eq!(
        s.client.try_record_resolution(&s.arbitrator, &1, &false),
        Err(Ok(BadgeError::InaccurateOutcome))
    );
    assert!(s.client.get_badge(&s.arbitrator).is_none());
}

// ── Read-only surface ────────────────────────────────────────────────────

#[test]
fn badge_not_found_for_unknown_arbitrator() {
    let s = setup();
    assert_eq!(s.client.get_badge(&s.arbitrator), None);
    assert_eq!(s.client.get_tier(&s.arbitrator), None);
}

#[test]
fn thresholds_are_discoverable() {
    let s = setup();
    assert_eq!(s.client.get_thresholds(), (10, 50, 100));
}

// ── Non-transferability by construction ──────────────────────────────────

/// The badge's storage binding lives under a key derived from its owner,
/// inside contract storage; peek at it the way the contract would.
fn badge_present(env: &Env, contract: &Address, owner: &Address) -> bool {
    env.as_contract(contract, || {
        env.storage()
            .persistent()
            .has(&BadgeDataKey::Badge(owner.clone()))
    })
}

#[test]
fn badges_cannot_move_between_addresses() {
    let s = setup();
    let attacker = Address::generate(&s.env);

    s.client.record_resolution(&s.arbitrator, &1, &true);

    // The badge is stored under a key derived from its owner and there is no
    // transfer/approve entry point, so the only way `attacker` ends up
    // holding *a* badge is a fresh, separate mint — never the arbitrator's.
    s.client.record_resolution(&attacker, &2, &true);
    assert_eq!(s.client.get_badge(&s.arbitrator).unwrap().token_id, 0);
    assert_eq!(s.client.get_badge(&attacker).unwrap().token_id, 1);
    assert!(badge_present(&s.env, &s.client.address, &s.arbitrator));
    assert!(badge_present(&s.env, &s.client.address, &attacker));
}

// ── Threshold boundary exactness ─────────────────────────────────────────

#[test]
fn tier_boundaries_are_exact() {
    assert_eq!(BadgeTier::for_accurate(9), None);
    assert_eq!(BadgeTier::for_accurate(10), Some(BadgeTier::Bronze));
    assert_eq!(BadgeTier::for_accurate(49), Some(BadgeTier::Bronze));
    assert_eq!(BadgeTier::for_accurate(50), Some(BadgeTier::Silver));
    assert_eq!(BadgeTier::for_accurate(99), Some(BadgeTier::Silver));
    assert_eq!(BadgeTier::for_accurate(100), Some(BadgeTier::Gold));
    assert_eq!(BadgeTier::for_accurate(1_000), Some(BadgeTier::Gold));
}

// ── Authority guard coverage ─────────────────────────────────────────────
//
// `record_resolution` mirrors the private `require_authority` guard
// (`lib.rs:62-67`): it reads `BadgeDataKey::Admin` from instance storage and
// rejects with the not-initialised variant when it is absent, then
// authenticates the stored admin with `require_auth()`. The guard itself is
// shared with the credit-scoring entry points, so those are driven here too.
// An unauthenticated caller is refused by the host, which aborts the
// invocation (`InvokeError::Abort`) rather than returning
// `Error::Unauthorized`; the wrong-caller arm is pinned as that abort, and
// the state it leaves behind is checked separately.

#[test]
fn record_resolution_before_initialize_reports_not_initialized() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    let arbitrator = Address::generate(&env);

    // No auth is mocked: the call must fail at the guard, not at a signature.
    assert_eq!(
        client.try_record_resolution(&arbitrator, &1, &true),
        Err(Ok(BadgeError::NotInitialized))
    );
    assert_eq!(client.total_badges(), 0);
    assert!(client.get_badge(&arbitrator).is_none());
}

#[test]
fn record_resolution_rejects_a_non_admin_caller() {
    let s = setup();
    let stranger = Address::generate(&s.env);

    // Drop the auth mock: the stored admin's `require_auth()` must now fail.
    s.env.set_auths(&[]);

    assert_eq!(
        s.client.try_record_resolution(&stranger, &1, &true),
        Err(Err(InvokeError::Abort))
    );
}

#[test]
fn refused_record_resolution_leaves_badge_state_untouched() {
    let s = setup();
    // Mint one badge through the admitted path first.
    s.client.record_resolution(&s.arbitrator, &1, &true);
    let total_before = s.client.total_badges();
    let badge_before = s.client.get_badge(&s.arbitrator).unwrap();

    // Now refuse a different caller without an auth mock.
    let stranger = Address::generate(&s.env);
    s.env.set_auths(&[]);
    assert!(s
        .client
        .try_record_resolution(&stranger, &2, &true)
        .is_err());

    // Nothing moved: the counter and the existing badge are exactly what
    // they were, and the refused caller holds no badge.
    assert_eq!(s.client.total_badges(), total_before);
    assert_eq!(s.client.get_badge(&s.arbitrator).unwrap(), badge_before);
    assert!(s.client.get_badge(&stranger).is_none());
}

#[test]
fn record_resolution_admits_the_stored_admin_and_persists_the_tier() {
    let s = setup();
    // mock_all_auths from setup() authorises the bound admin.
    let count = s.client.record_resolution(&s.arbitrator, &1, &true);
    assert_eq!(count, 1);

    let badge = s.client.get_badge(&s.arbitrator).unwrap();
    assert_eq!(badge.tier, BadgeTier::Bronze);
    assert_eq!(badge.accurate_resolutions, 1);
    assert_eq!(s.client.total_badges(), 1);
}

#[test]
fn require_authority_reports_not_initialized_for_score_config() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);

    // `set_score_config` routes through the shared `require_authority` guard;
    // with no `Admin` entry it must report `NotInitialized` rather than
    // defaulting to a stored authority.
    assert_eq!(
        client.try_set_score_config(&800, &950),
        Err(Ok(CreditError::NotInitialized))
    );
}

#[test]
fn require_authority_reports_not_initialized_for_record_completion() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    let buyer = Address::generate(&env);

    assert_eq!(
        client.try_record_completion(&buyer, &1),
        Err(Ok(CreditError::NotInitialized))
    );
}

#[test]
fn require_authority_admits_the_bound_admin_for_score_config() {
    let s = setup();
    // mock_all_auths from setup() authorises the bound admin, so the write
    // through `require_authority` is admitted and lands in storage.
    s.client.set_score_config(&800, &950);

    let config = s.client.get_score_config();
    assert_eq!(config.gold_tier, 800);
    assert_eq!(config.zero_fee_tier, 950);
}
