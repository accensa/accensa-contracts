#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup() -> (Env, InsurancePoolClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract = env.register(InsurancePool, (admin, token));
    let client = InsurancePoolClient::new(&env, &contract);
    (env, client, contract)
}

#[test]
fn missing_admin_returns_not_initialized() {
    let (env, client, contract) = setup();
    env.as_contract(&contract, || {
        env.storage().instance().remove(&DataKey::Admin);
    });

    assert_eq!(client.try_get_admin(), Err(Ok(Error::NotInitialized)));
}

#[test]
fn missing_token_returns_not_initialized() {
    let (env, client, contract) = setup();
    env.as_contract(&contract, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    assert_eq!(client.try_get_token(), Err(Ok(Error::NotInitialized)));
}
