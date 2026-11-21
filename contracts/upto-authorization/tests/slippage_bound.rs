//! Integration tests for slippage bound enforcement during settlement (issue #1031).

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};
use upto_authorization::{Error, UptoAuthorization, UptoAuthorizationClient};

const TOKEN_SUPPLY: i128 = 10_000_000;
const CAP: i128 = 1_000;
const SLIPPAGE_BPS: u32 = 500; // 5% slippage -> max = 1_050
const EXPIRY: u32 = 100_000;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: UptoAuthorizationClient<'static>,
    admin: Address,
    buyer: Address,
    seller: Address,
    token: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.sequence_number = 1_000);

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = sac.address();

    StellarAssetClient::new(&env, &token).mint(&buyer, &TOKEN_SUPPLY);

    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    client.initialize(&admin, &token);

    Fixture {
        env,
        client,
        admin,
        buyer,
        seller,
        token,
    }
}

fn payment_id(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn test_settle_rejects_amount_exceeding_slippage_bound() {
    let fx = setup();
    let pid = payment_id(&fx.env, 1);

    // Authorize with 500 bps (5%) slippage tolerance on cap 1,000 -> max settleable is 1,050
    fx.client
        .authorize_with_slippage(&pid, &fx.buyer, &fx.seller, &CAP, &EXPIRY, &SLIPPAGE_BPS);

    // Attempting to settle 1,051 exceeds cap + slippage bound -> AmountExceedsCap
    let res = fx.client.try_settle(&pid, &1_051);
    assert_eq!(res, Err(Ok(Error::AmountExceedsCap)));

    // Token balances remain untouched
    let token = TokenClient::new(&fx.env, &fx.token);
    assert_eq!(token.balance(&fx.seller), 0);
    assert_eq!(token.balance(&fx.buyer), TOKEN_SUPPLY);
}

#[test]
fn test_settle_succeeds_at_and_below_slippage_bound() {
    let fx = setup();
    let pid1 = payment_id(&fx.env, 2);

    // Authorize with 500 bps (5%) slippage tolerance on cap 1,000 -> max is 1,050
    fx.client
        .authorize_with_slippage(&pid1, &fx.buyer, &fx.seller, &CAP, &EXPIRY, &SLIPPAGE_BPS);

    // Settling exactly at the ceiling (1,050) succeeds
    fx.client.settle(&pid1, &1_050);

    let token = TokenClient::new(&fx.env, &fx.token);
    assert_eq!(token.balance(&fx.seller), 1_050);
    assert_eq!(token.balance(&fx.buyer), TOKEN_SUPPLY - 1_050);

    // Another authorization settled below the cap (900) succeeds
    let pid2 = payment_id(&fx.env, 3);
    fx.client
        .authorize_with_slippage(&pid2, &fx.buyer, &fx.seller, &CAP, &EXPIRY, &SLIPPAGE_BPS);
    fx.client.settle(&pid2, &900);
    assert_eq!(token.balance(&fx.seller), 1_050 + 900);
    assert_eq!(token.balance(&fx.buyer), TOKEN_SUPPLY - 1_050 - 900);
}
