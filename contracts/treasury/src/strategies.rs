//! Diversified stablecoin yield strategies (issue #466).
//!
//! Idle treasury reserves are split across several **whitelisted** yield
//! protocols (Aave-style lending markets, money markets, …) in proportions
//! governance chooses, so no single protocol's failure, rate freeze, or
//! haircut strands the treasury's float.
//!
//! # Model
//!
//! - **Whitelist first.** Only an admin-approved address may ever hold
//!   treasury tokens: [`whitelist`] is the gate and every deployment path
//!   re-checks it, so a rogue address cannot be funded by a typo.
//! - **Weights, not amounts.** Governance sets percentages
//!   ([`AllocationConfig::weight_bps`], in basis points, summing to exactly
//!   [`TOTAL_WEIGHT_BPS`]) and the engine derives the amounts. A rebalance is
//!   therefore idempotent with respect to the current balance: the same
//!   weights always describe the same portfolio shape.
//! - **Recall first, deploy second.** [`rebalance`] is a full rotation — every
//!   unit of principal comes home, the resulting balance is re-split by weight,
//!   and only then does the new target go out. Capital is never deployed twice,
//!   and a rebalance is the emergency exit as well as the routine one.
//! - **Booked, never trusted.** A strategy is an untrusted external contract
//!   (the same posture `refund-vault` takes for its yield hook, #415): its
//!   self-reported principal and yield are cross-checked against the treasury's
//!   own token balance, and a strategy that under-pays is rejected with
//!   [`Error::StrategyUnderpaid`] instead of being credited for tokens it
//!   never sent. Every strategy call runs under the reentrancy lock.
//! - **A liquid buffer survives.** [`set_reserve_bps`] keeps a slice of the
//!   balance out of the portfolio so vesting claims and other obligations are
//!   still payable. It defaults to fully liquid, so a freshly initialized
//!   treasury deploys nothing until governance deliberately lowers it.
//!
//! Strategy bookkeeping is stored per address under
//! [`DataKey::Allocation`] and iterated in the order governance supplied, with
//! the order itself kept in [`DataKey::AllocationOrder`].

use accensa_common::Error as CommonError;
use soroban_sdk::{contractclient, contractevent, contracttype, token, Address, Env, Vec};

use crate::{bump_ttl, DataKey, Error};

/// 100% expressed in basis points. Allocation weights must sum to exactly this
/// value, so a portfolio is always fully specified (and never over-commits).
pub const TOTAL_WEIGHT_BPS: u32 = 10_000;

/// Maximum number of simultaneously allocated strategies (issue #466).
///
/// A cap keeps `rebalance` bounded: each strategy is one external call, and
/// the weight list has to fit a single contract invocation's budget.
pub const MAX_STRATEGIES: u32 = 8;

/// One strategy's share of the portfolio and its deployed bookkeeping.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyAllocation {
    /// The whitelisted strategy contract.
    pub strategy: Address,
    /// Governance's allocation for this strategy, in basis points out of
    /// [`TOTAL_WEIGHT_BPS`]. Always `> 0` while the allocation exists.
    pub weight_bps: u32,
    /// Principal currently deployed at the strategy. Never exceeds what the
    /// treasury transferred out and has not yet recalled.
    pub deployed: i128,
    /// Yield this strategy has paid back, cumulatively. Yield is booked here
    /// and then sits liquid in the treasury until the next rebalance, so it is
    /// never counted twice.
    pub yield_earned: i128,
}

/// A governance-supplied allocation: which strategy, and what share.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationConfig {
    pub strategy: Address,
    /// Share of the deployable balance, in basis points.
    pub weight_bps: u32,
}

/// Interface for an external stablecoin yield protocol (issue #466).
///
/// The method set deliberately matches `refund-vault`'s `YieldStrategy`
/// (#415) so a single Aave-style adapter can back both the vault's and the
/// treasury's yield hook. The trait is annotated
/// `#[contractclient(name = "StrategyClient")]` (not `#[contractimpl]`, which
/// only accepts `impl` blocks) so a typed client is generated from it.
///
/// Errors use the shared [`CommonError`] code space (issue #98) so both
/// consumers of a given adapter can decode one table.
#[contractclient(name = "StrategyClient")]
pub trait Strategy {
    /// Deploy `amount` tokens. The treasury transfers the tokens to the
    /// strategy *before* calling this; the call is the strategy's cue to start
    /// accruing.
    fn deposit(env: Env, amount: i128) -> Result<(), CommonError>;

