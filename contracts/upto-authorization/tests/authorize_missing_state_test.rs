#![cfg(test)]

//! Test verifying that authorize_with_slippage reports Error::NotInitialized
//! when DataKey::Token is absent from instance storage.

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};
use upto_authorization::{DataKey, Error, UptoAuthorization, UptoAuthorizationClient};

#[test]
fn test_authorize_with_slippage_missing_token_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.sequence_number = 1_000);

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token = Address::generate(&env);

    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    client.initialize(&admin, &token);

    // Remove DataKey::Token to simulate uninitialized/missing Token state
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    let payment_id = BytesN::from_array(&env, &[1u8; 32]);
    let res =
        client.try_authorize_with_slippage(&payment_id, &buyer, &seller, &1000, &100_000, &500);
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}
