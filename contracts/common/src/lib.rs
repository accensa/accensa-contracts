//! Shared error codes for the Accensa contracts.
//!
//! Both [`ReceiptAnchor`] and [`RefundVault`] return errors from this single,
//! canonical [`Error`] enum. Every variant carries an explicit, distinct `u32`
//! value (issue #98). Indexers and frontends can therefore map one code space
//! across all contracts instead of maintaining per-contract tables.
//!
//! Values `4..=18` match the codes historically returned by `RefundVault`.
//! The codes that used to collide between the two contracts
//! (`AlreadyInitialized`, `NotInitialized`, `Unauthorized`) keep their original
//! values, while the `ReceiptAnchor`-only codes (`BatchNotFound`,
//! `BatchTooLarge`) are pushed to a dedicated block so no two variants overlap.
//!
//! # Stability
//!
//! Error codes are part of the contract's public interface and must not be
//! renumbered. New variants are appended with fresh, unused values.

#![no_std]

use soroban_sdk::{contractclient, contracterror, contracttype, Address, Bytes, BytesN, Env};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `initialize` was called after the contract was already initialized.
    AlreadyInitialized = 1,
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// The caller is not the authorized merchant/admin.
    Unauthorized = 3,
    /// Legacy single-refund marker (pre-#99). Retained for interface
    /// stability; the vault reports `ExceedsPayment` for over-ceiling and
    /// legacy records since cumulative partial refunds.
    AlreadyRefunded = 4,
    /// The refund window (measured from the original payment) has expired.
    WindowExpired = 5,
    /// Vault float is insufficient to cover the requested amount.
    InsufficientFloat = 6,
    /// An amount supplied was not strictly positive.
    InvalidAmount = 7,
    /// The vault is paused; the operation is not permitted.
    Paused = 8,
    /// No refund record exists for the given payment ref.
    RefundNotFound = 9,
    /// No admin transfer is pending.
    NoPendingTransfer = 12,
    /// No yield strategy has been configured.
    StrategyNotSet = 13,
    /// A yield deployment would breach the minimum reserve.
    InsufficientReserve = 14,
    /// A yield deployment would exceed the maximum deployment ratio.
    DeploymentExceedsMax = 15,
    /// Nothing to withdraw from the yield strategy.
    NothingToWithdraw = 16,
    /// Nothing to harvest from the yield strategy.
    NothingToHarvest = 17,
    /// A configured ratio exceeded the allowed range.
    InvalidRatio = 18,
    /// A refund call would push cumulative refunds past the payment ceiling.
    ExceedsPayment = 19,
    /// A guarded, external-call-making entry point was re-entered while a
    /// prior invocation of any guarded entry point was still in progress.
    ReentrancyBlocked = 20,
    /// A refund or withdraw was attempted where the recipient is the contract's own address.
    SelfTransfer = 21,
    /// An attempt to change the vault's token address was made while the vault holds a non-zero token balance.
    FloatNotEmpty = 22,
    /// A refund claim was submitted after the policy deadline timestamp passed.
    RefundExpired = 23,
    /// The requested batch does not exist (or was pruned).
    BatchNotFound = 100,
    /// A batch larger than `MAX_BATCH_SIZE` was submitted.
    BatchTooLarge = 101,
    /// A shard call returned something other than the expected value shape —
    /// a wasm-level invocation failure or a value that failed to decode.
    /// Distinct from `BatchNotFound`, which a shard returns deliberately.
    ShardCallFailed = 102,
    /// An attempt was made to anchor a Merkle root identical to the currently active root.
    DuplicateRoot = 103,
    /// The supplied Merkle root is not in the historical ring buffer.
    RootNotFound = 200,
    /// The Merkle proof exceeds the maximum valid length (`MAX_PROOF_LEN`).
    ProofTooLong = 201,
    /// An anchor was submitted before the minimum interval elapsed.
    AnchorRateLimited = 202,
    /// The supplied zero-knowledge validity proof is invalid or malformed.
    InvalidProof = 203,
    /// No pending policy change exists to execute.
    NoPendingPolicy = 300,
    /// The timelock period has not yet elapsed.
    TimelockNotExpired = 301,
    /// A refund was claimed against a policy with a VDF delay configured but
    /// no VDF proof was supplied.
    VdfProofRequired = 302,
    /// A supplied VDF proof failed verification (tampered output or witness,
    /// a premature proof computed for a smaller delay, or a degenerate
    /// challenge).
    InvalidVdfProof = 303,
    /// A VDF proof was supplied for a claim against a policy that has no VDF
    /// delay configured.
    VdfNotConfigured = 304,
    /// A reveal was attempted without a matching, pending commit
    /// (commit-reveal, issue #128).
    NoCommit = 305,
    /// A commit was submitted for a commitment hash that already has a pending
    /// commitment (commit-reveal, issue #128).
    CommitAlreadyExists = 306,
    /// The plaintext revealed does not hash to the committed value
    /// (commit-reveal, issue #128).
    CommitMismatch = 307,
    /// A reveal was attempted before the minimum commit-reveal ledger delay
    /// elapsed (commit-reveal, issue #128).
    CommitDelayNotElapsed = 308,
    /// A reveal was attempted under a different operation than the one the
    /// commitment was originally bound to (commit-reveal, issue #128).
    CommitOperationMismatch = 309,
    /// No oracle contracts are whitelisted on the vault, so the dynamic
    /// oracle policy cannot be evaluated (fail closed).
    NoOraclesConfigured = 310,
    /// An oracle contract is already on the whitelist.
    OracleAlreadyAdded = 311,
    /// The oracle contract is not on the whitelist.
    OracleNotFound = 312,
    /// Every whitelisted oracle returned stale data for the requested feed.
    StaleOracleData = 313,
    /// No dynamic oracle policy is configured.
    NoOraclePolicy = 314,
    /// A refund was rejected because the oracle policy condition was not met.
    OraclePolicyDenied = 315,
    /// `migrate_state` was called with a target layout version that is not
    /// greater than the current storage version (or is otherwise invalid).
    InvalidMigrationVersion = 316,

    // ── State channel errors (issue #134) ─────────────────────────────
    /// The channel does not exist.
    ChannelNotFound = 400,
    /// The channel is not in the expected state for this operation.
    ChannelNotOpen = 401,
    /// The channel is already open or has already been finalized.
    ChannelAlreadyClosed = 402,
    /// The submitted state has a nonce less than or equal to the current one.
    StaleState = 403,
    /// The signature does not match the sender's public key.
    InvalidSignature = 404,
    /// The dispute challenge period has not yet expired.
    ChallengeActive = 405,
    /// The dispute challenge period has expired; funds can no longer be claimed
    /// via dispute.
    ChallengeExpired = 406,
    /// The channel's escrowed balance is insufficient.
    InsufficientChannelBalance = 407,
    /// The timeout has already passed; the channel is expired.
    ChannelExpired = 408,
    /// A policy that requires the stateless policy contracts (time/VDF) was
    /// proposed or executed on a vault that was never wired with the contract
    /// addresses (issue #129: the factory wires them at construction, or the
    /// admin sets them via the setters).
    PolicyContractsNotConfigured = 317,
    /// A policy contract received a `params` blob that does not decode to the
    /// policy's schema (`TimePolicyParams` / `VdfPolicyParams`). Indicates a
    /// vault configured a policy entry against the wrong contract.
    InvalidPolicyParams = 318,
    /// An anchor rate-limit configuration was rejected (non-positive burst
    /// capacity or refill interval). Raised by `set_anchor_rate_limit`.
    InvalidRateLimitConfig = 319,
    /// A refund/claim was submitted before the minimum cooldown elapsed.
    ClaimCooldownNotElapsed = 320,
}