    /// Withdraw `principal` worth of tokens plus the proportional share of
    /// accrued yield. Returns `(principal_returned, yield_returned)`; the
    /// strategy transfers both back to the treasury before returning.
    fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), CommonError>;

    /// Read-only: total tokens held (principal + accrued yield). Advisory —
    /// the treasury never books against this value.
    fn total_balance(env: Env) -> i128;

    /// Read-only: accrued yield only. Advisory, as above.
    fn accrued_yield(env: Env) -> i128;
}

/// Emitted when the admin adds a strategy to, or removes it from, the
/// whitelist.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyWhitelistedEvent {
    #[topic]
    pub strategy: Address,
    pub whitelisted: bool,
}

/// Emitted when governance replaces the weight set. Carries the resulting
/// allocations, not just the request, so an indexer sees the weights that were
/// actually stored.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationsUpdatedEvent {
    pub allocations: Vec<StrategyAllocation>,
}

/// Emitted once per completed rebalance, after every recall and deployment
/// has settled. `total_deployed` is the sum of the new principal; the
/// allocations carry each strategy's resulting position.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortfolioRebalancedEvent {
    pub total_deployed: i128,
    pub allocations: Vec<StrategyAllocation>,
}

/// Emitted when principal (and the yield riding on it) is recalled from a
/// strategy, whether by `rebalance_portfolio` or `recall_strategy`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyRecalledEvent {
    #[topic]
    pub strategy: Address,
    /// Principal that came back.
    pub principal: i128,
    /// Accrued yield that came back with it.
    pub yield_amount: i128,
}

// ── Allocation math ───────────────────────────────────────────────────────

/// Split `total` across `weights` in basis points.
///
/// Each share is `floor(total * weight / TOTAL_WEIGHT_BPS)`. The rounding
/// remainder is added to the **largest** weight (ties resolve to the earliest
/// entry in list order), so the shares always sum to exactly `total` — the
/// portfolio is fully invested, and the dust lands on the largest allocation
/// rather than being stranded or arbitrarily inflating the last entry.
///
/// # Errors
///
/// - [`Error::InvalidAllocations`] for an empty list, more than
///   [`MAX_STRATEGIES`] entries, a zero weight, a weight sum that is not
///   exactly [`TOTAL_WEIGHT_BPS`], or a duplicate strategy;
/// - [`Error::MathOverflow`] if scaling `total` by a weight overflows.
pub fn split_by_weights(
    env: &Env,
    total: i128,
    weights: &Vec<(Address, u32)>,
) -> Result<Vec<(Address, i128)>, Error> {
    if weights.is_empty() || weights.len() > MAX_STRATEGIES {
        return Err(Error::InvalidAllocations);
    }

    let mut sum: u32 = 0;
    for (_, weight) in weights.iter() {
        if weight == 0 {
            return Err(Error::InvalidAllocations);
        }
        sum = sum.checked_add(weight).ok_or(Error::MathOverflow)?;
    }
    if sum != TOTAL_WEIGHT_BPS {
        return Err(Error::InvalidAllocations);
    }

    let mut shares: Vec<(Address, i128)> = Vec::new(env);
    let mut assigned: i128 = 0;
    for (strategy, weight) in weights.iter() {
        let scaled = total
            .checked_mul(weight as i128)
            .ok_or(Error::MathOverflow)?;
        let share = scaled / TOTAL_WEIGHT_BPS as i128;
        assigned = assigned.checked_add(share).ok_or(Error::MathOverflow)?;
        shares.push_back((strategy, share));
    }

    let dust = total.checked_sub(assigned).ok_or(Error::MathOverflow)?;
    if dust > 0 {
        let largest = largest_weight_index(weights);
        let entry = shares.get(largest).ok_or(Error::InvalidAllocations)?;
        let topped_up = entry.1.checked_add(dust).ok_or(Error::MathOverflow)?;
        shares.set(largest, (entry.0, topped_up));
    }
    Ok(shares)
}

