#![cfg(test)]
#![allow(unused_imports, unused_variables, dead_code)]

//! Tests for the oracle aggregator and the dynamic (oracle-gated) refund
//! policy engine.
//!
//! A `MockOracle` contract stands in for a real price feed: the tests set
//! prices per feed, advance the ledger, and then exercise the vault's
//! whitelist, median aggregation, staleness filtering and policy gating.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{Address as _, Events, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val, Vec,
};

use crate::{oracle::OraclePolicy, Error, RefundParam, RefundVault, RefundVaultClient};

// ── Mock oracle contract ───────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MockOracleError {
    Unauthorized = 1,
    FeedNotFound = 2,
}

#[contracttype]
pub enum MockOracleDataKey {
    Admin,
    Price(BytesN<32>),
    LastUpdate(BytesN<32>),
}

/// A trivially simple oracle for tests: the admin sets a price per feed, and
/// the last-update ledger is recorded as the current ledger at set time.
/// Implements the same two methods the vault's [`OracleClient`] expects.
#[contract]
pub struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn initialize(env: Env, admin: Address) {
        env.storage()
            .instance()
            .set(&MockOracleDataKey::Admin, &admin);
    }

    /// Set (or overwrite) the reported price for a feed. Records the current
    /// ledger as the last-update time, so advancing the ledger after setting
    /// makes the value age.
    pub fn set_price(env: Env, feed_id: BytesN<32>, price: i128) -> Result<(), MockOracleError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&MockOracleDataKey::Admin)
            .unwrap();
        admin.require_auth();

        env.storage()
            .instance()
            .set(&MockOracleDataKey::Price(feed_id.clone()), &price);
        env.storage().instance().set(
            &MockOracleDataKey::LastUpdate(feed_id),
            &env.ledger().sequence(),
        );
        Ok(())
    }

    pub fn get_price(env: Env, feed_id: BytesN<32>) -> i128 {
        env.storage()
            .instance()
            .get(&MockOracleDataKey::Price(feed_id))
            .unwrap_or(0)
    }

    pub fn get_last_update_ledger(env: Env, feed_id: BytesN<32>) -> u32 {
        env.storage()
            .instance()
            .get(&MockOracleDataKey::LastUpdate(feed_id))
            .unwrap_or(0)
    }
}

// ── Test helpers ───────────────────────────────────────────────────────────

const FLOAT: i128 = 10_000_000;

fn setup() -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    vault_client.initialize(&merchant, &token, &17_280);

    (env, vault_client, merchant, token)
}

/// Deploys a mock oracle whose admin is the merchant, so `set_price` mimics a
/// merchant-operated feed during tests.
fn deploy_oracle(env: &Env, merchant: &Address) -> Address {
    let oracle_id = env.register(MockOracle, ());
    MockOracleClient::new(env, &oracle_id).initialize(merchant);
    oracle_id
}

fn feed(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0xAAu8; 32])
}

fn set_feed_price(env: &Env, oracle: &Address, feed_id: &BytesN<32>, price: &i128) {
    MockOracleClient::new(env, oracle).set_price(feed_id, price);
}

// ── Whitelist management ───────────────────────────────────────────────────

#[test]
fn test_add_oracle_whitelists_and_reads() {
    let (env, vault_client, merchant, _token) = setup();
    let oracle = deploy_oracle(&env, &merchant);

    vault_client.add_oracle(&oracle);

    assert_eq!(vault_client.get_oracles(), vec![&env, oracle.clone()]);
    assert_eq!(
        vault_client.try_add_oracle(&oracle),
        Err(Ok(Error::OracleAlreadyAdded))
    );
}

