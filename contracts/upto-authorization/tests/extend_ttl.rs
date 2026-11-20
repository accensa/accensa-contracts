//! Integration tests for extend_authorization_ttl non-panicking error behavior on stale and active records (issue #1033).

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};
use upto_authorization::{Error, UptoAuthorization, UptoAuthorizationClient};

const TOKEN_SUPPLY: i128 = 10_000_000;

#[allow(dead_code)]
struct TestContext {
    env: Env,
    client: UptoAuthorizationClient<'static>,
    admin: Address,
    buyer: Address,
    seller: Address,
    token: Address,
}

fn setup() -> TestContext {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();

    StellarAssetClient::new(&env, &token).mint(&buyer, &TOKEN_SUPPLY);

    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    client.initialize(&admin, &token);

    TestContext {
        env,
        client,
        admin,
        buyer,
        seller,
        token,
    }
}

fn sample_pid(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn test_extend_authorization_ttl_nonexistent_returns_typed_error() {
    let ctx = setup();
    let uninitialized_pid = sample_pid(&ctx.env, 0xee);

    // Extending TTL on missing record must return typed Error::AuthorizationNotFound without panicking
    let res = ctx.client.try_extend_authorization_ttl(&uninitialized_pid);
    assert_eq!(res, Err(Ok(Error::AuthorizationNotFound)));
}

#[test]
fn test_extend_authorization_ttl_active_authorization_succeeds() {
    let ctx = setup();
    let p = sample_pid(&ctx.env, 0x01);

    ctx.client
        .authorize(&p, &ctx.buyer, &ctx.seller, &5_000, &1_000);

    // Active authorization TTL can be extended successfully
    let res = ctx.client.try_extend_authorization_ttl(&p);
    assert_eq!(res, Ok(Ok(())));

    let record = ctx.client.get_authorization(&p);
    assert!(record.is_some());
    assert!(!record.unwrap().consumed);
}

#[test]
fn test_extend_authorization_ttl_settled_authorization_does_not_panic() {
    let ctx = setup();
    let p = sample_pid(&ctx.env, 0x02);

    ctx.client
        .authorize(&p, &ctx.buyer, &ctx.seller, &10_000, &1_000);

    // Settle the authorization
    ctx.env.ledger().with_mut(|li| li.sequence_number = 50);
    ctx.client.settle(&p, &4_500);

    // Record is consumed/settled
    let record = ctx.client.get_authorization(&p).expect("record must exist");
    assert!(record.consumed);

    // Automated keeper bots calling extend_authorization_ttl on settled records must not panic or trap
    let res = ctx.client.try_extend_authorization_ttl(&p);
    assert!(
        res.is_ok(),
        "calling extend_authorization_ttl on settled record must complete without panic"
    );
}

#[test]
fn test_extend_authorization_ttl_expired_and_pruned_returns_typed_error() {
    let ctx = setup();
    let p = sample_pid(&ctx.env, 0x03);

    // Authorize with expiry at ledger 100
    ctx.client
        .authorize(&p, &ctx.buyer, &ctx.seller, &2_000, &100);

    // Advance ledger past expiry
    ctx.env.ledger().with_mut(|li| li.sequence_number = 150);

    // Prune expired authorization to delete it
    ctx.client.prune_authorization(&p);

    // Calling extend_authorization_ttl on pruned/expired record must return typed error
    let res = ctx.client.try_extend_authorization_ttl(&p);
    assert_eq!(res, Err(Ok(Error::AuthorizationNotFound)));
}
