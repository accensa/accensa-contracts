//! Discount coupon NFTs for merchant stores (issue #453).
//!
//! A coupon is a non-fungible token that grants its holder a percentage
//! discount on a single escrow deposit to this vault. Discounts are expressed
//! in basis points (1 bp = 0.01 %). A coupon is **single-use**: it is marked
//! redeemed on first use and rejected on any subsequent attempt, preventing
//! double-use.
//!
//! # Storage layout
//!
//! `DataKey::Coupon(u64)` — Persistent, one [`CouponRecord`] per coupon id.
//! The id is assigned by the merchant at mint time and must be unique within
//! this vault instance.
//!
//! # Discount math
//!
//! Uses [`accensa_common::math::apply_fee_bps`] (floor / truncating division)
//! so the merchant never grants more discount than the configured rate. The
//! effective deposit amount passed to the token transfer is:
//!
//! ```text
//! effective_amount = original_amount - floor(original_amount * discount_bps / 10_000)
//! ```
//!
//! # Ownership and auth
//!
//! Coupon ownership is verified inside [`apply_coupon`] against the `caller`
//! address supplied by the vault's `deposit` entry point. The vault already
//! enforces `from == admin` and `admin.require_auth()` before calling here,
//! so no additional auth check is needed in this module.

use accensa_common::{math::apply_fee_bps, Error};
use soroban_sdk::{contracttype, Address, Env};

use crate::{DataKey, TTL_EXTEND, TTL_THRESHOLD};

/// Maximum discount any single coupon may grant (50 %).
///
/// Kept below 100 % so a coupon can never make a deposit free, which would
/// allow griefing the vault's float accounting.
pub const MAX_COUPON_DISCOUNT_BPS: u32 = 5_000;

/// On-chain record for a single discount coupon NFT.
///
/// Stored under [`DataKey::Coupon`]`(id)` in **persistent** storage so it
/// survives across ledger epochs until the coupon is redeemed or expires
/// naturally with its TTL.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CouponRecord {
    /// The address authorised to redeem this coupon.
    pub owner: Address,
    /// Discount rate in basis points (0 – [`MAX_COUPON_DISCOUNT_BPS`]).
    pub discount_bps: u32,
    /// `true` once the coupon has been applied to a deposit; further
    /// redemption attempts are rejected with [`Error::CouponAlreadyRedeemed`].
    pub redeemed: bool,
}

/// Mint a new coupon and persist it under `coupon_id`.
///
/// # Errors
///
/// - [`Error::InvalidRatio`] — `discount_bps` exceeds [`MAX_COUPON_DISCOUNT_BPS`].
/// - [`Error::AlreadyInitialized`] — a coupon with `coupon_id` already exists
///   in storage; ids must be unique within a vault instance.
///
/// # Auth
///
/// The caller is responsible for ensuring only the vault admin may call this
/// function (enforced in the `RefundVault::mint_coupon` entry point).
pub fn mint_coupon(
    env: &Env,
    coupon_id: u64,
    owner: Address,
    discount_bps: u32,
) -> Result<(), Error> {
    if discount_bps > MAX_COUPON_DISCOUNT_BPS {
        return Err(Error::InvalidRatio);
    }

    let key = DataKey::Coupon(coupon_id);
    if env.storage().persistent().has(&key) {
        // Coupon id collision — the merchant must use a fresh id.
        return Err(Error::AlreadyInitialized);
    }

    let record = CouponRecord {
        owner,
        discount_bps,
        redeemed: false,
    };
    env.storage().persistent().set(&key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND);

    Ok(())
}

