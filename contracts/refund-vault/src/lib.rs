#![no_std]

use accensa_common::Error;
use soroban_sdk::{
    contract, contractclient, contractevent, contractimpl, contractmeta, contracttype, token,
    Address, Bytes, BytesN, Env, Symbol, Vec,
};

mod vdf;
use vdf::VdfProof;

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
    /// Wesolowski VDF proof, required when the policy carries a VDF delay
    /// (issue #138). The 256 bytes are the 128-byte big-endian output
    /// `x^(2^T) mod N` concatenated with the 128-byte witness `x^(floor(2^T/l))
    /// mod N`. `None` for policies without a delay.
    pub vdf_proof: Option<BytesN<256>>,
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
    /// VDF delay (in squarings) required to finalize refund claims against
    /// this policy (issue #138). `0` (the default) means no VDF proof is
    /// required. Configured with the policy (propose/execute) and read at
    /// claim time in `claim_single`.
    VdfDelay,
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
    /// Yield strategy contract address. Stored in **Persistent** storage so
    /// it is not loaded on every non-yield invocation (issue #131).
    YieldStrategy,
    /// Cumulative principal deployed to the yield strategy. Persistent
    /// storage; see `YieldStrategy` rationale above.
    DeployedPrincipal,
    /// Cumulative yield harvested from the strategy and held in the vault
    /// for operator withdrawal. Persistent storage (issue #131).
    HarvestedYield,
    /// Minimum liquid reserve ratio in basis points. Persistent storage
    /// (issue #131).
    ReserveRatio,
    /// Maximum deployment ratio in basis points. Persistent storage
    /// (issue #131).
    MaxDeployRatio,
    PendingPolicy,
    /// Whitelisted oracle contracts, in insertion order. The aggregator
    /// queries every whitelisted oracle for the same feed and takes the
    /// median of the fresh values, so no single provider is trusted.
    Oracles,
    /// Dynamic oracle policy gating refunds, if one is configured.
    OraclePolicy,
    /// Reentrancy guard flag. Set for the duration of any entry point that
    /// makes an external call (token transfer or yield-strategy invocation)
    /// so a callback into another guarded entry point during that call is
    /// rejected rather than allowed to observe pre-update state.
    ReentrancyLock,
    /// Monotonic storage-layout version. Missing on legacy deployments (v1).
    StorageVersion,
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
    /// VDF delay in squarings that a refund claim against this policy must
    /// prove. `0` (the default) means no VDF proof is required.
    pub vdf_delay: u32,
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
    /// Wesolowski VDF proof, required when the policy carries a VDF delay
    /// (issue #138). The 256 bytes are the 128-byte big-endian output
    /// `x^(2^T) mod N` concatenated with the 128-byte witness `x^(floor(2^T/l))
    /// mod N`. `None` for policies without a delay.
    pub vdf_proof: Option<BytesN<256>>,
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

/// Emitted when a (possibly partial) refund is made from the vault float via
/// the single-refund `refund` entry point.
///
/// Topics: `("refund_event", payment_ref)`. The data map carries the amount
/// for **this call** (`amount`) and the running total after it
/// (`cumulative_refunded`), so an indexer knows the state of a payment without
/// summing history.
///
/// Refunds processed through [`RefundVault::process_batch`] do **not** emit
/// one of these per item: a batch emits a single [`BatchRefundEvent`] instead
/// (see its docs for why).
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

/// Emitted when the merchant installs (or replaces) the dynamic oracle
/// policy that gates refunds.
///
/// Topics: `("oracle_policy_set_event", feed_id)`. The data map carries the
/// threshold, the comparison direction and the staleness bound, so an indexer
/// can reconstruct the exact condition in force.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePolicySetEvent {
    #[topic]
    pub feed_id: BytesN<32>,
    pub threshold: i128,
    pub refund_when_below: bool,
    pub max_staleness_ledgers: u32,
}

