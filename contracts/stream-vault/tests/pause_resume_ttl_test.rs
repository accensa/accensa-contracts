use soroban_sdk::{
    testutils::{storage::Instance as _, Address as _, Ledger},
    token, Address, Env,
};
use stream_vault::{StreamVault, StreamVaultClient};

fn setup() -> (Env, StreamVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(50);

    let merchant = Address::generate(&env);
    let buyer = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin);
    token::StellarAssetClient::new(&env, &token.address()).mint(&buyer, &10_000);

    let contract_id = env.register(StreamVault, (merchant, token.address()));
    let client = StreamVaultClient::new(&env, &contract_id);
    (env, client, buyer)
}

fn instance_ttl(env: &Env, client: &StreamVaultClient<'_>) -> u32 {
    env.as_contract(&client.address, || env.storage().instance().get_ttl())
}

fn age_instance_to_low_water(env: &Env, client: &StreamVaultClient<'_>) {
    let ttl = instance_ttl(env, client);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl - 50);
    assert!(instance_ttl(env, client) <= 50);
}

#[test]
fn pause_and_resume_extend_the_instance_ttl() {
    let (env, client, buyer) = setup();
    let stream_id = client.create_stream(&buyer, &100, &1_000_000, &1, &10_000);

    age_instance_to_low_water(&env, &client);
    client.pause_stream(&stream_id);
    assert!(client.get_stream(&stream_id).unwrap().paused_at.is_some());
    assert!(instance_ttl(&env, &client) >= 518_400);

    age_instance_to_low_water(&env, &client);
    client.resume_stream(&stream_id);
    assert_eq!(client.get_stream(&stream_id).unwrap().paused_at, None);
    assert!(instance_ttl(&env, &client) >= 518_400);
}
