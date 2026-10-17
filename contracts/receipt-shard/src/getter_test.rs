//! Unit tests for the typed-error config getters (issue #642).
//!
//! `get_router` and `get_range` used to `unwrap` their instance-storage
//! reads, so a call before the shard carried any configuration — or after the
//! instance's durability window had elapsed — aborted the host with a generic
//! error instead of answering with the crate's typed `NotInitialized`. These
//! cases pin the typed error on both paths and prove the happy path still
//! returns exactly what it did before the change.

#![cfg(test)]

use crate::{DataKey, Error, ReceiptShard, ReceiptShardClient, TTL_EXTEND, TTL_THRESHOLD};
use soroban_sdk::testutils::{storage::Instance as _, Address as _, Ledger};
use soroban_sdk::{Address, Env};

/// Registers a shard and then drops the construction-time config keys from
/// instance storage, modelling a shard that is installed but uninitialised.
/// The constructor runs atomically with deployment, so this is the state a
/// reader observes before construction writes land (or after the instance
/// entry itself has aged out): every config read sees `None`.
fn uninitialised_shard() -> (Env, ReceiptShardClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Router);
        env.storage().instance().remove(&DataKey::StartBatchId);
        env.storage().instance().remove(&DataKey::EndBatchId);
    });
    (env, client)
}

/// (a) Both getters on a freshly installed but uninitialised shard answer
/// with `NotInitialized` instead of aborting the host call.
#[test]
fn test_getters_on_uninitialised_shard_return_typed_error() {
    let (_env, client) = uninitialised_shard();
    assert_eq!(
        client.try_get_router(),
        Err(Ok(Error::NotInitialized)),
        "get_router must answer with the typed error, not a host abort"
    );
    assert_eq!(
        client.try_get_range(),
        Err(Ok(Error::NotInitialized)),
        "get_range must answer with the typed error, not a host abort"
    );
}

/// (a, second read site) `get_range` unwraps two keys; losing only the
/// second one must surface the same typed error rather than reaching the
/// old `unwrap` on `EndBatchId`.
#[test]
fn test_get_range_missing_end_batch_id_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::EndBatchId);
    });
    assert_eq!(client.try_get_range(), Err(Ok(Error::NotInitialized)));
}

/// (b) Once the ledger advances past the instance's durability window the
/// config is gone; both getters must answer with the same typed error, not
/// abort.
#[test]
fn test_getters_after_instance_ttl_window_return_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);

    // Age the ledger to the very end of the instance's durability window and
    // prove the window has fully elapsed (no ledgers of TTL left).
    let live_until = env.ledger().sequence()
        + env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    env.ledger().with_mut(|li| li.sequence_number = live_until);
    assert_eq!(
        env.as_contract(&contract_id, || env.storage().instance().get_ttl()),
        0,
        "the ledger must be at the end of the instance's durability window"
    );

    // The mock host does not run the network's archival job: an entry whose
    // live-until ledger has passed stays readable in-process. Model the
    // archive a real network performs at that point — the instance's config
    // is gone — and prove the getters answer with the typed error instead of
    // aborting the host call.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Router);
        env.storage().instance().remove(&DataKey::StartBatchId);
        env.storage().instance().remove(&DataKey::EndBatchId);
    });

    assert_eq!(
        client.try_get_router(),
        Err(Ok(Error::NotInitialized)),
        "post-eviction get_router must answer with the typed error"
    );
    assert_eq!(
        client.try_get_range(),
        Err(Ok(Error::NotInitialized)),
        "post-eviction get_range must answer with the typed error"
    );
}

/// (c) The normal path still returns the same values as before the change —
/// the constructor-recorded router and the assigned half-open range.
#[test]
fn test_getters_happy_path_returns_constructor_values() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let (start, end) = (1u64, 1001u64);
    let contract_id = env.register(ReceiptShard, (router.clone(), start, end));
    let client = ReceiptShardClient::new(&env, &contract_id);

    assert_eq!(client.get_router(), router);
    assert_eq!(client.get_range(), (start, end));
}

/// A successful read extends the instance TTL the same way this file's write
/// paths do (`extend_ttl(TTL_THRESHOLD, TTL_EXTEND)`): once the window has
/// decayed to the low-water mark, a read bumps it back out.
#[test]
fn test_successful_getter_read_extends_instance_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router, 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);

    // Age the instance until only half the low-water mark remains, exactly
    // as the shared TTL helper's tests do.
    let fresh = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    env.ledger()
        .with_mut(|li| li.sequence_number = fresh - TTL_THRESHOLD / 2);
    let before = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(
        before <= TTL_THRESHOLD,
        "instance TTL ({before}) must be at or below TTL_THRESHOLD ({TTL_THRESHOLD}) before the read"
    );

    client.get_router();

    let after = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(
        after >= TTL_EXTEND,
        "a successful get_router read must extend the instance TTL to TTL_EXTEND ({TTL_EXTEND}), got {after}"
    );
}
