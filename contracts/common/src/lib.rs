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

use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `initialize` was called after the contract was already initialized.
    AlreadyInitialized = 1,
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// The caller is not the authorized merchant/admin.
    Unauthorized = 3,
    /// The full refund ceiling for a payment has already been reached.
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
    /// A metadata payload exceeded the allowed length.
    MetadataTooLong = 10,
    /// A requested amount exceeded the configured maximum.
    AmountExceedsMax = 11,
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
    /// The requested batch does not exist (or was pruned).
    BatchNotFound = 100,
    /// A batch larger than `MAX_BATCH_SIZE` was submitted.
    BatchTooLarge = 101,
    /// A shard call returned something other than the expected value shape —
    /// a wasm-level invocation failure or a value that failed to decode.
    /// Distinct from `BatchNotFound`, which a shard returns deliberately.
    ShardCallFailed = 102,
}
