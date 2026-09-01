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

#![no_std]

use accensa_common::{Error, PolicyContext, RefundPolicy, TimePolicyParams};
use soroban_sdk::{contract, contractimpl, xdr::FromXdr, Bytes, Env};

#[contract]
pub struct TimePolicy;

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
    fn evaluate(env: Env, params: Bytes, ctx: PolicyContext) -> Result<(), Error> {
        let p =
            TimePolicyParams::from_xdr(&env, &params).map_err(|_| Error::InvalidPolicyParams)?;

        if p.window > 0 && ctx.current_ledger > ctx.paid_at_ledger + p.window {
            return Err(Error::WindowExpired);
        }

        if p.deadline > 0 && ctx.timestamp > p.deadline {
            return Err(Error::RefundExpired);
        }

        Ok(())
    }
}
