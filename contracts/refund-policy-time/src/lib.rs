//! Stateless **time** refund policy (issue #129).
//!
//! Evaluates the two clock-based gates that a `RefundVault` historically
//! applied inline:
//!
//! - the **window**: a claim is rejected once `current_ledger` exceeds
//!   `paid_at_ledger + window` (measured from the payment, never from a
//!   partial);
//! - the **deadline**: a wall-clock Unix timestamp after which claims are
//!   rejected (strictly past the deadline; a claim landing exactly on it
//!   succeeds). `0` disables either gate.
//!
//! The contract is fully stateless: configuration arrives as the `params`
//! blob of a [`accensa_common::PolicyEntry`] (an
//! [`accensa_common::TimePolicyParams`] XDR blob) and the claim facts arrive
//! as [`accensa_common::PolicyContext`]. It keeps no storage and must not
//! call back into the vault.
//!
//! One deployed instance serves every vault that points its time gate at it.
//!
//! The `params` blob may instead be an [`oracle::TimeOraclePolicyParams`],
//! which adds a delivery oracle that can resolve a dispute before the clock
//! gates are consulted (issue #426); see [`oracle`].

#![no_std]

use accensa_common::{Error, PolicyContext, RefundPolicy, TimePolicyParams};
use soroban_sdk::{
    contract, contractimpl, symbol_short, xdr::FromXdr, Bytes, Env, Map, Symbol, TryFromVal, Val,
};

#[contract]
pub struct TimePolicy;

pub mod oracle;
#[cfg(test)]
mod oracle_test;
#[cfg(test)]
mod test;

#[contractimpl]
impl RefundPolicy for TimePolicy {
    /// Rejects a claim that is outside the configured window or past the
    /// configured deadline.
    ///
    /// Always returns `Ok(())` when the gate is disabled in the params
    /// (`window == 0 && deadline == 0`) — a vault only emits a time entry
    /// when at least one of them is set, so this is defensive only.
    ///
    /// When `params` decodes as [`oracle::TimeOraclePolicyParams`] the
    /// attached oracle is consulted first (see [`oracle`]).
    fn evaluate(env: Env, params: Bytes, ctx: PolicyContext) -> Result<(), Error> {
        // Decoding into the wrong struct shape traps the host instead of
        // returning `Err`, so pick the schema by its distinguishing field.
        let raw = Val::from_xdr(&env, &params).map_err(|_| Error::InvalidPolicyParams)?;
        let fields =
            Map::<Symbol, Val>::try_from_val(&env, &raw).map_err(|_| Error::InvalidPolicyParams)?;
        if fields.contains_key(symbol_short!("oracle")) {
            let p = oracle::TimeOraclePolicyParams::try_from_val(&env, &raw)
                .map_err(|_| Error::InvalidPolicyParams)?;
            return oracle::evaluate(&env, &p, &ctx);
        }
        let p =
            TimePolicyParams::try_from_val(&env, &raw).map_err(|_| Error::InvalidPolicyParams)?;
        oracle::check_time_gates(p.window, p.deadline, &ctx)
    }
}
mod state_transition;
pub use state_transition::*;
