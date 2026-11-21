//! #971 — `set_reserve_ratio` accepts 10_000 and rejects 10_001, and
//! `set_max_deploy_ratio` keeps the same boundary.
//!
//! Both setters open with `if basis_points > 10_000 { InvalidRatio }` and then
//! write the value under their own `DataKey`; `deploy_to_yield` later divides
//! by `10_000` using whatever is stored. Nothing called either setter before
//! this file, so two mutations were invisible to CI:
//!
//!   * `>` widened to `>=` — a legitimate 100 % reserve (or a 100 % deploy
//!     ceiling) starts being refused, and
//!   * the comparison moved below the admin check or the storage write — an
//!     over-10_000 ratio would then be *stored* before being rejected, handing
//!     `deploy_to_yield` a divisor the check exists to make impossible.
//!
//! Each row of the table therefore asserts both directions: what the getter
//! reads after an accepted call, and what it still reads after a rejected one.
//! A fresh vault is used per row *and* the sentinel is written first, because
//! the defaults are not safe sentinels — `reserve_ratio` defaults to 0, which
//! is itself a table entry, and `max_deploy_ratio` defaults to 10_000, the
//! boundary. Values are read back through `get_yield_info` rather than raw
//! storage keys, so the read path `deploy_to_yield` uses stays covered.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

/// The boundary both setters enforce, spelled here once so the table cannot
/// silently move away from it.
const LIMIT: u32 = 10_000;

/// Every side of the boundary: two comfortably inside, the limit itself, one
/// past it, and the largest `u32`.
const TABLE: [u32; 6] = [0, 1, 2_000, LIMIT, LIMIT + 1, u32::MAX];

/// The value a case starts from, so a rejection has something to leave behind.
const SENTINEL_RESERVE: u32 = 4_321;
const SENTINEL_DEPLOY: u32 = 7_777;

/// `true` for the values the contract is specified to store.
fn in_range(basis_points: u32) -> bool {
    basis_points <= LIMIT
}

/// A vault with no yield strategy and no ratio ever set. `mock_all_auths` keeps
/// authorisation out of the way so each row answers the question about the
/// *range* check; the last test switches it off to pin where that check sits.
fn setup() -> RefundVaultClient<'static> {
    let env = Env::default();
    env.mock_all_auths();
    deploy_vault(&env)
}

/// The same vault in an environment that authorises nothing. `__constructor`
/// (like `initialize`) is not auth-gated, so the vault can still be deployed.
fn setup_unauthenticated() -> RefundVaultClient<'static> {
    let env = Env::default();
    deploy_vault(&env)
}

fn deploy_vault(env: &Env) -> RefundVaultClient<'static> {
    let merchant = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant,
            token,
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 100,
            deadline: 0,
            vdf_delay: 0,
        },),
    );

    RefundVaultClient::new(env, &contract)
}

/// Drive one table row against `set_reserve_ratio`: prime the sentinel, attempt
/// `bps`, and assert the ratio `get_yield_info` reports afterwards.
fn reserve_row(bps: u32) {
    let client = setup();
    client.set_reserve_ratio(&SENTINEL_RESERVE);
    assert_eq!(client.get_yield_info().reserve_ratio, SENTINEL_RESERVE);

    let result = client.try_set_reserve_ratio(&bps);
    if in_range(bps) {
        assert_eq!(
            result,
            Ok(Ok(())),
            "`set_reserve_ratio({bps})` is at or below the {LIMIT} bp limit",
        );
        assert_eq!(
            client.get_yield_info().reserve_ratio,
            bps,
            "an accepted ratio must be what the read path reports",
        );
    } else {
        assert_eq!(
            result,
            Err(Ok(Error::InvalidRatio)),
            "`set_reserve_ratio({bps})` is above {LIMIT} bp",
        );
        assert_eq!(
            client.get_yield_info().reserve_ratio,
            SENTINEL_RESERVE,
            "a refused ratio must not reach storage",
        );
    }
}