#[test]
fn test_add_oracle_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register(RefundVault, ());
    let vault_client = RefundVaultClient::new(&env, &vault_id);
    let oracle = Address::generate(&env);

    assert_eq!(
        vault_client.try_add_oracle(&oracle),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
#[should_panic]
fn test_add_oracle_requires_auth() {
    let (env, vault_client, _merchant, _token) = setup();
    env.set_auths(&[]);
    let oracle = Address::generate(&env);
    vault_client.add_oracle(&oracle);
}

#[test]
fn test_remove_oracle_works() {
    let (env, vault_client, merchant, _token) = setup();
    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);

    vault_client.remove_oracle(&oracle);
    assert_eq!(vault_client.get_oracles().len(), 0);
}

#[test]
fn test_remove_missing_oracle_fails() {
    let (env, vault_client, merchant, _token) = setup();
    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);

    let stranger = Address::generate(&env);
    assert_eq!(
        vault_client.try_remove_oracle(&stranger),
        Err(Ok(Error::OracleNotFound))
    );
}

#[test]
fn test_remove_oracle_with_empty_whitelist_fails() {
    let (env, vault_client, _merchant, _token) = setup();
    let oracle = Address::generate(&env);

    assert_eq!(
        vault_client.try_remove_oracle(&oracle),
        Err(Ok(Error::NoOraclesConfigured))
    );
}

// ── Median aggregation ─────────────────────────────────────────────────────

#[test]
fn test_median_without_oracles_fails() {
    let (env, vault_client, _merchant, _token) = setup();
    assert_eq!(
        vault_client.try_get_median_price(&feed(&env), &0),
        Err(Ok(Error::NoOraclesConfigured))
    );
}

#[test]
fn test_median_single_oracle() {
    let (env, vault_client, merchant, _token) = setup();
    let oracle = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &oracle, &feed(&env), &100);
    vault_client.add_oracle(&oracle);

    assert_eq!(vault_client.get_median_price(&feed(&env), &0), 100);
}

#[test]
fn test_median_of_odd_number_of_oracles() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let a = deploy_oracle(&env, &merchant);
    let b = deploy_oracle(&env, &merchant);
    let c = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &a, &feed_id, &300);
    set_feed_price(&env, &b, &feed_id, &100);
    set_feed_price(&env, &c, &feed_id, &200);

    // Added in a different order than the prices, to prove order of the
    // whitelist (and of the reported values) does not matter.
    vault_client.add_oracle(&a);
    vault_client.add_oracle(&c);
    vault_client.add_oracle(&b);

    assert_eq!(vault_client.get_median_price(&feed_id, &0), 200);
}

#[test]
fn test_median_of_even_number_of_oracles_averages_middle() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let a = deploy_oracle(&env, &merchant);
    let b = deploy_oracle(&env, &merchant);
    let c = deploy_oracle(&env, &merchant);
    let d = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &a, &feed_id, &400);
    set_feed_price(&env, &b, &feed_id, &100);
    set_feed_price(&env, &c, &feed_id, &200);
    set_feed_price(&env, &d, &feed_id, &300);
    vault_client.add_oracle(&a);
    vault_client.add_oracle(&b);
    vault_client.add_oracle(&c);
    vault_client.add_oracle(&d);

    // (200 + 300) / 2
    assert_eq!(vault_client.get_median_price(&feed_id, &0), 250);
}

/// The whole point of median aggregation: one wildly wrong provider cannot
/// move the price. An oracle reporting 100_000 gets neutralised by three
/// honest providers reporting ~250.
#[test]
fn test_median_ignores_single_extreme_outlier() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let honest_a = deploy_oracle(&env, &merchant);
    let honest_b = deploy_oracle(&env, &merchant);
    let honest_c = deploy_oracle(&env, &merchant);
    let outlier = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &honest_a, &feed_id, &250);
    set_feed_price(&env, &honest_b, &feed_id, &260);
    set_feed_price(&env, &honest_c, &feed_id, &270);
    set_feed_price(&env, &outlier, &feed_id, &100_000);
    vault_client.add_oracle(&honest_a);
    vault_client.add_oracle(&honest_b);
    vault_client.add_oracle(&honest_c);
    vault_client.add_oracle(&outlier);

    // 4 values: median is the average of the two middles, (260 + 270) / 2.
    // The 100_000 outlier is completely neutralised.
    assert_eq!(vault_client.get_median_price(&feed_id, &0), 265);
}

