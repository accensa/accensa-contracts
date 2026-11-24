use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig},
    token::StellarAssetClient,
    Address, BytesN, Env,
};
use upto_authorization::{DataKey, Error, UptoAuthorization, UptoAuthorizationClient};

#[test]
fn settle_with_missing_token_returns_not_initialized() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();

    StellarAssetClient::new(&env, &token).mint(&buyer, &10_000_000);

    let contract_id = env.register(UptoAuthorization, ());
    let client = UptoAuthorizationClient::new(&env, &contract_id);
    client.initialize(&admin, &token);

    let payment_id = BytesN::from_array(&env, &[42u8; 32]);
    let cap = 1_000i128;
    let expiry = 100u32;

    client.authorize(&payment_id, &buyer, &seller, &cap, &expiry);

    // Remove Token key from instance storage to simulate missing token state
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Token);
    });

    // settle returns typed Error::NotInitialized instead of panicking
    assert_eq!(
        client.try_settle(&payment_id, &500i128),
        Err(Ok(Error::NotInitialized))
    );
}
