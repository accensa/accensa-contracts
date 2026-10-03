//! Tiered dispute-resolution NFT badges for arbitrators (issue #451).
//!
//! Each arbitrator holds exactly one **badge**, a non-transferable
//! non-fungible token whose `tier` encodes their lifetime record of accurate
//! dispute resolutions:
//!
//! | Tier | Threshold (accurate resolutions) |
//! |---|---|
//! | [`BadgeTier::Bronze`] | `BRONZE_THRESHOLD` = 10 |
//! | [`BadgeTier::Silver`] | `SILVER_THRESHOLD` = 50 |
//! | [`BadgeTier::Gold`] | `GOLD_THRESHOLD` = 100 |
//!
//! Record-keeping is delegated, never claimed: `record_resolution` is
//! restricted to the admin **of the dispute-resolution contract** bound at
//! initialization, and every accepted resolution carries the ledger-scoped
//! `dispute_id` the arbiter resolved. The contract dedupes those ids, so a
//! replayed dispute cannot inflate an arbitrator's accuracy count, and no one
//! can mint themselves a Gold badge by calling in with a large number.
//!
//! # Minting model
//!
//! - A badge is **minted automatically** on the arbitrator's first accepted
//!   resolution (`record_resolution` mints Bronze when no badge exists) —
//!   there is no permissionless `mint` entry point.
//! - Tiers are **upgraded in place** when the lifetime count crosses a
//!   threshold; the badge's token id, mint ledger and dispute history are
//!   preserved, and a `BadgeUpgraded` event marks the transition.
//!
//! # Non-transferability by construction
//!
//! There is intentionally no `transfer`, `approve` or `transfer_from` entry
//! point, so a badge can never be bought, borrowed or farmed. Its whole value
//! is that it is worth nothing to anyone but the arbitrator it tracks.
//!
//! # Storage shape
//!
//! - `Badge(Address)` — persistent, one entry per badge: owner, tier, token
//!   id, mint ledger and lifetime accuracy stats.
//! - `ResolutionDispute(u64)` — persistent, one tombstone per consumed
//!   `dispute_id` (owner only; a `u64` keeps it a single small entry).
//! - `Admin` / `BadgeCount` — instance: the arbiter authority and the
//!   monotonic token id source.

use soroban_sdk::{contracterror, contractevent, contracttype, Address, Env};

/// Lifetime accuracy thresholds for each tier.
pub const BRONZE_THRESHOLD: u64 = 10;
pub const SILVER_THRESHOLD: u64 = 50;
pub const GOLD_THRESHOLD: u64 = 100;

/// The tier of an arbitrator's dispute-resolution badge.
///
/// Serialized discriminants are part of the public interface (indexers read
/// them from the WASM spec); do not renumber.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum BadgeTier {
    /// Entry tier: minted on the arbitrator's first accurate resolution.
    Bronze = 1,
    /// Crossed [`SILVER_THRESHOLD`] accurate resolutions.
    Silver = 2,
    /// Crossed [`GOLD_THRESHOLD`] accurate resolutions.
    Gold = 3,
}

impl BadgeTier {
    /// The lowest tier reachable at `accurate` lifetime accurate resolutions,
    /// or `None` below the entry threshold.
    pub fn for_accurate(accurate: u64) -> Option<Self> {
        if accurate >= GOLD_THRESHOLD {
            Some(Self::Gold)
        } else if accurate >= SILVER_THRESHOLD {
            Some(Self::Silver)
        } else if accurate >= BRONZE_THRESHOLD {
            Some(Self::Bronze)
        } else {
            None
        }
    }
}

/// Errors are local to this contract rather than added to a shared enum: every
/// contract that exposes a shared error enum embeds all of its variants in its
/// WASM spec, so growing it would enlarge unrelated contracts.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// `initialize` was called after the contract was already initialized.
    AlreadyInitialized = 1,
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// The caller is not the bound arbiter authority.
    Unauthorized = 3,
    /// The dispute id has already been recorded (replay protection).
    DisputeAlreadyRecorded = 4,
    /// A resolution was recorded without an accurate outcome, which is the
    /// only thing a badge tracks.
    InaccurateOutcome = 5,
    /// The arbitrator holds no badge (nothing to read or upgrade).
    BadgeNotFound = 6,
}

#[contracttype]
pub enum DataKey {
    /// Instance: the arbiter authority that alone may record resolutions.
    Admin,
    /// Instance: number of badges ever minted; also the next token id.
    BadgeCount,
    /// Persistent, one entry per badge, keyed by owner.
    Badge(Address),
    /// Persistent, one tombstone per consumed dispute id.
    ResolutionDispute(u64),
}

/// A non-transferable NFT badge bound to one arbitrator.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Badge {
    /// Monotonic token id assigned at mint time (0-based). Stable across
    /// tier upgrades.
    pub token_id: u64,
    /// Badge owner. Redundant with the storage key on purpose: it keeps the
    /// badge self-describing for off-chain indexers reading the WASM spec.
    pub owner: Address,
    /// Current tier. Upgraded in place as accuracy thresholds are crossed.
    pub tier: BadgeTier,
    /// Lifetime count of accurate dispute resolutions.
    pub accurate_resolutions: u64,
    /// Ledger at which the badge was minted.
    pub minted_at: u32,
    /// Ledger of the most recent recorded resolution.
    pub last_resolution_ledger: u32,
}

/// Emitted when a badge is minted to an arbitrator (first accurate
/// resolution).
///
/// Topics: `("badge_minted", token_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BadgeMintedEvent {
    #[topic]
    pub token_id: u64,
    #[topic]
    pub owner: Address,
    pub tier: BadgeTier,
}

/// Emitted when a badge's tier is raised in place.
///
/// Topics: `("badge_upgraded", token_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BadgeUpgradedEvent {
    #[topic]
    pub token_id: u64,
    #[topic]
    pub owner: Address,
    /// Tier held before the upgrade.
    pub from_tier: BadgeTier,
    /// Tier held after the upgrade.
    pub to_tier: BadgeTier,
}

/// Emitted on every accepted resolution, including non-upgrading and
/// mint-triggering ones, so indexers can reconstruct counts from events alone.
///
/// Topics: `("resolution_recorded", dispute_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionRecordedEvent {
    #[topic]
    pub dispute_id: u64,
    #[topic]
    pub owner: Address,
    pub accurate_resolutions: u64,
    pub ledger: u32,
}

/// Reads the badge owned by `owner`, if any. Read-only.
pub fn get_badge_impl(env: &Env, owner: &Address) -> Option<Badge> {
    env.storage()
        .persistent()
        .get(&DataKey::Badge(owner.clone()))
}

/// Persist a badge and keep it live for the shared ~30-day TTL policy.
pub fn store(env: &Env, owner: &Address, badge: &Badge) {
    let key = DataKey::Badge(owner.clone());
    env.storage().persistent().set(&key, badge);
    env.storage().persistent().extend_ttl(&key, 100, 518_400);
}

/// Marks `dispute_id` as consumed for `owner` and keeps the tombstone live.
pub fn mark_dispute(env: &Env, dispute_id: u64, owner: &Address) {
    let key = DataKey::ResolutionDispute(dispute_id);
    env.storage().persistent().set(&key, owner);
    env.storage().persistent().extend_ttl(&key, 100, 518_400);
}