/// Parameters for the stateless **time** policy contract (issue #129).
///
/// Guards refund claims by two independent clocks, evaluated in this order:
///
/// - `window`: the refund window measured in ledgers from the payment's
///   `paid_at_ledger`. `0` disables the window ("no window").
/// - `deadline`: a wall-clock Unix timestamp after which claims are rejected.
///   `0` disables the deadline ("never expires"). Expiry is strictly past the
///   deadline, so a claim landing exactly on the deadline succeeds.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimePolicyParams {
    pub window: u32,
    pub deadline: u64,
}

/// Parameters for the stateless **VDF** policy contract (issue #129).
///
/// Requires a valid Wesolowski proof that `delay` sequential squarings have
/// elapsed on the payment-ref challenge before a claim is honored. `delay`
/// must be `> 0`; a `0` delay would otherwise be a no-op, and the vault never
/// emits a VDF entry for a `0` delay.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdfPolicyParams {
    pub delay: u32,
}

/// The claim-derived context a vault passes to a policy contract's
/// `evaluate` call (issue #129). Carries every claim fact a stateless policy
/// needs; policies are pure and must not call back into the vault.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyContext {
    pub payment_ref: BytesN<32>,
    /// Claimed amount before any configured fee is deducted.
    pub amount: i128,
    /// Ledger at which the original payment occurred (window is measured
    /// from here, never from a partial).
    pub paid_at_ledger: u32,
    /// Ledger the claim is being evaluated at.
    pub current_ledger: u32,
    /// Wall-clock timestamp the claim is being evaluated at.
    pub timestamp: u64,
    /// Wesolowski VDF proof supplied on the claim, if any.
    pub vdf_proof: Option<BytesN<256>>,
}

