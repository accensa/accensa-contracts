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
    /// Legacy single-refund marker (pre-#99); kept for interface stability.
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
    /// A guarded entry point was re-entered while a prior invocation was still
    /// in progress.
    ReentrancyBlocked = 20,
    /// The recipient is the vault's own address.
    SelfTransfer = 21,
    /// The vault holds a non-zero token balance, so its token cannot change.
    FloatNotEmpty = 22,
    /// A refund claim was submitted after the policy deadline timestamp passed.
    RefundExpired = 23,
    /// The requested batch does not exist (or was pruned).
    BatchNotFound = 100,
    /// A batch larger than `MAX_BATCH_SIZE` was submitted.
    BatchTooLarge = 101,
    /// A shard call failed or returned a value that would not decode (not a
    /// deliberate `BatchNotFound`).
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
    /// Rejected rate-limit config: one of the pair is zero, or a value exceeds
    /// its cap (`{0, 0}` disables limiting).
    InvalidRateLimitConfig = 204,
    /// No pending policy change exists to execute.
    NoPendingPolicy = 300,
    /// The timelock period has not yet elapsed.
    TimelockNotExpired = 301,
    /// A VDF delay is configured but no proof was supplied.
    VdfProofRequired = 302,
    /// The supplied VDF proof failed verification.
    InvalidVdfProof = 303,
    /// A VDF proof was supplied but no VDF delay is configured.
    VdfNotConfigured = 304,
    /// No matching pending commitment exists (commit-reveal, issue #128).
    NoCommit = 305,
    /// A commitment is already pending for this hash (commit-reveal, issue #128).
    CommitAlreadyExists = 306,
    /// The revealed plaintext does not hash to the commitment (issue #128).
    CommitMismatch = 307,
    /// The minimum commit-reveal delay has not elapsed (issue #128).
    CommitDelayNotElapsed = 308,
    /// The reveal is bound to a different operation than the commitment
    /// (issue #128).
    CommitOperationMismatch = 309,
    /// No oracle is whitelisted, so the oracle gate fails closed.
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
    /// The requested layout version is not a valid `migrate_state` target.
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
    /// The challenge period expired, so the dispute path is closed.
    ChallengeExpired = 406,
    /// The channel's escrowed balance is insufficient.
    InsufficientChannelBalance = 407,
    /// The timeout has already passed; the channel is expired.
    ChannelExpired = 408,
    /// A multi-asset state names a token the channel does not escrow, or omits
    /// one it does (issue #423).
    UnsupportedAsset = 409,
    /// The referenced HTLC does not exist on the channel (issue #458).
    HtlcNotFound = 410,
    /// The HTLC is no longer pending (already resolved or refunded).
    HtlcNotPending = 411,
    /// The HTLC's timeout ledger has not yet passed, so it cannot be refunded.
    HtlcNotExpired = 412,
    /// The supplied preimage does not hash to the HTLC's hash lock.
    InvalidPreimage = 413,
    /// A downstream HTLC's timeout must be strictly smaller than its
    /// upstream parent's (issue #458).
    HtlcTimeoutOutOfOrder = 414,
    /// The HTLC would reserve more than the channel's uncommitted escrow.
    HtlcInsufficientEscrow = 415,
    /// The HTLC's timeout ledger is already in the past, so it could never be
    /// resolved before a refund (issue #458).
    HtlcTimeoutElapsed = 416,
    /// A time/VDF policy gate is active but its policy contract was never
    /// wired (issue #129).
    PolicyContractsNotConfigured = 317,
    /// A policy `params` blob does not decode to that policy's schema — the
    /// entry was pointed at the wrong contract.
    InvalidPolicyParams = 318,
    /// A refund/claim was submitted before the minimum cooldown elapsed.
    ClaimCooldownNotElapsed = 320,
    /// A shared math helper refused a checked operation that would overflow,
    /// truncate, or divide by zero, before any state changed (issue #396).
    MathOverflow = 321,
    /// The yield strategy is not on the admin-approved whitelist (issue #415).
    StrategyNotApproved = 322,
    /// The strategy still holds deployed principal, so it cannot be replaced
    /// or revoked yet (issue #415).
    StrategyHasPrincipal = 323,
    /// The requested coupon id does not exist in persistent storage (issue #453).
    CouponNotFound = 324,
    /// The coupon has already been redeemed and cannot be applied again (issue #453).
    CouponAlreadyRedeemed = 325,
    /// No escrow record exists for the NFT contract/token id (issue #474).
    NftEscrowNotFound = 326,
    /// The NFT contract/token id is already escrowed in this vault
    /// (issue #474).
    NftAlreadyEscrowed = 327,
    /// The vault is not the current owner of the NFT it was asked to release
    /// (issue #474).
    NftNotOwned = 328,
    /// No dispute is recorded under the given id in the fallback-oracle
    /// ledger (issue #469).
    DisputeNotFound = 329,
    /// A fallback-oracle dispute was already settled (issue #469).
    DisputeClosed = 330,
    /// A proposed merchant fee-tier ladder is malformed (empty, too long,
    /// not starting at zero, non-increasing, or with out-of-range fees).
    InvalidTierLadder = 331,
    /// No randomness committed yet for the VDF round.
    RandomnessNotFound = 332,
    /// A late counter-proof would extend the dispute window past the cap
    /// (issue #431). The window is bounded so a hostile party cannot stall
    /// settlement indefinitely by resubmitting newer states.
    DisputeExtensionLimitReached = 417,
    /// Explicit Soroban Host error mapping (issue #380).
    HostError = 500,
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
pub mod audit;
pub mod blacklist;
pub mod constant_time;
pub mod events;
pub mod keys;
pub mod math;
pub mod nonce;
pub mod reentrancy;
pub mod storage;
#[cfg(any(feature = "telemetry", test))]
pub mod telemetry;
