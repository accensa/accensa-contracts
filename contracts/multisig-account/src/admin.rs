//! Emergency pause circuit breaker for the multisig account.
//!
//! While paused the account refuses to authorize or execute anything except
//! its own recovery controls, so a compromised signer set cannot move funds
//! out of the contracts it administers during an incident. Recovery stays
//! possible: the pause controls and signer rotation remain callable, and
//! read-only queries are unaffected.
//!
//! `pause` / `unpause` may be called by:
//! - the account itself (i.e. `threshold` registered signers, verified by
//!   `__check_auth`), or
//! - the designated security guardian, if one is set via `set_guardian`.

use soroban_sdk::{auth::Context, contractevent, Address, Env, Symbol};

use crate::{DataKey, Error};

/// Emitted when the account is paused.
///
/// Topics: `("paused_event", ledger)`. The ledger sequence lets an indexer
/// reconstruct the pause window from the event log alone.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PausedEvent {
    #[topic]
    pub ledger: u32,
    /// Who triggered the pause: the account itself or the guardian.
    pub by: Address,
}

/// Emitted when the account is unpaused.
///
/// Topics: `("unpaused_event", ledger)`. Together with `PausedEvent` this
/// brackets a pause window in the event log.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnpausedEvent {
    #[topic]
    pub ledger: u32,
    /// Who lifted the pause: the account itself or the guardian.
    pub by: Address,
}

/// Emitted when the security guardian is set, replaced or cleared.
///
/// Topics: `("guardian_set_event",)`.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardianSetEvent {
    /// The previous guardian, if any.
    pub previous: Option<Address>,
    /// The guardian in force after the change; `None` means no guardian.
    pub new: Option<Address>,
}

/// Account functions that stay authorizable while paused: the pause
/// controls themselves and the signer/guardian rotation needed to recover
/// from a compromise.
const ALLOWED_WHILE_PAUSED: [&str; 4] = [
    "pause",
    "unpause",
    "set_guardian",
    "rotate_signers_and_threshold",
];

pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

/// Fail with [`Error::Paused`] if the circuit breaker is engaged. Call this
/// before any operation that executes or authorizes a transaction.
pub fn require_not_paused(env: &Env) -> Result<(), Error> {
    if is_paused(env) {
        return Err(Error::Paused);
    }
    Ok(())
}

pub fn get_guardian(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::PauseGuardian)
}

/// Set or clear the security guardian. Requires the account's own threshold
/// authorization; the guardian cannot appoint its own successor.
pub fn set_guardian(env: &Env, guardian: Option<Address>) {
    env.current_contract_address().require_auth();

    let previous = get_guardian(env);
    match &guardian {
        Some(g) => env.storage().instance().set(&DataKey::PauseGuardian, g),
        None => env.storage().instance().remove(&DataKey::PauseGuardian),
    }

    GuardianSetEvent {
        previous,
        new: guardian,
    }
    .publish(env);
}

pub fn pause(env: &Env, caller: Address) -> Result<(), Error> {
    require_pause_authority(env, &caller)?;
    env.storage().instance().set(&DataKey::Paused, &true);

    PausedEvent {
        ledger: env.ledger().sequence(),
        by: caller,
    }
    .publish(env);
    Ok(())
}

pub fn unpause(env: &Env, caller: Address) -> Result<(), Error> {
    require_pause_authority(env, &caller)?;
    env.storage().instance().set(&DataKey::Paused, &false);

    UnpausedEvent {
        ledger: env.ledger().sequence(),
        by: caller,
    }
    .publish(env);
    Ok(())
}

/// `caller` must be the account itself (threshold signers) or the guardian,
/// and must authorize this call.
fn require_pause_authority(env: &Env, caller: &Address) -> Result<(), Error> {
    let is_self = *caller == env.current_contract_address();
    let is_guardian = get_guardian(env).is_some_and(|g| g == *caller);
    if !is_self && !is_guardian {
        return Err(Error::Unauthorized);
    }
    caller.require_auth();
    Ok(())
}

/// Whether `__check_auth` may approve `context` while the account is paused.
/// Only calls to this account's own recovery functions qualify; every
/// outbound call (token transfers, admin calls on other contracts, ...) and
/// every contract creation is refused.
pub fn is_allowed_while_paused(env: &Env, context: &Context) -> bool {
    match context {
        Context::Contract(c) => {
            c.contract == env.current_contract_address()
                && ALLOWED_WHILE_PAUSED
                    .iter()
                    .any(|name| c.fn_name == Symbol::new(env, name))
        }
        _ => false,
    }
}
