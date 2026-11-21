//! #965 — `reserve_ratio + max_deploy_ratio` cannot exceed 100 %.
//!
//! The two ratios split the same pool: `reserve_ratio` is the share of total
//! value that must stay liquid, `max_deploy_ratio` the share that may leave. Set
//! 80 % and 50 % and the vault stores both happily, but `deploy_to_yield` reads
//! the reserve first, so deployment stops at 20 % and the 50 % ceiling the
//! merchant configured can never bind. The limit is unreachable rather than
//! violated, which is why no existing test noticed.
//!
//! The fix rejects the *pair* in whichever setter writes it second, so the
//! tests here pin three things: both write orders are caught, a legal pair is
//! accepted in both orders, and — the reason the check reads the counterpart as
//! an `Option` — a vault's first ratio is never rejected on account of a default
//! it never set.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env};

const FLOAT: i128 = 10_000_000;

fn setup() -> (Env, RefundVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 17_280,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &vault_id);
    (env, client, merchant)
}

/// `max = 50 %` then `reserve = 80 %`: the reserve is what would be written
/// second, and it is the one that makes the stored ceiling unreachable.
#[test]
fn test_ceiling_then_reserve_is_rejected() {
    let (_env, client, _merchant) = setup();

    client.set_max_deploy_ratio(&5_000);
    assert_eq!(
        client.try_set_reserve_ratio(&8_000),
        Err(Ok(Error::InvalidRatio))
    );

    // Nothing was written: the vault keeps a 0 reserve, which is the pre-call
    // state rather than a half-applied 80 %.
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 0);
    assert_eq!(info.max_deploy_ratio, 5_000);
}

/// The mirror image, and the order that actually bites in practice: a merchant
/// who sets an 80 % reserve first and then tries to raise the ceiling to 50 %.
#[test]
fn test_reserve_then_ceiling_is_rejected() {
    let (_env, client, _merchant) = setup();

    client.set_reserve_ratio(&8_000);
    assert_eq!(
        client.try_set_max_deploy_ratio(&5_000),
        Err(Ok(Error::InvalidRatio))
    );

    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 8_000);
    // `get_yield_info` reports the *effective* 10,000 default for an unset key,
    // which is exactly the pair the guard must not treat as configured.
    assert_eq!(info.max_deploy_ratio, 10_000);
}

/// `reserve + max == 10_000` is the whole pool divided with nothing left over,
/// so it is legal — and legal in either write order.
#[test]
fn test_pair_summing_exactly_to_total_is_accepted_both_ways() {
    let (_env, client, _merchant) = setup();
    client.set_reserve_ratio(&8_000);
    client.set_max_deploy_ratio(&2_000);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 8_000);
    assert_eq!(info.max_deploy_ratio, 2_000);

    let (_env, client, _merchant) = setup();
    client.set_max_deploy_ratio(&2_000);
    client.set_reserve_ratio(&8_000);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 8_000);
    assert_eq!(info.max_deploy_ratio, 2_000);
}

/// One basis point over the line, from either side.
#[test]
fn test_pair_one_basis_point_over_total_is_rejected() {
    let (_env, client, _merchant) = setup();
    client.set_reserve_ratio(&5_000);
    assert_eq!(
        client.try_set_max_deploy_ratio(&5_001),
        Err(Ok(Error::InvalidRatio))
    );
    client.set_max_deploy_ratio(&5_000);
    assert_eq!(client.get_yield_info().max_deploy_ratio, 5_000);

    let (_env, client, _merchant) = setup();
    client.set_max_deploy_ratio(&5_000);
    assert_eq!(
        client.try_set_reserve_ratio(&5_001),
        Err(Ok(Error::InvalidRatio))
    );
    client.set_reserve_ratio(&5_000);
    assert_eq!(client.get_yield_info().reserve_ratio, 5_000);
}

