//! Oracle integration for dynamic, SLA-based refund policies.
//!
//! Static policies (the refund window measured from `paid_at_ledger`) cannot
//! express refunds that depend on external facts — a network outage, an asset
//! price drop, a QoS breach. This module adds that capability without trusting
//! a single centralized price feed:
//!
//! - [`Oracle`] is the standard, pluggable interface any price/data oracle
//!   contract implements. The merchant whitelists oracle contracts via
//!   `RefundVault::add_oracle`.
//! - [`median_price`] is the aggregator: it queries **every** whitelisted
//!   oracle for the same `feed_id`, drops oracles whose value is older than
//!   the configured staleness bound, and returns the **median** of the
//!   remaining values. The median is robust to a single compromised or broken
//!   provider: to move the aggregated price an attacker must control a
//!   majority of the whitelist, not just one member.
//! - [`evaluate_policy`] feeds that median into an [`OraclePolicy`] condition,
//!   which the vault's `refund` path evaluates before any payout.
//!
//! # Trust and failure model
//!
//! The whitelist is maintained under merchant auth, so a *whitelisted* oracle
//! is a merchant-chosen counterparty (the same trust tier as the yield
//! strategy — see `docs/SECURITY_MODEL.md` §5). What the aggregator defends
//! against is any *single* whitelisted provider unilaterally moving the price:
//! the median neutralizes one outlier, and staleness filtering drops a
//! provider that stopped updating. The vault **fails closed**: if no oracle
//! is whitelisted, or every whitelisted oracle is stale, the policy cannot be
//! evaluated and `refund` rejects rather than guessing.
//!
//! A whitelisted oracle that *panics* during `get_price` aborts the whole
//! transaction (Soroban has no cross-contract catch), so a broken oracle
//! halts refunds rather than being silently skipped. That is intentional:
//! the merchant is expected to remove the broken oracle.

use crate::{DataKey, Error};
use soroban_sdk::{contractclient, contracttype, Address, BytesN, Env, Vec};

/// Standard interface for a price/data oracle that `RefundVault` can query.
///
/// Any contract implementing these two methods can be whitelisted via
/// `RefundVault::add_oracle`. The aggregator calls both on every whitelisted
/// oracle for the same `feed_id` and takes the median of the fresh values.
///
/// A `feed_id` is an opaque 32-byte identifier for the value being queried
/// (conventionally the SHA-256 of a canonical string such as `"XLM/USDC"`).
/// The reported value is in the feed's own fixed-point scale; the merchant
/// configures `OraclePolicy.threshold` in that same scale.
#[contractclient(name = "OracleClient")]
pub trait Oracle {
    /// Latest value of the feed identified by `feed_id` (e.g. the price of
    /// the base asset denominated in the quote asset, in the feed's scale).
    fn get_price(env: Env, feed_id: BytesN<32>) -> i128;

    /// Ledger sequence at which the feed's value was last updated. The
    /// aggregator uses this to drop stale oracles: a value older than the
    /// policy's `max_staleness_ledgers` is excluded from the median.
    fn get_last_update_ledger(env: Env, feed_id: BytesN<32>) -> u32;
}

/// Dynamic, oracle-gated refund condition.
///
/// When set via `RefundVault::set_oracle_policy`, `refund` only pays out
/// while the aggregated median of the whitelisted oracles for `feed_id`
/// satisfies the configured comparison. The condition is *strict*: with
/// `refund_when_below` the median must be `< threshold`; with
/// `refund_when_above` it must be `> threshold`.
///
/// `max_staleness_ledgers == 0` disables the staleness check (mirroring how a
/// `0` refund window means "no time bound"); any other value excludes
/// oracles whose last update is older than that many ledgers from the median.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePolicy {
    /// The feed the policy evaluates (e.g. the hash of `"XLM/USDC"`).
    pub feed_id: BytesN<32>,
    /// The median value (in the feed's scale) at which the condition flips.
    pub threshold: i128,
    /// Maximum allowed age of a feed value in ledgers; `0` = never stale.
    pub max_staleness_ledgers: u32,
    /// Comparison direction. `true`: refunds are permitted while the median is
    /// strictly **below** `threshold` (e.g. "refund when the asset price
    /// drops"). `false`: refunds are permitted while the median is strictly
    /// **above** `threshold` (e.g. "refund when the SLA metric exceeds its
    /// ceiling").
    pub refund_when_below: bool,
}

/// Aggregates the median of the fresh values reported by every whitelisted
/// oracle for `feed_id`.
///
/// - No whitelist (or an empty one) → [`Error::NoOraclesConfigured`].
/// - Every whitelisted oracle is stale → [`Error::StaleOracleData`].
/// - Otherwise the median of the fresh values, computed over a tiny
///   insertion-sorted host `Vec` (the whitelist is small by design, so O(n²)
///   is cheaper than a general-purpose sort or a guest-heap allocation).
pub(crate) fn median_price(
    env: &Env,
    feed_id: &BytesN<32>,
    max_staleness_ledgers: u32,
) -> Result<i128, Error> {
    let oracles: Vec<Address> = env
        .storage()
        .instance()
        .get(&DataKey::Oracles)
        .ok_or(Error::NoOraclesConfigured)?;
    if oracles.is_empty() {
        return Err(Error::NoOraclesConfigured);
    }

    let current_ledger = env.ledger().sequence();
    let mut prices: Vec<i128> = Vec::new(env);
    for oracle in oracles.iter() {
        let client = OracleClient::new(env, &oracle);
        let last_update = client.get_last_update_ledger(feed_id);
        // Stale = updated more than `max_staleness_ledgers` ledgers ago.
        // `saturating_add` keeps a maliciously huge `last_update` from
        // overflowing; such a value is never stale and simply stays eligible.
        let stale = max_staleness_ledgers > 0
            && last_update.saturating_add(max_staleness_ledgers) < current_ledger;
        if stale {
            continue;
        }
        prices.push_back(client.get_price(feed_id));
    }

    if prices.is_empty() {
        return Err(Error::StaleOracleData);
    }

    Ok(median(&mut prices))
}

/// Evaluates an [`OraclePolicy`] against the aggregated median, returning
/// `true` when the condition holds (refunds permitted).
pub(crate) fn evaluate_policy(env: &Env, policy: &OraclePolicy) -> Result<bool, Error> {
    let median = median_price(env, &policy.feed_id, policy.max_staleness_ledgers)?;
    if policy.refund_when_below {
        Ok(median < policy.threshold)
    } else {
        Ok(median > policy.threshold)
    }
}

/// Median of a small collection via in-place insertion sort.
///
/// The oracle whitelist is tiny (a handful of merchant-chosen contracts), so
/// the O(n²) insertion sort on the host-managed `Vec` avoids both a
/// general-purpose sort and any guest-heap allocation. Even-length inputs
/// average the two middle values: `lo + (hi - lo) / 2` stays overflow-safe
/// for the non-negative prices the sort guarantees `hi >= lo`.
fn median(prices: &mut Vec<i128>) -> i128 {
    let n = prices.len();
    let mut i = 1u32;
    while i < n {
        let key = prices.get_unchecked(i);
        let mut j = i;
        while j > 0 {
            let prev = prices.get_unchecked(j - 1);
            if prev <= key {
                break;
            }
            prices.set(j, prev);
            j -= 1;
        }
        prices.set(j, key);
        i += 1;
    }

    let mid = n / 2;
    if n % 2 == 1 {
        prices.get_unchecked(mid)
    } else {
        let hi = prices.get_unchecked(mid);
        let lo = prices.get_unchecked(mid - 1);
        lo + (hi - lo) / 2
    }
}
