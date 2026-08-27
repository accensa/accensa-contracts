#![no_std]

use accensa_common::Error;
use soroban_sdk::{
    contract, contractclient, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, BytesN, Env,
};

contractmeta!(key = "name", val = "RefundVault");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

contractmeta!(key = "commit_dirty", val = env!("GIT_DIRTY"));

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    RefundWindow,
    /// Address of the refund-policy contract every refund is routed to.
    /// Policy execution is external (see the [`RefundPolicy`] trait and the
    /// `refund-window-policy` crate), so a policy's rule can change without
    /// the vault's wasm changing.
    RefundPolicy,
    /// Cumulative refund record for a payment (new partial-refund layout).
    ///
    /// Stored under `RefundV2` so the decoder never attempts to interpret a
    /// legacy `Refund` record written by the single-refund rule.
    RefundV2(BytesN<32>),
    /// Legacy single-refund record (0.1.0 layout). Retained read-only for
    /// migration detection: a present `Refund` key means the payment was
    /// already fully refunded under the old rule.
    Refund(BytesN<32>),
    IsPaused,
    Metadata,
    RefundMax,
    Admins,
    Threshold,
    PendingAdmin,
    YieldStrategy,
    DeployedPrincipal,
    HarvestedYield,
    ReserveRatio,
    MaxDeployRatio,
    PendingPolicy,
    /// Reentrancy guard flag. Set for the duration of any entry point that
    /// makes an external call (token transfer or yield-strategy invocation)
    /// so a callback into another guarded entry point during that call is
    /// rejected rather than allowed to observe pre-update state.
    ReentrancyLock,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecord {
    /// Cumulative amount refunded so far for this payment.
    pub amount_refunded: i128,
    /// The original payment amount — the hard ceiling on cumulative refunds.
    pub payment_amount: i128,
    /// The ledger at which the original payment occurred (window is measured
    /// from here, never from a partial).
    pub paid_at_ledger: u32,
    pub recipient: Address,
    /// Ledger of the most recent refund call.
    pub ledger: u32,
}

/// A pending policy change waiting for the timelock to expire.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyProposal {
    pub window: u32,
    pub proposed_at_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldInfo {
    pub deployed_principal: i128,
    pub harvested_yield: i128,
    pub strategy: Option<Address>,
    pub reserve_ratio: u32,
    pub max_deploy_ratio: u32,
}

/// Emitted when a (possibly partial) refund is made from the vault float.
///
/// Topics: `("refund_event", payment_ref)`. The data map carries the amount
/// for **this call** (`amount`) and the running total after it
/// (`cumulative_refunded`), so an indexer knows the state of a payment without
/// summing history.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundEvent {
    #[topic]
    pub payment_ref: BytesN<32>,
    /// Amount refunded in this call.
    pub amount: i128,
    /// Running cumulative total across all refunds for this payment.
    pub cumulative_refunded: i128,
    pub recipient: Address,
    pub ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositEvent {
    #[topic]
    pub from: Address,
    pub amount: i128,
}

/// Emitted when the merchant pauses the vault, halting deposits, refunds and withdrawals.
///
/// Topics: `("pause_event", ledger)`. The ledger sequence lets an indexer
/// reconstruct the pause window from the event log alone.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PauseEvent {
    #[topic]
    pub ledger: u32,
}