/// The guard reads the counterpart, never the default. A vault whose ceiling key
/// was never written still deploys up to 100 %, but that is `deploy_to_yield`'s
/// reading of an absent key — it is not a stored 10,000 for the setter to
/// collide with. Without this, `set_reserve_ratio(2_000)` on a brand-new vault
/// would be rejected against a number nobody configured.
#[test]
fn test_first_ratio_is_accepted_at_any_legal_value() {
    for bp in [0u32, 1, 2_000, 9_999, 10_000] {
        let (_env, client, _merchant) = setup();
        client.set_reserve_ratio(&bp);
        assert_eq!(client.get_yield_info().reserve_ratio, bp);

        let (_env, client, _merchant) = setup();
        client.set_max_deploy_ratio(&bp);
        assert_eq!(client.get_yield_info().max_deploy_ratio, bp);
    }
}

/// A vault that never configures both keys keeps its pre-#965 behavior: an 80 %
/// reserve written alone is accepted, and the unwritten ceiling stays at its
/// 100 % default rather than being retroactively rejected.
#[test]
fn test_unset_counterpart_does_not_retroactively_reject() {
    let (_env, client, _merchant) = setup();

    client.set_reserve_ratio(&10_000);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 10_000);
    assert_eq!(info.max_deploy_ratio, 10_000);
}

/// The per-key 10,000 bp bound is still checked first, so an out-of-range value
/// fails the same way whether or not a counterpart is stored.
#[test]
fn test_range_check_runs_before_the_pair_check() {
    let (_env, client, _merchant) = setup();
    assert_eq!(
        client.try_set_reserve_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_set_max_deploy_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );

    // With a counterpart present the error is the same, so the two guards are
    // indistinguishable from outside — which is the point: one error, one rule.
    client.set_reserve_ratio(&2_000);
    assert_eq!(
        client.try_set_max_deploy_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(client.get_yield_info().max_deploy_ratio, 10_000);
}

/// Rejection is not a dead end: the operator recovers by moving whichever side
/// they own, and the previously stored value survives the failed attempt to be
/// used as the basis of that correction.
#[test]
fn test_operator_can_recover_after_a_rejected_pair() {
    let (_env, client, _merchant) = setup();

    client.set_reserve_ratio(&8_000);
    assert_eq!(
        client.try_set_max_deploy_ratio(&5_000),
        Err(Ok(Error::InvalidRatio))
    );

    // Lower the ceiling into the 20 % the reserve actually permits.
    client.set_max_deploy_ratio(&2_000);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 8_000);
    assert_eq!(info.max_deploy_ratio, 2_000);

    // Or relieve the reserve instead — both keys remain writable after a
    // rejection, so neither can be bricked by a bad pair.
    client.set_reserve_ratio(&5_000);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 5_000);
    assert_eq!(info.max_deploy_ratio, 2_000);
}

/// Every legal pair is writable in both orders and lands exactly as set, which
/// is what `deploy_to_yield` then reads.
#[test]
fn test_legal_pairs_are_writable_in_both_orders() {
    for (reserve, max) in [
        (0u32, 10_000u32),
        (2_000, 8_000),
        (7_500, 2_500),
        (10_000, 0),
        (5_000, 5_000),
    ] {
        for ceiling_first in [false, true] {
            let (_env, client, _merchant) = setup();
            if ceiling_first {
                client.set_max_deploy_ratio(&max);
                client.set_reserve_ratio(&reserve);
            } else {
                client.set_reserve_ratio(&reserve);
                client.set_max_deploy_ratio(&max);
            }
            let info = client.get_yield_info();
            assert_eq!((info.reserve_ratio, info.max_deploy_ratio), (reserve, max));
        }
    }
}

/// Once both keys are configured, no single write can break the invariant
/// again — the guard applies to every subsequent change, not only the first.
#[test]
fn test_invariant_holds_for_lateral_moves_after_configuration() {
    let (_env, client, _merchant) = setup();

    client.set_reserve_ratio(&1);
    client.set_max_deploy_ratio(&9_999);

    // One bp more reserve would put the pair at 10,001.
    assert_eq!(
        client.try_set_reserve_ratio(&2),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(client.get_yield_info().reserve_ratio, 1);

    // Freeing the ceiling from the same side makes the same reserve legal.
    client.set_max_deploy_ratio(&9_998);
    client.set_reserve_ratio(&2);
    let info = client.get_yield_info();
    assert_eq!(info.reserve_ratio, 2);
    assert_eq!(info.max_deploy_ratio, 9_998);
}
