//! On-chain reputation for Accensa.
//!
//! Three independent features share this contract:
//!
//! - **Merchant soulbound tokens** (issue #450): non-transferable KYC /
//!   volume-tier credentials minted and governed by the bound authority —
//!   see [`sbt`]. `transfer` and `approve` exist only to revert.
//! - **Arbitrator badges** (issue #451): tiered NFT badges minted and
//!   upgraded from a lifetime record of accurate dispute resolutions — see
//!   [`badges`].
//! - **Buyer credit scoring** (issue #452): a dynamic 0–1000 score per buyer
//!   driven by escrow outcomes, with inactivity decay and zero-fee tiers —
//!   see [`credit_score`].
//!
//! # Access model
//!
//! - `initialize` binds a single authority address (governance multisig, the
//!   dispute-resolution contract, or the escrow/settlement contract). Only
//!   that address may record resolutions, score events, or issue, revoke
//!   and slash credentials — delegated record-keeping, never claimed.
//! - Everything else is read-only, so indexers, bazaar listings and fee
//!   calculators can gate on credential class, badge tier or credit score
//!   without paying for auth.
//!
//! # MVP cuts
//!
//! Deliberately out of scope: token metadata URIs, per-dispute inaccuracy
//! tracking, credential expiry, and self-reported credit history.

#![no_std]

mod badges;
#[cfg(test)]
mod badges_test;
mod credit_score;
#[cfg(test)]
mod credit_score_test;
mod sbt;
#[cfg(test)]
mod sbt_test;

use badges::{Badge, BadgeTier, Error};
use soroban_sdk::{
    contract, contracterror, contractimpl, contractmeta, Address, Env, String as SdkString,
};

pub use badges::{
    BadgeMintedEvent, BadgeUpgradedEvent, DataKey as BadgeDataKey, ResolutionRecordedEvent,
    BRONZE_THRESHOLD, GOLD_THRESHOLD, SILVER_THRESHOLD,
};
pub use credit_score::{
    apply_decay, fee_bps_for, CreditDataKey, CreditRecord, CreditUpdatedEvent, ScoreChangeReason,
    ScoreConfig, DECAY_INTERVAL_LEDGERS, DECAY_RATE_BPS, DEFAULT_GOLD_TIER, DEFAULT_ZERO_FEE_TIER,
    GROWTH_RATE_BPS, MAX_SCORE, PENALTY_RATE_BPS, SCORE_FLOOR, STARTING_SCORE,
};
pub use sbt::{
    CredentialClass, SbtDataKey, SbtIssuedEvent, SbtRevokedEvent, SbtSlashedEvent, SoulboundToken,
};

/// Reads the bound authority address, failing with
/// [`CreditError::NotInitialized`] if absent.
fn require_authority(env: &Env) -> Result<Address, CreditError> {
    env.storage()
        .instance()
        .get(&BadgeDataKey::Admin)
        .ok_or(CreditError::NotInitialized)
}

contractmeta!(key = "name", val = "AccensaReputation");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Error variants added for buyer credit scoring (issue #452). The badge
/// variants live in [`badges::Error`]; keeping the additions separate keeps
/// the #451 discriminants stable.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum CreditError {
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// The caller is not the bound escrow authority.
    Unauthorized = 3,
    /// A re-tuned tier cut-off is out of range or ordered wrong
    /// (`zero_fee_tier` must be ≥ `gold_tier`, both ≤ [`MAX_SCORE`]).
    InvalidConfig = 7,
    /// The escrow id has already been recorded (replay protection).
    EscrowAlreadyRecorded = 8,
}

/// Error variants for merchant soulbound tokens (issue #450). The badge
/// variants live in [`badges::Error`] and the credit variants in
/// [`CreditError`]; keeping the additions separate keeps each feature's
/// discriminants stable.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum SbtError {
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// The merchant already holds a soulbound credential.
    AlreadyIssued = 7,
    /// No soulbound credential exists for this address.
    SbtNotFound = 8,
    /// The credential class is not a valid [`CredentialClass`] value.
    InvalidClass = 9,
    /// The credential is already slashed.
    AlreadySlashed = 10,
    /// A soulbound credential was revoked; it can never be re-issued.
    Revoked = 11,
    /// Soulbound credentials cannot move: `transfer` always reverts.
    SbtNonTransferable = 12,
    /// Soulbound credentials cannot be delegated: `approve` always reverts.
    SbtApprovalDisabled = 13,
}

