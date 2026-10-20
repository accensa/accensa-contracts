//! Buyer credit scoring tests (issue #452).
//!
//! Covers the issue's acceptance criteria: score updates on successful
//! escrow completion, heavy decreases on fraudulent dispute losses, score
//! variation over a series of simulated transactions, plus the decay curve,
//! tier unlocks, replay protection and access control.

extern crate std;

use super::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger as _};

struct Setup {
    env: Env,
    client: ReputationClient<'static>,
    buyer: Address,
}

fn setup() -> Setup {
    // Snapshots off: this suite pins observable behavior with explicit
    // assertions instead of golden JSON files.
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let contract_id = env.register(Reputation, ());
    let client = ReputationClient::new(&env, &contract_id);
    client.initialize(&admin);

    Setup { env, client, buyer }
}

/// Record `n` successful escrow completions for `who` with fresh escrow ids,
/// returning the next free id.
fn complete_n(s: &Setup, who: &Address, start: u64, n: u64) -> u64 {
    for i in 0..n {
        s.client.record_completion(who, &(start + i));
    }
    start + n
}

// ── Starting score / cold-start ──────────────────────────────────────────

#[test]
fn unknown_buyer_gets_neutral_starting_score() {
    let s = setup();
    assert_eq!(s.client.get_score(&s.buyer), STARTING_SCORE);
    assert_eq!(s.client.get_credit_record(&s.buyer), None);
}

// ── Growth on successful escrow completion ───────────────────────────────

#[test]
fn completion_grows_score_along_headroom_curve() {
    let s = setup();

    // First completion: 500 + 25% of 500 headroom = 625.
    let score = s.client.record_completion(&s.buyer, &1);
    assert_eq!(score, 625);
    assert_eq!(s.client.get_score(&s.buyer), 625);

    // Second: 625 + 25% of 375 = 625 + 93 = 718.
    let score = s.client.record_completion(&s.buyer, &2);
    assert_eq!(score, 718);
}

#[test]
fn score_varies_over_simulated_transaction_series() {
    let s = setup();

    // A realistic series: mixed completions and one fraud, sampled after
    // each event. Growth = floor(25% of headroom); fraud = score minus
    // floor(50% of score).
    let mut next = complete_n(&s, &s.buyer, 1, 3);
    assert_eq!(s.client.get_score(&s.buyer), 788); // 500→625→718→788

    let score = s.client.record_fraud(&s.buyer);
    assert_eq!(score, 394); // 788 - 394

    next = complete_n(&s, &s.buyer, next, 2);
    assert_eq!(next, 6); // ids 1..=5 consumed

    // 394 → +151 = 545 → +113 = 658 (floored each step).
    assert_eq!(s.client.get_score(&s.buyer), 658);

    let record = s.client.get_credit_record(&s.buyer).unwrap();
    assert_eq!(record.completions, 5);
    assert_eq!(record.fraud_count, 1);
}

#[test]
fn completions_approach_max_score_without_reaching_it() {
    let s = setup();
    // Many completions: the score asymptotically approaches 1000.
    complete_n(&s, &s.buyer, 1, 40);
    let score = s.client.get_score(&s.buyer);
    assert!(score > 990, "score should be near max, got {score}");
    assert!(score < MAX_SCORE, "growth curve must never reach the cap");
}

// ── Fraud penalty ────────────────────────────────────────────────────────

#[test]
fn fraud_halves_the_current_score() {
    let s = setup();
    complete_n(&s, &s.buyer, 1, 2); // 718
    let score = s.client.record_fraud(&s.buyer);
    assert_eq!(score, 359);

    let record = s.client.get_credit_record(&s.buyer).unwrap();
    assert_eq!(record.fraud_count, 1);
    assert_eq!(record.completions, 2);
}

#[test]
fn fraud_hurts_more_the_higher_the_score() {
    let s = setup();
    // Same neutral start, but fraud on a grown score drops harder.
    let before = s.client.get_score(&s.buyer); // 500
    let after = s.client.record_fraud(&s.buyer);
    assert_eq!(after, before - before / 2); // 250

    // Rebuild: 250 → 437 → 577.
    complete_n(&s, &s.buyer, 1, 2);
    let before = s.client.get_score(&s.buyer); // 577
    let after = s.client.record_fraud(&s.buyer);
    assert_eq!(after, 289); // 577 - floor(577/2)
    assert!(before - after > 250, "higher score loses more points");
}

