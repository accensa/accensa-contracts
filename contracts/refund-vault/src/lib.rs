#![no_std]

use accensa_common::Error;
use soroban_sdk::{
    contract, contractclient, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, BytesN, Env, Symbol, Vec,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundParam {
    pub payment_ref: BytesN<32>,
    pub recipient: Address,
    pub amount: i128,
    pub paid_at_ledger: u32,
    pub payment_amount: i128,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    RefundWindow,
    /// Wall-clock deadline (Unix timestamp) after which refund claims are
    /// rejected. `0` (the default) means no deadline. Configured with the
    /// policy (propose/execute) and read at claim time in `refund`.
    RefundDeadline,
    /// Refund fee, in basis points (1 bp = 0.01%), deducted from the amount
    /// sent to a refund recipient and paid to the fee recipient. `0` (the
    /// default) means no fee. Set via `set_fee_bps` and read at claim time.
    FeeBps,
    /// Address that receives the fee deducted from each refund. When unset,
    /// the merchant (admin) receives the fee. Set via `set_fee_recipient`.
    FeeRecipient,
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
    /// Monotonic operation counter incremented on every successful
    /// state-changing call (issue #136).
    Nonce,
    /// Domain separator — the SHA-256 of the contract address, stored at
    /// initialisation (issue #136).
    DomainSeparator,
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
    /// Wall-clock deadline (Unix timestamp) after which refund claims are
    /// rejected. `0` disables the deadline ("no expiry").
    pub deadline: u64,
    pub proposed_at_ledger: u32,
}

/// Parameters for a single refund claim, mirroring the arguments of
/// [`RefundVault::refund`]. One element of a [`RefundVault::claim_batch`]
/// call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundClaim {
    pub payment_ref: BytesN<32>,
    pub recipient: Address,
    /// Amount to refund in this call (before any configured fee is deducted).
    pub amount: i128,
    /// Ledger at which the original payment occurred (window is measured from
    /// here, never from a partial).
    pub paid_at_ledger: u32,
    /// The original payment amount — the hard ceiling on cumulative refunds —
    /// supplied fresh on every claim.
    pub payment_amount: i128,
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
    /// Amount refunded in this call (before the fee is deducted).
    pub amount: i128,
    /// The fee deducted from `amount` and paid to the fee recipient in this
    /// call. `0` when no fee is configured.
    pub fee: i128,
    /// Running cumulative total across all refunds for this payment.
    pub cumulative_refunded: i128,
    pub recipient: Address,
    pub ledger: u32,
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
}