/// Index of the largest weight, ties resolved to the earliest entry.
fn largest_weight_index(weights: &Vec<(Address, u32)>) -> u32 {
    let mut best: u32 = 0;
    let mut best_weight: u32 = 0;
    for i in 0..weights.len() {
        let Some((_, weight)) = weights.get(i) else {
            break;
        };
        if i == 0 || weight > best_weight {
            best = i;
            best_weight = weight;
        }
    }
    best
}

/// Whether `weights` is a complete, well-formed allocation set.
fn is_valid_weight_set(weights: &Vec<(Address, u32)>) -> bool {
    if weights.is_empty() || weights.len() > MAX_STRATEGIES {
        return false;
    }
    let mut sum: u32 = 0;
    for i in 0..weights.len() {
        let Some((strategy, weight)) = weights.get(i) else {
            return false;
        };
        if weight == 0 {
            return false;
        }
        // No strategy may appear twice: it would silently swallow another
        // entry's weight.
        if weights.slice(0..i).iter().any(|(s, _)| s == strategy) {
            return false;
        }
        sum = match sum.checked_add(weight) {
            Some(s) => s,
            None => return false,
        };
    }
    sum == TOTAL_WEIGHT_BPS
}

// ── Storage helpers ───────────────────────────────────────────────────────

/// Whether `strategy` is on the admin-approved whitelist.
pub fn is_whitelisted(env: &Env, strategy: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::WhitelistedStrategy(strategy.clone()))
        .unwrap_or(false)
}

/// One strategy's stored allocation, if it has one.
pub fn allocation(env: &Env, strategy: &Address) -> Option<StrategyAllocation> {
    env.storage()
        .persistent()
        .get(&DataKey::Allocation(strategy.clone()))
}

/// Every stored allocation, in governance-supplied order.
pub fn allocations(env: &Env) -> Vec<StrategyAllocation> {
    let order: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::AllocationOrder)
        .unwrap_or(Vec::new(env));
    let mut out = Vec::new(env);
    for strategy in order.iter() {
        if let Some(alloc) = allocation(env, &strategy) {
            out.push_back(alloc);
        }
    }
    out
}

/// Total principal currently deployed across every strategy.
pub fn deployed_total(env: &Env) -> i128 {
    let allocs = allocations(env);
    let mut total: i128 = 0;
    for alloc in allocs.iter() {
        total = total.saturating_add(alloc.deployed);
    }
    total
}

/// Total yield recalled from every strategy, cumulatively.
pub fn yield_earned_total(env: &Env) -> i128 {
    let allocs = allocations(env);
    let mut total: i128 = 0;
    for alloc in allocs.iter() {
        total = total.saturating_add(alloc.yield_earned);
    }
    total
}

fn save_allocation(env: &Env, alloc: &StrategyAllocation) {
    let key = DataKey::Allocation(alloc.strategy.clone());
    env.storage().persistent().set(&key, alloc);
    bump_ttl(env, &key);
}

fn token_client(env: &Env) -> Result<token::Client<'static>, Error> {
    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    Ok(token::Client::new(env, &token_addr))
}

// ── Admin entry-point bodies ──────────────────────────────────────────────

/// Add `strategy` to the whitelist. Admin only.
pub fn whitelist(env: &Env, strategy: Address) -> Result<(), Error> {
    if is_whitelisted(env, &strategy) {
        return Err(Error::StrategyAlreadyWhitelisted);
    }
    let key = DataKey::WhitelistedStrategy(strategy.clone());
    env.storage().persistent().set(&key, &true);
    bump_ttl(env, &key);

    StrategyWhitelistedEvent {
        strategy,
        whitelisted: true,
    }
    .publish(env);
    Ok(())
}