#[test]
fn fraud_can_never_underflow() {
    let s = setup();
    // Drive the score down with repeated frauds: 0 is a hard bound.
    for _ in 0..10 {
        s.client.record_fraud(&s.buyer);
    }
    // Halving floors the penalty, so odd scores keep their remainder and
    // the sequence bottoms out at 1 (500→250→125→63→…→1), never below.
    assert_eq!(s.client.get_score(&s.buyer), 1);
}

#[test]
fn repeated_fraud_does_not_go_below_zero() {
    let s = setup();
    // 500 → 250 → 125 → 63 → 32 → 16 → 8 → 4 → 2 → 1 → 1 → …
    for _ in 0..20 {
        s.client.record_fraud(&s.buyer);
    }
    assert_eq!(s.client.get_score(&s.buyer), 1);
}

// ── Inactivity decay ─────────────────────────────────────────────────────

#[test]
fn idle_score_decays_toward_the_floor_per_interval() {
    let s = setup();
    complete_n(&s, &s.buyer, 1, 2); // 718 at ledger L

    // One full decay interval later: 1% of the headroom above the floor
    // (718 - 100 = 618 → -6) per interval.
    s.env
        .ledger()
        .set_sequence_number(s.env.ledger().sequence() + DECAY_INTERVAL_LEDGERS);
    assert_eq!(s.client.get_score(&s.buyer), 712);

    // Read-only decay: the stored record's last_update is untouched, so a
    // second read over the same interval repeats the same decay view.
    assert_eq!(s.client.get_score(&s.buyer), 712);
}

#[test]
fn decay_is_not_applied_before_a_full_interval() {
    let s = setup();
    complete_n(&s, &s.buyer, 1, 1); // 625
    s.env
        .ledger()
        .set_sequence_number(s.env.ledger().sequence() + DECAY_INTERVAL_LEDGERS - 1);
    assert_eq!(s.client.get_score(&s.buyer), 625);
}

#[test]
fn decay_never_drops_below_the_floor() {
    let s = setup();
    // Push the score as low as fraud can take it, then idle far past it.
    for _ in 0..20 {
        s.client.record_fraud(&s.buyer);
    }
    assert_eq!(s.client.get_score(&s.buyer), 1);
    // A score already below the floor stays there: the floor is 100 and
    // apply_decay only decays headroom above it.
    s.env
        .ledger()
        .set_sequence_number(s.env.ledger().sequence() + 10 * DECAY_INTERVAL_LEDGERS);
    assert_eq!(s.client.get_score(&s.buyer), 1);
}

#[test]
fn decay_applies_before_growth_on_next_completion() {
    let s = setup();
    complete_n(&s, &s.buyer, 1, 2); // 718
    s.env
        .ledger()
        .set_sequence_number(s.env.ledger().sequence() + DECAY_INTERVAL_LEDGERS);
    // Decay view: 712. Growth then applies to the decayed base:
    // 712 + 25% of 288 = 712 + 72 = 784.
    let score = s.client.record_completion(&s.buyer, &10);
    assert_eq!(score, 784);
}

// ── Zero-fee tiers ───────────────────────────────────────────────────────

#[test]
fn fee_tiers_unlock_at_configured_scores() {
    let s = setup();
    let config = s.client.get_score_config();
    assert_eq!(
        config,
        ScoreConfig {
            gold_tier: DEFAULT_GOLD_TIER,
            zero_fee_tier: DEFAULT_ZERO_FEE_TIER,
        }
    );

    // Below gold: full fee.
    assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 100);
    // Gold tier (≥ 800): half fee.
    assert_eq!(fee_bps_for(800, 100, &config), 50);
    assert_eq!(fee_bps_for(899, 100, &config), 50);
    // Zero-fee tier (≥ 950): no fee.
    assert_eq!(fee_bps_for(950, 100, &config), 0);
    assert_eq!(fee_bps_for(1000, 100, &config), 0);

    // The contract surfaces the same mapping read-only.
    assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 100);
}

