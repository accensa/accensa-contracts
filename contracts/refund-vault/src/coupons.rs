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
pub fn apply_coupon(
    env: &Env,
    caller: &Address,
    coupon_id: u64,
    amount: i128,
) -> Result<i128, Error> {
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
    let effective_amount = amount.checked_sub(discount).ok_or(Error::MathOverflow)?;

    record.redeemed = true;
    env.storage().persistent().set(&key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND);

    Ok(effective_amount)
}

/// Read-only accessor for a coupon record.
pub fn get_coupon(env: &Env, coupon_id: u64) -> Option<CouponRecord> {
    env.storage().persistent().get(&DataKey::Coupon(coupon_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::vault_init;
    use crate::RefundVault;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, Address) {
        let env = Env::default();
        let merchant = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(merchant.clone())
            .address();
        let vault = env.register(RefundVault, (vault_init(&env, &merchant, &token, 100),));
        (env, vault)
    }

    fn record(owner: &Address, discount_bps: u32, redeemed: bool) -> CouponRecord {
        CouponRecord {
            owner: owner.clone(),
            discount_bps,
            redeemed,
        }
    }

    #[test]
    fn minting_above_the_discount_cap_is_rejected_without_storing_a_record() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            let owner = Address::generate(&env);

            for discount_bps in [MAX_COUPON_DISCOUNT_BPS + 1, u32::MAX] {
                assert_eq!(
                    mint_coupon(&env, 7, owner.clone(), discount_bps),
                    Err(Error::InvalidRatio)
                );
            }
            assert_eq!(get_coupon(&env, 7), None);
        });
    }

    #[test]
    fn minting_at_the_discount_cap_is_accepted() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            let owner = Address::generate(&env);

            assert_eq!(
                mint_coupon(&env, 1, owner.clone(), MAX_COUPON_DISCOUNT_BPS),
                Ok(())
            );
            assert_eq!(
                get_coupon(&env, 1),
                Some(record(&owner, MAX_COUPON_DISCOUNT_BPS, false))
            );
        });
    }

    #[test]
    fn minting_the_same_id_twice_is_rejected_and_leaves_the_record_unchanged() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            let owner = Address::generate(&env);
            let second_owner = Address::generate(&env);

            mint_coupon(&env, 9, owner.clone(), 1_000).unwrap();
            assert_eq!(
                mint_coupon(&env, 9, second_owner, MAX_COUPON_DISCOUNT_BPS),
                Err(Error::AlreadyInitialized)
            );
            assert_eq!(get_coupon(&env, 9), Some(record(&owner, 1_000, false)));
        });
    }

    #[test]
    fn applying_a_coupon_discounts_the_first_call_and_rejects_the_second() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            let owner = Address::generate(&env);

            mint_coupon(&env, 3, owner.clone(), 1_000).unwrap();

            // 10 % of 10 000 is 1 000, so the effective amount is 9 000.
            assert_eq!(apply_coupon(&env, &owner, 3, 10_000), Ok(9_000));
            assert_eq!(get_coupon(&env, 3), Some(record(&owner, 1_000, true)));

            assert_eq!(
                apply_coupon(&env, &owner, 3, 10_000),
                Err(Error::CouponAlreadyRedeemed)
            );
            assert_eq!(get_coupon(&env, 3), Some(record(&owner, 1_000, true)));
        });
    }

    #[test]
    fn a_coupon_at_the_cap_halves_the_amount() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            let owner = Address::generate(&env);

            mint_coupon(&env, 4, owner.clone(), MAX_COUPON_DISCOUNT_BPS).unwrap();

            assert_eq!(apply_coupon(&env, &owner, 4, 10_000), Ok(5_000));
        });
    }

    #[test]
    fn get_coupon_returns_none_before_minting() {
        let (env, vault) = setup();
        env.as_contract(&vault, || {
            assert_eq!(get_coupon(&env, 0), None);
        });
    }
}