/// Remove `strategy` from the whitelist, dropping its allocation. Admin only.
///
/// Refused while the strategy still holds principal or is owed yield —
/// otherwise the treasury would lose the record of funds it can reclaim.
pub fn revoke(env: &Env, strategy: Address) -> Result<(), Error> {
    if !is_whitelisted(env, &strategy) {
        return Err(Error::StrategyNotWhitelisted);
    }
    if let Some(alloc) = allocation(env, &strategy) {
        if alloc.deployed > 0 || alloc.yield_earned > 0 {
            return Err(Error::StrategyHasFunds);
        }
        let key = DataKey::Allocation(strategy.clone());
        env.storage().persistent().remove(&key);
        let order: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AllocationOrder)
            .unwrap_or(Vec::new(env));
        let mut kept = Vec::new(env);
        for entry in order.iter() {
            if entry != strategy {
                kept.push_back(entry);
            }
        }
        let order_key = DataKey::AllocationOrder;
        env.storage().persistent().set(&order_key, &kept);
        bump_ttl(env, &order_key);
    }
    env.storage()
        .persistent()
        .remove(&DataKey::WhitelistedStrategy(strategy.clone()));

    StrategyWhitelistedEvent {
        strategy,
        whitelisted: false,
    }
    .publish(env);
    Ok(())
}

/// Replace the weight set. Admin only.
///
/// Every strategy must be whitelisted, the list must be a complete weight set
/// (non-empty, at most [`MAX_STRATEGIES`] long, no zero weight, no duplicate,
/// summing to exactly [`TOTAL_WEIGHT_BPS`]), and a strategy that is dropped
/// from the list must not still hold funds. Bookkeeping for a retained
/// strategy — its `deployed` and `yield_earned` — is preserved; only the weight
/// changes.
pub fn set_allocations(
    env: &Env,
    configs: Vec<AllocationConfig>,
) -> Result<Vec<StrategyAllocation>, Error> {
    let mut weights: Vec<(Address, u32)> = Vec::new(env);
    for config in configs.iter() {
        if !is_whitelisted(env, &config.strategy) {
            return Err(Error::StrategyNotWhitelisted);
        }
        weights.push_back((config.strategy, config.weight_bps));
    }
    if !is_valid_weight_set(&weights) {
        return Err(Error::InvalidAllocations);
    }

    // A strategy leaving the portfolio must not be left holding funds.
    let current = allocations(env);
    for alloc in current.iter() {
        if !weights.iter().any(|(s, _)| s == alloc.strategy)
            && (alloc.deployed > 0 || alloc.yield_earned > 0)
        {
            return Err(Error::StrategyHasFunds);
        }
    }

    // Persist the order separately from the weights: the order is the
    // iteration order every read path (`allocations`) relies on, and it has to
    // stay a plain address list.
    let mut order: Vec<Address> = Vec::new(env);
    for (strategy, _) in weights.iter() {
        order.push_back(strategy);
    }
    let order_key = DataKey::AllocationOrder;
    env.storage().persistent().set(&order_key, &order);
    bump_ttl(env, &order_key);

    for alloc in current.iter() {
        if !weights.iter().any(|(s, _)| s == alloc.strategy) {
            let key = DataKey::Allocation(alloc.strategy.clone());
            env.storage().persistent().remove(&key);
        }
    }
    let mut stored: Vec<StrategyAllocation> = Vec::new(env);
    for (strategy, weight_bps) in weights.iter() {
        let alloc = allocation(env, &strategy).unwrap_or(StrategyAllocation {
            strategy: strategy.clone(),
            weight_bps: 0,
            deployed: 0,
            yield_earned: 0,
        });
        let updated = StrategyAllocation {
            weight_bps,
            ..alloc
        };
        save_allocation(env, &updated);
        stored.push_back(updated);
    }

    AllocationsUpdatedEvent {
        allocations: stored.clone(),
    }
    .publish(env);
    Ok(stored)
}

/// Set the share of the treasury balance that must stay liquid, in basis
/// points. Admin only. `10_000` keeps everything liquid; `0` deploys the whole
/// balance.
pub fn set_reserve_bps(env: &Env, reserve_bps: u32) -> Result<(), Error> {
    if reserve_bps > TOTAL_WEIGHT_BPS {
        return Err(Error::InvalidReserve);
    }
    let key = DataKey::ReserveBps;
    env.storage().persistent().set(&key, &reserve_bps);
    bump_ttl(env, &key);
    Ok(())
}

