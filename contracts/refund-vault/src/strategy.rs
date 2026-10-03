//! Pluggable yield-bearing escrow strategy hook (issue #415).
//!
//! Long-lived refund reserves may be parked in an external, admin-approved
//! Soroban yield protocol (a lending pool, an AMM position, …) while the vault
//! keeps 100% of the principal redeemable:
//!
//! - **Pluggable interface.** Any contract implementing [`YieldStrategy`] can
//!   back the vault, but only once the merchant (admin) has put it on the
//!   whitelist with `approve_yield_strategy`. `set_yield_strategy` and
//!   `deploy_to_yield` both refuse an unapproved address.
//! - **Instant principal redemption.** When a refund or a merchant withdrawal
//!   needs more than the vault holds liquid, [`ensure_liquidity`] recalls the
//!   shortfall from the strategy inside the same invocation, so a customer
//!   refund never fails just because the float is deployed. The reserve ratio
//!   enforced by `deploy_to_yield` remains the liquid buffer that serves
//!   ordinary refunds without touching the strategy at all.
//! - **Emergency exit.** `emergency_exit_yield` recalls every deployed unit of
//!   principal and, unlike the routine yield entry points, works while the
//!   vault is paused — pausing is exactly when the merchant wants capital home.
//! - **Yield routing.** Accrued interest is tracked separately from principal
//!   (`HarvestedYield`) and `distribute_yield` pays it out to the configured
//!   yield recipient: the protocol treasury or a merchant rebate pool. When no
//!   recipient is set it falls back to the merchant, so yield always has a
//!   deterministic destination.
//!
//! Every strategy is untrusted (`docs/AUDIT.md` §5): recalls are measured by
//! the vault's own token balance rather than the values the strategy returns,
//! and every strategy call runs under the vault's reentrancy lock.

use accensa_common::Error;
use soroban_sdk::{contractclient, contractevent, token, Address, Env};

use crate::{increment_nonce, persist_yield_ttl, DataKey, YieldWithdrawnEvent};

/// Interface for external yield-generating strategies (e.g., Soroban lending protocols).
///
/// Any contract that implements these methods can be registered as the vault's yield
/// strategy once the admin has approved it. The vault calls these to deploy idle funds
/// and harvest accrued yield. The trait is annotated
/// `#[contractclient(name = "YieldStrategyClient")]` (not `#[contractimpl]`, which only
/// accepts `impl` blocks) so its client is generated from the interface.
#[contractclient(name = "YieldStrategyClient")]
pub trait YieldStrategy {
    /// Deploy `amount` tokens into the strategy. The vault transfers tokens to the
    /// strategy contract before calling this.
    fn deposit(env: Env, amount: i128) -> Result<(), Error>;

    /// Withdraw `principal` worth of tokens plus any proportional accrued yield.
    /// Returns `(principal_returned, yield_returned)`. The strategy transfers tokens
    /// back to the vault before returning.
    fn withdraw(env: Env, principal: i128) -> Result<(i128, i128), Error>;

    /// Harvest all accrued yield without touching deployed principal.
    /// Returns the yield amount. The strategy transfers yield tokens to the vault.
    fn harvest(env: Env) -> Result<i128, Error>;

    /// Read-only: total tokens held by this strategy (principal + accrued yield).
    fn total_balance(env: Env) -> i128;

    /// Read-only: accrued yield only (total_balance - total principal deployed).
    fn accrued_yield(env: Env) -> i128;
}

/// Emitted when the admin adds a strategy to, or removes it from, the
/// whitelist.
///
/// Topics: `("strategy_approval_event", strategy)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyApprovalEvent {
    #[topic]
    pub strategy: Address,
    pub approved: bool,
}

/// Emitted when harvested yield is paid out to the yield recipient.
///
/// Topics: `("yield_distributed_event", recipient)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldDistributedEvent {
    #[topic]
    pub recipient: Address,
    pub amount: i128,
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
}

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;
    admin.require_auth();
    Ok(admin)
}

fn deployed_principal(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::DeployedPrincipal)
        .unwrap_or(0)
}