#[test]
fn zero_fee_unlock_requires_crossing_the_tier() {
    let s = setup();
    // 500 → 625 → 718: still full fee.
    complete_n(&s, &s.buyer, 1, 2);
    assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 100);

    // Keep completing until ≥ 950: fee must hit 0.
    let mut next = 3u64;
    while s.client.get_score(&s.buyer) < DEFAULT_ZERO_FEE_TIER {
        s.client.record_completion(&s.buyer, &next);
        next += 1;
    }
    assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 0);
}

#[test]
fn authority_can_retune_tiers_and_invalid_config_rejected() {
    let s = setup();
    s.client.set_score_config(&700, &900);
    let config = s.client.get_score_config();
    assert_eq!(config.gold_tier, 700);
    assert_eq!(config.zero_fee_tier, 900);
    assert_eq!(fee_bps_for(700, 100, &config), 50);
    assert_eq!(fee_bps_for(900, 100, &config), 0);

    // zero_fee < gold is invalid.
    assert_eq!(
        s.client.try_set_score_config(&900, &800),
        Err(Ok(CreditError::InvalidConfig))
    );
    // zero_fee > MAX_SCORE is invalid.
    assert_eq!(
        s.client.try_set_score_config(&800, &(MAX_SCORE + 1)),
        Err(Ok(CreditError::InvalidConfig))
    );
    // gold == 0 is invalid.
    assert_eq!(
        s.client.try_set_score_config(&0, &900),
        Err(Ok(CreditError::InvalidConfig))
    );
}

// ── Replay protection ────────────────────────────────────────────────────

#[test]
fn duplicate_escrow_id_rejected_without_score_change() {
    let s = setup();
    s.client.record_completion(&s.buyer, &7);
    assert_eq!(
        s.client.try_record_completion(&s.buyer, &7),
        Err(Ok(CreditError::EscrowAlreadyRecorded))
    );
    // The replay did not inflate the score.
    assert_eq!(s.client.get_score(&s.buyer), 625);
}

#[test]
fn escrow_ids_are_global_not_per_buyer() {
    let s = setup();
    let other = Address::generate(&s.env);
    s.client.record_completion(&s.buyer, &7);
    assert_eq!(
        s.client.try_record_completion(&other, &7),
        Err(Ok(CreditError::EscrowAlreadyRecorded))
    );
}

// ── Access control ───────────────────────────────────────────────────────

#[test]
#[should_panic]
fn record_completion_requires_authority_auth() {
    let s = setup();
    s.env.set_auths(&[]);
    s.client.record_completion(&s.buyer, &1);
}

#[test]
#[should_panic]
fn record_fraud_requires_authority_auth() {
    let s = setup();
    s.env.set_auths(&[]);
    s.client.record_fraud(&s.buyer);
}

#[test]
#[should_panic]
fn set_score_config_requires_authority_auth() {
    let s = setup();
    s.env.set_auths(&[]);
    s.client.set_score_config(&800, &950);
}

// ── Bounds ───────────────────────────────────────────────────────────────

#[test]
fn score_always_within_zero_to_max() {
    let s = setup();
    // Interleave heavy growth and heavy penalty; every observation is bound.
    for i in 0..10u64 {
        s.client.record_completion(&s.buyer, &(i + 1));
        let score = s.client.get_score(&s.buyer);
        assert!(score <= MAX_SCORE);
        s.client.record_fraud(&s.buyer);
        let score = s.client.get_score(&s.buyer);
        assert!(score <= MAX_SCORE);
    }
}

// ── Credit keys & event payload parity (issue #621) ─────────────────────

/// Pins the storage/event surface of `credit_score.rs` that the tests above
/// only ever observed through getters: the shared
/// `CreditDataKey::Credit(Address)` entry, the
/// `CreditDataKey::CompletionEscrow(u64)` replay tombstone — the guard that
/// makes double-recording a completion impossible — and the full
/// `CreditUpdatedEvent` payload.
///
/// Note: `env.events().all()` reflects only the last invocation, exactly as
/// `contracts/cross-chain/src/layerzero_test.rs:130` documents — each test
/// below captures a call's events before making any other contract call.
mod credit_keys_and_event_parity {
    use super::*;
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::Event;