/// The liquid-share policy. Defaults to fully liquid, so a treasury that has
/// never been configured deploys nothing.
pub fn reserve_bps(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::ReserveBps)
        .unwrap_or(TOTAL_WEIGHT_BPS)
}

// ── Portfolio operations ──────────────────────────────────────────────────

/// Rotate the whole portfolio: recall every strategy, then redeploy the
/// treasury's balance according to the governance weights, minus the liquid
/// reserve. Returns the total principal now deployed.
///
/// The recall step is what makes this safe to run against a strategy that has
/// misbehaved since the last rotation — and what makes it the emergency exit.
/// Callers must hold the reentrancy lock.
pub fn rebalance(env: &Env) -> Result<i128, Error> {
    let current = allocations(env);
    if current.is_empty() {
        // Nothing is allocated, so there is nothing to recall and no weights to
        // split by. An empty portfolio is a legitimate state, not an error.
        return Ok(0);
    }
    for alloc in current.iter() {
        if alloc.deployed > 0 {
            recall(env, &alloc.strategy, alloc.deployed)?;
        }
    }

    // Everything is home now, so the balance is the portfolio's true size —
    // including any yield the recall brought back.
    let client = token_client(env)?;
    let balance = client.balance(&env.current_contract_address());
    let reserve = reserve_bps(env);
    let deployable = balance
        .checked_mul((TOTAL_WEIGHT_BPS - reserve) as i128)
        .ok_or(Error::MathOverflow)?
        / TOTAL_WEIGHT_BPS as i128;

    let mut weights: Vec<(Address, u32)> = Vec::new(env);
    for alloc in current.iter() {
        weights.push_back((alloc.strategy.clone(), alloc.weight_bps));
    }
    let shares = split_by_weights(env, deployable, &weights)?;
    let mut total: i128 = 0;
    for (strategy, amount) in shares.iter() {
        if amount > 0 {
            deploy(env, &strategy, amount)?;
            total = total.checked_add(amount).ok_or(Error::MathOverflow)?;
        }
    }
    PortfolioRebalancedEvent {
        total_deployed: total,
        allocations: allocations(env),
    }
    .publish(env);
    Ok(total)
}

/// Recall `principal` from `strategy`, bringing back any proportional yield.
/// Returns `(principal_returned, yield_returned)`. Admin only; the yield stays
/// liquid in the treasury until the next rebalance.
pub fn recall_strategy(
    env: &Env,
    strategy: Address,
    principal: i128,
) -> Result<(i128, i128), Error> {
    if !is_whitelisted(env, &strategy) {
        return Err(Error::StrategyNotWhitelisted);
    }
    recall(env, &strategy, principal)
}

/// Make sure the treasury holds at least `needed` tokens liquid, recalling the
/// shortfall from the portfolio if necessary. `balance` is the treasury's
/// current token balance (already read by the caller); the return value is the
/// balance after any recall.
///
/// This is what keeps a portfolio deployment from ever making an obligation
/// unpayable: when a vesting claim outruns the liquid balance, the difference
/// is pulled back from the strategies in weight order inside the same
/// invocation, so an unlocked allocation is always claimable. When the
/// treasury is already liquid enough no strategy storage is touched at all.
/// If nothing is deployed, this is a no-op and the caller's own balance
/// handling applies. Callers must hold the reentrancy lock.
pub fn ensure_liquidity(
    env: &Env,
    client: &token::Client,
    balance: i128,
    needed: i128,
) -> Result<i128, Error> {
    if balance >= needed {
        return Ok(balance);
    }
    let mut outstanding: i128 = needed - balance;
    let allocs = allocations(env);
    for alloc in allocs.iter() {
        if outstanding <= 0 {
            break;
        }
        if alloc.deployed <= 0 {
            continue;
        }
        let take = core::cmp::min(outstanding, alloc.deployed);
        let (principal, _) = recall(env, &alloc.strategy, take)?;
        outstanding = outstanding.saturating_sub(principal);
    }
    Ok(client.balance(&env.current_contract_address()))
}

