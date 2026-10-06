//! Discount coupon NFTs for merchant stores (issue #453).
//!
//! A coupon is a non-fungible token that grants its holder a percentage
//! discount on a single escrow deposit to this vault. Discounts are expressed
//! in basis points (1 bp = 0.01 %). A coupon is **single-use**: it is marked
//! redeemed on first use and rejected on any subsequent attempt, preventing
//! double-use.

use accensa_common::{math::apply_fee_bps, Error};
use soroban_sdk::{contracttype, Address, Env};

use crate::{DataKey, TTL_EXTEND, TTL_THRESHOLD};

/// Maximum discount any single coupon may grant (50 %).
pub const MAX_COUPON_DISCOUNT_BPS: u32 = 5_000;

/// On-chain record for a single discount coupon NFT.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CouponRecord {
    /// The address authorised to redeem this coupon.
    pub owner: Address,
    /// Discount rate in basis points (0 – [`MAX_COUPON_DISCOUNT_BPS`]).
    pub discount_bps: u32,
    /// `true` once the coupon has been applied to a deposit.
    pub redeemed: bool,
}

/// Mint a new coupon and persist it under `coupon_id`.
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

/// Verify ownership, compute the discounted amount, and mark the coupon as redeemed.
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

    let discount = apply_fee_bps(amount, record.discount_bps).map_err(Error::from)?;
    let effective_amount = amount
        .checked_sub(discount)
        .ok_or(Error::MathOverflow)?;

    record.redeemed = true;
    env.storage().persistent().set(&key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND);

    Ok(effective_amount)
}

/// Read-only accessor for a coupon record.
pub fn get_coupon(env: &Env, coupon_id: u64) -> Option<CouponRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::Coupon(coupon_id))
}