/// The same row against `set_max_deploy_ratio`.
fn deploy_row(bps: u32) {
    let client = setup();
    client.set_max_deploy_ratio(&SENTINEL_DEPLOY);
    assert_eq!(client.get_yield_info().max_deploy_ratio, SENTINEL_DEPLOY);

    let result = client.try_set_max_deploy_ratio(&bps);
    if in_range(bps) {
        assert_eq!(
            result,
            Ok(Ok(())),
            "`set_max_deploy_ratio({bps})` is at or below the {LIMIT} bp limit",
        );
        assert_eq!(
            client.get_yield_info().max_deploy_ratio,
            bps,
            "an accepted ceiling must be what the read path reports",
        );
    } else {
        assert_eq!(
            result,
            Err(Ok(Error::InvalidRatio)),
            "`set_max_deploy_ratio({bps})` is above {LIMIT} bp",
        );
        assert_eq!(
            client.get_yield_info().max_deploy_ratio,
            SENTINEL_DEPLOY,
            "a refused ceiling must not reach storage",
        );
    }
}

#[test]
fn reserve_ratio_table_matches_the_documented_limit() {
    for bps in TABLE {
        reserve_row(bps);
    }
}

#[test]
fn max_deploy_ratio_table_matches_the_same_limit() {
    for bps in TABLE {
        deploy_row(bps);
    }
}

/// The two setters share one boundary, so on a single vault they must give the
/// same answer for the same value. Asserting them against each other — not only
/// against `in_range` — is what catches the pair drifting apart in one edit.
#[test]
fn the_two_limits_answer_identically_for_every_row() {
    for bps in TABLE {
        let client = setup();
        let reserve = client.try_set_reserve_ratio(&bps);
        let deploy = client.try_set_max_deploy_ratio(&bps);

        if in_range(bps) {
            assert_eq!(reserve, Ok(Ok(())), "{bps} bp: reserve setter refused");
            assert_eq!(deploy, Ok(Ok(())), "{bps} bp: deploy setter refused");
        } else {
            assert_eq!(
                reserve,
                Err(Ok(Error::InvalidRatio)),
                "{bps} bp: reserve setter accepted an over-limit ratio"
            );
            assert_eq!(
                deploy,
                Err(Ok(Error::InvalidRatio)),
                "{bps} bp: deploy setter accepted an over-limit ratio"
            );
        }

        let info = client.get_yield_info();
        if in_range(bps) {
            assert_eq!(info.reserve_ratio, bps);
            assert_eq!(info.max_deploy_ratio, bps);
        }
    }
}

/// The boundary itself, stated once outside the table so a change to `LIMIT`
/// that the contract does not make is caught by name.
#[test]
fn the_boundary_is_exactly_ten_thousand_basis_points() {
    let client = setup();
    assert_eq!(client.try_set_reserve_ratio(&10_000), Ok(Ok(())));
    assert_eq!(client.get_yield_info().reserve_ratio, 10_000);
    assert_eq!(
        client.try_set_reserve_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(client.get_yield_info().reserve_ratio, 10_000);

    assert_eq!(client.try_set_max_deploy_ratio(&10_000), Ok(Ok(())));
    assert_eq!(client.get_yield_info().max_deploy_ratio, 10_000);
    assert_eq!(
        client.try_set_max_deploy_ratio(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(client.get_yield_info().max_deploy_ratio, 10_000);
}

/// Where the comparison sits: an out-of-range value is answered by the
/// contract's own `InvalidRatio` even though nothing authorises the merchant,
/// which fails if the check moves below the `Admin` read or `require_auth`. The
/// in-range call shows authorisation is still enforced — it now reaches it and
/// fails at the host level instead.
#[test]
fn the_range_check_precedes_the_admin_check() {
    let client = setup_unauthenticated();

    assert_eq!(
        client.try_set_reserve_ratio(&u32::MAX),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_set_max_deploy_ratio(&u32::MAX),
        Err(Ok(Error::InvalidRatio))
    );

    let in_range = client.try_set_reserve_ratio(&2_000);
    assert!(
        matches!(in_range, Err(Err(_))),
        "an in-range ratio must reach the admin check, got {in_range:?}"
    );
    assert_eq!(
        client.get_yield_info().reserve_ratio,
        0,
        "and the unauthorised call must not have stored it",
    );
}
