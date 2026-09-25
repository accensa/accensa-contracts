//! Protocol TVL (Total Value Locked) query (issue #464).
//!
//! Analytics entrypoint for DefiLlama-style dashboards: [`get_tvl`] answers
//! "how much of `asset` is currently locked in every vault this factory
//! deployed?" in a single read-only call.
//!
//! # How it counts
//!
//! The factory already tracks every vault it minted in its `KEY_VAULTS` list
//! (see [`RefundVaultFactory::get_vaults`]). TVL is the sum of the `asset`
//! balance of each tracked vault, read straight from the SEP-41 token
//! contract — the vault's own bookkeeping is never trusted, so the figure is
//! the same one a token explorer would report.
//!
//! A vault that was configured with a *different* token simply holds no
//! `asset`, so the token contract reports `0` for it and it contributes
//! nothing. Callers therefore do not need to know which vaults hold which
//! token, and a factory can safely host vaults across many assets.
//!
//! This is a pure read: it mutates no state and holds no lock, so it is safe
//! to call from an indexer or an off-chain dashboard at any time.

use soroban_sdk::{contractimpl, token, Address, Env};

use crate::{RefundVaultFactory, RefundVaultFactoryArgs, RefundVaultFactoryClient};

#[contractimpl]
impl RefundVaultFactory {
    /// Read-only: the total `asset` balance locked across every vault this
    /// factory has deployed.
    ///
    /// Sums [`soroban_sdk::token::Client::balance`] over
    /// [`RefundVaultFactory::get_vaults`], so the result is exactly the value
    /// those vaults custody on-chain at the queried ledger. Returns `0` when
    /// the factory has deployed no vaults, or when none of them hold `asset`.
    ///
    /// The addition is saturating: an (unreachable in practice) total above
    /// `i128::MAX` clamps rather than panicking, because a read-only analytics
    /// query must never be able to abort a caller's transaction.
    pub fn get_tvl(env: Env, asset: Address) -> i128 {
        let token = token::Client::new(&env, &asset);
        let mut total: i128 = 0;
        for vault in Self::get_vaults(env.clone()).iter() {
            total = total.saturating_add(token.balance(&vault));
        }
        total
    }
}