fn active_strategy(env: &Env) -> Option<Address> {
    env.storage().persistent().get(&DataKey::YieldStrategy)
}

/// Whether `strategy` is on the admin-approved whitelist.
pub(crate) fn is_approved(env: &Env, strategy: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::ApprovedStrategy(strategy.clone()))
        .unwrap_or(false)
}

/// Add `strategy` to the whitelist. Admin only.
pub(crate) fn approve(env: &Env, strategy: Address) -> Result<(), Error> {
    require_admin(env)?;
    let key = DataKey::ApprovedStrategy(strategy.clone());
    env.storage().persistent().set(&key, &true);
    persist_yield_ttl(env, &key);
    StrategyApprovalEvent {
        strategy,
        approved: true,
    }
    .publish(env);
    Ok(())
}

/// Remove `strategy` from the whitelist. Admin only.
///
/// Revoking the active strategy also unregisters it, which is only allowed
/// once its principal has been fully recalled — otherwise the vault would lose
/// track of funds it is owed.
pub(crate) fn revoke(env: &Env, strategy: Address) -> Result<(), Error> {
    require_admin(env)?;
    if active_strategy(env).as_ref() == Some(&strategy) {
        if deployed_principal(env) > 0 {
            return Err(Error::StrategyHasPrincipal);
        }
        env.storage().persistent().remove(&DataKey::YieldStrategy);
    }
    env.storage()
        .persistent()
        .remove(&DataKey::ApprovedStrategy(strategy.clone()));
    StrategyApprovalEvent {
        strategy,
        approved: false,
    }
    .publish(env);
    Ok(())
}

/// Register `strategy` as the active strategy. Admin only; the strategy must
/// be whitelisted, and the previous strategy (if different) must hold no
/// principal.
pub(crate) fn set_active(env: &Env, strategy: Address) -> Result<(), Error> {
    require_admin(env)?;
    if !is_approved(env, &strategy) {
        return Err(Error::StrategyNotApproved);
    }
    if let Some(current) = active_strategy(env) {
        if current != strategy && deployed_principal(env) > 0 {
            return Err(Error::StrategyHasPrincipal);
        }
    }
    env.storage()
        .persistent()
        .set(&DataKey::YieldStrategy, &strategy);
    persist_yield_ttl(env, &DataKey::YieldStrategy);
    Ok(())
}

/// Set where harvested yield is paid: the protocol treasury or a merchant
/// rebate pool. Admin only.
pub(crate) fn set_recipient(env: &Env, recipient: Address) -> Result<(), Error> {
    require_admin(env)?;
    if recipient == env.current_contract_address() {
        return Err(Error::SelfTransfer);
    }
    env.storage()
        .persistent()
        .set(&DataKey::YieldRecipient, &recipient);
    persist_yield_ttl(env, &DataKey::YieldRecipient);
    Ok(())
}

/// The configured yield recipient, falling back to the merchant (admin).
pub(crate) fn recipient(env: &Env) -> Result<Address, Error> {
    match env.storage().persistent().get(&DataKey::YieldRecipient) {
        Some(r) => Ok(r),
        None => env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized),
    }
}

/// Recall `principal` from `strategy` and book the result.
///
/// The strategy's reported `(principal, yield)` is cross-checked against the
/// vault's actual token balance delta, so a strategy that under-pays is
/// rejected instead of being credited with principal it never returned.
/// Callers must hold the reentrancy lock.
pub(crate) fn recall_principal(
    env: &Env,
    strategy: &Address,
    principal: i128,
) -> Result<(i128, i128), Error> {
    let deployed = deployed_principal(env);
    if principal <= 0 || principal > deployed {
        return Err(Error::NothingToWithdraw);
    }

    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    let token_client = token::Client::new(env, &token_addr);
    let vault = env.current_contract_address();

    let before = token_client.balance(&vault);
    let (principal_returned, yield_returned) =
        YieldStrategyClient::new(env, strategy).withdraw(&principal);
    let received = token_client.balance(&vault) - before;

    if principal_returned <= 0
        || principal_returned > principal
        || yield_returned < 0
        || received < principal_returned + yield_returned
    {
        return Err(Error::InsufficientFloat);
    }

    let harvested: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::HarvestedYield)
        .unwrap_or(0);
    env.storage().persistent().set(
        &DataKey::DeployedPrincipal,
        &(deployed - principal_returned),
    );
    env.storage()
        .persistent()
        .set(&DataKey::HarvestedYield, &(harvested + yield_returned));
    persist_yield_ttl(env, &DataKey::DeployedPrincipal);
    persist_yield_ttl(env, &DataKey::HarvestedYield);

    let nonce = increment_nonce(env);
    YieldWithdrawnEvent {
        strategy: strategy.clone(),
        principal: principal_returned,
        yield_amount: yield_returned,
        nonce,
    }
    .publish(env);

    Ok((principal_returned, yield_returned))
}

