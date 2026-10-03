//! Soulbound token types for the reputation registry (issue #450).
//!
//! The credential itself is deliberately inert: a [`SoulboundToken`] is a
//! plain `#[contracttype]` record with no methods that could move it, and it
//! lives under a storage key derived from its owner, so "holding" it and
//! "being it" are the same thing. The transfer/approve entry points exist as
//! explicit reverts (see [`lib.rs`]) so that callers who try the standard
//! token interface get a typed error instead of a missing-function trap.

use soroban_sdk::{contractevent, contracttype, Address, Env, String};

/// The tier of a soulbound credential.
///
/// Serialized discriminants are part of the public interface (indexers read
/// them from the WASM spec); do not renumber.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum CredentialClass {
    /// KYC-verified merchant: the entry-level credential.
    Verified = 1,
    /// Sustained-volume merchant with an unblemished record.
    Trusted = 2,
    /// Top-tier merchant: high volume, long tenure, zero adversarial
    /// outcomes.
    Premium = 3,
}

impl CredentialClass {
    /// Decode a caller-supplied class id, or `None` if it is not a valid
    /// [`CredentialClass`] discriminant.
    pub fn from_repr(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Verified),
            2 => Some(Self::Trusted),
            3 => Some(Self::Premium),
            _ => None,
        }
    }
}

/// A non-transferable credential permanently bound to one address.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoulboundToken {
    /// Monotonic token id assigned at mint time (0-based). Not unique after
    /// a revoke + re-issue, but the `(token_id, owner)` event topics pin the
    /// history for indexers.
    pub token_id: u64,
    /// Tier assigned at mint time. A credential is never upgraded in place;
    /// governance revokes and re-issues at the new tier.
    pub class: CredentialClass,
    /// Ledger at which the credential was minted.
    pub issued_at: u32,
    /// Whether governance slashed this credential.
    pub slashed: bool,
    /// Ledger at which the slash happened, if any.
    pub slashed_at: Option<u32>,
    /// Public reason recorded for the slash, if any.
    pub reason: Option<String>,
}

/// Storage keys for the merchant SBT feature (issue #450). Kept separate
/// from the badge and credit keys so the three features evolve
/// independently.
#[contracttype]
pub enum SbtDataKey {
    /// Persistent, one credential per merchant, keyed by owner.
    Sbt(Address),
    /// Persistent, one tombstone per revoked credential, keyed by owner:
    /// blocks re-issue and preserves the burn record for indexers.
    Revoked(Address),
    /// Instance: number of credentials ever issued; also the next token id.
    SbtCount,
}

/// Emitted when a soulbound credential is minted to a merchant.
///
/// Topics: `("sbt_issued", token_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SbtIssuedEvent {
    #[topic]
    pub token_id: u64,
    #[topic]
    pub owner: Address,
    pub class: CredentialClass,
}

/// Emitted when a soulbound credential is burned (unslashed) by governance.
///
/// Topics: `("sbt_revoked", token_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SbtRevokedEvent {
    #[topic]
    pub token_id: u64,
    #[topic]
    pub owner: Address,
}

/// Emitted when a soulbound credential is slashed by governance. The
/// credential stays bound to its holder as a public death record — it is
/// flagged, not deleted, so a slashed merchant cannot re-mint a clean one.
///
/// Topics: `("sbt_slashed", token_id, owner)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SbtSlashedEvent {
    #[topic]
    pub token_id: u64,
    #[topic]
    pub owner: Address,
    pub slashed_at: u32,
    pub reason: String,
}

/// Reads the credential owned by `owner`, if any. Read-only.
pub fn get_sbt_impl(env: &Env, owner: &Address) -> Option<SoulboundToken> {
    env.storage()
        .persistent()
        .get(&SbtDataKey::Sbt(owner.clone()))
}

/// Persist a credential and keep it live for the shared ~30-day TTL policy.
pub fn store(env: &Env, owner: &Address, sbt: &SoulboundToken) {
    let key = SbtDataKey::Sbt(owner.clone());
    env.storage().persistent().set(&key, sbt);
    env.storage().persistent().extend_ttl(&key, 100, 518_400);
}

/// Deletes a credential (revoke) and writes the re-issue-blocking tombstone.
pub fn burn(env: &Env, owner: &Address) -> SoulboundToken {
    let key = SbtDataKey::Sbt(owner.clone());
    let sbt: SoulboundToken = env.storage().persistent().get(&key).unwrap();
    env.storage().persistent().remove(&key);
    let tombstone = SbtDataKey::Revoked(owner.clone());
    env.storage().persistent().set(&tombstone, &true);
    env.storage()
        .persistent()
        .extend_ttl(&tombstone, 100, 518_400);
    sbt
}
