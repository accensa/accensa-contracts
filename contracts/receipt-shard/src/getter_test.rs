#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Env,
};

#[test]
fn test_get_router_uninitialised_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router.clone(), 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // get_router should return Err(Error::NotInitialized), not panic
    assert_eq!(client.get_router(), Err(Error::NotInitialized));
}

#[test]
fn test_get_range_uninitialised_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router.clone(), 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // get_range should return Err(Error::NotInitialized), not panic
    assert_eq!(client.get_range(), Err(Error::NotInitialized));
}

#[test]
fn test_get_router_after_eviction_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router.clone(), 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // Advance past the TTL durability window (30 days ~ 5s/ledger = 518400 ledgers)
    let seq = env.ledger().sequence();
    env.ledger().with_mut(|li| li.sequence_number = seq + 600_000);
    // get_router should still return Err(Error::NotInitialized) after eviction
    assert_eq!(client.get_router(), Err(Error::NotInitialized));
}

#[test]
fn test_get_range_after_eviction_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router.clone(), 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // Advance past the TTL durability window
    let seq = env.ledger().sequence();
    env.ledger().with_mut(|li| li.sequence_number = seq + 600_000);
    // get_range should still return Err(Error::NotInitialized) after eviction
    assert_eq!(client.get_range(), Err(Error::NotInitialized));
}

#[test]
fn test_get_router_normal_path_returns_value() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let (start, end) = (1u64, 1001u64);
    let contract_id = env.register(ReceiptShard, (router.clone(), start, end));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // Normal path should return the router address
    assert_eq!(client.get_router(), Ok(router));
}

#[test]
fn test_get_range_normal_path_returns_value() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let (start, end) = (1u64, 1001u64);
    let contract_id = env.register(ReceiptShard, (router.clone(), start, end));
    let client = ReceiptShardClient::new(&env, &contract_id);
    // Normal path should return the range
    assert_eq!(client.get_range(), Ok((start, end)));
}