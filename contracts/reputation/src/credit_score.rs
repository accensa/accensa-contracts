//! On-chain credit scoring for buyers (issue #452).
//!
//! Computes a dynamic score in `[0, MAX_SCORE]` (0–1000) for each buyer from
//! their escrow track record, unlocking zero-fee tiers as the score climbs.
//!
//! # Score dynamics
//!
//! - **Start** — every buyer's score starts at [`STARTING_SCORE`] (500,
//!   neutral); a buyer with no recorded history simply has the starting
//!   score.
//! - **Growth** — each successful escrow completion applies the growth curve:
//!   the score rises by a fraction of the remaining headroom
//!   (`GROWTH_RATE_BPS` of `MAX_SCORE - score`), so early completions move
//!   the needle and the approach to 1000 tapers off.
//! - **Fraud** — a fraudulent dispute loss applies the penalty curve: the
//!   score drops by [`PENALTY_RATE_BPS`] of the *current* score, so losing a
//!   dispute near 1000 is far more expensive than near 0. A fraud event also
//!   writes a permanent [`CreditRecord::fraud_count`] flag for gating.
//! - **Decay** — inactivity is scored as mild risk: when a call arrives more
//!   than [`DECAY_INTERVAL_LEDGERS`] after the record's `last_update`, the
//!   score decays by [`DECAY_RATE_BPS`] of the *amount above the floor* per
//!   elapsed interval, and never below [`SCORE_FLOOR`]. An idle buyer drifts
//!   toward the floor, not to zero — scores are a standing reputation, and
//!   the floor keeps a long-inactive but historically good buyer distinguishable
//!   from a fraudulent one.
//!
//! All four curves are pure integer math (`u64`/`u128` basis points, rounded
//! down) and share one [`apply_decay`] helper so the two entry points can
//! never diverge.
//!
//! # Access model
//!
//! `record_completion` and `record_fraud` are restricted to the **escrow
//! authority** — the same admin bound by `initialize`, expected to be the
//! escrow/settlement contract (delegated record-keeping, never claimed).
//! `get_score` and `get_record` are read-only and free.
//!
//! # Storage shape
//!
//! - `Credit(Address)` — persistent, one entry per scored buyer.
//! - `ScoreConfig` — instance: tier cut-offs the escrow authority may
//!   re-tune. Never persisted as `None`; absent means factory defaults
//!   ([`DEFAULT_GOLD_TIER`], [`DEFAULT_ZERO_FEE_TIER`]).
//!
//! # Zero-fee tiers
//!
//! [`fee_bps_for`] maps a score to an escrow fee in basis points: the full
//! fee below [`DEFAULT_GOLD_TIER`], half at gold, and zero at
//! [`DEFAULT_ZERO_FEE_TIER`] — the "unlock" the issue asks for.

use soroban_sdk::{contractevent, contracttype, Address, Env};

/// Maximum possible score.
pub const MAX_SCORE: u64 = 1_000;
/// Neutral starting score for buyers with no recorded history.
pub const STARTING_SCORE: u64 = 500;
/// Score never decays below this floor.
pub const SCORE_FLOOR: u64 = 100;
/// Growth per successful completion: basis points of the remaining headroom
/// (2_500 bps = 25% of the gap to 1000).
pub const GROWTH_RATE_BPS: u64 = 2_500;
/// Fraud penalty: basis points of the current score lost per fraudulent
/// dispute loss (5_000 bps = half the score).
pub const PENALTY_RATE_BPS: u64 = 5_000;
/// Inactivity interval after which decay is applied.
pub const DECAY_INTERVAL_LEDGERS: u32 = 172_800; // ~10 days at ~5s ledgers
/// Decay per elapsed interval: basis points of the score above the floor
/// (100 bps = 1%).
pub const DECAY_RATE_BPS: u64 = 100;

/// Score at which the gold tier (half fee) unlocks. Re-tunable by the escrow
/// authority via `set_score_config`.
pub const DEFAULT_GOLD_TIER: u64 = 800;
/// Score at which the zero-fee tier unlocks. Re-tunable via
/// `set_score_config`.
pub const DEFAULT_ZERO_FEE_TIER: u64 = 950;

/// Basis-point divisor.
const BPS_DENOMINATOR: u128 = 10_000;