#[test]
fn test_remove_oracle_updates_median() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let a = deploy_oracle(&env, &merchant);
    let b = deploy_oracle(&env, &merchant);
    let c = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &a, &feed_id, &100);
    set_feed_price(&env, &b, &feed_id, &200);
    set_feed_price(&env, &c, &feed_id, &300);
    vault_client.add_oracle(&a);
    vault_client.add_oracle(&b);
    vault_client.add_oracle(&c);

    assert_eq!(vault_client.get_median_price(&feed_id, &0), 200);

    vault_client.remove_oracle(&b);
    // [100, 300] -> (100 + 300) / 2
    assert_eq!(vault_client.get_median_price(&feed_id, &0), 200);

    vault_client.remove_oracle(&a);
    // [300]
    assert_eq!(vault_client.get_median_price(&feed_id, &0), 300);
}

// ── Staleness filtering ────────────────────────────────────────────────────

#[test]
fn test_stale_oracle_excluded_from_median() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let fresh = deploy_oracle(&env, &merchant);
    let stale = deploy_oracle(&env, &merchant);

    // Both set at ledger 100.
    env.ledger().with_mut(|li| li.sequence_number = 100);
    set_feed_price(&env, &fresh, &feed_id, &50);
    set_feed_price(&env, &stale, &feed_id, &250);

    // Only the fresh oracle updates at ledger 200.
    env.ledger().with_mut(|li| li.sequence_number = 200);
    set_feed_price(&env, &fresh, &feed_id, &100);

    vault_client.add_oracle(&fresh);
    vault_client.add_oracle(&stale);

    // max_staleness = 50: fresh is 0 ledgers old, stale is 100 old -> excluded.
    assert_eq!(vault_client.get_median_price(&feed_id, &50), 100);

    // max_staleness = 150: both are fresh enough -> median of (100, 250).
    assert_eq!(vault_client.get_median_price(&feed_id, &150), 175);
}

#[test]
fn test_all_oracles_stale_fails() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let a = deploy_oracle(&env, &merchant);
    let b = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &a, &feed_id, &100);
    set_feed_price(&env, &b, &feed_id, &200);

    // Current ledger is now far past both last-updates.
    env.ledger().with_mut(|li| li.sequence_number = 500);

    vault_client.add_oracle(&a);
    vault_client.add_oracle(&b);

    assert_eq!(
        vault_client.try_get_median_price(&feed_id, &10),
        Err(Ok(Error::StaleOracleData))
    );
}

/// `max_staleness = 0` disables freshness filtering entirely, mirroring how a
/// `0` refund window means "no time bound".
#[test]
fn test_zero_staleness_disables_filtering() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let oracle = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &oracle, &feed_id, &300);

    env.ledger().with_mut(|li| li.sequence_number = 10_000);
    vault_client.add_oracle(&oracle);

    assert_eq!(vault_client.get_median_price(&feed_id, &0), 300);
}

// ── Oracle policy management ───────────────────────────────────────────────

#[test]
fn test_set_and_get_oracle_policy_roundtrip() {
    let (env, vault_client, _merchant, _token) = setup();
    let feed_id = feed(&env);

    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 100,
        refund_when_below: true,
    };
    vault_client.set_oracle_policy(&policy);

    assert_eq!(vault_client.get_oracle_policy(), Some(policy));
}

#[test]
fn test_clear_oracle_policy() {
    let (env, vault_client, _merchant, _token) = setup();
    let feed_id = feed(&env);

    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    };
    vault_client.set_oracle_policy(&policy);
    vault_client.clear_oracle_policy();

    assert_eq!(vault_client.get_oracle_policy(), None);
    assert_eq!(
        vault_client.try_clear_oracle_policy(),
        Err(Ok(Error::NoOraclePolicy))
    );
}

