use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// A delegated signer is not a registered signer of this account.
    UnknownSigner = 1,
    /// Fewer than `threshold` distinct signers authorized the call.
    InsufficientSignatures = 2,
    /// The caller is not authorized to perform this action.
    Unauthorized = 3,
    /// The timelock period has not yet elapsed.
    TimelockNotExpired = 4,
    /// The requested proposal or queue entry was not found.
    ProposalNotFound = 5,
    /// The signer has already approved this transaction.
    AlreadyVoted = 6,
    /// An Ed25519 signature's `s` scalar is not canonical (`s >= L`), i.e.
    /// it is a malleated form of some other valid signature.
    NonCanonicalSignature = 7,
    /// A sub-threshold spend would exceed a signer's remaining daily
    /// allowance; the full threshold is required.
    DailyLimitExceeded = 8,
    /// A daily limit must not be negative.
    InvalidLimit = 9,
    /// The account is paused: it will not authorize or execute any operation
    /// other than the pause controls and signer rotation.
    Paused = 10,
    /// A signer weight of `0` was supplied, or a weighted update would have
    /// overflowed the aggregate signer weight (issue #434).
    InvalidWeight = 11,
    /// A signer addition/removal or weight change would leave the account's
    /// aggregate signer weight below its configured threshold, which would
    /// make the account unable to ever authorize again (issue #434).
    TotalWeightBelowThreshold = 12,
    /// A signer was added that is already registered on the account
    /// (issue #434).
    SignerAlreadyRegistered = 13,
}