/// Storage keys for the credit scoring feature (issue #452). Kept separate
/// from the badge keys so the two features evolve independently.
#[contracttype]
pub enum CreditDataKey {
    /// Persistent, one credit record per scored buyer.
    Credit(Address),
    /// Persistent, one tombstone per consumed escrow id (replay protection).
    CompletionEscrow(u64),
    /// Instance: re-tunable tier cut-offs. Absent means factory defaults.
    ScoreConfig,
}

/// Stored per-buyer credit record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditRecord {
    /// Current score in `[0, MAX_SCORE]`.
    pub score: u64,
    /// Lifetime count of successful escrow completions.
    pub completions: u64,
    /// Lifetime count of fraudulent dispute losses.
    pub fraud_count: u64,
    /// Ledger of the most recent score-affecting event.
    pub last_update: u32,
}

/// Re-tunable tier configuration.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScoreConfig {
    /// Score unlocking the gold (half-fee) tier.
    pub gold_tier: u64,
    /// Score unlocking the zero-fee tier; must be ≥ `gold_tier`.
    pub zero_fee_tier: u64,
}

/// Emitted when a buyer's score changes, so indexers can reconstruct any
/// score from events alone.
///
/// Topics: `("credit_updated", buyer)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditUpdatedEvent {
    #[topic]
    pub buyer: Address,
    /// Score before the update (after decay).
    pub old_score: u64,
    /// Score after the update.
    pub new_score: u64,
    /// What moved the score.
    pub reason: ScoreChangeReason,
}

/// Why a score changed. Serialized discriminants are part of the public
/// interface; do not renumber.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ScoreChangeReason {
    /// A successful escrow completion.
    Completion = 1,
    /// A fraudulent dispute loss.
    Fraud = 2,
    /// Scheduled decay from inactivity.
    Decay = 3,
}

/// Reads the stored credit record for `buyer`, if any. Read-only.
pub fn get_record_impl(env: &Env, buyer: &Address) -> Option<CreditRecord> {
    env.storage()
        .persistent()
        .get(&CreditDataKey::Credit(buyer.clone()))
}

/// Reads the stored tier config, falling back to factory defaults.
pub fn get_config_impl(env: &Env) -> ScoreConfig {
    env.storage()
        .instance()
        .get(&CreditDataKey::ScoreConfig)
        .unwrap_or(ScoreConfig {
            gold_tier: DEFAULT_GOLD_TIER,
            zero_fee_tier: DEFAULT_ZERO_FEE_TIER,
        })
}

/// Applies inactivity decay to `score` given `last_update` and the current
/// ledger. Pure helper shared by all callers: one decay step per fully
/// elapsed [`DECAY_INTERVAL_LEDGERS`], proportional to the headroom above
/// [`SCORE_FLOOR`], floored at [`SCORE_FLOOR`].
pub fn apply_decay(score: u64, last_update: u32, current_ledger: u32) -> u64 {
    if current_ledger <= last_update {
        return score;
    }
    let elapsed = (current_ledger - last_update) as u64;
    let intervals = elapsed / DECAY_INTERVAL_LEDGERS as u64;
    if intervals == 0 {
        return score;
    }
    if score <= SCORE_FLOOR {
        return score;
    }
    // Headroom above the floor decays; u128 keeps the bps math exact.
    let headroom = (score - SCORE_FLOOR) as u128;
    let decay = headroom
        .saturating_mul(DECAY_RATE_BPS as u128)
        .saturating_mul(intervals as u128)
        / BPS_DENOMINATOR;
    let decayed = headroom.saturating_sub(decay) as u64;
    SCORE_FLOOR + decayed
}

/// Maps a score to the escrow fee in basis points for the buyer: the full
/// fee below the gold tier, half at gold, zero at the zero-fee tier.
pub fn fee_bps_for(score: u64, base_fee_bps: u64, config: &ScoreConfig) -> u64 {
    if score >= config.zero_fee_tier {
        0
    } else if score >= config.gold_tier {
        base_fee_bps / 2
    } else {
        base_fee_bps
    }
}

/// Persist a credit record and keep it live for the shared ~30-day TTL
/// policy.
pub fn store(env: &Env, buyer: &Address, record: &CreditRecord) {
    let key = CreditDataKey::Credit(buyer.clone());
    env.storage().persistent().set(&key, record);
    env.storage().persistent().extend_ttl(&key, 100, 518_400);
}
