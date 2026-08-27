#![no_std]

use accensa_common::Error;
use soroban_sdk::{contract, contractimpl, contractmeta, Bytes, BytesN, Env};

contractmeta!(key = "name", val = "RefundWindowPolicy");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);
contractmeta!(key = "commit", val = env!("GIT_SHA"));

contractmeta!(key = "commit_dirty", val = env!("GIT_DIRTY"));

/// The stateless, default refund policy: a refund is permitted iff the current
/// ledger is within `refund_window_ledgers` of the payment's `paid_at_ledger`.
///
/// The vault (not this contract) holds the window and every input the rule
/// needs, and passes them all in per call — so the policy keeps no storage and
/// no state of its own. `refund_window_ledgers == 0` means "no time bound",
/// mirroring the vault's historic `initialize` semantics.
///
/// This is a *separate, callable contract* the vault routes to (see
/// `refund_vault::refund`). New policy kinds — percentage caps, blacklists,
/// channel-scoped rules — are new contracts implementing the same
/// `check_refund` signature; a vault is pointed at one by passing its address
/// to `RefundVaultFactory::deploy_with_policy` or `RefundVault::set_refund_policy`.
/// Nothing in the vault's wasm needs to change to support them.
#[contract]
pub struct RefundWindowPolicy;

#[contractimpl]
impl RefundWindowPolicy {
    /// Decide whether a refund of `amount` against a payment is allowed.
    ///
    /// Every parameter is supplied by the calling vault — the policy only
    /// evaluates the rule and returns:
    /// * `Ok(())` if the refund is permitted;
    /// * `Err(Error::WindowExpired)` if `current_ledger > paid_at_ledger +
    ///   refund_window_ledgers`.
    ///
    /// `payment_ref`, `amount`, `payment_amount` and `cumulative_refunded` are
    /// not used by the window rule itself, but form the fixed interface any
    /// policy must accept so the vault can dispatch to a policy of a different
    /// rule without changing the call shape.
    pub fn check_refund(
        env: Env,
        _payment_ref: BytesN<32>,
        _amount: i128,
        paid_at_ledger: u32,
        _payment_amount: i128,
        _cumulative_refunded: i128,
        refund_window_ledgers: u32,
    ) -> Result<(), Error> {
        if refund_window_ledgers > 0 {
            let current_ledger = env.ledger().sequence();
            if current_ledger > paid_at_ledger + refund_window_ledgers {
                return Err(Error::WindowExpired);
            }
        }
        Ok(())
    }

    /// Human-readable name of this policy, for indexers and admins inspecting
    /// which contract a vault is bound to.
    pub fn get_policy_name(env: Env) -> Bytes {
        Bytes::from_slice(&env, b"RefundWindowPolicy")
    }
}

mod test;