#[test]
#[should_panic]
fn test_set_oracle_policy_requires_auth() {
    let (env, vault_client, _merchant, _token) = setup();
    env.set_auths(&[]);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed(&env),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });
}

#[test]
#[should_panic]
fn test_clear_oracle_policy_requires_auth() {
    let (env, vault_client, _merchant, _token) = setup();
    env.set_auths(&[]);
    vault_client.clear_oracle_policy();
}

// ── Policy-gated refunds ───────────────────────────────────────────────────

fn deposit_and_buyer(
    env: &Env,
    vault_client: &RefundVaultClient<'static>,
    merchant: &Address,
) -> (BytesN<32>, Address) {
    vault_client.deposit(merchant, &1_000_000);
    let payment_ref = BytesN::from_array(env, &[0xBBu8; 32]);
    let buyer = Address::generate(env);
    (payment_ref, buyer)
}

/// The headline SLA case: "refund buyers while the asset price is below the
/// floor". While the price is above the threshold the refund is denied with
/// `OraclePolicyDenied`; once the price drops, the same refund succeeds.
#[test]
fn test_refund_gated_by_price_drop_policy() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &300);

    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    };
    vault_client.set_oracle_policy(&policy);

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);

    // Price 300 >= 250: condition not met, refund denied and nothing recorded.
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::OraclePolicyDenied))
    );
    assert!(vault_client.get_refund(&payment_ref).is_none());

    // The price drops below the floor: the same refund now succeeds.
    set_feed_price(&env, &oracle, &feed_id, &200);
    vault_client.refund(&payment_ref, &buyer, &100_000, &0, &100_000);

    let record = vault_client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 100_000);
    assert_eq!(TokenClient::new(&env, &_token).balance(&buyer), 100_000);
}

/// The mirror-image policy: refunds only while the metric is *above* its
/// ceiling (e.g. "refund when network downtime exceeds the SLA allowance").
#[test]
fn test_refund_gated_by_rise_policy() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &100);

    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: false,
    };
    vault_client.set_oracle_policy(&policy);

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);

    // Metric 100 <= 250: condition not met (refunds only above the ceiling).
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::OraclePolicyDenied))
    );

    set_feed_price(&env, &oracle, &feed_id, &300);
    vault_client.refund(&payment_ref, &buyer, &100_000, &0, &100_000);
    assert!(vault_client.get_refund(&payment_ref).is_some());
}

/// A policy at exactly the threshold is a strict comparison in both
/// directions: `refund_when_below` requires `<`, not `<=`.
#[test]
fn test_policy_comparisons_are_strict() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &250);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);
    // 250 is not < 250.
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::OraclePolicyDenied))
    );
}

/// No policy installed -> the oracle whitelist has no effect on refunds
/// (existing behaviour is preserved when the feature is unused).
#[test]
fn test_no_policy_means_no_gating() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &1);

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);
    vault_client.refund(&payment_ref, &buyer, &100_000, &0, &100_000);
    assert!(vault_client.get_refund(&payment_ref).is_some());
}

/// A policy with no whitelisted oracles fails closed: the vault refuses to
/// guess at a price it cannot read.
#[test]
fn test_policy_without_oracles_fails_closed() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::NoOraclesConfigured))
    );
}

/// A policy whose every whitelisted oracle is stale also fails closed.
#[test]
fn test_policy_with_all_stale_oracles_fails_closed() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    env.ledger().with_mut(|li| li.sequence_number = 100);
    let oracle = deploy_oracle(&env, &merchant);
    set_feed_price(&env, &oracle, &feed_id, &100);
    env.ledger().with_mut(|li| li.sequence_number = 500);
    vault_client.add_oracle(&oracle);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 10,
        refund_when_below: true,
    });

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::StaleOracleData))
    );
}