/// Emitted when the merchant unpauses the vault.
///
/// Topics: `("unpause_event", ledger)`. Together with `PauseEvent` this
/// brackets a pause window in the event log.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnpauseEvent {
    #[topic]
    pub ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawEvent {
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferInitiatedEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferAcceptedEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldDeployedEvent {
    #[topic]
    pub strategy: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldWithdrawnEvent {
    #[topic]
    pub strategy: Address,
    pub principal: i128,
    pub yield_amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldHarvestedEvent {
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyProposedEvent {
    #[topic]
    pub window: u32,
    pub proposed_at_ledger: u32,
    pub execute_after_ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyExecutedEvent {
    #[topic]
    pub window: u32,
}

/// Interface for external yield-generating strategies (e.g., Soroban lending protocols).
///
/// Any contract that implements these methods can be registered as the vault's yield
/// strategy. The vault calls these to deploy idle funds and harvest accrued yield.
/// The trait is annotated `#[contractclient(name = "YieldStrategyClient")]` (not
/// `#[contractimpl]`, which only accepts `impl` blocks) so its client is
/// generated from the interface.
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

/// Interface of the refund-policy contract every refund is routed to
/// (issue #129).
///
/// Policy execution is deliberately external: the vault passes every input the
/// rule could need — the payment reference, the requested refund `amount`, the
/// payment's `paid_at_ledger` and `payment_amount`, the running cumulative
/// refund total, and the configured `refund_window_ledgers` — and the policy
/// returns `Ok(())` or a deliberate error (`WindowExpired`, ...). The default
/// `RefundWindowPolicy` (`contracts/refund-window-policy`) is stateless; other
/// policy kinds are new contracts implementing this same fixed signature, so
/// the vault wasm never changes when a policy rule changes.
///
/// Declared as a trait (rather than depending on the `refund-window-policy`
/// crate) so `#[contractclient]` can generate `RefundPolicyClient` without
/// pulling the policy's own `#[contract]` exports into this contract's wasm —
/// the same trick the `ReceiptAnchor` uses for its `ShardInterface`.
#[contractclient(name = "RefundPolicyClient")]
pub trait RefundPolicy {
    /// Decide whether a refund of `amount` against `payment_ref` is allowed.
    ///
    /// * `paid_at_ledger` — ledger of the original payment (window is measured
    ///   from here, never from a partial).
    /// * `payment_amount` — the original payment amount (the ceiling).
    /// * `cumulative_refunded` — running total refunded so far for this payment.
    /// * `refund_window_ledgers` — the configured window; `0` means "no time
    ///   bound".
    fn check_refund(
        env: Env,
        payment_ref: BytesN<32>,
        amount: i128,
        paid_at_ledger: u32,
        payment_amount: i128,
        cumulative_refunded: i128,
        refund_window_ledgers: u32,
    ) -> Result<(), Error>;
}

/// Approximately 30 days of ledgers, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 30 / 5 = 518,400.
/// This ensures refund records survive long-term audit use before requiring a TTL bump or restoration.
const TTL_EXTEND: u32 = 518_400;
/// The threshold before TTL is actually bumped, to prevent spamming updates on every call.
const TTL_THRESHOLD: u32 = 100;
/// Timelock delay for policy changes in ledgers (~24 hours at 5s/ledger).
const POLICY_TIMELOCK: u32 = 17_280;

/// Reentrancy guard for entry points that make an external call (a token
/// transfer or a yield-strategy invocation).
///
/// Soroban does not have EVM-style fallback functions, but an external call
/// still hands control to arbitrary contract code before this contract's own
/// state update runs: a non-standard token can invoke recipient/sender hooks
/// during `transfer`, and a registered yield strategy is fully untrusted
/// (`docs/AUDIT.md` §5, known issue #7) and can call straight back into any
/// `RefundVault` entry point from inside `deposit`/`withdraw`/`harvest`. A
/// single shared instance-storage flag protects every such entry point:
/// whichever one is first sets the flag before doing its external call and
/// clears it only after its own state has been fully written, so a reentrant
/// call — into the same entry point or a different one — observes the flag
/// set and is rejected with [`Error::ReentrancyBlocked`] instead of racing
/// ahead of the pending state update.
///
/// Because a `Result::Err` returned from a contract entry point rolls back
/// every storage write that invocation made (including the flag itself),
/// callers do not need to clear the flag on error paths — only the success
/// path needs an explicit `release_reentrancy_lock` call.
fn acquire_reentrancy_lock(env: &Env) -> Result<(), Error> {
    let locked: bool = env
        .storage()
        .instance()
        .get(&DataKey::ReentrancyLock)
        .unwrap_or(false);
    if locked {
        return Err(Error::ReentrancyBlocked);
    }
    env.storage()
        .instance()
        .set(&DataKey::ReentrancyLock, &true);
    Ok(())
}

fn release_reentrancy_lock(env: &Env) {
    env.storage()
        .instance()
        .set(&DataKey::ReentrancyLock, &false);
}

/// How many ledgers to extend a payment's `RefundV2` record's TTL by, so the
/// double-refund guard cannot go archived while `refund` calls against that
/// payment are still policy-valid.
///
/// The guard in `refund` is `storage().persistent().get/has(RefundV2(..))`,
/// backed by a persistent entry whose TTL was, before this fix, always bumped
/// by a flat [`TTL_EXTEND`] (~30 days) regardless of the merchant's configured
/// `refund_window_ledgers`. A window longer than 30 days — or `0`, which
/// `refund` treats as "no time bound" — could then legitimately still accept
/// a partial refund on a payment whose guard entry had already aged past its
/// TTL and gone archived, because nothing but `refund` itself (or the manual
/// `extend_refund_ttl`) ever touched that TTL. Sizing the extension to the
/// window itself closes that gap: the record is kept live for exactly as
/// long as the policy says another `refund` call could legitimately arrive.
///
/// `window == 0` mirrors `refund`'s own "no expiry" semantics: rather than
/// picking an arbitrary flat interval, extend to the network's actual
/// maximum TTL so the guard is never the reason a policy that says "any time"
/// stops holding.
///
/// Callers must pass the *returned value itself* as `extend_ttl`'s
/// `threshold` argument, not [`TTL_THRESHOLD`]. A freshly written entry
/// already carries the network's `min_persistent_entry_ttl` floor, which on
/// any realistic network exceeds `TTL_THRESHOLD` (100 ledgers, ~8 minutes) —
/// so `extend_ttl(TTL_THRESHOLD, extend_to)` is a no-op right after `set`,
/// no matter what `extend_to` is, and the record is left at the network
/// floor rather than the intended TTL. Using the target as its own
/// threshold (`extend_ttl(extend_to, extend_to)`) instead extends whenever
/// the current TTL is below what's needed, which is the actual invariant
/// this guard is supposed to hold.
fn refund_record_ttl_extend_to(env: &Env, window: u32, paid_at_ledger: u32) -> u32 {
    if window == 0 {
        return env.storage().max_ttl();
    }
    let target_live_until = paid_at_ledger.saturating_add(window);
    let current_ledger = env.ledger().sequence();
    target_live_until
        .saturating_sub(current_ledger)
        .max(TTL_EXTEND)
}

#[contract]
pub struct RefundVault;

#[contractimpl]
impl RefundVault {
    /// Constructor invoked atomically by [`RefundVaultFactory::deploy`] via
    /// `deploy_v2`, initialising the vault at deploy time (mirrors `ReceiptShard`,
    /// which is also constructor-only). A vault is always initialised when it
    /// exists on-chain; there is no separate `initialize` step to call
    /// afterwards.
    pub fn __constructor(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
        refund_policy: Address,
    ) {
        env.storage().instance().set(&DataKey::Admin, &merchant);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &refund_window_ledgers);
        env.storage()
            .instance()
            .set(&DataKey::RefundPolicy, &refund_policy);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
    }

    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        if from != merchant {
            return Err(Error::Unauthorized);
        }

        let token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let client = token::Client::new(&env, &token);
        client.transfer(&from, env.current_contract_address(), &amount);

        DepositEvent {
            from: from.clone(),
            amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    pub fn set_token(env: Env, new_token: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let current_token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &current_token);
        let balance = token_client.balance(&env.current_contract_address());
        if balance > 0 {
            return Err(Error::FloatNotEmpty);
        }

        env.storage().instance().set(&DataKey::Token, &new_token);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Refund part (or all) of an original payment.
    ///
    /// `payment_amount` is the original payment amount and therefore the hard
    /// ceiling: cumulative refunds for a payment may never exceed it. It is
    /// supplied by the merchant on **every** call, mirroring how `paid_at_ledger`
    /// is supplied, so the ceiling never depends on partial bookkeeping. The
    /// refund window is evaluated against `paid_at_ledger` (the original
    /// payment), not against a previous partial — each partial does not extend
    /// the window for the next. The window rule itself is not evaluated by
    /// this contract: it is routed to the vault's bound refund-policy contract
    /// (issue #129), which receives the payment inputs and the configured
    /// window and permits or rejects the refund.
    ///
    /// Storage note (#99): the layout changed from a single `amount` record to a
    /// cumulative record under a new `RefundV2` key. A `Refund` key written by
    /// the legacy single-refund rule still denotes a fully-refunded payment and
    /// is rejected with [`Error::ExceedsPayment`] rather than a silent
    /// misinterpretation.
    pub fn refund(
        env: Env,
        payment_ref: BytesN<32>,
        recipient: Address,
        amount: i128,
        paid_at_ledger: u32,
        payment_amount: i128,
    ) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if recipient == env.current_contract_address() {
            return Err(Error::SelfTransfer);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        // Legacy record: the payment was fully refunded under the single-refund
        // rule. Reject explicitly rather than mis-decoding the old shape.
        if env
            .storage()
            .persistent()
            .has(&DataKey::Refund(payment_ref.clone()))
        {
            return Err(Error::ExceedsPayment);
        }

        // Read the cumulative record first: the policy call needs the running
        // total, and the ceiling check below needs the stored payment amount.
        // The ceiling is read from the (re)stored record, freshly minted on the
        // first partial for this payment.
        let existing: Option<RefundRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::RefundV2(payment_ref.clone()));
        let (previous_refunded, record_ceiling) = match existing {
            Some(rec) => (rec.amount_refunded, rec.payment_amount),
            None => (0i128, payment_amount),
        };

        // Policy execution is external (issue #129): route the window rule to
        // the configured policy contract rather than evaluating it inline. The
        // policy is stateless and receives every input the rule could need.
        let window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap();
        let refund_policy: Address = env
            .storage()
            .instance()
            .get(&DataKey::RefundPolicy)
            .unwrap();
        Self::invoke_policy(
            &env,
            &refund_policy,
            &payment_ref,
            &amount,
            &paid_at_ledger,
            &payment_amount,
            &previous_refunded,
            &window,
        )?;

        // Ceiling check: cumulative refunds must not exceed the original amount.
        if previous_refunded.checked_add(amount).is_none()
            || record_ceiling <= 0
            || previous_refunded + amount > record_ceiling
        {
            return Err(Error::ExceedsPayment);
        }

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < amount {
            return Err(Error::InsufficientFloat);
        }

        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        let cumulative_refunded = previous_refunded + amount;
        let current_ledger = env.ledger().sequence();
        let record = RefundRecord {
            amount_refunded: cumulative_refunded,
            payment_amount: record_ceiling,
            paid_at_ledger,
            recipient: recipient.clone(),
            ledger: current_ledger,
        };

        env.storage()
            .persistent()
            .set(&DataKey::RefundV2(payment_ref.clone()), &record);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        let extend_to = refund_record_ttl_extend_to(&env, window, paid_at_ledger);
        // Threshold == extend_to (not TTL_THRESHOLD): see
        // `refund_record_ttl_extend_to` for why a small fixed threshold makes
        // this a no-op on a freshly-written entry.
        env.storage().persistent().extend_ttl(
            &DataKey::RefundV2(payment_ref.clone()),
            extend_to,
            extend_to,
        );

        RefundEvent {
            payment_ref,
            amount,
            cumulative_refunded,
            recipient: record.recipient,
            ledger: record.ledger,
        }
        .publish(&env);

        release_reentrancy_lock(&env);
        Ok(())
    }

    pub fn withdraw(env: Env, amount: i128, to: Address) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if to == env.current_contract_address() {
            return Err(Error::SelfTransfer);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < amount {
            return Err(Error::InsufficientFloat);
        }

        token_client.transfer(&env.current_contract_address(), &to, &amount);

        WithdrawEvent {
            to: to.clone(),
            amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Propose a new refund window. The change is not applied immediately;
    /// the admin must call `execute_policy` after the timelock (17,280 ledgers,
    /// ~24 hours) has elapsed. Proposing a new policy overwrites any existing
    /// pending proposal.
    pub fn propose_policy(env: Env, ledgers: u32) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let current_ledger = env.ledger().sequence();
        let proposal = PolicyProposal {
            window: ledgers,
            proposed_at_ledger: current_ledger,
        };

        env.storage()
            .instance()
            .set(&DataKey::PendingPolicy, &proposal);

        PolicyProposedEvent {
            window: ledgers,
            proposed_at_ledger: current_ledger,
            execute_after_ledger: current_ledger + POLICY_TIMELOCK,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Execute a pending policy change. Fails if no policy is pending or if
    /// the timelock has not yet expired.
    pub fn execute_policy(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let proposal: PolicyProposal = env
            .storage()
            .instance()
            .get(&DataKey::PendingPolicy)
            .ok_or(Error::NoPendingPolicy)?;

        let current_ledger = env.ledger().sequence();
        if current_ledger < proposal.proposed_at_ledger + POLICY_TIMELOCK {
            return Err(Error::TimelockNotExpired);
        }

        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &proposal.window);
        env.storage().instance().remove(&DataKey::PendingPolicy);

        PolicyExecutedEvent {
            window: proposal.window,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Point the vault at a different refund-policy contract. Only callable by
    /// the merchant. The new policy must implement the same `check_refund`
    /// interface; refunds after this call are routed to it, so a policy rule
    /// can change without redeploying the vault.
    pub fn set_refund_policy(env: Env, refund_policy: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::RefundPolicy, &refund_policy);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// The refund-policy contract address this vault is bound to, if set.
    pub fn get_refund_policy(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::RefundPolicy)
    }

    /// Call the bound policy contract and translate its result into the
    /// vault's own error space (mirrors `ReceiptAnchor::unwrap_shard_result`).
    /// A deliberate policy error (`WindowExpired`, ...) propagates as-is; a
    /// host-level invocation failure or a value that fails to decode becomes
    /// [`Error::PolicyCallFailed`].
    fn invoke_policy(
        env: &Env,
        refund_policy: &Address,
        payment_ref: &BytesN<32>,
        amount: &i128,
        paid_at_ledger: &u32,
        payment_amount: &i128,
        cumulative_refunded: &i128,
        refund_window_ledgers: &u32,
    ) -> Result<(), Error> {
        match RefundPolicyClient::new(env, refund_policy).try_check_refund(
            payment_ref,
            amount,
            paid_at_ledger,
            payment_amount,
            cumulative_refunded,
            refund_window_ledgers,
        ) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(Err(_)) => Err(Error::PolicyCallFailed),
            Err(Ok(e)) => Err(e),
        }
    }

    pub fn get_refund(env: Env, payment_ref: BytesN<32>) -> Option<RefundRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::RefundV2(payment_ref))
    }

    /// Returns the current pending policy proposal, if any.
    pub fn get_pending_policy(env: Env) -> Option<PolicyProposal> {
        env.storage().instance().get(&DataKey::PendingPolicy)
    }

    /// Returns the policy timelock delay in ledgers (read-only).
    pub fn get_policy_timelock() -> u32 {
        POLICY_TIMELOCK
    }

    // ── Yield strategy management ──────────────────────────────────────────

    /// Register an external yield strategy contract. Only callable by admin.
    pub fn set_yield_strategy(env: Env, strategy: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::YieldStrategy, &strategy);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Set the minimum reserve ratio in basis points (1 bp = 0.01%).
    /// E.g., 2000 = 20% of total vault value must remain as liquid token balance.
    pub fn set_reserve_ratio(env: Env, basis_points: u32) -> Result<(), Error> {
        if basis_points > 10_000 {
            return Err(Error::InvalidRatio);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::ReserveRatio, &basis_points);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Set the maximum deployment ratio in basis points.
    /// E.g., 8000 = at most 80% of total vault value can be deployed to yield.
    pub fn set_max_deploy_ratio(env: Env, basis_points: u32) -> Result<(), Error> {
        if basis_points > 10_000 {
            return Err(Error::InvalidRatio);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::MaxDeployRatio, &basis_points);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Deploy idle vault tokens into the registered yield strategy.
    ///
    /// Enforces:
    /// - Strategy must be configured
    /// - Amount must be positive
    /// - Post-deployment liquid balance >= reserve_ratio * total_value
    /// - Total deployed <= max_deploy_ratio * total_value
    pub fn deploy_to_yield(env: Env, amount: i128) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let strategy: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)?;

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        let token_balance = token_client.balance(&env.current_contract_address());

        if token_balance < amount {
            return Err(Error::InsufficientFloat);
        }

        let deployed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::DeployedPrincipal)
            .unwrap_or(0);
        let harvested: i128 = env
            .storage()
            .instance()
            .get(&DataKey::HarvestedYield)
            .unwrap_or(0);

        // total_value = liquid tokens + deployed principal
        // (harvested yield has already been transferred to the vault and is part of token_balance,
        //  but it belongs to the operator, not the principal pool — subtract it)
        let total_value = token_balance + deployed - harvested;

        // Reserve check: after deployment, liquid tokens must cover the reserve.
        let reserve_ratio: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ReserveRatio)
            .unwrap_or(0);
        let post_deploy_balance = token_balance - amount;
        let reserve_required = total_value * reserve_ratio as i128 / 10_000;
        if post_deploy_balance < reserve_required {
            return Err(Error::InsufficientReserve);
        }

        // Max deployment check.
        let max_deploy_ratio: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxDeployRatio)
            .unwrap_or(10_000);
        let post_deploy_total = deployed + amount;
        let max_deploy = total_value * max_deploy_ratio as i128 / 10_000;
        if post_deploy_total > max_deploy {
            return Err(Error::DeploymentExceedsMax);
        }

        // Transfer tokens to strategy, then notify the strategy of the deposit
        // (it needs to record the principal so it can return it on withdrawal).
        token_client.transfer(&env.current_contract_address(), &strategy, &amount);
        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        strategy_client.deposit(&amount);

        env.storage()
            .instance()
            .set(&DataKey::DeployedPrincipal, &(deployed + amount));

        YieldDeployedEvent {
            strategy: strategy.clone(),
            amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Withdraw principal from the yield strategy. The strategy returns the requested
    /// principal plus any proportional accrued yield.
    ///
    /// `principal` is the amount of originally-deployed principal to reclaim.
    pub fn withdraw_from_yield(env: Env, principal: i128) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        if principal <= 0 {
            return Err(Error::InvalidAmount);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let strategy: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)?;

        let deployed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::DeployedPrincipal)
            .unwrap_or(0);
        if principal > deployed {
            return Err(Error::NothingToWithdraw);
        }

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let (principal_returned, yield_returned) = strategy_client.withdraw(&principal);

        let harvested: i128 = env
            .storage()
            .instance()
            .get(&DataKey::HarvestedYield)
            .unwrap_or(0);

        env.storage().instance().set(
            &DataKey::DeployedPrincipal,
            &(deployed - principal_returned),
        );
        env.storage()
            .instance()
            .set(&DataKey::HarvestedYield, &(harvested + yield_returned));

        YieldWithdrawnEvent {
            strategy,
            principal: principal_returned,
            yield_amount: yield_returned,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Harvest accrued yield from the strategy without touching deployed principal.
    /// Yield tokens are transferred to the vault and tracked for operator withdrawal.
    pub fn harvest_yield(env: Env) -> Result<(), Error> {
        acquire_reentrancy_lock(&env)?;

        if env
            .storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let strategy: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)?;

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let yield_amount = strategy_client.harvest();

        if yield_amount <= 0 {
            return Err(Error::NothingToHarvest);
        }

        let harvested: i128 = env
            .storage()
            .instance()
            .get(&DataKey::HarvestedYield)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::HarvestedYield, &(harvested + yield_amount));

        YieldHarvestedEvent {
            amount: yield_amount,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Read-only: returns current yield strategy state.
    pub fn get_yield_info(env: Env) -> YieldInfo {
        YieldInfo {
            deployed_principal: env
                .storage()
                .instance()
                .get(&DataKey::DeployedPrincipal)
                .unwrap_or(0),
            harvested_yield: env
                .storage()
                .instance()
                .get(&DataKey::HarvestedYield)
                .unwrap_or(0),
            strategy: env.storage().instance().get(&DataKey::YieldStrategy),
            reserve_ratio: env
                .storage()
                .instance()
                .get(&DataKey::ReserveRatio)
                .unwrap_or(0),
            max_deploy_ratio: env
                .storage()
                .instance()
                .get(&DataKey::MaxDeployRatio)
                .unwrap_or(10_000),
        }
    }

    // ── Existing admin functions ───────────────────────────────────────────

    pub fn pause(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage().instance().set(&DataKey::IsPaused, &true);

        PauseEvent {
            ledger: env.ledger().sequence(),
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage().instance().set(&DataKey::IsPaused, &false);

        UnpauseEvent {
            ledger: env.ledger().sequence(),
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn extend_refund_ttl(env: Env, payment_ref: BytesN<32>) -> Result<(), Error> {
        let record: RefundRecord = env
            .storage()
            .persistent()
            .get(&DataKey::RefundV2(payment_ref.clone()))
            .ok_or(Error::RefundNotFound)?;

        let window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap();

        let extend_to = refund_record_ttl_extend_to(&env, window, record.paid_at_ledger);
        // Threshold == extend_to: a caller invoking this well before expiry
        // (which is the whole point of a manual top-up) must still see it
        // take effect. TTL_THRESHOLD (100 ledgers, ~8 minutes) would make
        // this silently succeed as a no-op unless called in that final
        // sliver before the entry actually expires.
        env.storage().persistent().extend_ttl(
            &DataKey::RefundV2(payment_ref),
            extend_to,
            extend_to,
        );
        Ok(())
    }

    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let current_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        current_admin.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);

        AdminTransferInitiatedEvent {
            from: current_admin,
            to: new_admin,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn accept_admin(env: Env) -> Result<(), Error> {
        let pending_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingTransfer)?;
        pending_admin.require_auth();

        let previous_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();

        env.storage()
            .instance()
            .set(&DataKey::Admin, &pending_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);

        AdminTransferAcceptedEvent {
            from: previous_admin,
            to: pending_admin,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    pub fn cancel_admin_transfer(env: Env) -> Result<(), Error> {
        let current_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        current_admin.require_auth();

        if !env.storage().instance().has(&DataKey::PendingAdmin) {
            return Err(Error::NoPendingTransfer);
        }

        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }
}

mod fuzz_test;
mod reentrancy_tests;
mod test;
mod token_agnostic_tests;
mod yield_tests;