/// Verify ownership of `coupon_id`, compute the discounted amount, and mark
/// the coupon as redeemed — all atomically.
///
/// Returns `Ok(effective_amount)` where `effective_amount <= amount`. The
/// caller (the vault's `deposit`) should transfer `effective_amount` instead
/// of `amount`.
///
/// # Errors
///
/// - [`Error::CouponNotFound`] — no coupon exists for `coupon_id`.
/// - [`Error::Unauthorized`] — `caller` is not the coupon's owner.
/// - [`Error::CouponAlreadyRedeemed`] — the coupon was previously consumed.
/// - [`Error::MathOverflow`] — discount arithmetic overflowed (unreachable for
///   valid inputs but checked for safety).
///
/// # Atomicity
///
/// The coupon is marked redeemed in the same storage transaction as the
/// discount computation. This function must be called **inside** the vault's
/// reentrancy lock and **after** all other deposit preconditions have been
/// verified, so that a failure after this point (e.g. the token transfer
/// reverting) still rolls back the storage write via the host's atomic
/// transaction semantics.
pub fn apply_coupon(env: &Env, caller: &Address, coupon_id: u64, amount: i128) -> Result<i128, Error> {
    let key = DataKey::Coupon(coupon_id);
    let mut record: CouponRecord = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::CouponNotFound)?;

    if &record.owner != caller {
        return Err(Error::Unauthorized);
    }
    if record.redeemed {
        return Err(Error::CouponAlreadyRedeemed);
    }

    // discount = floor(amount * discount_bps / 10_000)
    let discount = apply_fee_bps(amount, record.discount_bps).map_err(Error::from)?;
    let effective_amount = amount
        .checked_sub(discount)
        .ok_or(Error::MathOverflow)?;

    // Consume the coupon — single-use.
    record.redeemed = true;
    env.storage().persistent().set(&key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND);

    Ok(effective_amount)
}

/// Read-only accessor for a coupon record. Returns `None` when the coupon id
/// does not exist or its persistent entry has expired.
pub fn get_coupon(env: &Env, coupon_id: u64) -> Option<CouponRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::Coupon(coupon_id))
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn setup() -> (Env, Address) {
        let env = Env::default();
        let owner = Address::generate(&env);
        (env, owner)
    }

    #[test]
    fn mint_and_get_roundtrip() {
        let (env, owner) = setup();
        mint_coupon(&env, 1, owner.clone(), 1_000).unwrap();
        let rec = get_coupon(&env, 1).unwrap();
        assert_eq!(rec.owner, owner);
        assert_eq!(rec.discount_bps, 1_000);
        assert!(!rec.redeemed);
    }

    #[test]
    fn apply_coupon_reduces_amount() {
        let (env, owner) = setup();
        // 10 % discount on 1_000 → effective = 900
        mint_coupon(&env, 2, owner.clone(), 1_000).unwrap();
        let effective = apply_coupon(&env, &owner, 2, 1_000).unwrap();
        assert_eq!(effective, 900);
    }

    #[test]
    fn apply_coupon_marks_redeemed() {
        let (env, owner) = setup();
        mint_coupon(&env, 3, owner.clone(), 500).unwrap();
        apply_coupon(&env, &owner, 3, 2_000).unwrap();
        let rec = get_coupon(&env, 3).unwrap();
        assert!(rec.redeemed);
    }

    #[test]
    fn double_redemption_rejected() {
        let (env, owner) = setup();
        mint_coupon(&env, 4, owner.clone(), 200).unwrap();
        apply_coupon(&env, &owner, 4, 500).unwrap();
        let err = apply_coupon(&env, &owner, 4, 500).unwrap_err();
        assert_eq!(err, Error::CouponAlreadyRedeemed);
    }

    #[test]
    fn wrong_owner_rejected() {
        let (env, owner) = setup();
        let other = Address::generate(&env);
        mint_coupon(&env, 5, owner.clone(), 300).unwrap();
        let err = apply_coupon(&env, &other, 5, 1_000).unwrap_err();
        assert_eq!(err, Error::Unauthorized);
    }

    #[test]
    fn missing_coupon_rejected() {
        let (env, owner) = setup();
        let err = apply_coupon(&env, &owner, 99, 1_000).unwrap_err();
        assert_eq!(err, Error::CouponNotFound);
    }

    #[test]
    fn discount_above_max_rejected() {
        let (env, owner) = setup();
        let err = mint_coupon(&env, 6, owner.clone(), MAX_COUPON_DISCOUNT_BPS + 1).unwrap_err();
        assert_eq!(err, Error::InvalidRatio);
    }

    #[test]
    fn duplicate_coupon_id_rejected() {
        let (env, owner) = setup();
        mint_coupon(&env, 7, owner.clone(), 100).unwrap();
        let err = mint_coupon(&env, 7, owner.clone(), 200).unwrap_err();
        assert_eq!(err, Error::AlreadyInitialized);
    }

    #[test]
    fn zero_discount_effective_amount_unchanged() {
        let (env, owner) = setup();
        mint_coupon(&env, 8, owner.clone(), 0).unwrap();
        let effective = apply_coupon(&env, &owner, 8, 1_000).unwrap();
        assert_eq!(effective, 1_000);
    }
}
