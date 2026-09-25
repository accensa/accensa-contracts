//! Reusable Soroban storage TTL auto-extension helper (issue #373).
//!
//! On Mainnet/Testnet a contract *instance* (and every persistent entry it
//! owns) is archived once its TTL lapses, and restoring it costs rent — or,
//! for an instance, is impossible without the restore operation. Every
//! stateful entry point that writes storage must therefore bump the instance
//! TTL as part of the same call, so a contract that keeps being used never
//! silently archives its own configuration.
//!
//! [`extend_instance_ttl`] is the single, shared implementation of that bump.
//! The two tunables are exposed as constants so every contract defaults to
//! the same policy while still being able to pass site-specific values:
//!
//! - [`DEFAULT_TTL_LOW_WATER`]: the instance TTL is only extended once it
//!   decays to this many ledgers or fewer (the host's `threshold`), so calls
//!   in consecutive ledgers do not pay for redundant extension attempts.
//! - [`DEFAULT_TTL_BUMP`]: the TTL is extended *to* this many ledgers from
//!   the current one (the host's `extend_to`) — ~30 days at ~5 s/ledger.

use soroban_sdk::Env;

/// Low-water mark (host `threshold`): the extension only fires once the
/// instance's remaining TTL is at or below this many ledgers (~8 minutes at
/// ~5 s/ledger), so per-call bumps are cheap and rarely taken.
pub const DEFAULT_TTL_LOW_WATER: u32 = 100;

/// Extension target (host `extend_to`): extend the instance TTL to ~30 days
/// from the current ledger. `60 * 60 * 24 * 30 / 5 = 518_400`.
pub const DEFAULT_TTL_BUMP: u32 = 518_400;

/// Extend the **instance** storage TTL of the current contract.
///
/// `low_water` is the host threshold — the bump only happens when the
/// remaining TTL is at or below it — and `bump_to` is the TTL to extend to,
/// counted from the current ledger. The host rejects `threshold > extend_to`,
/// so `low_water` is clamped to `bump_to` here rather than trapping deep in
/// the host for what is a caller slip, not an attack.
///
/// Call this at the end of every state-changing entry point, after the
/// invocation's storage writes are in place, so the instance carrying those
/// writes cannot archive while the contract is live. See
/// [`extend_instance_ttl_default`] for the shared policy defaults.
pub fn extend_instance_ttl(env: &Env, low_water: u32, bump_to: u32) {
    let low_water = low_water.min(bump_to);
    env.storage().instance().extend_ttl(low_water, bump_to);
}

/// [`extend_instance_ttl`] with the shared default constants
/// ([`DEFAULT_TTL_LOW_WATER`] / [`DEFAULT_TTL_BUMP`]).
pub fn extend_instance_ttl_default(env: &Env) {
    extend_instance_ttl(env, DEFAULT_TTL_LOW_WATER, DEFAULT_TTL_BUMP);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{storage::Instance as _, Ledger as _},
        Env,
    };

    /// Minimal contract whose only job is to run the helper in-contract, so
    /// the tests exercise it exactly the way a real entry point does.
    #[contract]
    struct TtlProbe;

    #[contractimpl]
    impl TtlProbe {
        pub fn touch(env: Env) {
            extend_instance_ttl_default(&env);
        }
    }

    /// A freshly written instance carries the network's
    /// `min_persistent_entry_ttl` floor (4,096 ledgers in the mock env),
    /// which is far above the low-water mark — so the helper must be a no-op
    /// and leave the TTL exactly where it was.
    #[test]
    fn instance_ttl_is_a_no_op_above_the_low_water_mark() {
        let env = Env::default();
        let id = env.register(TtlProbe, ());
        let client = TtlProbeClient::new(&env, &id);

        let fresh = env.as_contract(&id, || env.storage().instance().get_ttl());
        assert!(
            fresh >= DEFAULT_TTL_LOW_WATER,
            "fresh instance TTL ({fresh}) must start above the low-water mark"
        );

        client.touch();

        let after = env.as_contract(&id, || env.storage().instance().get_ttl());
        assert_eq!(
            after, fresh,
            "TTL above the low-water mark must not be extended (no-op)"
        );
    }

    /// Once the instance TTL decays to the low-water mark, the helper must
    /// bump it all the way to the configured extension target.
    #[test]
    fn instance_ttl_bumps_to_the_target_below_the_low_water_mark() {
        let env = Env::default();
        let id = env.register(TtlProbe, ());
        let client = TtlProbeClient::new(&env, &id);

        // Age the instance until only half the low-water mark remains.
        let fresh = env.as_contract(&id, || env.storage().instance().get_ttl());
        env.ledger()
            .with_mut(|li| li.sequence_number = fresh - DEFAULT_TTL_LOW_WATER / 2);

        let before = env.as_contract(&id, || env.storage().instance().get_ttl());
        assert!(
            before <= DEFAULT_TTL_LOW_WATER,
            "TTL ({before}) must be at or below the low-water mark before the bump"
        );

        client.touch();

        let after = env.as_contract(&id, || env.storage().instance().get_ttl());
        assert!(
            after >= DEFAULT_TTL_BUMP,
            "TTL ({after}) must be bumped to at least DEFAULT_TTL_BUMP ({DEFAULT_TTL_BUMP})"
        );
    }

    /// `threshold > extend_to` is invalid host input; the helper clamps
    /// instead of trapping so a misconfigured call site degrades gracefully.
    #[test]
    fn threshold_above_target_is_clamped() {
        let env = Env::default();
        let id = env.register(TtlProbe, ());

        // Must not panic: low_water is clamped down to bump_to.
        env.as_contract(&id, || extend_instance_ttl(&env, 1_000, 500));
    }
}