/// Emitted when the admin changes the refund fee configuration (the basis-point
/// rate or the fee recipient).
///
/// Topics: `("fee_config_updated_event", field)` where `field` is the symbol
/// `fee_bps` or `fee_recipient`. The data map carries the *full* effective
/// configuration after the change, so a reader reconstructing fee logic never
/// needs to inspect two events.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeConfigUpdatedEvent {
    #[topic]
    pub field: Symbol,
    pub fee_bps: u32,
    pub fee_recipient: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositEvent {
    #[topic]
    pub from: Address,
    pub amount: i128,
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
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
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
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
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldWithdrawnEvent {
    #[topic]
    pub strategy: Address,
    pub principal: i128,
    pub yield_amount: i128,
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldHarvestedEvent {
    pub amount: i128,
    /// Monotonic nonce at the time of this operation (issue #136).
    pub nonce: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyProposedEvent {
    #[topic]
    pub window: u32,
    pub deadline: u64,
    pub proposed_at_ledger: u32,
    pub execute_after_ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyExecutedEvent {
    #[topic]
    pub window: u32,
    pub deadline: u64,
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

/// Increment the monotonic nonce and return its *previous* value (issue #136).
fn increment_nonce(env: &Env) -> u64 {
    let current: u64 = env.storage().instance().get(&DataKey::Nonce).unwrap_or(0);
    env.storage()
        .instance()
        .set(&DataKey::Nonce, &(current + 1));
    current
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

/// Refund fee in raw token units: `ceil(amount * fee_bps / 10_000)`.
///
/// Rounding **always rounds up**, so a remainder smaller than one smallest
/// unit of the token is collected by the protocol (the fee recipient) rather
/// than silently dropped.
///
/// The computation is overflow-free for every valid input (`amount > 0`,
/// `fee_bps <= 10_000`) without host 256-bit arithmetic: decomposing
/// `amount = q*10_000 + r` gives the equivalent `q*fee_bps + ceil(r*fee_bps/10_000)`,
/// where `q*fee_bps <= q*10_000 <= amount` fits in i128 and the remainder term
/// `r*fee_bps` never exceeds `9_999 * 10_000`.
fn refund_fee(amount: i128, fee_bps: u32) -> i128 {
    let q = amount / 10_000;
    let r = amount % 10_000;
    q * fee_bps as i128 + (r * fee_bps as i128 + 9_999) / 10_000
}

/// The address that receives refund fees: the explicitly-configured fee
/// recipient when one has been set, otherwise the merchant (admin). Fees thus
/// always have a deterministic destination and can never silently vanish into
/// an unconfigured "dead" address.
fn active_fee_recipient(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::FeeRecipient)
        .unwrap_or_else(|| {
            env.storage()
                .instance()
                .get(&DataKey::Admin)
                .expect("refund requires an initialized admin")
        })
}

/// Shared single-claim logic used by [`RefundVault::refund`],
/// [`RefundVault::claim_batch`], and [`RefundVault::process_batch`].
///
/// The caller is responsible for the per-invocation concerns: acquiring the
/// reentrancy lock, checking `IsPaused`, and authorizing the merchant. This
/// function applies the claim itself — validations (amount, self-transfer,
/// legacy record, window, deadline, ceiling, float), fee split and transfers,
/// cumulative-record storage and TTL extension, and the [`RefundEvent`].
///
/// The float is read from the token contract fresh on **every** call, so a
/// batch that overdraws the vault on a later claim fails there exactly as a
/// sequence of single refunds would.
fn claim_single(env: &Env, claim: &RefundClaim) -> Result<(), Error> {
    if claim.amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    if claim.recipient == env.current_contract_address() {
        return Err(Error::SelfTransfer);
    }

    // Legacy record: the payment was fully refunded under the single-refund
    // rule. Reject explicitly rather than mis-decoding the old shape.
    if env
        .storage()
        .persistent()
        .has(&DataKey::Refund(claim.payment_ref.clone()))
    {
        return Err(Error::ExceedsPayment);
    }

    let window: u32 = env
        .storage()
        .instance()
        .get(&DataKey::RefundWindow)
        .unwrap();
    if window > 0 {
        let current_ledger = env.ledger().sequence();
        if current_ledger > claim.paid_at_ledger + window {
            return Err(Error::WindowExpired);
        }
    }

    // Policy deadline: a wall-clock timestamp configured with the policy.
    // `0` (or unset) means no deadline. Expiry is strictly past the deadline,
    // so a claim landing exactly on the deadline still succeeds.
    let deadline: u64 = env
        .storage()
        .instance()
        .get(&DataKey::RefundDeadline)
        .unwrap_or(0);
    if deadline > 0 && env.ledger().timestamp() > deadline {
        return Err(Error::RefundExpired);
    }

    // Ceiling check: cumulative refunds must not exceed the original amount.
    // The ceiling is read from the (re)stored record, freshly minted on the
    // first partial for this payment.
    let existing: Option<RefundRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::RefundV2(claim.payment_ref.clone()));
    let (previous_refunded, record_ceiling) = match existing {
        Some(rec) => (rec.amount_refunded, rec.payment_amount),
        None => (0i128, claim.payment_amount),
    };

    if previous_refunded.checked_add(claim.amount).is_none()
        || record_ceiling <= 0
        || previous_refunded + claim.amount > record_ceiling
    {
        return Err(Error::ExceedsPayment);
    }

    let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
    let token_client = token::Client::new(env, &token_addr);
    let balance = token_client.balance(&env.current_contract_address());
    if balance < claim.amount {
        return Err(Error::InsufficientFloat);
    }

    // Fee: a fraction (basis points) of the claim is diverted to the fee
    // recipient; `recipient` receives the remainder. Total outflow is still
    // exactly `amount`, so the float check above and the ceiling check against
    // the payment amount are unchanged. The fee rounds *up* (the
    // fractional-token remainder goes to the protocol).
    let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
    let fee = refund_fee(claim.amount, fee_bps);
    let payout = claim.amount - fee;

    let fee_recipient = if fee > 0 {
        let r = active_fee_recipient(env);
        if r == env.current_contract_address() {
            return Err(Error::SelfTransfer);
        }
        Some(r)
    } else {
        None
    };

    token_client.transfer(&env.current_contract_address(), &claim.recipient, &payout);
    if let Some(r) = fee_recipient {
        token_client.transfer(&env.current_contract_address(), &r, &fee);
    }

    let cumulative_refunded = previous_refunded + claim.amount;
    let current_ledger = env.ledger().sequence();
    let record = RefundRecord {
        amount_refunded: cumulative_refunded,
        payment_amount: record_ceiling,
        paid_at_ledger: claim.paid_at_ledger,
        recipient: claim.recipient.clone(),
        ledger: current_ledger,
    };

    env.storage()
        .persistent()
        .set(&DataKey::RefundV2(claim.payment_ref.clone()), &record);

    env.storage()
        .instance()
        .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
    let extend_to = refund_record_ttl_extend_to(env, window, claim.paid_at_ledger);
    // Threshold == extend_to (not TTL_THRESHOLD): see
    // `refund_record_ttl_extend_to` for why a small fixed threshold makes
    // this a no-op on a freshly-written entry.
    env.storage().persistent().extend_ttl(
        &DataKey::RefundV2(claim.payment_ref.clone()),
        extend_to,
        extend_to,
    );

    let nonce = increment_nonce(env);

    RefundEvent {
        payment_ref: claim.payment_ref.clone(),
        amount: claim.amount,
        fee,
        cumulative_refunded,
        recipient: record.recipient,
        ledger: record.ledger,
        nonce,
    }
    .publish(env);

    Ok(())
}

/// Maximum number of refund requests allowed in a single `process_batch` call.
/// Bounds CPU and memory usage to ensure the transaction stays within Soroban
/// limits.
const MAX_REFUND_BATCH_SIZE: u32 = 100;

#[contract]
pub struct RefundVault;

#[contractimpl]
impl RefundVault {
    pub fn initialize(
        env: Env,
        merchant: Address,
        token: Address,
        refund_window_ledgers: u32,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &merchant);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &refund_window_ledgers);

        // Issue #136: domain separator and nonce.
        let contract_addr = env.current_contract_address();
        let addr_str = contract_addr.to_string();
        let separator = env.crypto().sha256(&soroban_sdk::Bytes::from(addr_str));
        env.storage()
            .instance()
            .set(&DataKey::DomainSeparator, &separator);
        env.storage().instance().set(&DataKey::Nonce, &0u64);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Domain separator for this vault instance (issue #136).
    pub fn get_domain_separator(env: Env) -> BytesN<32> {
        env.storage()
            .instance()
            .get(&DataKey::DomainSeparator)
            .unwrap()
    }

    /// Current monotonic nonce (issue #136).
    pub fn get_nonce(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
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

        let nonce = increment_nonce(&env);

        DepositEvent {
            from: from.clone(),
            amount,
            nonce,
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
    /// the window for the next.
    ///
    /// This is a thin wrapper around the same shared claim path as
    /// [`RefundVault::claim_batch`].
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

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let claim = RefundClaim {
            payment_ref,
            recipient,
            amount,
            paid_at_ledger,
            payment_amount,
        };
        claim_single(&env, &claim)?;

        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Refund multiple claims in a single transaction.
    ///
    /// Every element of `claims` is processed in order with exactly the same
    /// logic as a [`RefundVault::refund`] call — validations, ceilings, fees,
    /// the float check, cumulative-record storage, TTL extension and a
    /// [`RefundEvent`] per element — so the whole batch shares one merchant
    /// authorization and one reentrancy-lock acquisition. Unrelated
    /// `payment_ref`s are independent; repeated refs accumulate against the
    /// same ceiling across elements.
    ///
    /// The float is read afresh from the token contract before every element,
    /// so a batch can never overdraw the vault any more than an equivalent
    /// sequence of single refunds, and `paid_at_ledger` / `payment_amount` are
    /// evaluated per claim.
    ///
    /// # Atomicity
    ///
    /// If any element fails, the call returns that error. A contract error
    /// reverts the entire Soroban invocation — including the token transfers,
    /// storage writes and events of the claims that already succeeded within
    /// this call — so the batch is all-or-nothing: either every claim
    /// persists, or none of them do.
    ///
    /// An empty `claims` vector succeeds as a no-op.
    pub fn claim_batch(env: Env, claims: Vec<RefundClaim>) -> Result<(), Error> {
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

        for claim in claims.iter() {
            claim_single(&env, &claim)?;
        }
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Processes a batch of refund requests in a single transaction.
    ///
    /// Design choice: Best-effort execution model with per-item result booleans.
    /// Each refund is processed with exactly the same per-claim logic as
    /// [`RefundVault::refund`] (via the shared `claim_single` helper), so the
    /// pause, auth, window, deadline, ceiling, float, and fee checks all apply
    /// per item. If an individual refund fails (e.g. `ExceedsPayment` or
    /// `WindowExpired`), it records `false` for that item and continues
    /// processing subsequent items rather than aborting the entire batch. This
    /// allows valid refunds in a multi-item batch to complete successfully.
    ///
    /// Unlike [`RefundVault::claim_batch`], this is *not* atomic: a failing
    /// item does not roll back the others, and no reentrancy lock is held, so
    /// callers that require all-or-nothing semantics should use `claim_batch`.
    pub fn process_batch(env: Env, refunds: Vec<RefundParam>) -> Result<Vec<bool>, Error> {
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

        if refunds.len() > MAX_REFUND_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let mut results = Vec::new(&env);
        for item in refunds.into_iter() {
            let claim = RefundClaim {
                payment_ref: item.payment_ref,
                recipient: item.recipient,
                amount: item.amount,
                paid_at_ledger: item.paid_at_ledger,
                payment_amount: item.payment_amount,
            };
            results.push_back(claim_single(&env, &claim).is_ok());
        }
        Ok(results)
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

        let nonce = increment_nonce(&env);

        WithdrawEvent {
            to: to.clone(),
            amount,
            nonce,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        release_reentrancy_lock(&env);
        Ok(())
    }

    /// Propose a new refund policy: a window (in ledgers) and a wall-clock
    /// deadline (Unix timestamp, `0` = no deadline). The change is not applied
    /// immediately; the admin must call `execute_policy` after the timelock
    /// (17,280 ledgers, ~24 hours) has elapsed. Proposing a new policy
    /// overwrites any existing pending proposal.
    pub fn propose_policy(env: Env, ledgers: u32, deadline: u64) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let current_ledger = env.ledger().sequence();
        let proposal = PolicyProposal {
            window: ledgers,
            deadline,
            proposed_at_ledger: current_ledger,
        };

        env.storage()
            .instance()
            .set(&DataKey::PendingPolicy, &proposal);

        PolicyProposedEvent {
            window: ledgers,
            deadline,
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
    /// the timelock has not yet expired. Applies both the new window and the
    /// new deadline.
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
        env.storage()
            .instance()
            .set(&DataKey::RefundDeadline, &proposal.deadline);
        env.storage().instance().remove(&DataKey::PendingPolicy);

        PolicyExecutedEvent {
            window: proposal.window,
            deadline: proposal.deadline,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
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

    // ── Configuration getters ────────────────────────────────────────────

    /// Returns the admin (merchant) address, or `NotInitialized` if the vault
    /// has not been initialized.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the payment token address, or `NotInitialized` if the vault
    /// has not been initialized.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the refund window in ledgers, or `NotInitialized` if the vault
    /// has not been initialized. A value of 0 means no time-based restriction.
    pub fn get_refund_window(env: Env) -> Result<u32, Error> {
        env.storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .ok_or(Error::NotInitialized)
    }

    /// Returns whether the vault is currently paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::IsPaused)
            .unwrap_or(false)
    }
    /// Returns the current policy deadline as a Unix timestamp (read-only).
    /// `0` means no deadline is configured.
    pub fn get_refund_deadline(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::RefundDeadline)
            .unwrap_or(0)
    }

    // ── Fee configuration ──────────────────────────────────────────────────

    /// Returns the refund fee in basis points (1 bp = 0.01%). `0` means no
    /// fee is charged. Read-only.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    /// Returns the explicitly-configured fee recipient, if one has been set.
    /// When `None`, refund fees are paid to the merchant (admin). Read-only.
    pub fn get_fee_recipient(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::FeeRecipient)
    }

    /// Set the refund fee in basis points (1 bp = 0.01%, so 100 = 1%).
    /// Deducted from the amount sent to a refund recipient on every claim.
    /// Must be within `0..=10_000`. Only callable by admin.
    pub fn set_fee_bps(env: Env, bps: u32) -> Result<(), Error> {
        if bps > 10_000 {
            return Err(Error::InvalidRatio);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage().instance().set(&DataKey::FeeBps, &bps);

        let fee_recipient = active_fee_recipient(&env);
        FeeConfigUpdatedEvent {
            field: Symbol::new(&env, "fee_bps"),
            fee_bps: bps,
            fee_recipient,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Set the address that receives the fee deducted from each refund. The
    /// recipient must not be the vault's own address. Only callable by admin.
    pub fn set_fee_recipient(env: Env, recipient: Address) -> Result<(), Error> {
        if recipient == env.current_contract_address() {
            return Err(Error::SelfTransfer);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::FeeRecipient, &recipient);

        let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
        FeeConfigUpdatedEvent {
            field: Symbol::new(&env, "fee_recipient"),
            fee_bps,
            fee_recipient: recipient.clone(),
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
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

        let nonce = increment_nonce(&env);

        YieldDeployedEvent {
            strategy: strategy.clone(),
            amount,
            nonce,
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

        let nonce = increment_nonce(&env);

        YieldWithdrawnEvent {
            strategy,
            principal: principal_returned,
            yield_amount: yield_returned,
            nonce,
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

        let nonce = increment_nonce(&env);

        YieldHarvestedEvent {
            amount: yield_amount,
            nonce,
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

#[cfg(test)]
mod fuzz_test;
#[cfg(test)]
mod reentrancy_tests;
#[cfg(test)]
mod test;
mod token_agnostic_tests;
mod yield_tests;