/// Interface implemented by the stateless refund-policy contracts
/// (issue #129).
///
/// `evaluate` runs *inside the vault's reentrancy lock* (the vault's
/// `refund`/`claim_batch`/`process_batch` entry points hold it for the whole
/// call), so a policy contract MUST NOT invoke any guarded vault entry point
/// as a callback — that would be rejected with `ReentrancyBlocked`. Policies
/// are pure: they read [`PolicyContext`], optionally decode their own
/// `params`, and return `Err` to reject the claim.
#[contractclient(name = "RefundPolicyClient")]
pub trait RefundPolicy {
    /// Evaluate the policy against a claim. `Ok(())` admits the claim; any
    /// `Err` rejects it with the mapped [`Error`].
    fn evaluate(env: Env, params: Bytes, ctx: PolicyContext) -> Result<(), Error>;
}

/// Construction-time configuration for a `RefundVault` instance (issue #129).
///
/// Shared between `RefundVaultFactory::deploy_vault` (which feeds it to the
/// vault's `__constructor` through `deploy_v2`) and direct (non-factory)
/// deployments that call `RefundVault::initialize`.
///
/// `time_policy` / `vdf_policy` are the addresses of the stateless policy
/// contracts the vault will delegate gate evaluation to; both are optional
/// (`None` disables the corresponding gate — an active gate on a vault that
/// was never wired fails closed with `PolicyContractsNotConfigured`).
/// `refund_window` / `deadline` / `vdf_delay` seed the vault's read-path
/// mirrors of the active gates; they are updated by the timelocked
/// propose/execute flow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultInit {
    pub merchant: Address,
    pub token: Address,
    /// Stateless time-policy contract address (window + deadline gate).
    pub time_policy: Option<Address>,
    /// Stateless VDF-policy contract address (proof gate).
    pub vdf_policy: Option<Address>,
    /// Refund fee in basis points deducted from each payout.
    pub fee_bps: u32,
    /// Address that receives the fee; `None` falls back to the merchant.
    pub fee_recipient: Option<Address>,
    /// Mirror of the active time gate's window (read path).
    pub refund_window: u32,
    /// Mirror of the active time gate's deadline (read path).
    pub deadline: u64,
    /// Mirror of the active VDF gate's delay (read path).
    pub vdf_delay: u32,
}