/// Emitted when the merchant removes the dynamic oracle policy, restoring
/// purely time-window-based refunds.
///
/// Topics: `("oracle_policy_cleared_event", feed_id)` — the feed of the
/// policy that was in force, captured before it was removed.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePolicyClearedEvent {
    #[topic]
    pub feed_id: BytesN<32>,
}

pub mod oracle;

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

/// Helper to extend the TTL of a persistent yield-storage entry (issue #131).
fn persist_yield_ttl(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_EXTEND, TTL_THRESHOLD);
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

    // VDF delay (policy trigger, issue #138): when the policy carries a
    // configured delay, finalizing this refund requires a valid Wesolowski
    // proof that `vdf_delay` sequential squarings have genuinely elapsed. The
    // delay is computational, so unlike the ledger window or the wall-clock
    // deadline above it cannot be shortened by a validator controlling block
    // timestamps or transaction ordering. The challenge is derived from the
    // payment ref (`sha256(payment_ref)`), binding the proof to this payment
    // and preventing replay across payments or across policy changes.
    let vdf_delay: u32 = env
        .storage()
        .instance()
        .get(&DataKey::VdfDelay)
        .unwrap_or(0);
    match (vdf_delay, &claim.vdf_proof) {
        (0, None) => {}
        (0, Some(_)) => return Err(Error::VdfNotConfigured),
        (_, None) => return Err(Error::VdfProofRequired),
        (delay, Some(proof)) => {
            let payment_hash = env
                .crypto()
                .sha256(&Bytes::from_slice(env, &claim.payment_ref.to_array()));
            let mut challenge = [0u8; 128];
            challenge[96..].copy_from_slice(&payment_hash.to_array());
            let packed = proof.to_array();
            let mut output = [0u8; 128];
            let mut witness = [0u8; 128];
            output.copy_from_slice(&packed[..128]);
            witness.copy_from_slice(&packed[128..]);
            vdf::verify_vdf(env, &challenge, delay, &output, &witness)?;
        }
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

const INITIAL_STORAGE_VERSION: u32 = 1;

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
        env.storage()
            .instance()
            .set(&DataKey::StorageVersion, &INITIAL_STORAGE_VERSION);

        // Issue #136: domain separator and nonce.
        let contract_addr = env.current_contract_address();
        let addr_str = contract_addr.to_string();
        let separator = env.crypto().sha256(&soroban_sdk::Bytes::from(addr_str));
        env.storage()
            .instance()
            .set(&DataKey::DomainSeparator, &separator);
        env.storage().instance().set(&DataKey::Nonce, &0u64);

        // Issue #136: store the domain separator (this contract's address)
        // and initialise the monotonic nonce to 0.
        let contract_addr = env.current_contract_address();
        // The domain separator is a hash of the contract address so that
        // events and off-chain signatures can be bound to exactly one
        // deployment.
        let separator = env.crypto().sha256(&contract_addr.to_buffer());
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
        vdf_proof: Option<BytesN<256>>,
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
            vdf_proof,
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
            .get(&DataKey::RefundWindow)
            .unwrap();
        if window > 0 {
            let current_ledger = env.ledger().sequence();
            if current_ledger > paid_at_ledger + window {
                return Err(Error::WindowExpired);
            }
        }

        // Dynamic oracle policy: when configured, refunds are only processed
        // while the aggregated external feed satisfies the condition (e.g.
        // the asset price is below the SLA threshold). Fails closed on a
        // missing whitelist or all-stale data rather than guessing. This runs
        // inside the reentrancy lock acquired by `refund`, so a whitelisted
        // oracle cannot re-enter the vault from its `get_price` callback.
        let oracle_policy: Option<oracle::OraclePolicy> =
            env.storage().instance().get(&DataKey::OraclePolicy);
        if let Some(policy) = oracle_policy {
            if !oracle::evaluate_policy(env, &policy)? {
                return Err(Error::OraclePolicyDenied);
            }
        }

        // Ceiling check: cumulative refunds must not exceed the original amount.
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

        if previous_refunded.checked_add(amount).is_none()
            || record_ceiling <= 0
            || previous_refunded + amount > record_ceiling
        {
            return Err(Error::Paused);
        }

        let merchant: Address = env
            .storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        let extend_to = refund_record_ttl_extend_to(env, window, paid_at_ledger);
        // Threshold == extend_to (not TTL_THRESHOLD): see
        // `refund_record_ttl_extend_to` for why a small fixed threshold makes
        // this a no-op on a freshly-written entry.
        env.storage().persistent().extend_ttl(
            &DataKey::RefundV2(payment_ref.clone()),
            extend_to,
            extend_to,
        );

        for claim in claims.iter() {
            claim_single(&env, &claim)?;
        }
        .publish(env);

        release_reentrancy_lock(env);
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

        // An empty batch is a no-op; return before touching any state so the
        // caller can probe auth without paying for state loads.
        if refunds.is_empty() {
            return Ok(Vec::new(&env));
        }

        // State loads shared across the whole batch: one balance query and one
        // window/ledger/token read — the loop below only touches per-payment
        // storage and performs the transfers.
        let mut ctx = Self::load_refund_context(&env);
        let mut payment_refs: Vec<BytesN<32>> = Vec::new(&env);
        let mut results = Vec::new(&env);
        for item in refunds.into_iter() {
            let claim = RefundClaim {
                payment_ref: item.payment_ref,
                recipient: item.recipient,
                amount: item.amount,
                paid_at_ledger: item.paid_at_ledger,
                payment_amount: item.payment_amount,
                vdf_proof: item.vdf_proof,
            };
            results.push_back(claim_single(&env, &claim).is_ok());
        }

        BatchRefundEvent {
            payment_refs,
            results: results.clone(),
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
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

    /// Propose a new refund policy: a window (in ledgers), a wall-clock
    /// deadline (Unix timestamp, `0` = no deadline), and a VDF delay in
    /// squarings (`0` = no VDF proof required, see `vdf` module docs). The
    /// change is not applied immediately; the admin must call `execute_policy`
    /// after the timelock (17,280 ledgers, ~24 hours) has elapsed. Proposing a
    /// new policy overwrites any existing pending proposal.
    pub fn propose_policy(
        env: Env,
        ledgers: u32,
        deadline: u64,
        vdf_delay: u32,
    ) -> Result<(), Error> {
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
            vdf_delay,
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
        env.storage()
            .instance()
            .set(&DataKey::VdfDelay, &proposal.vdf_delay);
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

    // ── Oracle aggregation ────────────────────────────────────────────────

    /// Whitelist an oracle contract implementing the [`oracle::Oracle`]
    /// interface. Only callable by the merchant. The aggregator queries every
    /// whitelisted oracle and takes the median of the fresh values, so a
    /// single provider can never unilaterally move the aggregated price.
    pub fn add_oracle(env: Env, oracle: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

    /// Returns the persisted storage layout version. Legacy deployments that
    /// predate this marker are treated as version 1.
    pub fn get_storage_version(env: Env) -> Result<u32, Error> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::NotInitialized);
        }
        env.storage()
            .instance()
            .get(&DataKey::StorageVersion)
            .ok_or(Error::NotInitialized)
            .or(Ok(INITIAL_STORAGE_VERSION))
    }

    /// Marks a completed, resumable state migration and records its target
    /// layout version. This must be called before the WASM upgrade so the
    /// migration marker survives the code handoff.
    pub fn migrate_state(env: Env, target_version: u32) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();

        let current = env
            .storage()
            .instance()
            .get(&DataKey::StorageVersion)
            .unwrap_or(INITIAL_STORAGE_VERSION);
        if target_version <= current {
            return Err(Error::InvalidMigrationVersion);
        }

        // Optional fields introduced by later layouts deliberately use their
        // existing defaults. Writing the marker last makes the operation
        // resumable and prevents a partial migration from being reported as
        // complete.
        env.storage()
            .instance()
            .set(&DataKey::StorageVersion, &target_version);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Performs the code handoff after `migrate_state` has completed.
    /// `wasm_hash` must refer to a WASM already uploaded to the network.
    pub fn upgrade_wasm(env: Env, wasm_hash: BytesN<32>) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();
        env.deployer().update_current_contract_wasm(wasm_hash);
        Ok(())
    }

    /// Returns the payment token address, or `NotInitialized` if the vault
    /// has not been initialized.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Oracles)
            .unwrap_or_else(|| Vec::new(&env));
        if oracles.contains(&oracle) {
            return Err(Error::OracleAlreadyAdded);
        }
        oracles.push_back(oracle);
        env.storage().instance().set(&DataKey::Oracles, &oracles);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Remove an oracle from the whitelist. Only callable by the merchant.
    pub fn remove_oracle(env: Env, oracle: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let mut oracles: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Oracles)
            .ok_or(Error::NoOraclesConfigured)?;
        let index = oracles
            .first_index_of(&oracle)
            .ok_or(Error::OracleNotFound)?;
        let _ = oracles.remove(index);
        env.storage().instance().set(&DataKey::Oracles, &oracles);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Returns the policy's VDF delay in squarings (read-only). `0` means no
    /// VDF proof is required to finalize refunds.
    pub fn get_vdf_delay(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::VdfDelay)
            .unwrap_or(0)
    }

    /// Verifies a Wesolowski VDF proof that `output == challenge^(2^delay)
    /// mod N` for the contract's fixed modulus (issue #138). Read-only and
    /// unauthenticated, so anyone can check a VDF output — the surface used
    /// by random-selection / randomness-beacon flows that never touch the
    /// vault. Returns `Error::InvalidVdfProof` if the proof does not verify
    /// or the challenge is degenerate.
    pub fn verify_vdf(
        env: Env,
        challenge: BytesN<128>,
        delay: u32,
        proof: VdfProof,
    ) -> Result<(), Error> {
        vdf::verify_vdf(
            &env,
            &challenge.to_array(),
            delay,
            &proof.output.to_array(),
            &proof.proof.to_array(),
        )
    }

    // ── Fee configuration ──────────────────────────────────────────────────

    /// Returns the refund fee in basis points (1 bp = 0.01%). `0` means no
    /// fee is charged. Read-only.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    /// Aggregate the current value of `feed_id` across the whitelisted
    /// oracles: the median of the fresh (non-stale) reported values.
    ///
    /// Read-only, so it is safe to call from an indexer or a wallet.
    /// `max_staleness_ledgers` is the caller's freshness bound for this
    /// query (`0` = never stale).
    pub fn get_median_price(
        env: Env,
        feed_id: BytesN<32>,
        max_staleness_ledgers: u32,
    ) -> Result<i128, Error> {
        oracle::median_price(&env, &feed_id, max_staleness_ledgers)
    }

    /// Install (or replace) the dynamic oracle policy gating refunds. Only
    /// callable by the merchant. Once set, `refund` and `process_batch` only
    /// pay out while the aggregated feed satisfies the policy's condition.
    pub fn set_oracle_policy(env: Env, policy: oracle::OraclePolicy) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::OraclePolicy, &policy);

        OraclePolicySetEvent {
            feed_id: policy.feed_id.clone(),
            threshold: policy.threshold,
            refund_when_below: policy.refund_when_below,
            max_staleness_ledgers: policy.max_staleness_ledgers,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Remove the dynamic oracle policy, restoring purely time-window-based
    /// refunds. Only callable by the merchant.
    pub fn clear_oracle_policy(env: Env) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        let policy: oracle::OraclePolicy = env
            .storage()
            .instance()
            .get(&DataKey::OraclePolicy)
            .ok_or(Error::NoOraclePolicy)?;
        env.storage().instance().remove(&DataKey::OraclePolicy);

        OraclePolicyClearedEvent {
            feed_id: policy.feed_id,
        }
        .publish(&env);

        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Read-only: the currently installed oracle policy, if any.
    pub fn get_oracle_policy(env: Env) -> Option<oracle::OraclePolicy> {
        env.storage().instance().get(&DataKey::OraclePolicy)
    }

    // ── Yield strategy management ──────────────────────────────────────────
    //
    // Issue #131: yield-related storage keys are kept in **Persistent**
    // storage rather than Instance storage. Non-yield calls (deposit,
    // refund, withdraw, pause, unpause, admin transfer) never touch these
    // keys, so moving them out of Instance reduces the read/write byte
    // cost of every non-yield invocation. Persistent entries are extended
    // with the standard TTL budget after every write.

    /// Register an external yield strategy contract. Only callable by admin.
    pub fn set_yield_strategy(env: Env, strategy: Address) -> Result<(), Error> {
        let merchant: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        merchant.require_auth();

        env.storage()
            .persistent()
            .set(&DataKey::YieldStrategy, &strategy);
        persist_yield_ttl(&env, &DataKey::YieldStrategy);

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
            .persistent()
            .set(&DataKey::ReserveRatio, &basis_points);
        persist_yield_ttl(&env, &DataKey::ReserveRatio);

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
            .persistent()
            .set(&DataKey::MaxDeployRatio, &basis_points);
        persist_yield_ttl(&env, &DataKey::MaxDeployRatio);

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
            .persistent()
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
            .persistent()
            .get(&DataKey::DeployedPrincipal)
            .unwrap_or(0);
        let harvested: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::HarvestedYield)
            .unwrap_or(0);

        // total_value = liquid tokens + deployed principal
        // (harvested yield has already been transferred to the vault and is part of token_balance,
        //  but it belongs to the operator, not the principal pool — subtract it)
        let total_value = token_balance + deployed - harvested;

        // Reserve check: after deployment, liquid tokens must cover the reserve.
        let reserve_ratio: u32 = env
            .storage()
            .persistent()
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
            .persistent()
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
            .persistent()
            .set(&DataKey::DeployedPrincipal, &(deployed + amount));
        persist_yield_ttl(&env, &DataKey::DeployedPrincipal);

        let nonce = increment_nonce(&env);

        YieldDeployedEvent {
            strategy,
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
            .persistent()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)?;

        let deployed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::DeployedPrincipal)
            .unwrap_or(0);
        if principal > deployed {
            return Err(Error::NothingToWithdraw);
        }

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let (principal_returned, yield_returned) = strategy_client.withdraw(&principal);

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
        persist_yield_ttl(&env, &DataKey::DeployedPrincipal);
        persist_yield_ttl(&env, &DataKey::HarvestedYield);

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
            .persistent()
            .get(&DataKey::YieldStrategy)
            .ok_or(Error::StrategyNotSet)?;

        let strategy_client = YieldStrategyClient::new(&env, &strategy);
        let yield_amount = strategy_client.harvest();

        if yield_amount <= 0 {
            return Err(Error::NothingToHarvest);
        }

        let harvested: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::HarvestedYield)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::HarvestedYield, &(harvested + yield_amount));
        persist_yield_ttl(&env, &DataKey::HarvestedYield);

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
                .persistent()
                .get(&DataKey::DeployedPrincipal)
                .unwrap_or(0),
            harvested_yield: env
                .storage()
                .persistent()
                .get(&DataKey::HarvestedYield)
                .unwrap_or(0),
            strategy: env.storage().persistent().get(&DataKey::YieldStrategy),
            reserve_ratio: env
                .storage()
                .persistent()
                .get(&DataKey::ReserveRatio)
                .unwrap_or(0),
            max_deploy_ratio: env
                .storage()
                .persistent()
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
mod oracle_tests;
#[cfg(test)]
mod reentrancy_tests;
#[cfg(test)]
mod test;
mod token_agnostic_tests;
#[cfg(test)]
mod vdf_test;
mod yield_tests;