/// Internal recall shared by `rebalance` and `recall_strategy`.
///
/// The strategy's reported `(principal, yield)` is cross-checked against the
/// treasury's actual token balance delta, so a strategy that under-pays is
/// rejected rather than credited with principal it never returned. Callers must
/// hold the reentrancy lock.
fn recall(env: &Env, strategy: &Address, principal: i128) -> Result<(i128, i128), Error> {
    let mut alloc = allocation(env, strategy).ok_or(Error::StrategyNotAllocated)?;
    if principal <= 0 || alloc.deployed <= 0 {
        return Err(Error::NothingToRecall);
    }
    if principal > alloc.deployed {
        return Err(Error::RecallExceedsDeployed);
    }

    let client = token_client(env)?;
    let treasury = env.current_contract_address();
    let before = client.balance(&treasury);
    let (principal_returned, yield_returned) =
        StrategyClient::new(env, strategy).withdraw(&principal);
    let received = client.balance(&treasury) - before;

    if principal_returned <= 0
        || principal_returned > principal
        || yield_returned < 0
        || received < principal_returned.saturating_add(yield_returned)
    {
        return Err(Error::StrategyUnderpaid);
    }

    alloc.deployed = alloc
        .deployed
        .checked_sub(principal_returned)
        .ok_or(Error::MathOverflow)?;
    alloc.yield_earned = alloc
        .yield_earned
        .checked_add(yield_returned)
        .ok_or(Error::MathOverflow)?;
    save_allocation(env, &alloc);

    StrategyRecalledEvent {
        strategy: strategy.clone(),
        principal: principal_returned,
        yield_amount: yield_returned,
    }
    .publish(env);

    Ok((principal_returned, yield_returned))
}

/// Transfer `amount` to `strategy` and book it as deployed principal.
///
/// Booked before the external calls (checks-effects-interactions): a failing
/// transfer or `deposit` reverts the whole invocation, while a re-entrant
/// strategy cannot see a half-applied position. The amount credited is the
/// balance delta the treasury actually observed. Callers must hold the
/// reentrancy lock.
fn deploy(env: &Env, strategy: &Address, amount: i128) -> Result<(), Error> {
    if !is_whitelisted(env, strategy) {
        return Err(Error::StrategyNotWhitelisted);
    }
    let mut alloc = allocation(env, strategy).ok_or(Error::StrategyNotAllocated)?;
    if amount <= 0 {
        return Err(Error::NothingToDeploy);
    }

    alloc.deployed = alloc
        .deployed
        .checked_add(amount)
        .ok_or(Error::MathOverflow)?;
    save_allocation(env, &alloc);

    let client = token_client(env)?;
    let treasury = env.current_contract_address();
    let before = client.balance(&treasury);
    client.transfer(&treasury, strategy, &amount);
    if before - client.balance(&treasury) != amount {
        return Err(Error::StrategyUnderpaid);
    }
    // The generated client unwraps the strategy's `Result`, so a strategy that
    // rejects the deposit aborts the whole invocation rather than leaving the
    // treasury holding a position it never told the strategy about.
    StrategyClient::new(env, strategy).deposit(&amount);

    Ok(())
}

// ── Reentrancy guard ──────────────────────────────────────────────────────

/// Run `f` with the treasury's strategy reentrancy lock held.
///
/// A whitelisted strategy is still untrusted: without this, a strategy called
/// during a rebalance could re-enter a portfolio entry point and see — or
/// double-count — a position that is only half applied. Re-entry is rejected
/// with [`Error::ReentrancyBlocked`].
pub fn with_lock<R>(env: &Env, f: impl FnOnce() -> Result<R, Error>) -> Result<R, Error> {
    let held: bool = env
        .storage()
        .instance()
        .get(&DataKey::ReentrancyLock)
        .unwrap_or(false);
    if held {
        return Err(Error::ReentrancyBlocked);
    }
    env.storage()
        .instance()
        .set(&DataKey::ReentrancyLock, &true);
    let out = f();
    env.storage()
        .instance()
        .set(&DataKey::ReentrancyLock, &false);
    out
}