/// Make sure the vault holds at least `needed` tokens liquid, recalling the
/// shortfall from the active strategy if necessary. `balance` is the vault's
/// current token balance (already read by the caller); the returned value is
/// the balance after any recall.
///
/// This is what keeps deployed principal instantly redeemable: a customer
/// refund or merchant withdrawal that outruns the liquid reserve pulls the
/// difference back from the strategy in the same invocation. When the vault
/// is already liquid enough no yield storage is touched at all, so ordinary
/// refunds pay nothing for the hook (issue #131). If nothing is deployed the
/// function is a no-op and the caller's own float check reports the shortfall.
///
/// Every caller already holds the reentrancy lock except `process_batch`
/// (best-effort, lock-free by design); for that path the lock is taken just
/// around the strategy call so an untrusted strategy still cannot re-enter.
pub(crate) fn ensure_liquidity(
    env: &Env,
    token_client: &token::Client,
    balance: i128,
    needed: i128,
) -> Result<i128, Error> {
    if balance >= needed {
        return Ok(balance);
    }
    let deployed = deployed_principal(env);
    if deployed <= 0 {
        return Ok(balance);
    }
    let Some(strategy) = active_strategy(env) else {
        return Ok(balance);
    };

    let recall = core::cmp::min(needed - balance, deployed);
    let held: bool = env
        .storage()
        .instance()
        .get(&DataKey::ReentrancyLock)
        .unwrap_or(false);
    if !held {
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyLock, &true);
    }
    recall_principal(env, &strategy, recall)?;
    if !held {
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyLock, &false);
    }
    Ok(token_client.balance(&env.current_contract_address()))
}

/// Recall every unit of deployed principal (plus its proportional yield).
/// Admin only; deliberately **not** gated on pause. Returns the principal
/// recalled.
pub(crate) fn emergency_exit(env: &Env) -> Result<i128, Error> {
    require_admin(env)?;
    let strategy = active_strategy(env).ok_or(Error::StrategyNotSet)?;
    let deployed = deployed_principal(env);
    if deployed <= 0 {
        return Err(Error::NothingToWithdraw);
    }
    let (principal_returned, _) = recall_principal(env, &strategy, deployed)?;
    Ok(principal_returned)
}

/// Pay all harvested yield to the yield recipient. Admin only. Returns the
/// amount paid.
pub(crate) fn distribute(env: &Env) -> Result<i128, Error> {
    require_admin(env)?;
    let harvested: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::HarvestedYield)
        .unwrap_or(0);
    if harvested <= 0 {
        return Err(Error::NothingToHarvest);
    }

    let to = recipient(env)?;
    let vault = env.current_contract_address();
    if to == vault {
        return Err(Error::SelfTransfer);
    }
    let token_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)?;
    let token_client = token::Client::new(env, &token_addr);
    if token_client.balance(&vault) < harvested {
        return Err(Error::InsufficientFloat);
    }

    // Book first, then transfer: the lock is held by the caller, and a
    // failing transfer reverts the whole invocation anyway.
    env.storage()
        .persistent()
        .set(&DataKey::HarvestedYield, &0i128);
    persist_yield_ttl(env, &DataKey::HarvestedYield);
    token_client.transfer(&vault, &to, &harvested);

    let nonce = increment_nonce(env);
    YieldDistributedEvent {
        recipient: to,
        amount: harvested,
        nonce,
    }
    .publish(env);
    Ok(harvested)
}
