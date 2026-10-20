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

/// Whether the emergency circuit breaker is currently engaged.
///
/// Reads the `Paused` instance-storage flag, defaulting to `false` when unset.
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

/// The designated security guardian, or `None` when no guardian is set.
///
/// Reads the `PauseGuardian` instance-storage key maintained by `set_guardian`.
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

/// Engage the circuit breaker: store `Paused = true` and emit [`PausedEvent`].
///
/// Pre-condition: `caller` is the account itself (threshold signers) or the
/// guardian, and authorizes the call — enforced by `require_pause_authority`,
/// whose sole error arm is `Error::Unauthorized`.
pub fn pause(env: &Env, caller: Address) -> Result<(), Error> {
    require_pause_authority(env, &caller)?;
    if is_paused(env) {
        return Err(Error::Paused);
    }
    env.storage().instance().set(&DataKey::Paused, &true);

    PausedEvent {
        ledger: env.ledger().sequence(),
        by: caller,
    }
    .publish(env);
    Ok(())
}

/// Lift the circuit breaker: store `Paused = false` and emit [`UnpausedEvent`].
///
/// Pre-condition and error arms are identical to `pause`: `caller` must be the
/// account itself or the guardian and must authorize, else `Error::Unauthorized`
/// from `require_pause_authority`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MultisigAccount;
    use soroban_sdk::{
        testutils::{Address as _, Events as _},
        vec, Address, BytesN, Env, Event,
    };

    #[test]
    fn pause_unpause_and_guardian_events_match_their_payloads() {
        let env = Env::default();
        env.mock_all_auths();
        let signer = Address::generate(&env);
        let contract = env.register(MultisigAccount, (vec![&env, signer], 1u32));
        let client = crate::MultisigAccountClient::new(&env, &contract);
        let guardian = Address::generate(&env);

        client.set_guardian(&Some(guardian.clone()));
        let events = env.events().all().filter_by_contract(&contract);
        assert_eq!(events.events().len(), 1);
        let expected = GuardianSetEvent {
            previous: None,
            new: Some(guardian.clone()),
        }
        .to_xdr(&env, &contract);
        assert_eq!(&events.events()[0], &expected);

        client.pause(&guardian);
        let events = env.events().all().filter_by_contract(&contract);
        assert_eq!(events.events().len(), 1);
        let expected_pause = PausedEvent {
            ledger: env.ledger().sequence(),
            by: guardian.clone(),
        }
        .to_xdr(&env, &contract);
        assert_eq!(&events.events()[0], &expected_pause);

        assert_eq!(client.try_pause(&guardian), Err(Ok(Error::Paused)));
        let events = env.events().all().filter_by_contract(&contract);
        assert!(events.events().iter().all(|event| event != &expected_pause));

        client.unpause(&guardian);
        let events = env.events().all().filter_by_contract(&contract);
        assert_eq!(events.events().len(), 1);
        let expected = UnpausedEvent {
            ledger: env.ledger().sequence(),
            by: guardian.clone(),
        }
        .to_xdr(&env, &contract);
        assert_eq!(&events.events()[0], &expected);

        let recorded_guardian = client.get_guardian().unwrap();
        assert_eq!(recorded_guardian, guardian);
        env.as_contract(&contract, || {
            let queue_id = crate::timelock::queue_transaction(
                &env,
                BytesN::from_array(&env, &[1u8; 32]),
                10,
                1,
                recorded_guardian,
            );
            assert!(crate::timelock::cancel_queued_transaction(&env, queue_id, &guardian).is_ok());
        });
    }
}
