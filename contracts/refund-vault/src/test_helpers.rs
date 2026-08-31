// Shared test helpers for the RefundVault suite (issue #129).
//
// Every test setup now constructs a `VaultInit` and wires the stateless
// policy contracts, because the vault delegates its active time/VDF gates to
// them. `vault_init` is the drop-in replacement for the legacy
// `client.initialize(&merchant, &token, &window)` helper: it registers the
// time and VDF policy contracts and seeds a zero-fee, window-only
// configuration, ready for `env.register(RefundVault, init)`. Tests that need
// a deadline, VDF delay or a fee configure the returned `VaultInit` before
// registering.

use accensa_common::VaultInit;
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use soroban_sdk::{Address, Env};

pub(crate) fn reg_time_policy(env: &Env) -> Address {
    env.register(TimePolicy, ())
}

pub(crate) fn reg_vdf_policy(env: &Env) -> Address {
    env.register(VdfPolicy, ())
}

/// A window-only, zero-fee vault configuration with both policy contracts
/// wired. Equivalent to the legacy `initialize(merchant, token, window)`.
pub(crate) fn vault_init(env: &Env, merchant: &Address, token: &Address, window: u32) -> VaultInit {
    VaultInit {
        merchant: merchant.clone(),
        token: token.clone(),
        time_policy: Some(reg_time_policy(env)),
        vdf_policy: Some(reg_vdf_policy(env)),
        fee_bps: 0,
        fee_recipient: None,
        refund_window: window,
        deadline: 0,
        vdf_delay: 0,
    }
}