/// Clearing the policy restores unconditional (window-only) refunds.
#[test]
fn test_clearing_policy_disables_gating() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &300);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });

    let (payment_ref, buyer) = deposit_and_buyer(&env, &vault_client, &merchant);
    assert_eq!(
        vault_client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000),
        Err(Ok(Error::OraclePolicyDenied))
    );

    vault_client.clear_oracle_policy();
    vault_client.refund(&payment_ref, &buyer, &100_000, &0, &100_000);
    assert!(vault_client.get_refund(&payment_ref).is_some());
}

/// `process_batch` inherits the gate: items denied by the policy come back
/// `false` in the per-item result vector, and succeed once the condition
/// holds. This proves the dynamic policy is enforced on the batched path,
/// not just the single `refund` entry point.
#[test]
fn test_process_batch_respects_oracle_policy() {
    let (env, vault_client, merchant, _token) = setup();
    let feed_id = feed(&env);

    let oracle = deploy_oracle(&env, &merchant);
    vault_client.add_oracle(&oracle);
    set_feed_price(&env, &oracle, &feed_id, &300);

    vault_client.set_oracle_policy(&OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 0,
        refund_when_below: true,
    });
    vault_client.deposit(&merchant, &1_000_000);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);
    let p1 = RefundParam {
        payment_ref: BytesN::from_array(&env, &[0x11u8; 32]),
        recipient: buyer1.clone(),
        amount: 100_000,
        paid_at_ledger: 0,
        payment_amount: 100_000,
    };
    let p2 = RefundParam {
        payment_ref: BytesN::from_array(&env, &[0x22u8; 32]),
        recipient: buyer2.clone(),
        amount: 200_000,
        paid_at_ledger: 0,
        payment_amount: 200_000,
    };
    let batch = vec![&env, p1.clone(), p2.clone()];

    // Price 300 >= 250: every item denied, nothing recorded, no payouts.
    assert_eq!(vault_client.process_batch(&batch), vec![&env, false, false]);
    assert!(vault_client.get_refund(&p1.payment_ref).is_none());
    assert!(vault_client.get_refund(&p2.payment_ref).is_none());

    // Price drops: the identical batch now succeeds end to end.
    set_feed_price(&env, &oracle, &feed_id, &200);
    assert_eq!(vault_client.process_batch(&batch), vec![&env, true, true]);
    assert!(vault_client.get_refund(&p1.payment_ref).is_some());
    assert!(vault_client.get_refund(&p2.payment_ref).is_some());
}

// ── Events ─────────────────────────────────────────────────────────────────

#[test]
fn test_oracle_policy_events_emitted() {
    let (env, vault_client, _merchant, _token) = setup();
    let feed_id = feed(&env);

    let policy = OraclePolicy {
        feed_id: feed_id.clone(),
        threshold: 250,
        max_staleness_ledgers: 100,
        refund_when_below: true,
    };
    vault_client.set_oracle_policy(&policy);

    let mut data = Map::<Val, Val>::new(&env);
    data.set(
        Symbol::new(&env, "threshold").into_val(&env),
        250i128.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "refund_when_below").into_val(&env),
        true.into_val(&env),
    );
    data.set(
        Symbol::new(&env, "max_staleness_ledgers").into_val(&env),
        100u32.into_val(&env),
    );

    assert_eq!(
        env.events().all().filter_by_contract(&vault_client.address),
        vec![
            &env,
            (
                vault_client.address.clone(),
                (
                    Symbol::new(&env, "oracle_policy_set_event"),
                    feed_id.clone()
                )
                    .into_val(&env),
                data.into_val(&env)
            )
        ]
    );

    vault_client.clear_oracle_policy();

    let empty_data: Map<Val, Val> = Map::new(&env);
    assert_eq!(
        env.events().all().filter_by_contract(&vault_client.address),
        vec![
            &env,
            (
                vault_client.address.clone(),
                (
                    Symbol::new(&env, "oracle_policy_cleared_event"),
                    feed_id.clone()
                )
                    .into_val(&env),
                empty_data.into_val(&env)
            )
        ]
    );
}
