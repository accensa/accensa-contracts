//! Oracle-assisted dispute resolution (issue #426).
//!
//! A merchant can point its time-policy entry at a trusted delivery oracle by
//! encoding [`TimeOraclePolicyParams`] instead of plain
//! [`accensa_common::TimePolicyParams`]. The oracle address travels inside
//! the policy entry's `params`, which the vault only changes through its
//! timelocked propose/execute flow, so the oracle is registered by the
//! merchant exactly like any other policy setting and this contract stays
//! storage-free.
//!
//! On every evaluation the policy asks the oracle for the parcel's
//! [`DeliveryReport`] and resolves the dispute from it:
//!
//! | Oracle says  | Result                                                     |
//! |--------------|------------------------------------------------------------|
//! | `Lost`       | refund admitted, even outside the window or past deadline  |
//! | `Delivered`  | refund rejected with `Error::OraclePolicyDenied`           |
//! | `Pending`    | falls back to the standard window/deadline check           |
//!
//! A report is only trusted when it is **fresh** — its timestamp is not in
//! the future and not older than `max_report_age` seconds — and names the
//! same `payment_ref` as the claim. A stale, mismatched or missing report, or
//! an oracle that traps or does not exist, also falls back to the standard
//! window/deadline check, so an unreachable oracle can never block or force a
//! refund on its own.
//!
//! When the oracle decides the outcome, [`OracleResolutionApplied`] is
//! emitted with the oracle's `proof`. For a `Delivered` rejection the event is
//! rolled back with the rest of the failed invocation; the
//! `OraclePolicyDenied` error is the signal in that case.

use accensa_common::{Error, PolicyContext};
use soroban_sdk::{contractclient, contractevent, contracttype, Address, BytesN, Env};

/// Delivery state reported by an oracle.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Lost,
}

/// A signed-off oracle answer for one payment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryReport {
    /// Payment the report is about; must match the claim.
    pub payment_ref: BytesN<32>,
    pub status: DeliveryStatus,
    /// Unix timestamp at which the oracle observed `status`.
    pub timestamp: u64,
    /// Opaque evidence (e.g. a hash of the carrier's tracking record).
    pub proof: BytesN<32>,
}

/// Interface a delivery oracle implements.
#[contractclient(name = "DeliveryOracleClient")]
pub trait DeliveryOracle {
    /// Latest delivery report for `payment_ref`.
    fn get_delivery_status(env: Env, payment_ref: BytesN<32>) -> DeliveryReport;
}

/// Time-policy params with an oracle attached. `window` and `deadline` keep
/// their [`accensa_common::TimePolicyParams`] meaning and apply whenever the
/// oracle does not decide the outcome.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeOraclePolicyParams {
    pub window: u32,
    pub deadline: u64,
    /// Delivery oracle to consult.
    pub oracle: Address,
    /// Maximum age of a report, in seconds, for it to be trusted.
    pub max_report_age: u64,
}

/// Emitted when an oracle report decides a claim.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleResolutionApplied {
    #[topic]
    pub payment_ref: BytesN<32>,
    pub oracle: Address,
    pub status: DeliveryStatus,
    pub reported_at: u64,
    pub proof: BytesN<32>,
}

/// The oracle's decision for a claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// Refund the buyer regardless of the clock gates.
    Admit,
    /// Reject the refund.
    Deny,
    /// No usable oracle answer; apply the standard window/deadline.
    Fallback,
}

/// Whether `report` can be trusted for a claim evaluated at `ctx`.
fn is_usable(report: &DeliveryReport, ctx: &PolicyContext, max_age: u64) -> bool {
    report.payment_ref == ctx.payment_ref
        && report.timestamp <= ctx.timestamp
        && ctx.timestamp - report.timestamp <= max_age
}

/// Query the oracle and map its answer to a [`Resolution`], emitting
/// [`OracleResolutionApplied`] when the oracle decides the outcome.
pub fn resolve(env: &Env, params: &TimeOraclePolicyParams, ctx: &PolicyContext) -> Resolution {
    let report = match DeliveryOracleClient::new(env, &params.oracle)
        .try_get_delivery_status(&ctx.payment_ref)
    {
        Ok(Ok(report)) => report,
        // Trapped, missing, or returned something that is not a report.
        _ => return Resolution::Fallback,
    };

    if !is_usable(&report, ctx, params.max_report_age) {
        return Resolution::Fallback;
    }

    let resolution = match report.status {
        DeliveryStatus::Lost => Resolution::Admit,
        DeliveryStatus::Delivered => Resolution::Deny,
        DeliveryStatus::Pending => return Resolution::Fallback,
    };

    OracleResolutionApplied {
        payment_ref: report.payment_ref,
        oracle: params.oracle.clone(),
        status: report.status,
        reported_at: report.timestamp,
        proof: report.proof,
    }
    .publish(env);

    resolution
}

/// Standard clock gates shared by the plain and oracle-backed params.
pub(crate) fn check_time_gates(
    window: u32,
    deadline: u64,
    ctx: &PolicyContext,
) -> Result<(), Error> {
    if window > 0 && ctx.current_ledger > ctx.paid_at_ledger + window {
        return Err(Error::WindowExpired);
    }
    if deadline > 0 && ctx.timestamp > deadline {
        return Err(Error::RefundExpired);
    }
    Ok(())
}

/// Evaluate oracle-backed params: the oracle decides when it can, otherwise
/// the standard gates apply.
pub(crate) fn evaluate(
    env: &Env,
    params: &TimeOraclePolicyParams,
    ctx: &PolicyContext,
) -> Result<(), Error> {
    match resolve(env, params, ctx) {
        Resolution::Admit => Ok(()),
        Resolution::Deny => Err(Error::OraclePolicyDenied),
        Resolution::Fallback => check_time_gates(params.window, params.deadline, ctx),
    }
}