    /// Reads the raw `CreditDataKey::Credit(buyer)` storage entry — the one
    /// `record_completion`, `record_fraud`, `get_credit_record` and
    /// `get_score` all go through.
    fn stored_record(s: &Setup) -> CreditRecord {
        s.env
            .as_contract(&s.client.address, || {
                s.env
                    .storage()
                    .persistent()
                    .get::<CreditDataKey, CreditRecord>(&CreditDataKey::Credit(s.buyer.clone()))
            })
            .expect("credit entry must be stored under Credit(Address)")
    }

    // ── CompletionEscrow replay guard ────────────────────────────────

    #[test]
    fn duplicate_escrow_id_is_a_noop_on_score_and_record() {
        let s = setup();
        let contract = s.client.address.clone();

        let score = s.client.record_completion(&s.buyer, &7);
        assert_eq!(score, 625);
        let first = s.client.get_credit_record(&s.buyer).unwrap();

        // The tombstone lives under the shared replay-protection key…
        assert!(s.env.as_contract(&contract, || {
            s.env
                .storage()
                .persistent()
                .has(&CreditDataKey::CompletionEscrow(7))
        }));

        // …so replaying the same escrow_id is rejected outright.
        assert_eq!(
            s.client.try_record_completion(&s.buyer, &7),
            Err(Ok(CreditError::EscrowAlreadyRecorded))
        );

        // The score changed exactly once: 500 → 625, then nothing.
        assert_eq!(s.client.get_score(&s.buyer), 625);
        // No-op on the stored record: the same Credit entry reads back
        // identical, with the completion counted once.
        assert_eq!(s.client.get_credit_record(&s.buyer).unwrap(), first);
        assert_eq!(stored_record(&s), first);
        assert_eq!(first.score, 625);
        assert_eq!(first.completions, 1);
        assert_eq!(first.fraud_count, 0);

        // The rejected replay published no CreditUpdatedEvent.
        // (`events().all()` now covers the failed invocation, which emitted
        // nothing.)
        assert!(s
            .env
            .events()
            .all()
            .filter_by_contract(&contract)
            .events()
            .is_empty());
    }

    // ── One shared Credit entry ──────────────────────────────────────