#[contract]
pub struct Reputation;

#[contractimpl]
impl Reputation {
    /// Bind this instance to `admin` — the arbiter authority (dispute
    /// resolution contract or governance multisig) that alone may record
    /// resolutions and thereby mint or upgrade badges.
    ///
    /// # Errors
    /// - `AlreadyInitialized`: called twice.
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&BadgeDataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&BadgeDataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&BadgeDataKey::BadgeCount, &0u64);
        env.storage().instance().set(&SbtDataKey::SbtCount, &0u64);
        Ok(())
    }

    /// Record an **accurate** dispute resolution by `arbitrator`, minting
    /// their Bronze badge on the first accepted resolution and upgrading the
    /// badge's tier in place when a lifetime threshold is crossed. Admin
    /// (arbiter authority) only.
    ///
    /// `dispute_id` is ledger-scoped replay protection supplied by the
    /// caller; recording the same id twice is rejected.
    ///
    /// Returns the arbitrator's post-call lifetime accurate count.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `Unauthorized`: caller is not the arbiter authority.
    /// - `DisputeAlreadyRecorded`: `dispute_id` was already consumed.
    /// - `InaccurateOutcome`: `accurate` is `false` — a badge only tracks
    ///   accurate resolutions.
    pub fn record_resolution(
        env: Env,
        arbitrator: Address,
        dispute_id: u64,
        accurate: bool,
    ) -> Result<u64, Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&BadgeDataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        if !accurate {
            return Err(Error::InaccurateOutcome);
        }

        let dispute_key = BadgeDataKey::ResolutionDispute(dispute_id);
        if env.storage().persistent().has(&dispute_key) {
            return Err(Error::DisputeAlreadyRecorded);
        }

        let mut badge = match badges::get_badge_impl(&env, &arbitrator) {
            Some(b) => b,
            None => {
                // Mint: one badge per arbitrator, token ids monotonic.
                let token_id: u64 = env
                    .storage()
                    .instance()
                    .get(&BadgeDataKey::BadgeCount)
                    .unwrap_or(0);
                env.storage()
                    .instance()
                    .set(&BadgeDataKey::BadgeCount, &(token_id + 1));

                let tier = BadgeTier::for_accurate(1).unwrap_or(BadgeTier::Bronze);
                let badge = Badge {
                    token_id,
                    owner: arbitrator.clone(),
                    tier,
                    accurate_resolutions: 1,
                    minted_at: env.ledger().sequence(),
                    last_resolution_ledger: env.ledger().sequence(),
                };
                badges::store(&env, &arbitrator, &badge);
                badges::mark_dispute(&env, dispute_id, &arbitrator);

                BadgeMintedEvent {
                    token_id,
                    owner: arbitrator.clone(),
                    tier: badge.tier,
                }
                .publish(&env);
                ResolutionRecordedEvent {
                    dispute_id,
                    owner: arbitrator.clone(),
                    accurate_resolutions: 1,
                    ledger: env.ledger().sequence(),
                }
                .publish(&env);
                return Ok(1);
            }
        };

        // Upgrade path: dedupe check first, then effects.
        let prev_tier = badge.tier;
        badge.accurate_resolutions += 1;
        badge.last_resolution_ledger = env.ledger().sequence();
        if let Some(new_tier) = BadgeTier::for_accurate(badge.accurate_resolutions) {
            if new_tier > prev_tier {
                badge.tier = new_tier;
            }
        }
        badges::store(&env, &arbitrator, &badge);
        badges::mark_dispute(&env, dispute_id, &arbitrator);

        if badge.tier > prev_tier {
            BadgeUpgradedEvent {
                token_id: badge.token_id,
                owner: arbitrator.clone(),
                from_tier: prev_tier,
                to_tier: badge.tier,
            }
            .publish(&env);
        }
        ResolutionRecordedEvent {
            dispute_id,
            owner: arbitrator.clone(),
            accurate_resolutions: badge.accurate_resolutions,
            ledger: env.ledger().sequence(),
        }
        .publish(&env);

        Ok(badge.accurate_resolutions)
    }

    /// Returns the badge held by `arbitrator`, if any. Read-only.
    pub fn get_badge(env: Env, arbitrator: Address) -> Option<Badge> {
        badges::get_badge_impl(&env, &arbitrator)
    }

    /// Returns the tier held by `arbitrator`, if any. Read-only.
    pub fn get_tier(env: Env, arbitrator: Address) -> Option<BadgeTier> {
        badges::get_badge_impl(&env, &arbitrator).map(|b| b.tier)
    }

    /// Returns the bound arbiter authority, if initialized. Read-only.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&BadgeDataKey::Admin)
    }

    /// Returns the number of badges ever minted. Read-only.
    pub fn total_badges(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&badges::DataKey::BadgeCount)
            .unwrap_or(0)
    }

    /// Returns the lifetime accuracy threshold for each tier. Read-only;
    /// clients should discover thresholds via this getter rather than
    /// hard-coding them.
    pub fn get_thresholds(_env: Env) -> (u64, u64, u64) {
        (
            badges::BRONZE_THRESHOLD,
            badges::SILVER_THRESHOLD,
            badges::GOLD_THRESHOLD,
        )
    }

    // ── Buyer credit scoring (issue #452) ─────────────────────────────────

    /// Record a **successful escrow completion** for `buyer`, growing their
    /// score along the growth curve and applying any pending inactivity
    /// decay first. Escrow authority only.
    ///
    /// `escrow_id` is ledger-scoped replay protection supplied by the
    /// authority; recording the same id twice is rejected.
    ///
    /// Returns the buyer's post-call score.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `Unauthorized`: caller is not the escrow authority.
    /// - `EscrowAlreadyRecorded`: `escrow_id` was already consumed.
    pub fn record_completion(env: Env, buyer: Address, escrow_id: u64) -> Result<u64, CreditError> {
        require_authority(&env)?.require_auth();

        let escrow_key = CreditDataKey::CompletionEscrow(escrow_id);
        if env.storage().persistent().has(&escrow_key) {
            return Err(CreditError::EscrowAlreadyRecorded);
        }

        let mut record = credit_score::get_record_impl(&env, &buyer).unwrap_or(CreditRecord {
            score: STARTING_SCORE,
            completions: 0,
            fraud_count: 0,
            last_update: env.ledger().sequence(),
        });
        let old_score =
            credit_score::apply_decay(record.score, record.last_update, env.ledger().sequence());

        // Growth: a fraction of the remaining headroom to MAX_SCORE.
        let headroom = MAX_SCORE - old_score;
        let growth = (headroom as u128 * GROWTH_RATE_BPS as u128 / 10_000) as u64;
        record.score = old_score + growth;
        record.completions += 1;
        record.last_update = env.ledger().sequence();

        credit_score::store(&env, &buyer, &record);
        env.storage().persistent().set(&escrow_key, &buyer);
        env.storage()
            .persistent()
            .extend_ttl(&escrow_key, 100, 518_400);

        CreditUpdatedEvent {
            buyer: buyer.clone(),
            old_score,
            new_score: record.score,
            reason: ScoreChangeReason::Completion,
        }
        .publish(&env);

        Ok(record.score)
    }

    /// Record a **fraudulent dispute loss** for `buyer`, dropping their
    /// score along the penalty curve (a fraction of the *current* score, so
    /// the higher the score the harder the fall) and applying any pending
    /// inactivity decay first. Escrow authority only.
    ///
    /// Returns the buyer's post-call score.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `Unauthorized`: caller is not the escrow authority.
    pub fn record_fraud(env: Env, buyer: Address) -> Result<u64, CreditError> {
        require_authority(&env)?.require_auth();

        let mut record = credit_score::get_record_impl(&env, &buyer).unwrap_or(CreditRecord {
            score: STARTING_SCORE,
            completions: 0,
            fraud_count: 0,
            last_update: env.ledger().sequence(),
        });
        let old_score =
            credit_score::apply_decay(record.score, record.last_update, env.ledger().sequence());

        // Penalty: a fraction of the current score.
        let penalty = (old_score as u128 * PENALTY_RATE_BPS as u128 / 10_000) as u64;
        record.score = old_score - penalty;
        record.fraud_count += 1;
        record.last_update = env.ledger().sequence();

        credit_score::store(&env, &buyer, &record);

        CreditUpdatedEvent {
            buyer: buyer.clone(),
            old_score,
            new_score: record.score,
            reason: ScoreChangeReason::Fraud,
        }
        .publish(&env);

        Ok(record.score)
    }

    /// Re-tune the zero-fee tier cut-offs. Escrow authority only.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `Unauthorized`: caller is not the escrow authority.
    /// - `InvalidConfig`: `zero_fee_tier` < `gold_tier`, either exceeds
    ///   `MAX_SCORE`, or `gold_tier` is `0` (scores below the gold tier pay
    ///   the full fee, so a zero gold cut-off would be meaningless).
    pub fn set_score_config(
        env: Env,
        gold_tier: u64,
        zero_fee_tier: u64,
    ) -> Result<(), CreditError> {
        require_authority(&env)?.require_auth();
        if zero_fee_tier < gold_tier || zero_fee_tier > MAX_SCORE || gold_tier == 0 {
            return Err(CreditError::InvalidConfig);
        }
        env.storage().instance().set(
            &CreditDataKey::ScoreConfig,
            &ScoreConfig {
                gold_tier,
                zero_fee_tier,
            },
        );
        Ok(())
    }

    /// Returns the buyer's current score, applying pending inactivity decay
    /// without recording anything. Buyers with no history get the neutral
    /// [`STARTING_SCORE`]. Read-only.
    pub fn get_score(env: Env, buyer: Address) -> u64 {
        match credit_score::get_record_impl(&env, &buyer) {
            None => STARTING_SCORE,
            Some(record) => {
                credit_score::apply_decay(record.score, record.last_update, env.ledger().sequence())
            }
        }
    }

    /// Returns the buyer's full credit record, if they have one. Read-only.
    pub fn get_credit_record(env: Env, buyer: Address) -> Option<CreditRecord> {
        credit_score::get_record_impl(&env, &buyer)
    }

    /// Returns the active tier configuration, including factory defaults if
    /// the authority has not re-tuned it. Read-only.
    pub fn get_score_config(env: Env) -> ScoreConfig {
        credit_score::get_config_impl(&env)
    }

    /// Maps a score to the escrow fee in basis points: full fee below the
    /// gold tier, half at gold, zero at the zero-fee tier. Read-only.
    pub fn get_fee_bps(env: Env, buyer: Address, base_fee_bps: u64) -> u64 {
        let score = Self::get_score(env.clone(), buyer);
        credit_score::fee_bps_for(score, base_fee_bps, &credit_score::get_config_impl(&env))
    }

    // ── Merchant soulbound tokens (issue #450) ──────────────────────────

    /// Mint a non-transferable soulbound credential of `class` to
    /// `merchant`. Governance (the bound authority) only.
    ///
    /// Returns the new token id. A revoked address can never be re-issued.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `InvalidClass`: `class` is not a valid [`CredentialClass`].
    /// - `Revoked`: the merchant's credential was revoked — permanent.
    /// - `AlreadyIssued`: the merchant already holds a credential,
    ///   including a slashed one, which stays bound as a public record.
    pub fn issue(env: Env, merchant: Address, class: u32) -> Result<u64, SbtError> {
        sbt_authority(&env)?.require_auth();

        let class = CredentialClass::from_repr(class).ok_or(SbtError::InvalidClass)?;
        if env
            .storage()
            .persistent()
            .has(&SbtDataKey::Revoked(merchant.clone()))
        {
            return Err(SbtError::Revoked);
        }
        if sbt::get_sbt_impl(&env, &merchant).is_some() {
            return Err(SbtError::AlreadyIssued);
        }

        let token_id: u64 = env
            .storage()
            .instance()
            .get(&SbtDataKey::SbtCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&SbtDataKey::SbtCount, &(token_id + 1));

        let credential = SoulboundToken {
            token_id,
            class,
            issued_at: env.ledger().sequence(),
            slashed: false,
            slashed_at: None,
            reason: None,
        };
        sbt::store(&env, &merchant, &credential);

        SbtIssuedEvent {
            token_id,
            owner: merchant.clone(),
            class,
        }
        .publish(&env);
        Ok(token_id)
    }

    /// Burn the credential held by `merchant` (governance only) and write a
    /// permanent tombstone that blocks re-issue. Returns the burned token
    /// id.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `SbtNotFound`: the merchant holds no credential.
    pub fn revoke(env: Env, merchant: Address) -> Result<u64, SbtError> {
        sbt_authority(&env)?.require_auth();

        if sbt::get_sbt_impl(&env, &merchant).is_none() {
            return Err(SbtError::SbtNotFound);
        }
        let credential = sbt::burn(&env, &merchant);

        SbtRevokedEvent {
            token_id: credential.token_id,
            owner: merchant.clone(),
        }
        .publish(&env);
        Ok(credential.token_id)
    }

    /// Flag the credential held by `merchant` as slashed (governance only).
    /// The credential stays bound to its holder as a public death record:
    /// it cannot be transferred, and it cannot be re-issued while present.
    /// Returns the token id.
    ///
    /// # Errors
    /// - `NotInitialized`: before `initialize`.
    /// - `SbtNotFound`: the merchant holds no credential.
    /// - `AlreadySlashed`: the credential is already slashed.
    pub fn slash(env: Env, merchant: Address, reason: SdkString) -> Result<u64, SbtError> {
        sbt_authority(&env)?.require_auth();

        let mut credential = sbt::get_sbt_impl(&env, &merchant).ok_or(SbtError::SbtNotFound)?;
        if credential.slashed {
            return Err(SbtError::AlreadySlashed);
        }
        credential.slashed = true;
        credential.slashed_at = Some(env.ledger().sequence());
        credential.reason = Some(reason.clone());
        sbt::store(&env, &merchant, &credential);

        SbtSlashedEvent {
            token_id: credential.token_id,
            owner: merchant.clone(),
            slashed_at: env.ledger().sequence(),
            reason,
        }
        .publish(&env);
        Ok(credential.token_id)
    }

    /// Soulbound credentials cannot move. Exists only so callers trying the
    /// standard token interface get a typed revert instead of a
    /// missing-function trap; no authorization is required because the call
    /// can never do anything.
    ///
    /// # Errors
    /// - Always: `SbtNonTransferable`.
    pub fn transfer(
        _env: Env,
        _from: Address,
        _to: Address,
        _token_id: u64,
    ) -> Result<(), SbtError> {
        Err(SbtError::SbtNonTransferable)
    }

    /// Soulbound credentials cannot be delegated. Exists only so callers
    /// trying the standard token interface get a typed revert instead of a
    /// missing-function trap.
    ///
    /// # Errors
    /// - Always: `SbtApprovalDisabled`.
    pub fn approve(_env: Env, _spender: Address, _token_id: u64) -> Result<(), SbtError> {
        Err(SbtError::SbtApprovalDisabled)
    }

    /// Returns the soulbound credential held by `merchant`, if any.
    /// Read-only.
    pub fn get_sbt(env: Env, merchant: Address) -> Option<SoulboundToken> {
        sbt::get_sbt_impl(&env, &merchant)
    }

    /// Returns the number of credentials ever issued. Read-only.
    pub fn total_sbt(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&SbtDataKey::SbtCount)
            .unwrap_or(0)
    }
}

/// Reads the bound governance authority, failing with
/// [`SbtError::NotInitialized`] if absent.
fn sbt_authority(env: &Env) -> Result<Address, SbtError> {
    env.storage()
        .instance()
        .get(&BadgeDataKey::Admin)
        .ok_or(SbtError::NotInitialized)
}