    #[test]
    fn fraud_and_completion_share_and_update_one_credit_entry() {
        let s = setup();

        s.client.record_completion(&s.buyer, &1); // 500 → 625
        let after_completion = stored_record(&s);
        assert_eq!(after_completion.score, 625);
        assert_eq!(after_completion.completions, 1);
        assert_eq!(after_completion.fraud_count, 0);

        s.client.record_fraud(&s.buyer); // 625 − floor(625/2) = 313
        let after_fraud = stored_record(&s);

        // Same entry, updated in place: both paths' counters coexist, so
        // fraud did not fork a second Credit record.
        assert_eq!(after_fraud.score, 313);
        assert_eq!(after_fraud.completions, 1);
        assert_eq!(after_fraud.fraud_count, 1);

        // The getters read that same entry.
        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record, after_fraud);
        assert_eq!(s.client.get_score(&s.buyer), after_fraud.score);
    }

    // ── CreditUpdatedEvent payload ───────────────────────────────────

    #[test]
    fn completion_event_payload_agrees_with_record_and_getters() {
        let s = setup();
        let contract = s.client.address.clone();

        s.client.record_completion(&s.buyer, &1); // 500 → 625

        // Last invocation only: capture before any other contract call.
        let events = s.env.events().all().filter_by_contract(&contract);
        assert_eq!(events.events().len(), 1);
        let expected = CreditUpdatedEvent {
            buyer: s.buyer.clone(),
            old_score: STARTING_SCORE,
            new_score: 625,
            reason: ScoreChangeReason::Completion,
        };
        // Full payload asserted by XDR comparison, in the style of
        // contracts/state-channel/src/hashlock_test.rs:105-115.
        assert_eq!(&events.events()[0], &expected.to_xdr(&s.env, &contract));

        // Stored record and both getters agree with the event payload.
        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record.score, expected.new_score);
        assert_eq!(s.client.get_score(&s.buyer), expected.new_score);
    }

    #[test]
    fn fraud_event_payload_agrees_with_record_and_getters() {
        let s = setup();
        let contract = s.client.address.clone();

        s.client.record_fraud(&s.buyer); // 500 → 250

        // Last invocation only: capture before any other contract call.
        let events = s.env.events().all().filter_by_contract(&contract);
        assert_eq!(events.events().len(), 1);
        let expected = CreditUpdatedEvent {
            buyer: s.buyer.clone(),
            old_score: STARTING_SCORE,
            new_score: 250,
            reason: ScoreChangeReason::Fraud,
        };
        assert_eq!(&events.events()[0], &expected.to_xdr(&s.env, &contract));

        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record.score, expected.new_score);
        assert_eq!(record.fraud_count, 1);
        assert_eq!(s.client.get_score(&s.buyer), expected.new_score);
    }

    #[test]
    fn completion_and_fraud_publish_distinct_events() {
        let s = setup();
        let contract = s.client.address.clone();

        s.client.record_completion(&s.buyer, &1); // 500 → 625
                                                  // `events().all()` reflects only the last invocation, so the
                                                  // completion's event is cloned here, before the fraud call.
        let completion_events = s.env.events().all().filter_by_contract(&contract);
        let completion_xdr = completion_events.events()[0].clone();

        s.client.record_fraud(&s.buyer); // 625 → 313
        let fraud_events = s.env.events().all().filter_by_contract(&contract);
        assert_eq!(fraud_events.events().len(), 1);

        let completion = CreditUpdatedEvent {
            buyer: s.buyer.clone(),
            old_score: 500,
            new_score: 625,
            reason: ScoreChangeReason::Completion,
        };
        let fraud = CreditUpdatedEvent {
            buyer: s.buyer.clone(),
            old_score: 625,
            new_score: 313,
            reason: ScoreChangeReason::Fraud,
        };
        assert_eq!(
            completion_xdr,
            completion.to_xdr(&s.env, &contract),
            "completion path must publish its own payload"
        );
        assert_eq!(&fraud_events.events()[0], &fraud.to_xdr(&s.env, &contract));

        // Same buyer, same contract — the two paths publish different events.
        assert_ne!(
            completion.to_xdr(&s.env, &contract),
            fraud.to_xdr(&s.env, &contract)
        );

        // After the fraud path, the getters agree with the latest payload
        // while keeping the completion's contribution.
        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record.score, fraud.new_score);
        assert_eq!(record.completions, 1);
        assert_eq!(record.fraud_count, 1);
        assert_eq!(s.client.get_score(&s.buyer), fraud.new_score);
    }

    // ── Fee outcome stays in sync with the stored record ─────────────

    #[test]
    fn fee_bps_stays_in_sync_with_the_stored_credit_record() {
        let s = setup();
        let config = s.client.get_score_config();

        // Grow past the gold tier: 500 → 625 → 718 → 788 → 841.
        complete_n(&s, &s.buyer, 1, 4);
        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record.score, 841);
        assert_eq!(s.client.get_score(&s.buyer), record.score);
        // The fee lookup maps the same stored score: gold → half fee…
        assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 50);
        assert_eq!(
            s.client.get_fee_bps(&s.buyer, &100),
            fee_bps_for(record.score, 100, &config)
        );

        // …and a fraud drop is visible immediately through the same entry:
        // 841 − floor(841/2) = 421 → back below gold.
        s.client.record_fraud(&s.buyer);
        let record = s.client.get_credit_record(&s.buyer).unwrap();
        assert_eq!(record.score, 421);
        assert_eq!(s.client.get_score(&s.buyer), record.score);
        assert_eq!(s.client.get_fee_bps(&s.buyer, &100), 100);
        assert_eq!(
            s.client.get_fee_bps(&s.buyer, &100),
            fee_bps_for(record.score, 100, &config)
        );
    }
}
